/**
 * Copyright Soramitsu Co., Ltd. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#ifndef IROHA_ON_DEMAND_ORDERING_GATE_HPP
#define IROHA_ON_DEMAND_ORDERING_GATE_HPP

#include "network/ordering_gate.hpp"

#include <shared_mutex>

#include "interfaces/common_objects/types.hpp"
#include "interfaces/iroha_internal/proposal.hpp"
#include "interfaces/iroha_internal/unsafe_proposal_factory.hpp"
#include "logger/logger_fwd.hpp"
#include "ordering/impl/on_demand_common.hpp"
#include "ordering/on_demand_ordering_service.hpp"
#include "ordering/on_demand_os_transport.hpp"

namespace iroha {
  namespace ametsuchi {
    class TxPresenceCache;
  }

  namespace ordering {

    /**
     * Ordering gate which requests proposals from the ordering service
     * votes for proposals, and passes committed proposals to the pipeline
     */
    class OnDemandOrderingGate : public network::OrderingGate {
     public:
      struct RoundSwitch {
        consensus::Round next_round;
        std::shared_ptr<const LedgerState> ledger_state;

        RoundSwitch(consensus::Round next_round,
                    std::shared_ptr<const LedgerState> ledger_state)
            : next_round(std::move(next_round)),
              ledger_state(std::move(ledger_state)) {}
      };

      OnDemandOrderingGate(
          std::shared_ptr<OnDemandOrderingService> ordering_service,
          std::shared_ptr<transport::OdOsNotification> network_client,
          std::shared_ptr<shared_model::interface::UnsafeProposalFactory>
              factory,
          std::shared_ptr<ametsuchi::TxPresenceCache> tx_cache,
          size_t transaction_limit,
          logger::LoggerPtr log);

      ~OnDemandOrderingGate() override;

      void propagateBatch(
          std::shared_ptr<shared_model::interface::TransactionBatch> batch)
          override;

      void processRoundSwitch(RoundSwitch const &event);

      /**
       * Handle an incoming proposal from ordering service
       */
      std::optional<network::OrderingEvent> processProposalRequest(
          ProposalEvent const &event) const;

      void stop() override;

     private:
      void sendCachedTransactions();

      /**
       * remove already processed transactions from proposal
       */
      std::shared_ptr<const shared_model::interface::Proposal>
      removeReplaysAndDuplicates(
          std::shared_ptr<const shared_model::interface::Proposal> proposal)
          const;

      logger::LoggerPtr log_;

      /// max number of transactions passed to one ordering service
      size_t transaction_limit_;
      std::shared_ptr<OnDemandOrderingService> ordering_service_;
      std::shared_ptr<transport::OdOsNotification> network_client_;
      std::shared_ptr<shared_model::interface::UnsafeProposalFactory>
          proposal_factory_;
      std::shared_ptr<ametsuchi::TxPresenceCache> tx_cache_;
      consensus::Round current_round_;
      std::shared_ptr<const LedgerState> current_ledger_state_;

      std::shared_timed_mutex stop_mutex_;
      bool stop_requested_{false};
    };

  }  // namespace ordering
}  // namespace iroha

#endif  // IROHA_ON_DEMAND_ORDERING_GATE_HPP
