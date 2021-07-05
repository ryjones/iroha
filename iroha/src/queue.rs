//! Module with queue actor

use std::time::Duration;

use crossbeam::{atomic::AtomicCell, queue::ArrayQueue};
use dashmap::{mapref::entry::Entry, DashMap};
use iroha_data_model::prelude::*;
use iroha_error::Result;

use self::config::QueueConfiguration;
use crate::{prelude::*, wsv::WorldTrait};

/// Lockfree queue for transactions
///
/// Multiple producers, single consumer
#[derive(Debug)]
pub struct Queue {
    queue: ArrayQueue<Hash>,
    txs: DashMap<Hash, VersionedAcceptedTransaction>,
    /// Length of dashmap.
    ///
    /// DashMap right now just iterates over itself and calculates its length like this:
    /// self.txs.iter().len()
    len: AtomicCell<usize>,
    txs_in_block: usize,
    txs_in_queue: usize,
    ttl: Duration,
}

impl Queue {
    /// Makes queue from configuration
    pub fn from_configuration(cfg: &QueueConfiguration) -> Self {
        Self {
            queue: ArrayQueue::new(cfg.maximum_transactions_in_queue as usize),
            txs: DashMap::new(),
            txs_in_queue: cfg.maximum_transactions_in_queue as usize,
            txs_in_block: cfg.maximum_transactions_in_block as usize,
            ttl: Duration::from_millis(cfg.transaction_time_to_live_ms),
            len: AtomicCell::new(0),
        }
    }

    /// Returns all pending transactions paginated
    pub fn waiting(&self) -> PendingTransactions {
        self.txs
            .iter()
            .map(|e| e.value().clone())
            .map(VersionedAcceptedTransaction::into_inner_v1)
            .map(Transaction::from)
            .collect()
    }

    /// Pushes transaction into queue
    /// # Errors
    /// Returns transaction if queue is full
    #[allow(clippy::unwrap_in_result)]
    pub fn push(
        &self,
        tx: VersionedAcceptedTransaction,
    ) -> Result<(), VersionedAcceptedTransaction> {
        let hash = tx.hash();
        let entry = match self.txs.entry(hash) {
            Entry::Occupied(mut old_tx) => {
                // MST case
                old_tx
                    .get_mut()
                    .as_mut_inner_v1()
                    .signatures
                    .append(&mut tx.into_inner_v1().signatures);
                return Ok(());
            }
            Entry::Vacant(entry) => entry,
        };

        if self.len.load() >= self.txs_in_queue {
            return Err(tx);
        }

        #[allow(clippy::expect_used)]
        self.queue
            .push(tx.hash())
            .expect("Safe as we checked number of items just above");
        let _ = self.len.fetch_add(1);
        drop(entry.insert(tx));
        Ok(())
    }

    /// Pops single transaction.
    ///
    /// Records unsigned transaction in seen.
    #[allow(clippy::unwrap_used, clippy::unwrap_in_result)]
    fn pop<W: WorldTrait>(
        &self,
        is_leader: bool,
        wsv: &WorldStateView<W>,
        seen: &mut Vec<Hash>,
    ) -> Option<VersionedAcceptedTransaction> {
        loop {
            let hash = self.queue.pop()?;
            let tx = self.txs.get(&hash).unwrap();

            if tx.is_expired(self.ttl) || tx.is_in_blockchain(wsv) {
                drop(tx);
                let _ = self.len.fetch_sub(1);
                drop(self.txs.remove(&hash));
                continue;
            }

            let sig_condition = match tx.check_signature_condition(wsv) {
                Ok(condition) => condition,
                Err(error) => {
                    iroha_logger::error!(%error, "Not passed signature");
                    drop(tx);
                    let _ = self.len.fetch_sub(1);
                    drop(self.txs.remove(&hash));
                    continue;
                }
            };

            if !is_leader || sig_condition {
                drop(tx);
                let _ = self.len.fetch_sub(1);
                return Some(self.txs.remove(&hash).unwrap().1);
            }

            // MST case:
            // if signature is not passed, put it to behind
            seen.push(hash);
        }
    }

    /// Pops transactions till it fills whole block or till the end of queue
    ///
    /// BEWARE: Shouldn't be called concurently, as it can become inconsistent
    #[allow(
        clippy::unwrap_used,
        clippy::missing_panics_doc,
        clippy::unwrap_in_result
    )]
    pub fn pop_avaliable<W: WorldTrait>(
        &self,
        is_leader: bool,
        wsv: &WorldStateView<W>,
    ) -> Vec<VersionedAcceptedTransaction> {
        let mut seen = Vec::new();

        let out = std::iter::repeat_with(|| self.pop(is_leader, wsv, &mut seen))
            .take_while(Option::is_some)
            .map(Option::unwrap)
            .take(self.txs_in_block)
            .collect::<Vec<_>>();

        #[allow(clippy::expect_used)]
        seen.into_iter()
            .try_for_each(|hash| self.queue.push(hash))
            .expect("As we never exceed the number of transactions pending");

        out
    }
}

/// This module contains all configuration related logic.
pub mod config {
    use iroha_config::derive::Configurable;
    use serde::{Deserialize, Serialize};

