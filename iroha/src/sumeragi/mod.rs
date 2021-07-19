//! This module contains consensus related logic of the Iroha.
//!
//! `Consensus` trait is now implemented only by `Sumeragi` for now.

#![allow(clippy::missing_inline_in_public_items)]

use std::{
    collections::{BTreeMap, HashSet},
    fmt::{self, Debug, Formatter},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use dashmap::{DashMap, DashSet};
use futures::future;
use iroha_actor::{broker::*, prelude::*};
use iroha_crypto::{Hash, KeyPair};
use iroha_data_model::{events::Event, peer::Id as PeerId};
use iroha_error::{error, Result};
use iroha_logger::Instrument;
use network_topology::{Role, Topology};
use tokio::{sync::RwLock, task, time};

pub mod network_topology;
pub mod view_change;

use self::{
    message::{Message, *},
    view_change::ProofChain as ViewChangeProofs,
};
use crate::{
    block::{BlockHeader, ChainedBlock, EmptyChainHash, VersionedPendingBlock},
    event::EventsSender,
    genesis::GenesisNetworkTrait,
    kura::StoreBlock,
    prelude::*,
    queue::{PopPendingTransactions, QueueTrait},
    smartcontracts::permissions::{IsInstructionAllowedBoxed, IsQueryAllowedBoxed},
    wsv::WorldTrait,
    VersionedValidBlock,
};

trait Consensus {
    fn round(
        &mut self,
        transactions: Vec<VersionedAcceptedTransaction>,
    ) -> Option<VersionedPendingBlock>;
}

/// Message to send to sumeragi. It will call `update_network_topology` method on it.
#[derive(Debug, Clone, Copy, Default, iroha_actor::Message)]
pub struct UpdateNetworkTopology;

/// Message reminder for voting
#[derive(Debug, Clone, Copy, Default, iroha_actor::Message)]
pub struct Voting;

/// Message reminder for initialization of sumeragi
#[derive(Debug, Clone, Copy, iroha_actor::Message)]
pub struct Init {
    /// Latest block hash
    pub latest_block_hash: Hash,
    /// Height of merkle tree
    pub height: u64,
}

/// Get sorted peers
#[derive(Debug, Clone, Copy, iroha_actor::Message)]
#[message(result = "Vec<PeerId>")]
pub struct GetSortedPeers;

/// Get network topology
#[derive(Debug, Clone, iroha_actor::Message)]
#[message(result = "Topology")]
pub struct GetNetworkTopology(pub BlockHeader);

/// Commit block
#[derive(Debug, Clone, iroha_actor::Message)]
pub struct CommitBlock(pub VersionedValidBlock);

/// `Sumeragi` is the implementation of the consensus.
pub struct Sumeragi<Q, G, W>
where
    Q: QueueTrait,
    G: GenesisNetworkTrait,
    W: WorldTrait,
{
    key_pair: KeyPair,
    /// Address of queue
    pub queue: AlwaysAddr<Q>,
    /// The current topology of the peer to peer network.
    pub topology: Topology,
    /// The peer id of myself.
    pub peer_id: PeerId,
    /// The block in discussion this round, received from a leader.
    voting_block: Arc<RwLock<Option<VotingBlock>>>,
    /// This field is used to count votes when the peer is a proxy tail role.
    votes_for_blocks: BTreeMap<Hash, VersionedValidBlock>,
    events_sender: EventsSender,
    wsv: Arc<WorldStateView<W>>,
    /// Hashes of the transactions that were forwarded to a leader, but not yet confirmed with a receipt.
    /// And time at which this transaction was sent to the leader by this peer.
    txs_awaiting_receipts: Arc<DashMap<Hash, Instant>>,
    /// Hashes of the transactions that were accepted by the leader and are waiting to be stored in CreatedBlock.
    txs_awaiting_created_block: Arc<DashSet<Hash>>,
    commit_time: Duration,
    tx_receipt_time: Duration,
    block_time: Duration,
    block_height: u64,
    /// Hashes of invalidated blocks
    pub invalidated_blocks_hashes: Vec<Hash>,
    is_instruction_allowed: Arc<IsInstructionAllowedBoxed<W>>,
    is_query_allowed: Arc<IsQueryAllowedBoxed<W>>,
    max_instruction_number: u64,
    /// Genesis network
    pub genesis_network: Option<G>,
    /// Broker
    pub broker: Broker,
}

/// Generic sumeragi trait
pub trait SumeragiTrait:
    Actor
    + ContextHandler<UpdateNetworkTopology, Result = ()>
    + ContextHandler<Message, Result = ()>
    + ContextHandler<Init, Result = ()>
    + ContextHandler<CommitBlock, Result = ()>
    + ContextHandler<GetNetworkTopology, Result = Topology>
    + ContextHandler<GetSortedPeers, Result = Vec<PeerId>>
    + ContextHandler<IsLeader, Result = bool>
    + ContextHandler<GetLeader, Result = PeerId>
    + Debug
{
    /// Queue for sending direct messages to it
    type Queue: QueueTrait;
    /// Genesis for sending genesis txs
    type GenesisNetwork: GenesisNetworkTrait;
    /// World for updating WSV after block commitment
    type World: WorldTrait;

    /// Default `Sumeragi` constructor.
    ///
    /// # Errors
    /// Can fail during initing network topology
    fn from_configuration(
        configuration: &config::SumeragiConfiguration,
        events_sender: EventsSender,
        wsv: Arc<WorldStateView<Self::World>>,
        is_instruction_allowed: IsInstructionAllowedBoxed<Self::World>,
        is_query_allowed: Arc<IsQueryAllowedBoxed<Self::World>>,
        genesis_network: Option<Self::GenesisNetwork>,
        queue: AlwaysAddr<Self::Queue>,
        broker: Broker,
        //TODO: separate initialization from construction and do not return Result in `new`
    ) -> Result<Self>;
}

impl<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait> SumeragiTrait for Sumeragi<Q, G, W> {
    type Queue = Q;
    type GenesisNetwork = G;
    type World = W;

    fn from_configuration(
        configuration: &config::SumeragiConfiguration,
        events_sender: EventsSender,
        wsv: Arc<WorldStateView<W>>,
        is_instruction_allowed: IsInstructionAllowedBoxed<W>,
        is_query_allowed: Arc<IsQueryAllowedBoxed<W>>,
        genesis_network: Option<G>,
        queue: AlwaysAddr<Self::Queue>,
        broker: Broker,
    ) -> Result<Self> {
        let network_topology = Topology::builder()
            .at_block(EmptyChainHash.into())
            .with_max_faults(configuration.max_faulty_peers)
            .reshuffle_after(configuration.n_topology_shifts_before_reshuffle)
            .with_peers(configuration.trusted_peers.peers.clone())
            .build()?;
        Ok(Self {
            key_pair: configuration.key_pair.clone(),
            topology: network_topology,
            peer_id: configuration.peer_id.clone(),
            voting_block: Arc::new(RwLock::new(None)),
            votes_for_blocks: BTreeMap::new(),
            events_sender,
            wsv,
            txs_awaiting_receipts: Arc::new(DashMap::new()),
            txs_awaiting_created_block: Arc::new(DashSet::new()),
            commit_time: Duration::from_millis(configuration.commit_time_ms),
            tx_receipt_time: Duration::from_millis(configuration.tx_receipt_time_ms),
            block_time: Duration::from_millis(configuration.block_time_ms),
            block_height: 0,
            invalidated_blocks_hashes: Vec::new(),
            is_instruction_allowed: Arc::new(is_instruction_allowed),
            is_query_allowed,
            max_instruction_number: configuration.max_instruction_number,
            genesis_network,
            queue,
            broker,
        })
    }
}

/// The interval at which sumeragi checks if there are tx in the `queue`.
pub const TX_RETRIEVAL_INTERVAL: Duration = Duration::from_millis(100);

#[async_trait::async_trait]
impl<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait> Actor for Sumeragi<Q, G, W> {
    async fn on_start(&mut self, ctx: &mut Context<Self>) {
        self.broker.subscribe::<UpdateNetworkTopology, _>(ctx);
        self.broker.subscribe::<Message, _>(ctx);
        self.broker.subscribe::<Init, _>(ctx);
        self.broker.subscribe::<CommitBlock, _>(ctx);
    }
}

#[async_trait::async_trait]
impl<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait> Handler<UpdateNetworkTopology>
    for Sumeragi<Q, G, W>
{
    type Result = ();
    async fn handle(&mut self, UpdateNetworkTopology: UpdateNetworkTopology) {
        self.update_network_topology().await;
    }
}

#[async_trait::async_trait]
impl<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait> Handler<Message> for Sumeragi<Q, G, W> {
    type Result = ();
    async fn handle(&mut self, message: Message) {
        if let Err(error) = message.handle(&mut self).await {
            iroha_logger::error!(%error, "Handle message failed");
        }
    }
}

#[async_trait::async_trait]
impl<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait> Handler<Voting> for Sumeragi<Q, G, W> {
    type Result = ();
    async fn handle(&mut self, Voting: Voting) {
        if self.voting_in_progress().await {
            return;
        }
        let is_leader = self.is_leader();
        let transactions = self.queue.send(PopPendingTransactions { is_leader }).await;

        if let Err(error) = self.round(transactions).await {
            iroha_logger::error!(%error, "Round failed");
        }
    }
}

#[async_trait::async_trait]
impl<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait> ContextHandler<Init>
    for Sumeragi<Q, G, W>
{
    type Result = ();
    async fn handle(
        &mut self,
        ctx: &mut Context<Self>,
        Init {
            latest_block_hash,
            height,
        }: Init,
    ) {
        if height != 0 && latest_block_hash != Hash([0; 32]) {
            self.init(latest_block_hash, height);
        } else if let Some(genesis_network) = self.genesis_network.take() {
            if let Err(err) = genesis_network.submit_transactions(&mut self).await {
                iroha_logger::error!("Failed to submit genesis transactions: {}", err)
            }
        }
        self.update_network_topology().await;
        ctx.notify_every::<Voting>(TX_RETRIEVAL_INTERVAL);
    }
}

#[async_trait::async_trait]
impl<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait> Handler<GetSortedPeers>
    for Sumeragi<Q, G, W>
{
    type Result = Vec<PeerId>;
    async fn handle(&mut self, GetSortedPeers: GetSortedPeers) -> Vec<PeerId> {
        self.topology.sorted_peers().to_vec()
    }
}

#[async_trait::async_trait]
impl<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait> Handler<GetNetworkTopology>
    for Sumeragi<Q, G, W>
{
    type Result = Topology;
    async fn handle(&mut self, GetNetworkTopology(header): GetNetworkTopology) -> Topology {
        self.network_topology_current_or_genesis(&header)
    }
}

#[async_trait::async_trait]
impl<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait> Handler<CommitBlock>
    for Sumeragi<Q, G, W>
{
    type Result = ();
    async fn handle(&mut self, CommitBlock(block): CommitBlock) {
        self.commit_block(block).await
    }
}

/// Returns if peer is leader
#[derive(Clone, Copy, Debug, iroha_actor::Message)]
#[message(result = "bool")]
pub struct IsLeader;

#[async_trait::async_trait]
impl<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait> Handler<IsLeader> for Sumeragi<Q, G, W> {
    type Result = bool;
    async fn handle(&mut self, IsLeader: IsLeader) -> bool {
        self.is_leader()
    }
}

/// Gets leader from sumeragi
#[derive(Clone, Copy, Debug, iroha_actor::Message)]
#[message(result = "PeerId")]
pub struct GetLeader;

#[async_trait::async_trait]
impl<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait> Handler<GetLeader>
    for Sumeragi<Q, G, W>
{
    type Result = PeerId;
    async fn handle(&mut self, GetLeader: GetLeader) -> PeerId {
        self.topology.leader().clone()
    }
}

impl<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait> Sumeragi<Q, G, W> {
    /// Initializes sumeragi with the `latest_block_hash` and `block_height` after Kura loads the blocks.
    pub fn init(&mut self, latest_block_hash: Hash, block_height: u64) {
        self.block_height = block_height;
        self.topology.apply_block(latest_block_hash);
    }

    /// Updates network topology by taking the actual list of peers from `WorldStateView`.
    /// Updates it only if the new peers were added, otherwise leaves the order unchanged.
    #[allow(clippy::expect_used)]
    pub async fn update_network_topology(&mut self) {
        let wsv_peers: HashSet<_> = self.wsv.trusted_peers_ids().clone().into_iter().collect();
        let topology_peers: HashSet<_> = self.topology.sorted_peers().iter().cloned().collect();
        if topology_peers != wsv_peers {
            self.topology = self.topology
                .clone()
                .into_builder()
                .with_peers(wsv_peers)
                .build()
                // TODO: Check it during instruction execution.
                .expect("The safety of changing the number of peers should have been checked at Instruction execution stage.");
        }
    }

    /// Returns `true` if some block is in discussion, `false` otherwise.
    pub async fn voting_in_progress(&self) -> bool {
        self.voting_block.write().await.is_some()
    }

    /// Latest block hash as seen by sumeragi.
    pub fn latest_block_hash(&self) -> Hash {
        self.topology.at_block()
    }

    /// Number of view changes.
    /// Where a view change is a change in topology made if there was some consensus misfunction during round (faulty peers).
    pub fn number_of_view_changes(&self) -> u64 {
        self.topology.view_change_proofs().len() as u64
    }

    /// The proofs of view changes that happened after the last block was committed.
    pub fn view_change_proofs(&self) -> &ViewChangeProofs {
        self.topology.view_change_proofs()
    }

    /// The hash of the latest view change.
    pub fn latest_view_change_hash(&self) -> Hash {
        self.view_change_proofs().latest_hash()
    }

    /// Get peers as a hash set of their ids.
    pub fn peers(&self) -> HashSet<PeerId> {
        self.topology.sorted_peers().iter().cloned().collect()
    }

    /// Assumes this peer is a leader and starts the round with the given `genesis_topology`.
    ///
    /// # Errors
    /// Can fail if:
    /// * transactions are empty
    /// * peer is not leader
    /// * there are already some blocks in blockchain
    #[iroha_futures::telemetry_future]
    #[iroha_logger::log(skip(self, transactions, genesis_topology))]
    pub async fn start_genesis_round(
        &mut self,
        transactions: Vec<VersionedAcceptedTransaction>,
        genesis_topology: Topology,
    ) -> Result<()> {
        if transactions.is_empty() {
            Err(error!("Genesis transactions set is empty."))
        } else if genesis_topology.leader() != &self.peer_id {
            Err(error!(
                "Incorrect network topology this peer should be {:?} but is {:?}",
                Role::Leader,
                genesis_topology.role(&self.peer_id)
            ))
        } else if self.block_height > 0 {
            Err(error!(
                "Block height should be 0 for genesis round. But it is: {}",
                self.block_height
            ))
        } else {
            self.validate_and_publish_created_block(
                PendingBlock::new(transactions).chain_first_with_genesis_topology(genesis_topology),
            )
            .await
        }
    }

    /// The leader of each round just uses the transactions they have at hand to create a block.
    ///
    /// # Errors
    /// Can fail during signing of block
    #[iroha_futures::telemetry_future]
    pub async fn round(&mut self, transactions: Vec<VersionedAcceptedTransaction>) -> Result<()> {
        if transactions.is_empty() {
            return Ok(());
        }

        self.gossip_transactions(&transactions).await;
        if let Role::Leader = self.topology.role(&self.peer_id) {
            let block = PendingBlock::new(transactions).chain(
                self.block_height,
                self.latest_block_hash(),
                self.view_change_proofs().clone(),
                self.invalidated_blocks_hashes.clone(),
            );
            self.validate_and_publish_created_block(block).await?;
        } else {
            self.forward_transactions_to_leader(&transactions).await;
        }
        Ok(())
    }

    /// Forwards transactions to the leader and waits for receipts.
    #[iroha_futures::telemetry_future]
    pub async fn forward_transactions_to_leader(
        &mut self,
        transactions: &[VersionedAcceptedTransaction],
    ) {
        let mut send_futures = Vec::new();
        for transaction in transactions {
            let transaction_hash = transaction.hash();
            if self.txs_awaiting_receipts.contains_key(&transaction_hash) {
                // This peer has already sent this transaction to leader and is waiting for a receipt.
                // Without this `if` depending on the round time, the peers might DOS themselves.
                continue;
            }
            iroha_logger::info!(
                "{:?} - {} - Forwarding transaction to leader({}). Transaction hash: {}",
                self.topology.role(&self.peer_id),
                self.peer_id.address,
                self.topology.leader().address,
                transaction_hash,
            );
            send_futures.push(
                VersionedMessage::from(Message::from(TransactionForwarded::new(
                    transaction,
                    &self.peer_id,
                )))
                .send_to(self.topology.leader()),
            );
            // Don't require leader to submit receipts and therefore create blocks if the transaction is still waiting for more signatures.
            #[allow(clippy::expect_used)]
            if let Ok(true) = transaction.check_signature_condition(&self.wsv) {
                let _ = self
                    .txs_awaiting_receipts
                    .insert(transaction.hash(), Instant::now());
            }
            let transactions_awaiting_receipts = Arc::clone(&self.txs_awaiting_receipts);
            let mut no_tx_receipt = view_change::Proof::no_transaction_receipt_received(
                transaction_hash,
                self.latest_view_change_hash(),
                self.latest_block_hash(),
            );
            #[allow(clippy::expect_used)]
            {
                no_tx_receipt = no_tx_receipt
                    .sign(&self.key_pair)
                    .expect("Failed to put first signature.");
            }
            let recipient_peers = self.topology.sorted_peers().to_vec();
            let peer_id = self.peer_id.clone();
            let tx_receipt_time = self.tx_receipt_time;
            drop(task::spawn(async move {
                time::sleep(tx_receipt_time).await;
                if transactions_awaiting_receipts.contains_key(&transaction_hash) {
                    iroha_logger::warn!(
                        "Transaction receipt timeout detected! Transaction hash: {}",
                        transaction_hash
                    );
                    let mut send_futures = Vec::new();
                    for peer in &recipient_peers {
                        if *peer != peer_id {
                            send_futures.push(
                                VersionedMessage::from(Message::ViewChangeSuggested(
                                    no_tx_receipt.clone().into(),
                                ))
                                .send_to(peer),
                            );
                        }
                    }
                    future::join_all(send_futures)
                        .await
                        .into_iter()
                        .filter_map(Result::err)
                        .for_each(|error| {
                            iroha_logger::error!(
                                "Failed to send ViewChangeSuggested (NoTransactionReceiptReceived) message to peers: {:?}",
                                error
                            )
                        });
                }
            }.in_current_span()));
        }
        future::join_all(send_futures)
            .await
            .into_iter()
            .filter_map(Result::err)
            .for_each(|error| {
                iroha_logger::error!("Failed to send transactions to the leader: {:?}", error)
            });
    }

    /// Gossip transactions to other peers.
    #[iroha_futures::telemetry_future]
    pub async fn gossip_transactions(&mut self, transactions: &[VersionedAcceptedTransaction]) {
        iroha_logger::debug!(
            "{:?} - Gossiping transactions. Number of transactions to forward: {}",
            self.topology.role(&self.peer_id),
            transactions.len(),
        );
        let leader = self.topology.leader().clone();
        let this_peer = self.peer_id.clone();
        let peers = self.topology.sorted_peers().to_vec();
        let transactions = transactions.to_vec();
        let mut send_futures = Vec::new();
        // TODO: send transactions in batch not to crowd message channels.
        for peer in &peers {
            for transaction in &transactions {
                if peer != &leader && peer != &this_peer {
                    let message = VersionedMessage::from(Message::from(TransactionForwarded::new(
                        transaction,
                        &this_peer,
                    )));
                    send_futures.push(message.send_to(peer));
                }
            }
        }
        let results = future::join_all(send_futures).await;
        results
            .into_iter()
            .filter_map(Result::err)
            .for_each(|error| iroha_logger::error!("Failed to gossip transactions: {:?}", error));
    }

    /// Should be called by a leader to start the consensus round with `BlockCreated` message.
    ///
    /// # Errors
    /// Can fail signing block
    #[iroha_futures::telemetry_future]
    pub async fn validate_and_publish_created_block(&mut self, block: ChainedBlock) -> Result<()> {
        let block = block.validate(
            &*self.wsv,
            &self.is_instruction_allowed,
            &self.is_query_allowed,
        );
        let network_topology = self.network_topology_current_or_genesis(block.header());
        iroha_logger::info!(
            "{:?} - Created a block with hash {}.",
            network_topology.role(&self.peer_id),
            block.hash(),
        );
        for event in Vec::<Event>::from(&block.clone()) {
            if let Err(err) = self.events_sender.send(event).await {
                iroha_logger::warn!("Failed to publish event: {}", err);
            }
        }
        if !network_topology.is_consensus_required() {
            self.commit_block(block).await;
            return Ok(());
        }

        let voting_block = VotingBlock::new(block.clone());
        *self.voting_block.write().await = Some(voting_block.clone());
        let message = VersionedMessage::from(Message::BlockCreated(
            block.clone().sign(&self.key_pair)?.into(),
        ));
        let recipient_peers = network_topology.sorted_peers().to_vec();
        let this_peer = self.peer_id.clone();
        let mut send_futures = Vec::new();
        for peer in &recipient_peers {
            if this_peer != *peer {
                send_futures.push(message.clone().send_to(peer));
            }
        }
        let results = futures::future::join_all(send_futures).await;
        results
            .into_iter()
            .filter_map(Result::err)
            .for_each(|error| {
                iroha_logger::error!(
                    "Failed to send BlockCreated messages from {}: {:?}",
                    this_peer.address,
                    error
                )
            });
        self.start_commit_countdown(
            voting_block.clone(),
            self.latest_block_hash(),
            self.latest_view_change_hash(),
        )
        .await;
        Ok(())
    }

    /// Starts countdown for a period in which the `voting_block` should be committed.
    #[iroha_futures::telemetry_future]
    #[iroha_logger::log(skip(self, voting_block))]
    pub async fn start_commit_countdown(
        &self,
        voting_block: VotingBlock,
        latest_block_hash: Hash,
        latest_view_change_hash: Hash,
    ) {
        let old_voting_block = voting_block;
        let voting_block = Arc::clone(&self.voting_block);
        let key_pair = self.key_pair.clone();
        let recipient_peers = self.topology.sorted_peers().to_vec();
        let peer_id = self.peer_id.clone();
        let commit_time = self.commit_time;
        drop(task::spawn(
            async move {
                time::sleep(commit_time).await;
                let voting_block = if let Some(voting_block) = voting_block.write().await.clone() {
                    voting_block
                } else {
                    return;
                };

                // If the block was not yet committed send commit timeout to other peers to initiate view change.
                if voting_block.block.hash() != old_voting_block.block.hash() {
                    return;
                }

                iroha_logger::warn!(
                    "Block commit timeout detected! Voting block hash: {}",
                    voting_block.block.hash()
                );
                #[allow(clippy::expect_used)]
                let message = VersionedMessage::from(Message::ViewChangeSuggested(
                    view_change::Proof::commit_timeout(
                        voting_block.block.hash(),
                        latest_view_change_hash,
                        latest_block_hash,
                    )
                    .sign(&key_pair)
                    .expect("Failed to sign CommitTimeout")
                    .into(),
                ));
                let mut send_futures = Vec::new();
                for peer in &recipient_peers {
                    if *peer != peer_id {
                        send_futures.push(message.clone().send_to(peer));
                    }
                }
                future::join_all(send_futures)
                    .await
                    .into_iter()
                    .filter_map(Result::err)
                    .for_each(|error| {
                        iroha_logger::error!("Failed to send CommitTimeout messages: {:?}", error)
                    });
            }
            .in_current_span(),
        ));
    }

    /// Commits `ValidBlock` and changes the state of the `Sumeragi` and its `NetworkTopology`.
    #[iroha_logger::log(skip(self, block))]
    #[iroha_futures::telemetry_future]
    pub async fn commit_block(&mut self, block: VersionedValidBlock) {
        let block_hash = block.hash();
        self.invalidated_blocks_hashes.clear();
        self.txs_awaiting_created_block.clear();
        self.txs_awaiting_receipts.clear();
        self.block_height = block.header().height;

        let block = block.commit();

        for event in Vec::<Event>::from(&block) {
            if let Err(error) = self.events_sender.send(event).await {
                iroha_logger::warn!(%error, "Failed to publish event");
            }
        }

        self.broker.issue_send(StoreBlock(block)).await;

        let previous_role = self.topology.role(&self.peer_id);
        self.topology.apply_block(block_hash);
        iroha_logger::info!(
            "{:?} - Commiting block with hash {}. New role: {:?}. New height: {}",
            previous_role,
            block_hash,
            self.topology.role(&self.peer_id),
            self.block_height,
        );
        *self.voting_block.write().await = None;
        self.votes_for_blocks.clear();
    }

    #[iroha_futures::telemetry_future]
    async fn change_view(
        &mut self,
        proof: view_change::Proof,
        invalidated_block_hash: Option<Hash>,
    ) {
        self.txs_awaiting_created_block.clear();
        self.txs_awaiting_receipts.clear();
        let previous_role = self.topology.role(&self.peer_id);
        if let Some(invalidated_block_hash) = invalidated_block_hash {
            self.invalidated_blocks_hashes.push(invalidated_block_hash)
        }
        self.topology.apply_view_change(proof.clone());
        *self.voting_block.write().await = None;
        iroha_logger::info!(
            "{} - {:?} - Changing view at block with hash {}. New role: {:?}. Number of view changes (including this): {}. Reason for a view change: {}",
            self.peer_id.address,
            previous_role,
            self.latest_block_hash(),
            self.topology.role(&self.peer_id),
            self.number_of_view_changes(),
            proof.reason()
        );
    }

    /// If this peer is a leader in this round.
    pub fn is_leader(&self) -> bool {
        self.topology.role(&self.peer_id) == Role::Leader
    }

    /// Returns current network topology or genesis specific one, if the `block` is a genesis block.
    pub fn network_topology_current_or_genesis(&self, header: &BlockHeader) -> Topology {
        if header.is_genesis() && self.block_height == 0 {
            if let Some(genesis_topology) = &header.genesis_topology {
                iroha_logger::info!("Using network topology from genesis block.");
                return genesis_topology.clone();
            }
        }

        self.topology.clone()
    }
}

impl<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait> Debug for Sumeragi<Q, G, W> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sumeragi")
            .field("public_key", &self.key_pair.public_key)
            .field("network_topology", &self.topology)
            .field("peer_id", &self.peer_id)
            .field("voting_block", &self.voting_block)
            .finish()
    }
}

