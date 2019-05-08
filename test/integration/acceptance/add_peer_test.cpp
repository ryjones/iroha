/**
 * Copyright Soramitsu Co., Ltd. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#include "builders/protobuf/transaction.hpp"
#include "consensus/yac/vote_message.hpp"
#include "consensus/yac/yac_hash_provider.hpp"
#include "datetime/time.hpp"
#include "framework/integration_framework/fake_peer/behaviour/honest.hpp"
#include "framework/integration_framework/fake_peer/block_storage.hpp"
#include "framework/integration_framework/fake_peer/fake_peer.hpp"
#include "framework/integration_framework/integration_test_framework.hpp"
#include "framework/test_logger.hpp"
#include "integration/acceptance/acceptance_fixture.hpp"
#include "main/server_runner.hpp"
#include "module/irohad/multi_sig_transactions/mst_mocks.hpp"
#include "module/shared_model/builders/protobuf/block.hpp"
#include "ordering/impl/on_demand_common.cpp"

using namespace common_constants;
using namespace shared_model;
using namespace integration_framework;
using namespace shared_model::interface::permissions;

static constexpr std::chrono::seconds kMstStateWaitingTime(20);
static constexpr std::chrono::seconds kSynchronizerWaitingTime(20);

template <size_t N>
void checkBlockHasNTxs(const std::shared_ptr<const interface::Block> &block) {
  ASSERT_EQ(block->transactions().size(), N);
}

class FakePeerExampleFixture : public AcceptanceFixture {
 public:
  using FakePeer = fake_peer::FakePeer;

  std::unique_ptr<IntegrationTestFramework> itf_;

  /**
   * Create honest fake iroha peers
   *
   * @param num_fake_peers - the amount of fake peers to create
   */
  void createFakePeers(size_t num_fake_peers) {
    fake_peers_ = itf_->addFakePeers(num_fake_peers);
  }

  /**
   * Prepare state of ledger:
   * - create account of target user
   * - add assets to admin
   *
   * @return reference to ITF
   */
  IntegrationTestFramework &prepareState() {
    itf_->setGenesisBlock(itf_->defaultBlock()).subscribeQueuesAndRun();

    auto permissions =
        interface::RolePermissionSet({Role::kReceive, Role::kTransfer});

    return itf_->sendTxAwait(makeUserWithPerms(permissions),
                             checkBlockHasNTxs<1>);
  }

 protected:
  void SetUp() override {
    itf_ =
        std::make_unique<IntegrationTestFramework>(1, boost::none, true, true);
    itf_->initPipeline(kAdminKeypair);
  }

  std::vector<std::shared_ptr<FakePeer>> fake_peers_;
};

auto makePeerPointeeMatcher(interface::types::AddressType address,
                            interface::types::PubkeyType pubkey) {
  return ::testing::Truly(
      [address = std::move(address),
       pubkey = std::move(pubkey)](std::shared_ptr<interface::Peer> peer) {
        return peer->address() == address and peer->pubkey() == pubkey;
      });
}

auto makePeerPointeeMatcher(std::shared_ptr<interface::Peer> peer) {
  return makePeerPointeeMatcher(peer->address(), peer->pubkey());
}

/**
 * @given a network of single peer
 * @when it receives a valid signed addPeer command
 * @then the transaction is committed
 *    @and the ledger state after commit contains the two peers,
 *    @and the WSV reports that there are two peers: the initial and the added one
 */