    const DEFAULT_MAXIMUM_TRANSACTIONS_IN_BLOCK: u32 = 2_u32.pow(13);
    // 24 hours
    const DEFAULT_TRANSACTION_TIME_TO_LIVE_MS: u64 = 24 * 60 * 60 * 1000;
    const DEFAULT_MAXIMUM_TRANSACTIONS_IN_QUEUE: u32 = 2_u32.pow(16);

    /// Configuration for `Queue`.
    #[derive(Copy, Clone, Deserialize, Serialize, Debug, Configurable)]
    #[serde(rename_all = "UPPERCASE")]
    #[serde(default)]
    #[config(env_prefix = "QUEUE_")]
    pub struct QueueConfiguration {
        /// The upper limit of the number of transactions per block.
        pub maximum_transactions_in_block: u32,
        /// The upper limit of the number of transactions waiting in this queue.
        pub maximum_transactions_in_queue: u32,
        /// The transaction will be dropped after this time if it is still in a `Queue`.
        pub transaction_time_to_live_ms: u64,
    }

    impl Default for QueueConfiguration {
        fn default() -> Self {
            Self {
                maximum_transactions_in_block: DEFAULT_MAXIMUM_TRANSACTIONS_IN_BLOCK,
                maximum_transactions_in_queue: DEFAULT_MAXIMUM_TRANSACTIONS_IN_QUEUE,
                transaction_time_to_live_ms: DEFAULT_TRANSACTION_TIME_TO_LIVE_MS,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::restriction)]

    use std::{
        collections::{BTreeMap, BTreeSet},
        thread,
        time::Duration,
    };

    use iroha_data_model::{domain::DomainsMap, peer::PeersIds};

    use super::*;
    use crate::wsv::World;

    fn accepted_tx(
        account: &str,
        domain: &str,
        proposed_ttl_ms: u64,
        key: Option<&KeyPair>,
    ) -> VersionedAcceptedTransaction {
        let key = key
            .cloned()
            .unwrap_or_else(|| KeyPair::generate().expect("Failed to generate keypair."));

        let tx = Transaction::new(
            Vec::new(),
            <Account as Identifiable>::Id::new(account, domain),
            proposed_ttl_ms,
        )
        .sign(&key)
        .expect("Failed to sign.");
        VersionedAcceptedTransaction::from_transaction(tx, 4096)
            .expect("Failed to accept Transaction.")
    }

    pub fn world_with_test_domains(public_key: PublicKey) -> World {
        let domains = DomainsMap::new();
        let mut domain = Domain::new("wonderland");
        let account_id = AccountId::new("alice", "wonderland");
        let mut account = Account::new(account_id.clone());
        account.signatories.push(public_key);
        drop(domain.accounts.insert(account_id, account));
        drop(domains.insert("wonderland".to_string(), domain));
        World::with(domains, PeersIds::new())
    }

    #[test]
    fn push_available_tx() {
        let queue = Queue::from_configuration(&QueueConfiguration {
            maximum_transactions_in_block: 2,
            transaction_time_to_live_ms: 100_000,
            maximum_transactions_in_queue: 100,
        });

        queue
            .push(accepted_tx("account", "domain", 100_000, None))
            .expect("Failed to push tx into queue");
    }

    #[test]
    fn push_available_tx_overflow() {
        let max_txs_in_queue = 10;
        let queue = Queue::from_configuration(&QueueConfiguration {
            maximum_transactions_in_block: 2,
            transaction_time_to_live_ms: 100_000,
            maximum_transactions_in_queue: max_txs_in_queue,
        });
        for _ in 0..max_txs_in_queue {
            queue
                .push(accepted_tx("account", "domain", 100_000, None))
                .expect("Failed to push tx into queue");
            thread::sleep(Duration::from_millis(10));
        }

        assert!(queue
            .push(accepted_tx("account", "domain", 100_000, None))
            .is_err());
    }

    #[test]
    fn push_multisignature_tx() {
        let queue = Queue::from_configuration(&QueueConfiguration {
            maximum_transactions_in_block: 2,
            transaction_time_to_live_ms: 100_000,
            maximum_transactions_in_queue: 100,
        });
        let tx = Transaction::new(
            Vec::new(),
            <Account as Identifiable>::Id::new("account", "domain"),
            100_000,
        );
        let get_tx = || {
            VersionedAcceptedTransaction::from_transaction(
                tx.clone()
                    .sign(&KeyPair::generate().expect("Failed to generate keypair."))
                    .expect("Failed to sign."),
                4096,
            )
            .expect("Failed to accept Transaction.")
        };

        queue.push(get_tx()).expect("Failed to push tx into queue");
        queue.push(get_tx()).expect("Failed to push tx into queue");

        assert_eq!(queue.queue.len(), 1);
        let signature_count = queue
            .txs
            .get(&queue.queue.pop().unwrap())
            .unwrap()
            .as_inner_v1()
            .signatures
            .len();
        assert_eq!(signature_count, 2);
    }