/// Structure represents a block that is currently in discussion.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct VotingBlock {
    /// At what time has this peer voted for this block
    pub voted_at: Duration,
    /// Valid Block
    pub block: VersionedValidBlock,
}

impl VotingBlock {
    /// Constructs new `VotingBlock.`
    #[allow(clippy::expect_used)]
    pub fn new(block: VersionedValidBlock) -> VotingBlock {
        VotingBlock {
            voted_at: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("Failed to get System Time."),
            block,
        }
    }
}

/// Contains message structures for p2p communication during consensus.
pub mod message {
    #![allow(clippy::module_name_repetitions)]

    use std::{
        collections::HashSet,
        sync::Arc,
        time::{Duration, Instant, SystemTime},
    };

    use iroha_crypto::{Hash, KeyPair, Signature, Signatures};
    use iroha_data_model::prelude::*;
    use iroha_derive::*;
    use iroha_error::{error, Result, WrapErr};
    use iroha_logger::Instrument;
    use iroha_network::prelude::*;
    use iroha_version::prelude::*;
    use parity_scale_codec::{Decode, Encode};
    use tokio::{task, time};

    use super::view_change;
    use crate::{
        genesis::GenesisNetworkTrait,
        queue::QueueTrait,
        sumeragi::{Role, Sumeragi, Topology, VotingBlock},
        torii::uri,
        wsv::WorldTrait,
        VersionedAcceptedTransaction, VersionedValidBlock,
    };

