/**
 * Copyright Soramitsu Co., Ltd. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#ifndef IROHA_UNSIGNED_PROTO_HPP
#define IROHA_UNSIGNED_PROTO_HPP

#include "backend/protobuf/common_objects/signature.hpp"
#include "backend/protobuf/transaction.hpp"
#include "cryptography/crypto_provider/crypto_defaults.hpp"
#include "cryptography/crypto_provider/crypto_signer.hpp"
#include "cryptography/crypto_provider/crypto_signer_internal.hpp"
#include "cryptography/keypair.hpp"
#include "interfaces/common_objects/types.hpp"

namespace shared_model {
  namespace proto {
    /**
     * Class for holding built but still unsigned objects
     * @tparam T - type of object received from builder
     *
     * NOTE: finish() moves internal object, so calling methods after
     * finish() throws an exception
     */
    template <typename T>
    class [[deprecated]] UnsignedWrapper {
     public:
      using ModelType = T;

      /**
       * Constructs new unsigned object instance
       * @param o - object received from builder
       */
      explicit UnsignedWrapper(const T &o) : object_(o) {}

      explicit UnsignedWrapper(T && o) : object_(std::move(o)) {}

      UnsignedWrapper(UnsignedWrapper<T> && w)
          : object_(std::move(w.object_)),
            object_finalized_(w.object_finalized_) {
        w.object_finalized_ = true;
      }

      UnsignedWrapper<T> &operator=(UnsignedWrapper<T> &&w) {
        object_ = std::move(w.object_);
        object_finalized_ = w.object_finalized_;
        w.object_finalized_ = true;

        return *this;
      }

      UnsignedWrapper(const UnsignedWrapper<T> &o) = default;
      UnsignedWrapper<T> &operator=(const UnsignedWrapper<T> &w) = default;

      /**
       * Add signature and retrieve signed result
       * @param signer - signer to use
       * @return signed object
       */
      UnsignedWrapper &signAndAddSignature(const crypto::CryptoSigner &signer) {
        if (object_finalized_) {
          throw std::runtime_error("object has already been finalized");
        }
        using namespace shared_model::interface::types;
        object_.addSignature(
            SignedHexStringView{signer.sign(object_.payload())},
            PublicKeyHexStringView{signer.publicKey()});
        // TODO: 05.12.2017 luckychess think about false case
        return *this;
      }

      /**
       * Add signature and retrieve signed result
       * @param keypair to initialize default internal signer
       * @return signed object
       */
      UnsignedWrapper &signAndAddSignature(const crypto::Keypair &keypair) {
        using namespace ::shared_model::crypto;
        return signAndAddSignature(
            CryptoSignerInternal<DefaultCryptoAlgorithmType>{Keypair{keypair}});
      }

      /**
       * Finishes object building
       * @return built signed object
       */
      T finish() {
        if (boost::size(object_.signatures()) == 0) {
          throw std::invalid_argument("Cannot get object without signatures");
        }
        if (object_finalized_) {
          throw std::runtime_error("object has already been finalized");
        }

        object_finalized_ = true;
        return std::move(object_);
      }

      interface::types::HashType hash() {
        return object_.hash();
      }

      template <typename U = T>
      std::enable_if_t<
          std::is_base_of<shared_model::interface::Transaction, U>::value,
          interface::types::HashType>
      reducedHash() const {
        return object_.reducedHash();
      }

     private:
      T object_;
      bool object_finalized_{false};
    };
  }  // namespace proto
}  // namespace shared_model

#endif  // IROHA_UNSIGNED_PROTO_HPP