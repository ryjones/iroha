/**
 * Copyright Soramitsu Co., Ltd. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#include <gtest/gtest.h>

#include "common/result.hpp"
#include "datetime/time.hpp"
#include "framework/crypto_literals.hpp"
#include "framework/result_gtest_checkers.hpp"
#include "framework/test_logger.hpp"
#include "module/irohad/ametsuchi/mock_tx_presence_cache.hpp"
#include "module/irohad/multi_sig_transactions/mst_test_helpers.hpp"
#include "multi_sig_transactions/state/mst_state.hpp"
#include "pending_txs_storage/impl/pending_txs_storage_impl.hpp"

class PendingTxsStorageFixture : public ::testing::Test {
 public:
  using Batch = shared_model::interface::TransactionBatch;
  using BatchInfo =
      shared_model::interface::PendingTransactionsPageResponse::BatchInfo;
  using Response = iroha::PendingTransactionStorage::Response;
  using ErrorCode = iroha::PendingTransactionStorage::ErrorCode;

  /**
   * Get the closest to now timestamp from the future but never return the same
   * value twice.
   * @return iroha timestamp
   */
  iroha::time::time_t getUniqueTime() {
    static iroha::time::time_t latest_timestamp = 0;
    auto now = iroha::time::now();
    if (now > latest_timestamp) {
      latest_timestamp = now;
      return now;
    } else {
      return ++latest_timestamp;
    }
  }

  auto emptyState() {
    return std::make_shared<iroha::MstState>(
        iroha::MstState::empty(mst_state_log_, completer_));
  }

  auto twoTransactionsBatch() {
    return addSignatures(
        makeTestBatch(txBuilder(2, getUniqueTime(), 2, "alice@iroha"),
                      txBuilder(2, getUniqueTime(), 2, "bob@iroha")),
        0,
        makeSignature("1"_hex_sig, "pub_key_1"_hex_pubkey));
  }

  void checkResponse(const Response &actual, const Response &expected) {
    EXPECT_EQ(actual.transactions.size(), expected.transactions.size());
    // generally it's illegal way to verify the correctness.
    // here we can do it because the order is preserved by batch meta and
    // there are no transactions non-related to requested account
    for (auto i = 0u; i < expected.transactions.size(); ++i) {
      EXPECT_EQ(*actual.transactions[i], *expected.transactions[i]);
    }
    EXPECT_EQ(actual.all_transactions_size, expected.all_transactions_size);
    if (expected.next_batch_info) {
      EXPECT_TRUE(actual.next_batch_info);
      EXPECT_EQ(actual.next_batch_info->first_tx_hash,
                expected.next_batch_info->first_tx_hash);
      EXPECT_EQ(actual.next_batch_info->batch_size,
                expected.next_batch_info->batch_size);
    } else {
      EXPECT_FALSE(actual.next_batch_info);
    }
  }

  void SetUp() override {
    storage_ = std::make_shared<iroha::PendingTransactionStorageImpl>();
    presence_cache_ = std::make_shared<iroha::ametsuchi::MockTxPresenceCache>();
    storage_->insertPresenceCache(presence_cache_);
  }

  std::shared_ptr<iroha::ametsuchi::TxPresenceCache> presence_cache_;
  std::shared_ptr<iroha::PendingTransactionStorageImpl> storage_;
  std::shared_ptr<iroha::DefaultCompleter> completer_ =
      std::make_shared<iroha::DefaultCompleter>(std::chrono::minutes(0));

  logger::LoggerPtr mst_state_log_{getTestLogger("MstState")};
  logger::LoggerPtr log_{getTestLogger("PendingTxsStorageFixture")};
};

/**
 * Test that checks that fixture common preparation procedures can be done
 * successfully.
 * @given empty MST state
 * @when two mst transactions generated as batch
 * @then the transactions can be added to MST state successfully
 */
TEST_F(PendingTxsStorageFixture, FixtureSelfCheck) {
  auto state = emptyState();
  auto transactions = twoTransactionsBatch();
  *state += transactions;
  ASSERT_EQ(state->getBatches().size(), 1) << "Failed to prepare MST state";
  ASSERT_EQ((*state->getBatches().begin())->transactions().size(), 2)
      << "Test batch contains wrong amount of transactions";
}

