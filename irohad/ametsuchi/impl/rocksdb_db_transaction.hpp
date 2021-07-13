/**
 * Copyright Soramitsu Co., Ltd. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#ifndef IROHA_ROCKSDB_DB_TRANSACTION_HPP
#define IROHA_ROCKSDB_DB_TRANSACTION_HPP

#include "ametsuchi/impl/DbTransaction.hpp"

#include "ametsuchi/impl/rocksdb_common.hpp"

namespace iroha::ametsuchi {

  class RocksDbTransaction final : public DatabaseTransaction {
   public:
    RocksDbTransaction(RocksDbTransaction const &) = delete;
    RocksDbTransaction(RocksDbTransaction &&) = delete;

    RocksDbTransaction &operator=(RocksDbTransaction const &) = delete;
    RocksDbTransaction &operator=(RocksDbTransaction &&) = delete;

    RocksDbTransaction(std::shared_ptr<RocksDBContext> tx_context)
        : tx_context_(std::move(tx_context)) {
      assert(tx_context_);
    }

    void begin() override {}

    void savepoint(std::string const &) override {
      RocksDbCommon common(tx_context_);
      common.savepoint();
    }

    void releaseSavepoint(std::string const &) override {
      RocksDbCommon common(tx_context_);
      common.release();
    }

    void commit() override {
      RocksDbCommon common(tx_context_);
      common.commit();
    }

    void rollback() override {
      RocksDbCommon common(tx_context_);
      common.rollback();
    }

    void prepare(std::string const &) override {
      RocksDbCommon common(tx_context_);
      common.prepare();
    }

    void commitPrepared(std::string const &) override {
      RocksDbCommon common(tx_context_);
      common.commit();
    }

    void rollbackToSavepoint(std::string const &) override {
      RocksDbCommon common(tx_context_);
      common.rollbackToSavepoint();
    }

   private:
    std::shared_ptr<RocksDBContext> tx_context_;
  };

}  // namespace iroha::ametsuchi

#endif  // IROHA_ROCKSDB_DB_TRANSACTION_HPP