TEST_F(FakePeerExampleFixture, FakePeerIsAdded) {
  // ------------------------ GIVEN ------------------------
  // init the real peer with no other peers in the genesis block
  auto &itf = prepareState();

  const std::string new_peer_address = "127.0.0.1:1234";
  const auto new_peer_pubkey =
      shared_model::crypto::DefaultCryptoAlgorithmType::generateKeypair()
          .publicKey();

  // capture itf synchronization events
  auto itf_sync_events_observable = itf_->getPcsOnCommitObservable().replay();
  itf_sync_events_observable.connect();

  // ------------------------ WHEN -------------------------
  // send addPeer command
  itf.sendTxAwait(
      complete(baseTx(kAdminId).addPeer(new_peer_address, new_peer_pubkey),
               kAdminKeypair),
      checkBlockHasNTxs<1>);

  // ------------------------ THEN -------------------------
  // check that ledger state contains the two peers
  itf_sync_events_observable
      .timeout(kSynchronizerWaitingTime, rxcpp::observe_on_new_thread())
      .filter([](const auto &sync_event) {
        return sync_event.sync_outcome
            == iroha::synchronizer::SynchronizationOutcomeType::kCommit;
      })
      .take(1)
      .as_blocking()
      .subscribe(
          [&, itf_peer = itf_->getThisPeer()](const auto &sync_event) {
            EXPECT_THAT(
                *sync_event.ledger_state->ledger_peers,
                ::testing::UnorderedElementsAre(
                    makePeerPointeeMatcher(itf_peer),
                    makePeerPointeeMatcher(new_peer_address, new_peer_pubkey)));
          },
          [](std::exception_ptr ep) {
            try {
              std::rethrow_exception(ep);
            } catch (const std::exception &e) {
              FAIL() << "Error waiting for synchronization: " << e.what();
            }
          });

  // query WSV peers
  auto opt_peers = itf.getIrohaInstance()
                       .getIrohaInstance()
                       ->getStorage()
                       ->createPeerQuery()
                       .value()
                       ->getLedgerPeers();

  // check the two peers are there
  ASSERT_TRUE(opt_peers);
  EXPECT_THAT(*opt_peers,
              ::testing::UnorderedElementsAre(
                  makePeerPointeeMatcher(itf.getThisPeer()),
                  makePeerPointeeMatcher(new_peer_address, new_peer_pubkey)));
}

/**
 * @given a network of single peer
 * @when it receives a not fully signed transaction and then a new peer is added
 * @then the first peer propagates MST state to the newly added peer
 */
TEST_F(FakePeerExampleFixture, MstStatePropagtesToNewPeer) {
  // ------------------------ GIVEN ------------------------
  // init the real peer with no other peers in the genesis block
  auto &itf = prepareState();

  // then create a fake peer
  auto new_peer = itf.addFakePeer(boost::none);
  auto mst_states_observable = new_peer->getMstStatesObservable().replay();
  mst_states_observable.connect();
  auto new_peer_server = new_peer->run();

  // ------------------------ WHEN -------------------------
  // and add it with addPeer
  itf.sendTxWithoutValidation(complete(
      baseTx(kAdminId).setAccountDetail(kAdminId, "fav_meme", "doge").quorum(2),
      kAdminKeypair));

  itf.sendTxAwait(
      complete(baseTx(kAdminId).addPeer(new_peer->getAddress(),
                                        new_peer->getKeypair().publicKey()),
               kAdminKeypair),
      checkBlockHasNTxs<1>);

  // ------------------------ THEN -------------------------
  mst_states_observable
      .timeout(kMstStateWaitingTime, rxcpp::observe_on_new_thread())
      .take(1)
      .as_blocking()
      .subscribe([](const auto &) {},
                 [](std::exception_ptr ep) {
                   try {
                     std::rethrow_exception(ep);
                   } catch (const std::exception &e) {
                     FAIL() << "Error waiting for MST state: " << e.what();
                   }
                 });

  new_peer_server->shutdown();
}

/**
 * @given a network of a single fake peer with a block store containing addPeer
 * command that adds itf peer
 * @when itf peer is brought up
 * @then itf peer gets synchronized, sees itself in the WSV and can commit txs
 */
