/**
 * Copyright Soramitsu Co., Ltd. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#include "network/impl/peer_communication_service_impl.hpp"

#include <rxcpp/rx-lite.hpp>
#include "interfaces/iroha_internal/transaction_batch.hpp"
#include "logger/logger.hpp"
#include "network/ordering_gate.hpp"
#include "simulator/verified_proposal_creator.hpp"

namespace iroha {
  namespace network {
    PeerCommunicationServiceImpl::PeerCommunicationServiceImpl(
        std::shared_ptr<OrderingGate> ordering_gate,
        std::shared_ptr<iroha::simulator::VerifiedProposalCreator>
            proposal_creator,
        logger::LoggerPtr log)
        : ordering_gate_(std::move(ordering_gate)),
          proposal_creator_(std::move(proposal_creator)),
          log_{std::move(log)} {}

    void PeerCommunicationServiceImpl::propagate_batch(
        std::shared_ptr<shared_model::interface::TransactionBatch> batch)
        const {
      log_->info("propagate batch");
      ordering_gate_->propagateBatch(batch);
    }

    rxcpp::observable<OrderingEvent> PeerCommunicationServiceImpl::onProposal()
        const {
      return ordering_gate_->onProposal();
    }

    rxcpp::observable<simulator::VerifiedProposalCreatorEvent>
    PeerCommunicationServiceImpl::onVerifiedProposal() const {
      return proposal_creator_->onVerifiedProposal();
    }
  }  // namespace network
}  // namespace iroha