    declare_versioned_with_scale!(VersionedMessage 1..2, Debug, Clone, iroha_derive::FromVariant);

    impl VersionedMessage {
        /// Same as [`as_v1`](`VersionedMessage::as_v1()`) but also does conversion
        pub const fn as_inner_v1(&self) -> &Message {
            match self {
                Self::V1(v1) => &v1.0,
            }
        }

        /// Same as [`as_inner_v1`](`VersionedMessage::as_inner_v1()`) but returns mutable reference
        pub fn as_mut_inner_v1(&mut self) -> &mut Message {
            match self {
                Self::V1(v1) => &mut v1.0,
            }
        }

        /// Same as [`into_v1`](`VersionedMessage::into_v1()`) but also does conversion
        #[allow(clippy::missing_const_for_fn)]
        pub fn into_inner_v1(self) -> Message {
            match self {
                Self::V1(v1) => v1.0,
            }
        }

        /// Send this message over the network to the specified `peer`.
        /// # Errors
        /// Fails if network sending fails
        #[iroha_futures::telemetry_future]
        #[iroha_logger::log(skip(self))]
        pub async fn send_to(self, peer: &PeerId) -> Result<()> {
            match Network::send_request_to(
                &peer.address,
                Request::new(uri::CONSENSUS_URI, self.encode_versioned()?),
            )
            .await
            .wrap_err_with(|| format!("Failed to send to peer {} with error", peer.address))?
            {
                Response::Ok(_) => Ok(()),
                Response::InternalError => Err(error!(
                    "Failed to send message - Internal Error on peer: {:?}",
                    peer
                )),
            }
        }