TEST_F(FakePeerExampleFixture, RealPeerIsAdded) {
  // ------------------------ GIVEN ------------------------
  // create the initial fake peer
  auto initial_peer = itf_->addFakePeer(boost::none);

  // create a genesis block without only initial fake peer in it
  shared_model::interface::RolePermissionSet all_perms{};
  for (size_t i = 0; i < all_perms.size(); ++i) {
    auto perm = static_cast<shared_model::interface::permissions::Role>(i);
    all_perms.set(perm);
  }
  auto genesis_tx =
      proto::TransactionBuilder()
          .creatorAccountId(kAdminId)
          .createdTime(iroha::time::now())
          .addPeer(initial_peer->getAddress(),
                   initial_peer->getKeypair().publicKey())
          .createRole(kAdminRole, all_perms)
          .createRole(kDefaultRole, {})
          .createDomain(kDomain, kDefaultRole)
          .createAccount(kAdminName, kDomain, kAdminKeypair.publicKey())
          .detachRole(kAdminId, kDefaultRole)
          .appendRole(kAdminId, kAdminRole)
          .createAsset(kAssetName, kDomain, 1)
          .quorum(1)
          .build()
          .signAndAddSignature(kAdminKeypair)
          .finish();
  auto genesis_block =
      proto::BlockBuilder()
          .transactions(std::vector<shared_model::proto::Transaction>{
              std::move(genesis_tx)})
          .height(1)
          .prevHash(crypto::DefaultHashProvider::makeHash(crypto::Blob("")))
          .createdTime(iroha::time::now())
          .build()
          .signAndAddSignature(initial_peer->getKeypair())
          .finish();

  auto block_with_add_peer =
      proto::BlockBuilder()
          .transactions(std::vector<shared_model::proto::Transaction>{
              complete(baseTx(kAdminId).addPeer(itf_->getAddress(),
                                                itf_->getThisPeer()->pubkey()),
                       kAdminKeypair)})
          .height(genesis_block.height() + 1)
          .prevHash(genesis_block.hash())
          .createdTime(iroha::time::now())
          .build()
          .signAndAddSignature(initial_peer->getKeypair())
          .finish();

  // provide the initial_peer with the blocks 
  auto block_storage =
      std::make_shared<fake_peer::BlockStorage>(getTestLogger("BlockStorage"));
  block_storage->storeBlock(clone(genesis_block));
  block_storage->storeBlock(clone(block_with_add_peer));
  initial_peer->setBlockStorage(block_storage);

  // instruct the initial fake peer to send a commit when synchronization needed
  using iroha::consensus::yac::YacHash;
  struct SynchronizerBehaviour : public fake_peer::HonestBehaviour {
    explicit SynchronizerBehaviour(YacHash sync_hash)
        : sync_hash_(std::move(sync_hash)) {}
    void processYacMessage(
        std::shared_ptr<const fake_peer::YacMessage> message) override {
      if (not message->empty()
          and message->front().hash.vote_round.block_round
              <= sync_hash_.vote_round.block_round) {
        getFakePeer().sendYacState({getFakePeer().makeVote(sync_hash_)});
      } else {
        fake_peer::HonestBehaviour::processYacMessage(std::move(message));
      }
    }
    YacHash sync_hash_;
  };

  initial_peer->setBehaviour(std::make_shared<SynchronizerBehaviour>(
      YacHash{iroha::consensus::Round{block_with_add_peer.height(),
                                      iroha::ordering::kFirstRejectRound},
              "proposal_hash",
              block_with_add_peer.hash().hex()}));

  // launch the initial_peer
  auto new_peer_server = initial_peer->run();

  // init the itf peer with our genesis block
  itf_->setGenesisBlock(genesis_block);

  // capture itf synchronization events
  auto itf_sync_events_observable = itf_->getPcsOnCommitObservable().replay();
  itf_sync_events_observable.connect();

  // ------------------------ WHEN -------------------------
  // launch the itf peer
  itf_->subscribeQueuesAndRun();

  // ------------------------ THEN -------------------------
  // check that itf peer is synchronized
  itf_sync_events_observable
      .timeout(kSynchronizerWaitingTime, rxcpp::observe_on_new_thread())
      .filter([](const auto &sync_event) {
        return sync_event.sync_outcome
            == iroha::synchronizer::SynchronizationOutcomeType::kCommit;
      })
      .take(1)
      .as_blocking()
      .subscribe(
          [height = block_with_add_peer.height(),
           itf_peer = itf_->getThisPeer(),
           initial_peer = initial_peer->getThisPeer()](const auto &sync_event) {
            EXPECT_EQ(sync_event.ledger_state->height, height);
            EXPECT_THAT(*sync_event.ledger_state->ledger_peers,
                        ::testing::UnorderedElementsAre(
                            makePeerPointeeMatcher(itf_peer),
                            makePeerPointeeMatcher(initial_peer)));
          },
          [](std::exception_ptr ep) {
            try {
              std::rethrow_exception(ep);
            } catch (const std::exception &e) {
              FAIL() << "Error waiting for synchronization: " << e.what();
            }
          });

  // check that itf peer sees the two peers in the WSV
  auto opt_peers = itf_->getIrohaInstance()
                       .getIrohaInstance()
                       ->getStorage()
                       ->createPeerQuery()
                       .value()
                       ->getLedgerPeers();
  ASSERT_TRUE(opt_peers);
  EXPECT_THAT(*opt_peers,
              ::testing::UnorderedElementsAre(
                  makePeerPointeeMatcher(itf_->getThisPeer()),
                  makePeerPointeeMatcher(initial_peer->getThisPeer())));

  // send some valid tx to itf and check that it gets committed
  itf_->sendTxAwait(complete(baseTx(kAdminId)
                                 .setAccountDetail(kUserId, "fav_meme", "doge")
                                 .quorum(1),
                             kAdminKeypair),
                    checkBlockHasNTxs<1>);

  new_peer_server->shutdown();
}
