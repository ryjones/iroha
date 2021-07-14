/**
 * Copyright Soramitsu Co., Ltd. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#ifndef IROHA_CONSENSUS_INIT_HPP
#define IROHA_CONSENSUS_INIT_HPP

#include <memory>

#include "ametsuchi/peer_query_factory.hpp"
#include "consensus/consensus_block_cache.hpp"
#include "consensus/gate_object.hpp"
#include "consensus/yac/consensus_outcome_type.hpp"
#include "consensus/yac/consistency_model.hpp"
#include "consensus/yac/outcome_messages.hpp"
#include "consensus/yac/timer.hpp"
#include "consensus/yac/transport/impl/consensus_service_impl.hpp"
#include "consensus/yac/yac_gate.hpp"
#include "consensus/yac/yac_hash_provider.hpp"
#include "consensus/yac/yac_peer_orderer.hpp"
#include "cryptography/keypair.hpp"
#include "logger/logger_manager_fwd.hpp"
#include "main/subscription_fwd.hpp"
#include "network/block_loader.hpp"
#include "network/impl/async_grpc_client.hpp"

namespace iroha {
  namespace network {
    class GenericClientFactory;
  }
  namespace consensus {
    namespace yac {
      class Yac;
      class YacGateImpl;

      class YacInit {
       public:
        std::shared_ptr<YacGate> initConsensusGate(
            Round initial_round,
            // TODO 30.01.2019 lebdron: IR-262 Remove PeerQueryFactory
            std::shared_ptr<ametsuchi::PeerQueryFactory> peer_query_factory,
            boost::optional<shared_model::interface::types::PeerList>
                alternative_peers,
            std::shared_ptr<const LedgerState> ledger_state,
            std::shared_ptr<network::BlockLoader> block_loader,
            const shared_model::crypto::Keypair &keypair,
            std::shared_ptr<consensus::ConsensusResultCache> block_cache,
            std::chrono::milliseconds vote_delay_milliseconds,
            std::shared_ptr<
                iroha::network::AsyncGrpcClient<google::protobuf::Empty>>
                async_call,
            ConsistencyModel consistency_model,
            const logger::LoggerManagerTreePtr &consensus_log_manager,
            std::shared_ptr<iroha::network::GenericClientFactory>
                client_factory);

        std::shared_ptr<ServiceImpl> getConsensusNetwork() const;

        void subscribe(std::function<void(GateObject const &)> callback);

       private:
        auto createTimer(std::chrono::milliseconds delay_milliseconds);

        bool initialized_{false};
        std::shared_ptr<ServiceImpl> consensus_network_;
        std::shared_ptr<Yac> yac_;
        std::shared_ptr<YacGateImpl> yac_gate_;
        std::shared_ptr<BaseSubscriber<bool, std::vector<VoteMessage>>>
            states_subscription_;
      };
    }  // namespace yac
  }    // namespace consensus
}  // namespace iroha

#endif  // IROHA_CONSENSUS_INIT_HPP
