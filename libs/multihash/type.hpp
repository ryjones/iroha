/**
 * Copyright Soramitsu Co., Ltd. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#ifndef IROHA_MULTIHASH_HASH_TYPE_HPP
#define IROHA_MULTIHASH_HASH_TYPE_HPP

#include <cstdint>

#include <boost/preprocessor/comparison/equal.hpp>
#include <boost/preprocessor/repetition/repeat.hpp>
#include <boost/preprocessor/tuple/elem.hpp>
#include <optional>
#include <string_view>
#include <vector>

// Below go defenitions of multihash constants according to
// https://github.com/multiformats/js-multihash/blob/master/src/constants.js
//
// contents of IROHA_MULTIHASH_TYPE tuple:
// - entry type (hash or signature algorithm)
// - iroha::multihash::Type enum entry
// - decoded varint code

// clang-format off

#define IROHA_MULTIHASH_HASH_TYPE 0
#define IROHA_MULTIHASH_SIGNATURE_TYPE 1

#define IROHA_MULTIHASH_HASH_ENTRY(name, code)      (IROHA_MULTIHASH_HASH_TYPE, name, code)
#define IROHA_MULTIHASH_SIGNATURE_ENTRY(name, code) (IROHA_MULTIHASH_SIGNATURE_TYPE, name, code)

#define IROHA_MULTIHASH_TYPE_IS_SIGNATURE(i) \
    BOOST_PP_EQUAL(BOOST_PP_TUPLE_ELEM(3, 0, IROHA_MULTIHASH_TYPE(i)), \
                   IROHA_MULTIHASH_SIGNATURE_TYPE)

//
// --- Hash types ---
//
#define IROHA_MULTIHASH_TYPE0  IROHA_MULTIHASH_HASH_ENTRY(sha1,       0x11)
#define IROHA_MULTIHASH_TYPE1  IROHA_MULTIHASH_HASH_ENTRY(sha256,     0x12)
#define IROHA_MULTIHASH_TYPE2  IROHA_MULTIHASH_HASH_ENTRY(sha512,     0x13)
#define IROHA_MULTIHASH_TYPE3  IROHA_MULTIHASH_HASH_ENTRY(blake2s128, 0xb250)
#define IROHA_MULTIHASH_TYPE4  IROHA_MULTIHASH_HASH_ENTRY(blake2s256, 0xb260)

//
// --- public key and signature algorithm types ---
//
// Ed25519
#define IROHA_MULTIHASH_TYPE5  IROHA_MULTIHASH_SIGNATURE_ENTRY(ed25519_sha2_224, 0x9196d)
#define IROHA_MULTIHASH_TYPE6  IROHA_MULTIHASH_SIGNATURE_ENTRY(ed25519_sha2_256, 0xed)
#define IROHA_MULTIHASH_TYPE7  IROHA_MULTIHASH_SIGNATURE_ENTRY(ed25519_sha2_384, 0x119ed)
#define IROHA_MULTIHASH_TYPE8  IROHA_MULTIHASH_SIGNATURE_ENTRY(ed25519_sha2_512, 0x49aed)
#define IROHA_MULTIHASH_TYPE9  IROHA_MULTIHASH_SIGNATURE_ENTRY(ed25519_sha3_224, 0x9216d)
#define IROHA_MULTIHASH_TYPE10 IROHA_MULTIHASH_SIGNATURE_ENTRY(ed25519_sha3_256, 0x15a16d)
#define IROHA_MULTIHASH_TYPE11 IROHA_MULTIHASH_SIGNATURE_ENTRY(ed25519_sha3_384, 0x121ed)
#define IROHA_MULTIHASH_TYPE12 IROHA_MULTIHASH_SIGNATURE_ENTRY(ed25519_sha3_512, 0x4a2ed)

// secp256r1
#define IROHA_MULTIHASH_TYPE13 IROHA_MULTIHASH_SIGNATURE_ENTRY(ecdsa_secp256r1_sha2_224, 0x48ca8ec)
#define IROHA_MULTIHASH_TYPE14 IROHA_MULTIHASH_SIGNATURE_ENTRY(ecdsa_secp256r1_sha2_256, 0xacca8ec)
#define IROHA_MULTIHASH_TYPE15 IROHA_MULTIHASH_SIGNATURE_ENTRY(ecdsa_secp256r1_sha2_384, 0x8ce8ec)
#define IROHA_MULTIHASH_TYPE16 IROHA_MULTIHASH_SIGNATURE_ENTRY(ecdsa_secp256r1_sha2_512, 0x24d68ec)
#define IROHA_MULTIHASH_TYPE17 IROHA_MULTIHASH_SIGNATURE_ENTRY(ecdsa_secp256r1_sha3_224, 0x490a8ec)
#define IROHA_MULTIHASH_TYPE18 IROHA_MULTIHASH_SIGNATURE_ENTRY(ecdsa_secp256r1_sha3_256, 0xad0a8ec)
#define IROHA_MULTIHASH_TYPE19 IROHA_MULTIHASH_SIGNATURE_ENTRY(ecdsa_secp256r1_sha3_384, 0x90e8ec)
#define IROHA_MULTIHASH_TYPE20 IROHA_MULTIHASH_SIGNATURE_ENTRY(ecdsa_secp256r1_sha3_512, 0x25168ec)

// secp384r1
#define IROHA_MULTIHASH_TYPE21 IROHA_MULTIHASH_SIGNATURE_ENTRY(ecdsa_secp384r1_sha2_224, 0x48ca96c)
#define IROHA_MULTIHASH_TYPE22 IROHA_MULTIHASH_SIGNATURE_ENTRY(ecdsa_secp384r1_sha2_256, 0xacca96c)
#define IROHA_MULTIHASH_TYPE23 IROHA_MULTIHASH_SIGNATURE_ENTRY(ecdsa_secp384r1_sha2_384, 0x8ce96c)
#define IROHA_MULTIHASH_TYPE24 IROHA_MULTIHASH_SIGNATURE_ENTRY(ecdsa_secp384r1_sha2_512, 0x24d696c)
#define IROHA_MULTIHASH_TYPE25 IROHA_MULTIHASH_SIGNATURE_ENTRY(ecdsa_secp384r1_sha3_224, 0x490a96c)
#define IROHA_MULTIHASH_TYPE26 IROHA_MULTIHASH_SIGNATURE_ENTRY(ecdsa_secp384r1_sha3_256, 0xad0a96c)
#define IROHA_MULTIHASH_TYPE27 IROHA_MULTIHASH_SIGNATURE_ENTRY(ecdsa_secp384r1_sha3_384, 0x90e96c)
#define IROHA_MULTIHASH_TYPE28 IROHA_MULTIHASH_SIGNATURE_ENTRY(ecdsa_secp384r1_sha3_512, 0x251696c)

#define IROHA_MULTIHASH_TYPES_NUMBER 29

#define IROHA_MULTIHASH_TYPE(i) BOOST_PP_CAT(IROHA_MULTIHASH_TYPE, i)

// clang-format on

namespace iroha {
  namespace multihash {
    enum Type : uint64_t {
#define IROHA_MULTIHASH_GET_TYPE(_, i, ...)            \
  BOOST_PP_TUPLE_ELEM(3, 1, IROHA_MULTIHASH_TYPE(i)) = \
      BOOST_PP_TUPLE_ELEM(3, 2, IROHA_MULTIHASH_TYPE(i)),
      BOOST_PP_REPEAT(IROHA_MULTIHASH_TYPES_NUMBER, IROHA_MULTIHASH_GET_TYPE, )
#undef IROHA_MULTIHASH_GET_TYPE
    };

  }  // namespace multihash
}  // namespace iroha

#endif