        /// Handles this message as part of `Sumeragi` consensus.
        /// # Errors
        /// Fails if message handling fails
        #[iroha_futures::telemetry_future]
        pub async fn handle<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait>(
            &self,
            sumeragi: &mut Sumeragi<Q, G, W>,
        ) -> Result<()> {
            self.as_inner_v1().handle(sumeragi).await
        }
    }

    /// Message's variants that are used by peers to communicate in the process of consensus.
    #[version_with_scale(n = 1, versioned = "VersionedMessage", derive = "Debug, Clone")]
    #[derive(Io, Decode, Encode, Debug, Clone, FromVariant, iroha_actor::Message)]
    pub enum Message {
        /// Is sent by leader to all validating peers, when a new block is created.
        BlockCreated(BlockCreated),
        /// Is sent by validating peers to proxy tail and observing peers when they have signed this block.
        BlockSigned(BlockSigned),
        /// Is sent by proxy tail to validating peers and to leader, when the block is committed.
        BlockCommitted(BlockCommitted),
        /// Receipt of receiving tx from peer. Sent by a leader.
        TransactionReceived(TransactionReceipt),
        /// Tx forwarded from client by a peer to a leader.
        TransactionForwarded(TransactionForwarded),
        /// View change is suggested due to some faulty peer or general fault in consensus.
        ViewChangeSuggested(ViewChangeSuggested),
    }