/**
 * Transactions insertion works in PendingTxsStorage
 * @given Batch of two transactions and storage
 * @when storage receives updated mst state with the batch
 * @then list of pending transactions can be received for all batch creators
 */
TEST_F(PendingTxsStorageFixture, InsertionTest) {
  auto state = emptyState();
  auto transactions = twoTransactionsBatch();
  *state += transactions;

  const auto kPageSize = 100u;
  Response expected;
  expected.transactions.insert(expected.transactions.end(),
                               transactions->transactions().begin(),
                               transactions->transactions().end());
  expected.all_transactions_size = transactions->transactions().size();

  storage_->updatedBatchesHandler(state);
  for (const auto &creator : {"alice@iroha", "bob@iroha"}) {
    auto pending =
        storage_->getPendingTransactions(creator, kPageSize, std::nullopt);
    IROHA_ASSERT_RESULT_VALUE(pending);
    checkResponse(pending.assumeValue(), expected);
  }
}

/**
 * All the transactions can be received when exact page size is specified
 * @given a storage with a batch with two transactions
 * @when pending transactions are queired with page size equal to the batch size
 * @then all the transactions are correctly returned
 */
TEST_F(PendingTxsStorageFixture, ExactSize) {
  auto state = emptyState();
  auto transactions = twoTransactionsBatch();
  *state += transactions;

  const auto kPageSize = transactions->transactions().size();
  Response expected;
  expected.transactions.insert(expected.transactions.end(),
                               transactions->transactions().begin(),
                               transactions->transactions().end());
  expected.all_transactions_size = transactions->transactions().size();

  storage_->updatedBatchesHandler(state);
  for (const auto &creator : {"alice@iroha", "bob@iroha"}) {
    auto pending =
        storage_->getPendingTransactions(creator, kPageSize, std::nullopt);
    IROHA_ASSERT_RESULT_VALUE(pending);
    checkResponse(pending.assumeValue(), expected);
  }
}

/**
 * All the transactions appeared in a proposal from pcs are not pending anymore
 * @given A storage with a batch of two transactions
 * @when a pcs emits an avent about received proposal
 * @then all the mentioned txs have to be removed from MST's pending txs storage
 */
TEST_F(PendingTxsStorageFixture, CompletedTransactionsAreRemoved) {
  auto state = emptyState();
  auto transactions = twoTransactionsBatch();
  *state += transactions;

  const auto kPageSize = transactions->transactions().size();

  storage_->updatedBatchesHandler(state);
  storage_->removeBatch(transactions);
  Response empty_response;
  for (const auto &creator : {"alice@iroha", "bob@iroha"}) {
    auto pending =
        storage_->getPendingTransactions(creator, kPageSize, std::nullopt);
    IROHA_ASSERT_RESULT_VALUE(pending);
    checkResponse(pending.assumeValue(), empty_response);
  }
}

/**
 * Correctly formed response is returned when queried page size smaller than the
 * size of the smallest batch
 * @given a storage with a batch with two transactions
 * @when pending transactions are queired with page size equal to 1
 * @then no transactions are returned, but all the meta is correctly set
 */
TEST_F(PendingTxsStorageFixture, InsufficientSize) {
  auto state = emptyState();
  auto transactions = twoTransactionsBatch();
  *state += transactions;

  const auto kPageSize = 1;
  ASSERT_NE(kPageSize, transactions->transactions().size());
  Response expected;
  expected.all_transactions_size = transactions->transactions().size();
  expected.next_batch_info = BatchInfo{};
  expected.next_batch_info->first_tx_hash =
      transactions->transactions().front()->hash();
  expected.next_batch_info->batch_size = transactions->transactions().size();

  storage_->updatedBatchesHandler(state);
  for (const auto &creator : {"alice@iroha", "bob@iroha"}) {
    auto pending =
        storage_->getPendingTransactions(creator, kPageSize, std::nullopt);
    IROHA_ASSERT_RESULT_VALUE(pending);
    checkResponse(pending.assumeValue(), expected);
  }
}

/**
 * Correctly formed response is returned when there are two batches are in the
 * storage and the page size is bigger than the size of the first batch and
 * smaller than the sum of the first and the second batches sizes.
 */
