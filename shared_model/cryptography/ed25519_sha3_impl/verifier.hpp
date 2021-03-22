/**
 * Copyright Soramitsu Co., Ltd. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#ifndef IROHA_SHARED_MODEL_VERIFIER_HPP
#define IROHA_SHARED_MODEL_VERIFIER_HPP

#include "cryptography/crypto_provider/crypto_verifier_multihash.hpp"

namespace shared_model::crypto::ed25519_sha3 {
  /**
   * Class for signature verification.
   */
  class Verifier : public shared_model::crypto::CryptoVerifierMultihash {
   public:
    ~Verifier() override;

    iroha::expected::Result<void, std::string> verify(
        iroha::multihash::Type type,
        shared_model::interface::types::SignatureByteRangeView signature,
        shared_model::interface::types::ByteRange source,
        shared_model::interface::types::PublicKeyByteRangeView public_key)
        const override;

    static bool verifyEd25519Sha3(
        shared_model::interface::types::SignatureByteRangeView signature,
        shared_model::interface::types::ByteRange source,
        shared_model::interface::types::PublicKeyByteRangeView public_key);

    std::vector<iroha::multihash::Type> getSupportedTypes() const override;
  };
}  // namespace shared_model::crypto::ed25519_sha3

#endif  // IROHA_SHARED_MODEL_VERIFIER_HPP