    impl Message {
        /// Handles this message as part of `Sumeragi` consensus.
        /// # Errors
        /// Fails if message handling fails
        #[iroha_logger::log(skip(self, sumeragi))]
        #[iroha_futures::telemetry_future]
        pub async fn handle<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait>(
            &self,
            sumeragi: &mut Sumeragi<Q, G, W>,
        ) -> Result<()> {
            match self {
                Message::BlockCreated(block_created) => block_created.handle(sumeragi).await,
                Message::BlockSigned(block_signed) => block_signed.handle(sumeragi).await,
                Message::BlockCommitted(block_committed) => block_committed.handle(sumeragi).await,
                Message::TransactionReceived(transaction_receipt) => {
                    transaction_receipt.handle(sumeragi).await
                }
                Message::TransactionForwarded(transaction_forwarded) => {
                    transaction_forwarded.handle(sumeragi).await
                }
                Message::ViewChangeSuggested(view_change_suggested) => {
                    view_change_suggested.handle(sumeragi).await
                }
            }
        }
    }

    /// `ViewChangeSuggested` message structure.
    #[derive(Io, Decode, Encode, Debug, Clone)]
    pub struct ViewChangeSuggested {
        /// Proof of view change. As part of this message handling, all peers which agree with view change should sign it.
        pub proof: view_change::Proof,
    }

    impl ViewChangeSuggested {
        /// Handles this message as part of `Sumeragi` consensus.
        ///
        /// # Errors
        /// Can fail during signing.
        #[iroha_futures::telemetry_future]
        pub async fn handle<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait>(
            &self,
            sumeragi: &mut Sumeragi<Q, G, W>,
        ) -> Result<()> {
            use view_change::Reason::*;

            if !self.proof.has_same_state(
                sumeragi.latest_block_hash(),
                sumeragi.latest_view_change_hash(),
            ) {
                return Ok(());
            }
            let (should_vote, invalidated_block_hash) = match self.proof.reason() {
                CommitTimeout(reason) => (
                    Self::is_commit_timeout(reason, sumeragi).await,
                    Some(reason.voting_block_hash),
                ),
                NoTransactionReceiptReceived(reason) => (
                    Self::is_no_transaction_receipt_recieved(reason, sumeragi).await,
                    None,
                ),
                BlockCreationTimeout(reason) => (
                    Self::is_block_creation_timeout(reason, sumeragi).await,
                    None,
                ),
            };
            let already_voted = self
                .proof
                .signatures()
                .contains(&sumeragi.key_pair.public_key);
            let view_change_suggested = if should_vote && !already_voted {
                let view_change_suggested = self.clone().sign(&sumeragi.key_pair)?;
                let peer_id = sumeragi.peer_id.clone();
                let peers = sumeragi.peers();
                let view_change_suggested_cloned = view_change_suggested.clone();
                // Sending message in parallel as it can block peer and during consensus whole blockchain.
                drop(task::spawn(async move {
                    view_change_suggested_cloned
                        .send_to_all(peers, peer_id)
                        .await
                }));
                view_change_suggested
            } else {
                self.clone()
            };
            if view_change_suggested
                .proof
                .verify(&sumeragi.peers(), sumeragi.topology.max_faults())
            {
                sumeragi
                    .change_view(view_change_suggested.proof, invalidated_block_hash)
                    .await;
            } else {
            }
            Ok(())
        }

        async fn is_commit_timeout<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait>(
            reason: &view_change::CommitTimeout,
            sumeragi: &Sumeragi<Q, G, W>,
        ) -> bool {
            let voting_block = sumeragi.voting_block.read().await.clone();
            #[allow(clippy::expect_used)]
            let current_time = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("Failed to get System Time.");
            voting_block.map_or(false, |voting_block| {
                voting_block.block.hash() == reason.voting_block_hash
                    && (current_time - voting_block.voted_at) >= sumeragi.commit_time
            })
        }

        async fn is_block_creation_timeout<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait>(
            reason: &view_change::BlockCreationTimeout,
            sumeragi: &Sumeragi<Q, G, W>,
        ) -> bool {
            reason.transaction_receipt.is_valid(&sumeragi.topology)
                && reason.transaction_receipt.is_block_should_be_created(sumeragi.block_time)
                // Block is not yet created
                && sumeragi.voting_block.write().await.is_none()
        }