TEST_F(PendingTxsStorageFixture, BatchAndAHalfPageSize) {
  auto state1 = emptyState();
  auto state2 = emptyState();
  auto batch1 = twoTransactionsBatch();
  auto batch2 = twoTransactionsBatch();
  *state1 += batch1;
  *state2 += batch2;

  const auto kPageSize =
      batch1->transactions().size() + batch2->transactions().size() - 1;
  Response expected;
  expected.transactions.insert(expected.transactions.end(),
                               batch1->transactions().begin(),
                               batch1->transactions().end());
  expected.all_transactions_size =
      batch1->transactions().size() + batch2->transactions().size();
  expected.next_batch_info = BatchInfo{};
  expected.next_batch_info->first_tx_hash =
      batch2->transactions().front()->hash();
  expected.next_batch_info->batch_size = batch2->transactions().size();

  storage_->updatedBatchesHandler(state1);
  storage_->updatedBatchesHandler(state2);
  for (const auto &creator : {"alice@iroha", "bob@iroha"}) {
    auto pending =
        storage_->getPendingTransactions(creator, kPageSize, std::nullopt);
    IROHA_ASSERT_RESULT_VALUE(pending);
    checkResponse(pending.assumeValue(), expected);
  }
}

/**
 * Correctly formed response is returned when there are two batches are in the
 * storage and first tx hash in request is equal to the hash of the first
 * transaction in the second stored batch.
 */
TEST_F(PendingTxsStorageFixture, StartFromTheSecondBatch) {
  auto state1 = emptyState();
  auto state2 = emptyState();
  auto batch1 = twoTransactionsBatch();
  auto batch2 = twoTransactionsBatch();
  *state1 += batch1;
  *state2 += batch2;

  const auto kPageSize = batch2->transactions().size();
  Response expected;
  expected.transactions.insert(expected.transactions.end(),
                               batch2->transactions().begin(),
                               batch2->transactions().end());
  expected.all_transactions_size =
      batch1->transactions().size() + batch2->transactions().size();

  storage_->updatedBatchesHandler(state1);
  storage_->updatedBatchesHandler(state2);
  for (const auto &creator : {"alice@iroha", "bob@iroha"}) {
    auto pending = storage_->getPendingTransactions(
        creator, kPageSize, batch2->transactions().front()->hash());
    IROHA_ASSERT_RESULT_VALUE(pending);
    checkResponse(pending.assumeValue(), expected);
  }
}

/**
 * @given non empty pending transactions storage
 * @when a user requests pending transactions (and the storage has nothing for
 * the user)
 * @then an empty response is produced for the user
 */
TEST_F(PendingTxsStorageFixture, NoPendingBatches) {
  auto state = emptyState();
  auto transactions = twoTransactionsBatch();
  *state += transactions;

  const auto kThirdAccount = "clark@iroha";
  const auto kPageSize = 100u;
  Response empty_response;

  storage_->updatedBatchesHandler(state);
  auto pending =
      storage_->getPendingTransactions(kThirdAccount, kPageSize, std::nullopt);
  IROHA_ASSERT_RESULT_VALUE(pending);
  checkResponse(pending.assumeValue(), empty_response);
}

/**
 * Updated batch replaces previously existed
 * @given Batch with one transaction with one signature and storage
 * @when transaction inside batch receives additional signature
 * @then pending transactions response is also updated
 */
TEST_F(PendingTxsStorageFixture, SignaturesUpdate) {
  auto state1 = emptyState();
  auto state2 = emptyState();
  auto transactions = addSignatures(
      makeTestBatch(txBuilder(3, getUniqueTime(), 3, "alice@iroha")),
      0,
      makeSignature("1"_hex_sig, "pub_key_1"_hex_pubkey));
  *state1 += transactions;
  transactions = addSignatures(
      transactions, 0, makeSignature("2"_hex_sig, "pub_key_2"_hex_pubkey));
  *state2 += transactions;

  const auto kPageSize = 100u;
  storage_->updatedBatchesHandler(state1);
  storage_->updatedBatchesHandler(state2);
  auto pending =
      storage_->getPendingTransactions("alice@iroha", kPageSize, std::nullopt);
  pending.match(
      [&txs = transactions](const auto &response) {
        const auto &resp = response.value;
        EXPECT_EQ(resp.transactions.size(), txs->transactions().size());
        EXPECT_EQ(boost::size(resp.transactions.front()->signatures()), 2);
      },
      [](const auto &error) {
        FAIL() << "An error was not expected, the error code is "
               << error.error;
      });
}

