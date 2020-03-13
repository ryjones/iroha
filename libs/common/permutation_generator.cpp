/**
 * Copyright Soramitsu Co., Ltd. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#include "common/permutation_generator.hpp"

#include <algorithm>
#include <numeric>
#include <random>

#include "interfaces/common_objects/range_types.hpp"

using namespace iroha;

namespace {
  Seeder::ValueType kInitialSeedValue = 0;
}

RandomEngine iroha::makeSeededPrng(const char *seed_start, size_t seed_length) {
  return Seeder{}.feed(seed_start, seed_length).makePrng();
}

RandomEngine iroha::makeSeededPrng(const unsigned char *seed_start,
                                   size_t seed_length) {
  return Seeder{}
      .feed(reinterpret_cast<const char *>(seed_start), seed_length)
      .makePrng();
}

Seeder::Seeder() : current_seed_(kInitialSeedValue) {}

RandomEngine Seeder::makePrng() const {
  return RandomEngine{current_seed_};
}

Seeder &Seeder::feed(const char *seed_start, size_t seed_length) {
  const ValueType *full_numbers_start =
      reinterpret_cast<const ValueType *>(seed_start);
  const ValueType *full_numbers_end =
      full_numbers_start + seed_length / sizeof(ValueType);

  for (; full_numbers_start < full_numbers_end; ++full_numbers_start) {
    feed(*full_numbers_start);
  }

  const char *tail_start = reinterpret_cast<const char *>(full_numbers_end);
  const char *seed_end = seed_start + seed_length;
  if (tail_start < seed_end) {
    ValueType tail = 0;
    for (; tail_start < seed_end; ++tail_start) {
      tail |= *tail_start;
      tail <<= 8;
    }
    feed(tail);
  }
  return *this;
}

inline Seeder &Seeder::feed(ValueType value) {
  // like CBC
  current_seed_ = RandomEngine{current_seed_ ^ value}();
  return *this;
}

void iroha::generatePermutation(std::vector<size_t> &permutation,
                                RandomEngine prng,
                                size_t size) {
  permutation.resize(size);
  std::iota(permutation.begin(), permutation.end(), 0);

  for (auto it = permutation.begin(); it != permutation.end(); ++it) {
    std::iter_swap(it, permutation.begin() + prng() % size);
  }
}