        async fn is_no_transaction_receipt_recieved<
            Q: QueueTrait,
            G: GenesisNetworkTrait,
            W: WorldTrait,
        >(
            reason: &view_change::NoTransactionReceiptReceived,
            sumeragi: &Sumeragi<Q, G, W>,
        ) -> bool {
            let current_time = Instant::now();
            // Due to the fact that transactions are all the time gossiped -
            // if the leader is not sending a receipt for some transaction every peer will know it.
            // And therefore will have it in `transactions_awaiting_receipts`.
            // If it doesn't have it then either this peer is faulty or the one sending this message is faulty.
            let sent_at = if let Some(sent_at) =
                sumeragi.txs_awaiting_receipts.get(&reason.transaction_hash)
            {
                sent_at.to_owned()
            } else {
                return false;
            };

            current_time.duration_since(sent_at) >= sumeragi.tx_receipt_time
        }

        fn sign(self, key_pair: &KeyPair) -> Result<Self> {
            Ok(Self {
                proof: self.proof.sign(key_pair)?,
            })
        }

        async fn send_to_all(&self, peers: HashSet<PeerId>, this_peer: PeerId) {
            let view_change_suggested =
                VersionedMessage::from(Message::ViewChangeSuggested(self.clone()));
            drop(
                futures::future::join_all(
                    peers
                        .iter()
                        .filter(|peer_id| peer_id != &&this_peer)
                        .map(|peer| view_change_suggested.clone().send_to(peer)),
                )
                .await,
            );
        }
    }

    impl From<view_change::Proof> for ViewChangeSuggested {
        fn from(proof: view_change::Proof) -> Self {
            ViewChangeSuggested { proof }
        }
    }

    /// `BlockCreated` message structure.
    #[derive(Io, Decode, Encode, Debug, Clone)]
    #[non_exhaustive]
    pub struct BlockCreated {
        /// The corresponding block.
        pub block: VersionedValidBlock,
    }

    impl BlockCreated {
        fn update_view_changes<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait>(
            &self,
            sumeragi: &mut Sumeragi<Q, G, W>,
        ) {
            let leader_view_changes = self.block.header().view_change_proofs.clone();
            #[allow(clippy::expect_used)]
            if leader_view_changes.len() > sumeragi.topology.view_change_proofs().len()
                && leader_view_changes.verify_with_state(
                    &sumeragi.peers(),
                    sumeragi.topology.max_faults(),
                    sumeragi.latest_block_hash(),
                )
            {
                iroha_logger::info!("Updating number of view changes on BlockCreated from leader. Number of view changes {} -> {}", sumeragi.topology.view_change_proofs().len(), leader_view_changes.len());
                sumeragi.topology = sumeragi
                    .topology
                    .clone()
                    .into_builder()
                    .with_view_changes(leader_view_changes)
                    .build()
                    .expect("When only changing view changes it should not fail.")
            }
        }

        /// Handles this message as part of `Sumeragi` consensus.
        ///
        /// # Errors
        /// Can fail due to signing of block
        #[iroha_futures::telemetry_future]
        pub async fn handle<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait>(
            &self,
            sumeragi: &mut Sumeragi<Q, G, W>,
        ) -> Result<()> {
            // There should be only one block in discussion during a round.
            if sumeragi.voting_block.write().await.is_some() {
                return Ok(());
            }
            for event in Vec::<Event>::from(&self.block.clone()) {
                if let Err(err) = sumeragi.events_sender.send(event).await {
                    iroha_logger::error!("Failed to send block created event: {}", err)
                }
            }
            self.update_view_changes(sumeragi);
            let network_topology =
                sumeragi.network_topology_current_or_genesis(self.block.header());
            if network_topology
                .filter_signatures_by_roles(&[Role::Leader], &self.block.verified_signatures())
                .is_empty()
            {
                iroha_logger::error!(
                    "{:?} - Rejecting Block as it is not signed by leader.",
                    sumeragi.topology.role(&sumeragi.peer_id),
                );
                return Ok(());
            }
            sumeragi.txs_awaiting_created_block.clear();
            if network_topology.role(&sumeragi.peer_id) == Role::ValidatingPeer
                && self.block.validation_check(
                    &sumeragi.wsv,
                    sumeragi.latest_block_hash(),
                    sumeragi.latest_view_change_hash(),
                    sumeragi.block_height,
                    sumeragi.max_instruction_number,
                )
            {
                let block_clone = self.block.clone();
                let wsv_clone = Arc::clone(&sumeragi.wsv);
                let is_instruction_allowed_clone = Arc::clone(&sumeragi.is_instruction_allowed);
                let is_query_allowed_clone = Arc::clone(&sumeragi.is_query_allowed);
                let key_pair_clone = sumeragi.key_pair.clone();
                let signed_block = task::spawn_blocking(move || -> Result<BlockSigned> {
                    block_clone
                        .revalidate(
                            &*wsv_clone,
                            &*is_instruction_allowed_clone,
                            &*is_query_allowed_clone,
                        )
                        .sign(&key_pair_clone)
                        .map(Into::into)
                })
                .await??;
                if let Err(e) = VersionedMessage::from(Message::BlockSigned(signed_block))
                    .send_to(network_topology.proxy_tail())
                    .await
                {
                    iroha_logger::error!(
                        "Failed to send BlockSigned message to the proxy tail: {:?}",
                        e
                    );
                } else {
                    iroha_logger::info!(
                        "{:?} - Signed block candidate with hash {}.",
                        network_topology.role(&sumeragi.peer_id),
                        self.block.hash(),
                    );
                }
                //TODO: send to set b so they can observe
            }
            let voting_block = VotingBlock::new(self.block.clone());
            *sumeragi.voting_block.write().await = Some(voting_block.clone());
            sumeragi
                .start_commit_countdown(
                    voting_block,
                    sumeragi.latest_block_hash(),
                    sumeragi.latest_view_change_hash(),
                )
                .await;
            Ok(())
        }
    }

    impl From<VersionedValidBlock> for BlockCreated {
        fn from(block: VersionedValidBlock) -> Self {
            Self { block }
        }
    }

    /// `BlockSigned` message structure.
    #[derive(Io, Decode, Encode, Debug, Clone)]
    #[non_exhaustive]
    pub struct BlockSigned {
        /// The corresponding block.
        pub block: VersionedValidBlock,
    }