/**
 * Storage correctly handles storing of several batches
 * @given MST state update with three batches inside
 * @when different users asks pending transactions
 * @then users receives correct responses
 */
TEST_F(PendingTxsStorageFixture, SeveralBatches) {
  auto state = emptyState();
  auto batch1 = twoTransactionsBatch();
  auto batch2 = addSignatures(
      makeTestBatch(txBuilder(2, getUniqueTime(), 2, "alice@iroha"),
                    txBuilder(3, getUniqueTime(), 3, "alice@iroha")),
      0,
      makeSignature("1"_hex_sig, "pub_key_1"_hex_pubkey));
  auto batch3 = addSignatures(
      makeTestBatch(txBuilder(2, getUniqueTime(), 2, "bob@iroha")),
      0,
      makeSignature("2"_hex_sig, "pub_key_2"_hex_pubkey));
  *state += batch1;
  *state += batch2;
  *state += batch3;

  const auto kPageSize = 100u;
  storage_->updatedBatchesHandler(state);
  auto alice_pending =
      storage_->getPendingTransactions("alice@iroha", kPageSize, std::nullopt);
  IROHA_ASSERT_RESULT_VALUE(alice_pending);
  EXPECT_EQ(alice_pending.assumeValue().transactions.size(), 4);

  auto bob_pending =
      storage_->getPendingTransactions("bob@iroha", kPageSize, std::nullopt);
  IROHA_ASSERT_RESULT_VALUE(bob_pending);
  EXPECT_EQ(bob_pending.assumeValue().transactions.size(), 3);
}

/**
 * New updates do not overwrite the whole state
 * @given two MST updates with different batches
 * @when updates arrives to storage sequentially
 * @then updates don't overwrite the whole storage state
 */
TEST_F(PendingTxsStorageFixture, SeparateBatchesDoNotOverwriteStorage) {
  auto state1 = emptyState();
  auto batch1 = twoTransactionsBatch();
  *state1 += batch1;
  auto state2 = std::make_shared<iroha::MstState>(
      iroha::MstState::empty(mst_state_log_, completer_));
  auto batch2 = addSignatures(
      makeTestBatch(txBuilder(2, getUniqueTime(), 2, "alice@iroha"),
                    txBuilder(3, getUniqueTime(), 3, "alice@iroha")),
      0,
      makeSignature("1"_hex_sig, "pub_key_1"_hex_pubkey));
  *state2 += batch2;

  const auto kPageSize = 100u;
  storage_->updatedBatchesHandler(state1);
  storage_->updatedBatchesHandler(state2);
  auto alice_pending =
      storage_->getPendingTransactions("alice@iroha", kPageSize, std::nullopt);
  IROHA_ASSERT_RESULT_VALUE(alice_pending);
  EXPECT_EQ(alice_pending.assumeValue().transactions.size(), 4);

  auto bob_pending =
      storage_->getPendingTransactions("bob@iroha", kPageSize, std::nullopt);
  IROHA_ASSERT_RESULT_VALUE(bob_pending);
  EXPECT_EQ(bob_pending.assumeValue().transactions.size(), 2);
}

/**
 * Batches with fully signed transactions (prepared transactions) should be
 * removed from storage
 * @given a batch with semi-signed transaction as MST update
 * @when the batch collects all the signatures
 * @then storage removes the batch
 */
TEST_F(PendingTxsStorageFixture, PreparedBatch) {
  auto state = emptyState();
  std::shared_ptr<shared_model::interface::TransactionBatch> batch =
      addSignatures(
          makeTestBatch(txBuilder(3, getUniqueTime(), 3, "alice@iroha")),
          0,
          makeSignature("1"_hex_sig, "pub_key_1"_hex_pubkey));
  *state += batch;

  storage_->updatedBatchesHandler(state);
  batch = addSignatures(batch,
                        0,
                        makeSignature("2"_hex_sig, "pub_key_2"_hex_pubkey),
                        makeSignature("3"_hex_sig, "pub_key_3"_hex_pubkey));
  storage_->removeBatch(batch);
  const auto kPageSize = 100u;
  auto pending =
      storage_->getPendingTransactions("alice@iroha", kPageSize, std::nullopt);
  IROHA_ASSERT_RESULT_VALUE(pending);
  EXPECT_EQ(pending.assumeValue().transactions.size(), 0);
}