    #[test]
    fn get_available_txs() {
        let max_block_tx = 2;
        let alice_key = KeyPair::generate().expect("Failed to generate keypair.");
        let wsv = WorldStateView::new(world_with_test_domains(alice_key.public_key.clone()));
        let queue = Queue::from_configuration(&QueueConfiguration {
            maximum_transactions_in_block: max_block_tx,
            transaction_time_to_live_ms: 100_000,
            maximum_transactions_in_queue: 100,
        });
        for _ in 0..5 {
            queue
                .push(accepted_tx(
                    "alice",
                    "wonderland",
                    100_000,
                    Some(&alice_key),
                ))
                .expect("Failed to push tx into queue");
            thread::sleep(Duration::from_millis(10));
        }

        let available = queue.pop_avaliable(false, &wsv);
        assert_eq!(available.len(), max_block_tx as usize);
    }

    #[test]
    fn drop_tx_if_in_blockchain() {
        let max_block_tx = 2;
        let alice_key = KeyPair::generate().expect("Failed to generate keypair.");
        let wsv = WorldStateView::new(world_with_test_domains(alice_key.public_key.clone()));
        let tx = accepted_tx("alice", "wonderland", 100_000, Some(&alice_key));
        let _ = wsv.transactions.insert(tx.hash());
        let queue = Queue::from_configuration(&QueueConfiguration {
            maximum_transactions_in_block: max_block_tx,
            transaction_time_to_live_ms: 100_000,
            maximum_transactions_in_queue: 100,
        });
        queue.push(tx).expect("Failed to push tx into queue");
        assert_eq!(queue.pop_avaliable(false, &wsv).len(), 0);
    }

    #[test]
    fn get_available_txs_with_timeout() {
        let max_block_tx = 6;
        let alice_key = KeyPair::generate().expect("Failed to generate keypair.");
        let wsv = WorldStateView::new(world_with_test_domains(alice_key.public_key.clone()));
        let queue = Queue::from_configuration(&QueueConfiguration {
            maximum_transactions_in_block: max_block_tx,
            transaction_time_to_live_ms: 200,
            maximum_transactions_in_queue: 100,
        });
        for _ in 0..(max_block_tx - 1) {
            queue
                .push(accepted_tx("alice", "wonderland", 100, Some(&alice_key)))
                .expect("Failed to push tx into queue");
            thread::sleep(Duration::from_millis(10));
        }

        queue
            .push(accepted_tx("alice", "wonderland", 200, Some(&alice_key)))
            .expect("Failed to push tx into queue");
        std::thread::sleep(Duration::from_millis(101));
        assert_eq!(queue.pop_avaliable(false, &wsv).len(), 1);

        let wsv = WorldStateView::new(World::new());

        queue
            .push(accepted_tx("alice", "wonderland", 300, Some(&alice_key)))
            .expect("Failed to push tx into queue");
        std::thread::sleep(Duration::from_millis(101));
        assert_eq!(queue.pop_avaliable(false, &wsv).len(), 0);
    }

    #[test]
    fn get_available_txs_on_leader() {
        let max_block_tx = 2;

        let alice_key_1 = KeyPair::generate().expect("Failed to generate keypair.");
        let alice_key_2 = KeyPair::generate().expect("Failed to generate keypair.");
        let mut domain = Domain::new("wonderland");
        let account_id = AccountId::new("alice", "wonderland");
        let mut account = Account::new(account_id.clone());
        account.signatories.push(alice_key_1.public_key.clone());
        account.signatories.push(alice_key_2.public_key.clone());
        let _result = domain.accounts.insert(account_id, account);
        let mut domains = BTreeMap::new();
        let _result = domains.insert("wonderland".to_string(), domain);

        let wsv = WorldStateView::new(World::with(domains, BTreeSet::new()));
        let queue = Queue::from_configuration(&QueueConfiguration {
            maximum_transactions_in_block: max_block_tx,
            transaction_time_to_live_ms: 100_000,
            maximum_transactions_in_queue: 100,
        });

        let bob_key = KeyPair::generate().expect("Failed to generate keypair.");
        let alice_tx_1 = accepted_tx("alice", "wonderland", 100_000, Some(&alice_key_1));
        thread::sleep(Duration::from_millis(10));
        let alice_tx_2 = accepted_tx("alice", "wonderland", 100_000, Some(&alice_key_2));
        thread::sleep(Duration::from_millis(10));
        let alice_tx_3 = accepted_tx("alice", "wonderland", 100_000, Some(&bob_key));
        thread::sleep(Duration::from_millis(10));
        let alice_tx_4 = accepted_tx("alice", "wonderland", 100_000, Some(&alice_key_1));
        queue.push(alice_tx_1.clone()).unwrap();
        queue.push(alice_tx_2.clone()).unwrap();
        queue.push(alice_tx_3).unwrap();
        queue.push(alice_tx_4).unwrap();
        let output_txs: Vec<_> = queue
            .pop_avaliable(true, &wsv)
            .into_iter()
            .map(|tx| tx.hash())
            .collect();
        assert_eq!(output_txs, vec![alice_tx_1.hash(), alice_tx_2.hash()]);
        assert_eq!(queue.queue.len(), 2);
    }
}