    impl BlockSigned {
        /// Handles this message as part of `Sumeragi` consensus.
        ///
        /// # Errors
        /// Can fail due to signing of block
        #[iroha_futures::telemetry_future]
        pub async fn handle<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait>(
            &self,
            sumeragi: &mut Sumeragi<Q, G, W>,
        ) -> Result<()> {
            let network_topology =
                sumeragi.network_topology_current_or_genesis(self.block.header());
            if let Role::ProxyTail = network_topology.role(&sumeragi.peer_id) {
                let block_hash = self.block.hash();
                let entry = sumeragi
                    .votes_for_blocks
                    .entry(block_hash)
                    .or_insert_with(|| self.block.clone());
                entry
                    .as_mut_inner_v1()
                    .signatures
                    .append(&self.block.verified_signatures());
                let valid_signatures = network_topology.filter_signatures_by_roles(
                    &[Role::ValidatingPeer, Role::Leader],
                    &entry.verified_signatures(),
                );
                iroha_logger::info!(
                    "{:?} - Recieved a vote for block with hash {}. Now it has {} signatures out of {} required (not counting ProxyTail signature).",
                    network_topology.role(&sumeragi.peer_id),
                    block_hash,
                    valid_signatures.len(),
                    network_topology.min_votes_for_commit() - 1,
                );
                if valid_signatures.len() >= network_topology.min_votes_for_commit() as usize - 1 {
                    let mut signatures = Signatures::default();
                    signatures.append(&valid_signatures);
                    let mut block = entry.clone();
                    block.as_mut_inner_v1().signatures = signatures;
                    let block = block.sign(&sumeragi.key_pair)?;
                    iroha_logger::info!(
                        "{:?} - Block reached required number of votes. Block hash {}.",
                        network_topology.role(&sumeragi.peer_id),
                        block_hash,
                    );
                    let message =
                        VersionedMessage::from(Message::BlockCommitted(block.clone().into()));
                    let mut send_futures = Vec::new();
                    for peer in network_topology.validating_peers() {
                        send_futures.push(message.clone().send_to(peer));
                    }
                    send_futures.push(message.clone().send_to(network_topology.leader()));
                    for peer in network_topology.peers_set_b() {
                        send_futures.push(message.clone().send_to(peer));
                    }
                    let results = futures::future::join_all(send_futures).await;
                    results
                        .iter()
                        .filter(|result| result.is_err())
                        .for_each(|error_result| {
                            iroha_logger::error!(
                                "Failed to send BlockCommitted messages: {:?}",
                                error_result
                            )
                        });
                    sumeragi.votes_for_blocks.clear();
                    sumeragi.commit_block(block).await;
                }
            }
            Ok(())
        }
    }

    impl From<VersionedValidBlock> for BlockSigned {
        fn from(block: VersionedValidBlock) -> Self {
            Self { block }
        }
    }

    /// `BlockCommitted` message structure.
    #[derive(Io, Decode, Encode, Debug, Clone)]
    #[non_exhaustive]
    pub struct BlockCommitted {
        /// The corresponding block.
        pub block: VersionedValidBlock,
    }

    impl BlockCommitted {
        /// Handles this message as part of `Sumeragi` consensus.
        ///
        /// # Errors
        /// Actually infallible
        #[iroha_futures::telemetry_future]
        pub async fn handle<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait>(
            &self,
            sumeragi: &mut Sumeragi<Q, G, W>,
        ) -> Result<()> {
            let network_topology =
                sumeragi.network_topology_current_or_genesis(self.block.header());
            let verified_signatures = self.block.verified_signatures();
            let valid_signatures = network_topology.filter_signatures_by_roles(
                &[Role::ValidatingPeer, Role::Leader, Role::ProxyTail],
                &verified_signatures,
            );
            let proxy_tail_signatures = network_topology
                .filter_signatures_by_roles(&[Role::ProxyTail], &verified_signatures);
            if valid_signatures.len() >= network_topology.min_votes_for_commit() as usize
                && proxy_tail_signatures.len() == 1
                && sumeragi.latest_block_hash() == self.block.header().previous_block_hash
            {
                let mut block = self.block.clone();
                block.as_mut_inner_v1().signatures.clear();
                block.as_mut_inner_v1().signatures.append(&valid_signatures);
                sumeragi.commit_block(block).await;
            }
            Ok(())
        }
    }

    impl From<VersionedValidBlock> for BlockCommitted {
        fn from(block: VersionedValidBlock) -> Self {
            Self { block }
        }
    }

    /// `Message` structure describing a transaction that is forwarded from a client by a peer to the leader.
    #[derive(Io, Decode, Encode, Debug, Clone)]
    #[non_exhaustive]
    pub struct TransactionForwarded {
        /// Transaction that is forwarded from a client by a peer to the leader
        pub transaction: VersionedAcceptedTransaction,
        /// `PeerId` of the peer that forwarded this transaction to a leader.
        pub peer: PeerId,
    }

    impl TransactionForwarded {
        /// Constructs `TransactionForwarded` message.
        pub fn new(
            transaction: &VersionedAcceptedTransaction,
            peer: &PeerId,
        ) -> TransactionForwarded {
            TransactionForwarded {
                transaction: transaction.clone(),
                peer: peer.clone(),
            }
        }

        /// Handles this message as part of `Sumeragi` consensus.
        ///
        /// # Errors
        /// Can fail due to signing transaction
        #[iroha_futures::telemetry_future]
        pub async fn handle<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait>(
            &self,
            sumeragi: &mut Sumeragi<Q, G, W>,
        ) -> Result<()> {
            if sumeragi.is_leader() {
                if let Err(err) = VersionedMessage::from(Message::TransactionReceived(
                    TransactionReceipt::new(&self.transaction, &sumeragi.key_pair)?,
                ))
                .send_to(&self.peer)
                .await
                {
                    iroha_logger::error!(
                        "{:?} - Failed to send a transaction receipt to peer {}: {}",
                        sumeragi.topology.role(&sumeragi.peer_id),
                        self.peer.address,
                        err
                    )
                }
            }
            sumeragi.broker.issue_send(self.transaction.clone()).await;
            Ok(())
        }
    }

    /// `Message` structure describing a receipt sent by the leader to the peer it got this transaction from.
    #[derive(Io, Decode, Encode, Debug, Clone)]
    #[non_exhaustive]
    pub struct TransactionReceipt {
        /// The hash of the transaction that the leader received.
        pub transaction_hash: Hash,
        /// The time at which the leader claims to have received this transaction.
        pub received_at: Duration,
        /// The signature of the leader.
        pub signature: Signature,
    }

    impl TransactionReceipt {
        /// Constructs a new receipt.
        ///
        /// # Errors
        /// Can fail creating new signature
        #[allow(clippy::expect_used, clippy::unwrap_in_result)]
        pub fn new(
            transaction: &VersionedAcceptedTransaction,
            key_pair: &KeyPair,
        ) -> Result<TransactionReceipt> {
            let transaction_hash = transaction.hash();
            Ok(TransactionReceipt {
                transaction_hash,
                received_at: SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .expect("Failed to get System Time."),
                signature: Signature::new(key_pair.clone(), transaction_hash.as_ref())?,
            })
        }

        /// Checks that this `TransactionReceipt` is valid.
        pub fn is_valid(&self, network_topology: &Topology) -> bool {
            network_topology
                .verify_signature_with_role(
                    &self.signature,
                    Role::Leader,
                    self.transaction_hash.as_ref(),
                )
                .is_ok()
        }

        /// Checks if the block should have been already created by the `Leader`.
        pub fn is_block_should_be_created(&self, block_time: Duration) -> bool {
            #[allow(clippy::expect_used)]
            let current_time = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("Failed to get System Time.");
            (current_time - self.received_at) >= block_time
        }