/**
 * Batches with expired transactions should be removed from storage.
 * @given a batch with semi-signed transaction as MST update
 * @when the batch expires
 * @then storage removes the batch
 */
TEST_F(PendingTxsStorageFixture, ExpiredBatch) {
  auto state = emptyState();
  std::shared_ptr<shared_model::interface::TransactionBatch> batch =
      addSignatures(
          makeTestBatch(txBuilder(3, getUniqueTime(), 3, "alice@iroha")),
          0,
          makeSignature("1"_hex_sig, "pub_key_1"_hex_pubkey));
  *state += batch;

  storage_->updatedBatchesHandler(state);
  storage_->removeBatch(batch);
  const auto kPageSize = 100u;
  auto pending =
      storage_->getPendingTransactions("alice@iroha", kPageSize, std::nullopt);
  IROHA_ASSERT_RESULT_VALUE(pending);
  EXPECT_EQ(pending.assumeValue().transactions.size(), 0);
}

/**
 * @given a storage
 * @when the non-existing batch is queried via first tx hash
 * @then not found error is returned by the storage
 */
TEST_F(PendingTxsStorageFixture, QueryingWrongBatch) {
  auto state = emptyState();
  auto transactions = twoTransactionsBatch();
  *state += transactions;

  const auto kThirdAccount = "clark@iroha";
  const auto kPageSize = 100u;
  storage_->updatedBatchesHandler(state);
  auto response = storage_->getPendingTransactions(
      kThirdAccount, kPageSize, transactions->transactions().front()->hash());
  IROHA_ASSERT_RESULT_ERROR(response);
  EXPECT_EQ(response.assumeError(),
            iroha::PendingTransactionStorage::ErrorCode::kNotFound);
}

/**
 * @given a storage with two batches
 * @when a user requests the first batch only
 * @then the second using starting tx hash returned by the first response
 */
TEST_F(PendingTxsStorageFixture, QueryAllTheBatches) {
  auto state1 = emptyState();
  auto state2 = emptyState();
  auto batch1 = twoTransactionsBatch();
  auto batch2 = twoTransactionsBatch();
  *state1 += batch1;
  *state2 += batch2;

  auto batchSize = [](const auto &batch) {
    return batch->transactions().size();
  };
  auto firstHash = [](const auto &batch) {
    return batch->transactions().front()->hash();
  };

  Response first_page_expected;
  first_page_expected.transactions.insert(
      first_page_expected.transactions.end(),
      batch1->transactions().begin(),
      batch1->transactions().end());
  first_page_expected.all_transactions_size =
      batchSize(batch1) + batchSize(batch2);
  first_page_expected.next_batch_info = BatchInfo{};
  first_page_expected.next_batch_info->first_tx_hash = firstHash(batch2);
  first_page_expected.next_batch_info->batch_size = batchSize(batch2);

  Response second_page_expected;
  second_page_expected.transactions.insert(
      second_page_expected.transactions.end(),
      batch2->transactions().begin(),
      batch2->transactions().end());
  second_page_expected.all_transactions_size =
      batchSize(batch1) + batchSize(batch2);

  storage_->updatedBatchesHandler(state1);
  storage_->updatedBatchesHandler(state2);
  for (const auto &creator : {"alice@iroha", "bob@iroha"}) {
    auto first_page = storage_->getPendingTransactions(
        creator, batchSize(batch1), std::nullopt);
    IROHA_ASSERT_RESULT_VALUE(first_page);
    checkResponse(first_page.assumeValue(), first_page_expected);
    auto second_page = storage_->getPendingTransactions(
        creator, batchSize(batch2), firstHash(batch2));
    IROHA_ASSERT_RESULT_VALUE(second_page);
    checkResponse(second_page.assumeValue(), second_page_expected);
  }
}