        /// Handles this message as part of `Sumeragi` consensus.
        ///
        /// # Errors
        /// Can fail due to signing of block
        #[iroha_futures::telemetry_future]
        pub async fn handle<Q: QueueTrait, G: GenesisNetworkTrait, W: WorldTrait>(
            &self,
            sumeragi: &mut Sumeragi<Q, G, W>,
        ) -> Result<()> {
            // Implausible time in the future, means that the leader lies
            if sumeragi.topology.role(&sumeragi.peer_id) != Role::Leader
                && self.received_at
                    <= SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .wrap_err("Failed to get System Time.")?
                && self.is_valid(&sumeragi.topology)
                && sumeragi
                    .txs_awaiting_receipts
                    .contains_key(&self.transaction_hash)
            {
                let _ = sumeragi
                    .txs_awaiting_receipts
                    .remove(&self.transaction_hash);
                let block_time = sumeragi.block_time;
                let transactions_awaiting_created_block =
                    Arc::clone(&sumeragi.txs_awaiting_created_block);
                let tx_hash = self.transaction_hash;
                let mut block_creation_timeout = view_change::Proof::block_creation_timeout(
                    self.clone(),
                    sumeragi.latest_view_change_hash(),
                    sumeragi.latest_block_hash(),
                );
                block_creation_timeout = block_creation_timeout
                    .sign(&sumeragi.key_pair)
                    .wrap_err("Failed to put first signature.")?;
                let _ = transactions_awaiting_created_block.insert(tx_hash);
                let recipient_peers = sumeragi.topology.sorted_peers().to_vec();
                drop(task::spawn(
                    async move {
                        time::sleep(block_time).await;
                        // Suspect leader if the block was not yet created
                        if transactions_awaiting_created_block.contains(&tx_hash) {
                            iroha_logger::warn!("Block creation timeout detected!");
                            let block_creation_timeout_message = VersionedMessage::from(
                                Message::ViewChangeSuggested(block_creation_timeout.into()),
                            );
                            drop(
                                futures::future::join_all(recipient_peers.iter().map(|peer| {
                                    block_creation_timeout_message.clone().send_to(peer)
                                }))
                                .await,
                            );
                        }
                    }
                    .in_current_span(),
                ));
            }
            Ok(())
        }
    }
}

/// This module contains all configuration related logic.
pub mod config {
    use std::{collections::HashSet, fmt::Debug, fs::File, io::BufReader, path::Path};

    use iroha_config::derive::Configurable;
    use iroha_crypto::prelude::*;
    use iroha_data_model::prelude::*;
    use iroha_error::{Result, WrapErr};
    use serde::{Deserialize, Serialize};

    const DEFAULT_BLOCK_TIME_MS: u64 = 1000;
    const DEFAULT_MAX_FAULTY_PEERS: u32 = 0;
    const DEFAULT_COMMIT_TIME_MS: u64 = 1000;
    const DEFAULT_TX_RECEIPT_TIME_MS: u64 = 200;
    const DEFAULT_MAX_INSTRUCTION_NUMBER: u64 = 2_u64.pow(12);
    const DEFAULT_N_TOPOLOGY_SHIFTS_BEFORE_RESHUFFLE: u64 = 1;

    /// `SumeragiConfiguration` provides an ability to define parameters such as `BLOCK_TIME_MS`
    /// and list of `TRUSTED_PEERS`.
    #[derive(Clone, Debug, Deserialize, Serialize, Configurable)]
    #[serde(default)]
    #[serde(rename_all = "UPPERCASE")]
    #[config(env_prefix = "SUMERAGI_")]
    pub struct SumeragiConfiguration {
        /// Key pair of private and public keys.
        #[serde(skip)]
        pub key_pair: KeyPair,
        /// Current Peer Identification.
        pub peer_id: PeerId,
        /// Amount of time peer waits for the `CreatedBlock` message after getting a `TransactionReceipt`
        pub block_time_ms: u64,
        /// Optional list of predefined trusted peers.
        pub trusted_peers: TrustedPeers,
        /// Maximum amount of peers to fail and do not compromise the consensus.
        pub max_faulty_peers: u32,
        /// Amount of time Peer waits for CommitMessage from the proxy tail.
        pub commit_time_ms: u64,
        /// Amount of time Peer waits for TxReceipt from the leader.
        pub tx_receipt_time_ms: u64,
        /// After N view changes topology will change tactic from shifting by one, to reshuffle.
        pub n_topology_shifts_before_reshuffle: u64,
        /// Maximum instruction number per transaction
        pub max_instruction_number: u64,
    }

    impl Default for SumeragiConfiguration {
        fn default() -> Self {
            Self {
                key_pair: KeyPair::default(),
                trusted_peers: default_empty_trusted_peers(),
                peer_id: default_peer_id(),
                block_time_ms: DEFAULT_BLOCK_TIME_MS,
                max_faulty_peers: DEFAULT_MAX_FAULTY_PEERS,
                commit_time_ms: DEFAULT_COMMIT_TIME_MS,
                tx_receipt_time_ms: DEFAULT_TX_RECEIPT_TIME_MS,
                n_topology_shifts_before_reshuffle: DEFAULT_N_TOPOLOGY_SHIFTS_BEFORE_RESHUFFLE,
                max_instruction_number: DEFAULT_MAX_INSTRUCTION_NUMBER,
            }
        }
    }

    impl SumeragiConfiguration {
        /// Set `trusted_peers` configuration parameter - will overwrite the existing one.
        pub fn trusted_peers(&mut self, trusted_peers: Vec<PeerId>) {
            self.trusted_peers.peers = trusted_peers.into_iter().collect();
        }

        /// Set `max_faulty_peers` configuration parameter - will overwrite the existing one.
        pub fn max_faulty_peers(&mut self, max_faulty_peers: u32) {
            self.max_faulty_peers = max_faulty_peers;
        }

        /// Time estimation from receiving a transaction to storing it in a block on all peers.
        #[allow(clippy::integer_arithmetic)]
        pub const fn pipeline_time_ms(&self) -> u64 {
            self.tx_receipt_time_ms + self.block_time_ms + self.commit_time_ms
        }
    }

    /// `SumeragiConfiguration` provides an ability to define parameters such as `BLOCK_TIME_MS`
    /// and list of `TRUSTED_PEERS`.
    #[derive(Default, Clone, Debug, Deserialize, Serialize)]
    #[serde(rename_all = "UPPERCASE")]
    #[serde(transparent)]
    #[allow(clippy::exhaustive_structs)]
    pub struct TrustedPeers {
        /// Optional list of predefined trusted peers.
        pub peers: HashSet<PeerId>,
    }

    impl TrustedPeers {
        /// Load trusted peers variables from a json *pretty* formatted file.
        ///
        /// # Errors
        /// Fails if there is no file or if file is not valid json
        pub fn from_path<P: AsRef<Path> + Debug>(path: P) -> Result<TrustedPeers> {
            let file = File::open(&path)
                .wrap_err_with(|| format!("Failed to open trusted peers file {:?}", &path))?;
            let reader = BufReader::new(file);
            let trusted_peers: HashSet<PeerId> = serde_json::from_reader(reader)
                .wrap_err("Failed to deserialize json from reader")?;
            Ok(TrustedPeers {
                peers: trusted_peers,
            })
        }
    }

    fn default_peer_id() -> PeerId {
        PeerId {
            address: "".to_owned(),
            public_key: PublicKey::default(),
        }
    }

    // Allowed because `HashSet::new()` is not const yet.
    #[allow(clippy::missing_const_for_fn)]
    fn default_empty_trusted_peers() -> TrustedPeers {
        TrustedPeers {
            peers: HashSet::new(),
        }
    }
}
