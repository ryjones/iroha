//! This module contains incoming requests handling logic of Iroha.
//! `Torii` is used to receive, accept and route incoming instructions, queries and messages.

use std::{convert::Infallible, fmt::Debug, sync::Arc};

use config::ToriiConfiguration;
use iroha_actor::{broker::*, prelude::*};
use iroha_config::{derive::Error as ConfigError, Configurable};
use iroha_data_model::prelude::*;
use iroha_error::{derive::Error, error};
use iroha_http_server::{http::Json, prelude::*, web_socket::WebSocketStream, Server};
use iroha_logger::InstrumentFutures;
#[cfg(feature = "mock")]
use iroha_network::mock::prelude::*;
#[cfg(not(feature = "mock"))]
use iroha_network::prelude::*;
use iroha_version::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::{sync::RwLock, task};

use crate::{
    block_sync::message::VersionedMessage as BlockSyncVersionedMessage,
    config::Configuration,
    event::{Consumer, EventsReceiver, EventsSender},
    maintenance::{Health, System},
    prelude::*,
    queue::{GetPendingTransactions, QueueTrait},
    smartcontracts::isi::query::VerifiedQueryRequest,
    sumeragi::{
        message::VersionedMessage as SumeragiVersionedMessage, GetLeader, IsLeader, SumeragiTrait,
    },
    wsv::WorldTrait,
};

/// Main network handler and the only entrypoint of the Iroha.
#[derive(Debug)]
pub struct Torii<Q: QueueTrait, S: SumeragiTrait, W: WorldTrait> {
    config: ToriiConfiguration,
    wsv: Arc<WorldStateView<W>>,
    system: Arc<RwLock<System>>,
    events_sender: EventsSender,
    events_receiver: EventsReceiver,
    transactions_queue: AlwaysAddr<Q>,
    sumeragi: AlwaysAddr<S>,
    broker: Broker,
}

/// Errors of torii
#[derive(Error, Debug)]
pub enum Error {
    /// Failed to decode transaction
    #[error("Failed to decode transaction")]
    VersionedTransaction(#[source] iroha_version::error::Error),
    /// Failed to accept transaction
    #[error("Failed to accept transaction")]
    AcceptTransaction(iroha_error::Error),
    /// Failed to execute query
    #[error("Failed to execute query")]
    ExecuteQuery(iroha_error::Error),
    /// Failed to get pending transaction
    #[error("Failed to get pending transactions")]
    RequestPendingTransactions(iroha_error::Error),
    /// Failed to decode pending transactions from leader
    #[error("Failed to decode pending transactions from leader")]
    DecodeRequestPendingTransactions(#[source] iroha_version::error::Error),
    /// Failed to encode pending transactions
    #[error("Failed to encode pending transactions")]
    EncodePendingTransactions(#[source] iroha_version::error::Error),
    /// The block sync message channel is full. Dropping the incoming message.
    #[error("Transaction is too big")]
    TxTooBig,
    /// Error while getting or setting configuration
    #[error("Configuration error")]
    Config(#[source] ConfigError),
}

impl iroha_http_server::http::HttpResponseError for Error {
    fn status_code(&self) -> iroha_http_server::http::StatusCode {
        use Error::*;

        match *self {
            ExecuteQuery(_)
            | RequestPendingTransactions(_)
            | DecodeRequestPendingTransactions(_)
            | EncodePendingTransactions(_) => {
                iroha_http_server::http::HTTP_CODE_INTERNAL_SERVER_ERROR
            }
            TxTooBig | VersionedTransaction(_) | AcceptTransaction(_) => {
                iroha_http_server::http::HTTP_CODE_BAD_REQUEST
            }
            Config(_) => iroha_http_server::http::HTTP_CODE_NOT_FOUND,
        }
    }

    fn error_body(&self) -> Vec<u8> {
        self.to_string().into()
    }
}

/// Result type
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl<Q: QueueTrait, S: SumeragiTrait, W: WorldTrait> Torii<Q, S, W> {
    /// Construct `Torii` from `ToriiConfiguration`.
    #[allow(clippy::clippy::too_many_arguments)]
    pub fn from_configuration(
        config: ToriiConfiguration,
        wsv: Arc<WorldStateView<W>>,
        system: System,
        transactions_queue: AlwaysAddr<Q>,
        sumeragi: AlwaysAddr<S>,
        (events_sender, events_receiver): (EventsSender, EventsReceiver),
        broker: Broker,
    ) -> Self {
        Self {
            config,
            wsv,
            system: Arc::new(RwLock::new(system)),
            events_sender,
            events_receiver,
            transactions_queue,
            sumeragi,
            broker,
        }
    }

    fn create_state(&self) -> ToriiState<Q, S, W> {
        let wsv = Arc::clone(&self.wsv);
        let transactions_queue = self.transactions_queue.clone();
        let sumeragi = self.sumeragi.clone();
        let system = Arc::clone(&self.system);
        let consumers = Arc::new(RwLock::new(Vec::new()));
        let config = self.config.clone();
        let broker = self.broker.clone();

        ToriiState {
            config,
            wsv,
            system,
            consumers,
            events_sender: self.events_sender.clone(),
            transactions_queue,
            sumeragi,
            broker,
        }
    }

    /// To handle incoming requests `Torii` should be started first.
    ///
    /// # Errors
    /// Can fail due to listening to network or if http server fails
    #[iroha_futures::telemetry_future]
    pub async fn start(self) -> iroha_error::Result<Infallible> {
        let state = self.create_state();
        let connections = Arc::clone(&state.consumers);
        let state = Arc::new(RwLock::new(state));
        let mut server = Server::new(Arc::clone(&state));
        server.at(uri::INSTRUCTIONS_URI).post(handle_instructions);
        server.at(uri::QUERY_URI).get(handle_queries);
        server.at(uri::HEALTH_URI).get(handle_health);
        server.at(uri::METRICS_URI).get(handle_metrics);
        server
            .at(uri::PENDING_TRANSACTIONS_ON_LEADER_URI)
            .get(handle_pending_transactions_on_leader);
        server
            .at(uri::CONFIGURATION_URI)
            .get(handle_get_configuration);
        server
            .at(uri::SUBSCRIPTION_URI)
            .web_socket(handle_subscription);

        let (http_server_result, _event_consumer_result) = tokio::join!(
            server.start(&self.config.torii_api_url),
            consume_events(self.events_receiver, connections),
        );
        http_server_result
    }
}

#[derive(Debug)]
struct ToriiState<Q: QueueTrait, S: SumeragiTrait, W: WorldTrait> {
    config: ToriiConfiguration,
    wsv: Arc<WorldStateView<W>>,
    consumers: Arc<RwLock<Vec<Consumer>>>,
    system: Arc<RwLock<System>>,
    events_sender: EventsSender,
    transactions_queue: AlwaysAddr<Q>,
    sumeragi: AlwaysAddr<S>,
    broker: Broker,
}

#[iroha_futures::telemetry_future]
async fn handle_instructions<Q: QueueTrait, S: SumeragiTrait, W: WorldTrait>(
    state: State<ToriiState<Q, S, W>>,
    _path_params: PathParams,
    _query_params: QueryParams,
    request: RawHttpRequest,
) -> Result<()> {
    if request.body.len() > state.read().await.config.torii_max_transaction_size {
        return Err(Error::TxTooBig);
    }
    let transaction = VersionedTransaction::decode_versioned(&request.body)
        .map_err(Error::VersionedTransaction)?;
    let transaction: Transaction = transaction.into_inner_v1();
    let transaction = VersionedAcceptedTransaction::from_transaction(
        transaction,
        state.read().await.config.torii_max_instruction_number,
    )
    .map_err(Error::AcceptTransaction)?;
    state.read().await.broker.issue_send(transaction).await;
    Ok(())
}

#[iroha_futures::telemetry_future]
async fn handle_queries<Q: QueueTrait, S: SumeragiTrait, W: WorldTrait>(
    state: State<ToriiState<Q, S, W>>,
    _path_params: PathParams,
    pagination: Pagination,
    request: VerifiedQueryRequest,
) -> Result<QueryResult> {
    let result = request
        .query
        .execute(&*state.read().await.wsv)
        .map_err(Error::ExecuteQuery)?;
    let result = QueryResult(if let Value::Vec(value) = result {
        Value::Vec(value.into_iter().paginate(pagination).collect())
    } else {
        result
    });
    Ok(result)
}

#[iroha_futures::telemetry_future]
async fn handle_health<Q: QueueTrait, S: SumeragiTrait, W: WorldTrait>(
    _state: State<ToriiState<Q, S, W>>,
    _path_params: PathParams,
    _query_params: QueryParams,
    _request: RawHttpRequest,
) -> Json<Health> {
    Json(Health::Healthy)
}

#[iroha_futures::telemetry_future]
async fn handle_pending_transactions_on_leader<Q: QueueTrait, S: SumeragiTrait, W: WorldTrait>(
    state: State<ToriiState<Q, S, W>>,
    _path_params: PathParams,
    pagination: Pagination,
    _request: RawHttpRequest,
) -> Result<VersionedPendingTransactions> {
    #[allow(clippy::unwrap_used)]
    let PendingTransactions(pending_transactions) =
        if state.read().await.sumeragi.send(IsLeader).await {
            state
                .read()
                .await
                .transactions_queue
                .send(GetPendingTransactions)
                .await
        } else {
            let bytes = Network::send_request_to(
                state
                    .read()
                    .await
                    .sumeragi
                    .send(GetLeader)
                    .await
                    .address
                    .as_ref(),
                Request::empty(uri::PENDING_TRANSACTIONS_URI),
            )
            .await
            .map_err(Error::RequestPendingTransactions)?
            .into_result()
            .map_err(Error::RequestPendingTransactions)?;

            VersionedPendingTransactions::decode_versioned(&bytes)
                .map_err(Error::DecodeRequestPendingTransactions)?
                .into_inner_v1()
        };

    Ok(PendingTransactions(
        pending_transactions
            .into_iter()
            .paginate(pagination)
            .collect(),
    )
    .into())
}

#[derive(Clone, Debug)]
enum GetConfigurationType {
    Docs,
    Value,
}

impl From<&QueryParams> for GetConfigurationType {
    fn from(params: &QueryParams) -> Self {
        match params.get("docs").map(AsRef::<str>::as_ref) {
            Some("true") => Self::Docs,
            _ => Self::Value,
        }
    }
}

impl From<QueryParams> for GetConfigurationType {
    fn from(params: QueryParams) -> Self {
        Self::from(&params)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct GetConfiguration {
    field: Vec<String>,
}

#[iroha_futures::telemetry_future]
async fn handle_get_configuration<Q: QueueTrait, S: SumeragiTrait, W: WorldTrait>(
    _state: State<ToriiState<Q, S, W>>,
    _path_params: PathParams,
    ty: GetConfigurationType,
    Json(GetConfiguration { field }): Json<GetConfiguration>,
) -> Result<Json<serde_json::Value>> {
    let field = field.iter().map(AsRef::as_ref).collect::<Vec<&str>>();
    #[allow(clippy::todo)]
    match ty {
        GetConfigurationType::Docs => Configuration::get_doc_recursive(field).map(|doc| {
            if let Some(doc) = doc {
                Json(serde_json::json!(doc))
            } else {
                Json(serde_json::json!(null))
            }
        }),
        GetConfigurationType::Value => todo!(),
    }
    .map_err(Error::Config)
}

#[iroha_futures::telemetry_future]
async fn handle_metrics<Q: QueueTrait, S: SumeragiTrait, W: WorldTrait>(
    state: State<ToriiState<Q, S, W>>,
    _path_params: PathParams,
    _query_params: QueryParams,
    _request: RawHttpRequest,
) -> Result<HttpResponse> {
    match state
        .read()
        .await
        .system
        .read()
        .await
        .scrape_metrics()
        .await
    {
        Ok(metrics) => Ok(HttpResponse::ok(Headers::new(), metrics.into())),
        Err(e) => {
            iroha_logger::error!("Failed to scrape metrics: {}", e);
            Ok(HttpResponse::internal_server_error())
        }
    }
}

#[iroha_futures::telemetry_future]
async fn handle_subscription<Q: QueueTrait, S: SumeragiTrait, W: WorldTrait>(
    state: State<ToriiState<Q, S, W>>,
    _path_params: PathParams,
    _query_params: QueryParams,
    stream: WebSocketStream,
) -> iroha_error::Result<()> {
    let state = state.read().await;

    // The lock is kept on `consumers` for the whole function so that a consumer can tell the client that it is ready,
    // before sending events.
    let mut consumers = state.consumers.write().await;
    let consumer = Consumer::new(stream).await?;
    consumers.push(consumer);
    Ok(())
}

#[iroha_futures::telemetry_future]
async fn handle_requests<Q: QueueTrait, S: SumeragiTrait, W: WorldTrait>(
    state: State<ToriiState<Q, S, W>>,
    stream: Box<dyn AsyncStream>,
) -> iroha_error::Result<()> {
    let state_arc = Arc::clone(&state);
    task::spawn(async {
        if let Err(e) = Network::handle_message_async(state_arc, stream, handle_request).await {
            let e = e.report();
            iroha_logger::error!("Failed to handle message: {}", e);
        }
    })
    .in_current_span()
    .await?;
    Ok(())
}

#[iroha_futures::telemetry_future]
async fn consume_events(
    mut events_receiver: EventsReceiver,
    consumers: Arc<RwLock<Vec<Consumer>>>,
) {
    while let Some(change) = events_receiver.recv().await {
        iroha_logger::trace!("Event occurred: {:?}", change);
        let mut open_connections = Vec::new();
        for connection in consumers.write().await.drain(..) {
            match connection.consume(&change).await {
                Ok(consumer) => open_connections.push(consumer),
                Err(err) => {
                    iroha_logger::error!("Failed to notify client: {}. Closed connection.", err)
                }
            }
        }
        consumers.write().await.append(&mut open_connections);
    }
}

#[iroha_futures::telemetry_future]
#[iroha_logger::log("TRACE")]
async fn handle_request<Q: QueueTrait, S: SumeragiTrait, W: WorldTrait>(
    state: State<ToriiState<Q, S, W>>,
    request: Request,
) -> iroha_error::Result<Response> {
    #[allow(clippy::pattern_type_mismatch)]
    match request.uri_path.as_ref() {
        uri::CONSENSUS_URI
            if request.payload.len()
                > state.read().await.config.torii_max_sumeragi_message_size =>
        {
            iroha_logger::error!("Message is too big. Droping");
            Ok(Response::InternalError)
        }
        uri::CONSENSUS_URI => {
            let message = match SumeragiVersionedMessage::decode_versioned(&request.payload) {
                Ok(message) => message,
                Err(e) => {
                    iroha_logger::error!("Failed to decode peer message: {}", e);
                    return Ok(Response::InternalError);
                }
            };
            state
                .read()
                .await
                .broker
                .issue_send(message.into_inner_v1())
                .await;
            Ok(Response::empty_ok())
        }
        uri::BLOCK_SYNC_URI => {
            let message = match BlockSyncVersionedMessage::decode_versioned(&request.payload) {
                Ok(message) => message.into_inner_v1(),
                Err(e) => {
                    iroha_logger::error!("Failed to decode peer message: {}", e);
                    return Ok(Response::InternalError);
                }
            };

            state.read().await.broker.issue_send(message).await;
            Ok(Response::empty_ok())
        }
        uri::HEALTH_URI => Ok(Response::empty_ok()),
        uri::PENDING_TRANSACTIONS_URI => {
            let pending_transactions: VersionedPendingTransactions = state
                .read()
                .await
                .transactions_queue
                .send(GetPendingTransactions)
                .await
                .into();
            Ok(Response::Ok(
                pending_transactions
                    .encode_versioned()
                    .map_err(Error::EncodePendingTransactions)?,
            ))
        }
        non_supported_uri => {
            iroha_logger::error!("URI not supported: {}.", &non_supported_uri);
            Ok(Response::InternalError)
        }
    }
}

/// URI that `Torii` uses to route incoming requests.
pub mod uri {
    /// Query URI is used to handle incoming Query requests.
    pub const QUERY_URI: &str = "/query";
    /// Instructions URI is used to handle incoming ISI requests.
    pub const INSTRUCTIONS_URI: &str = "/instruction";
    /// Block URI is used to handle incoming Block requests.
    pub const CONSENSUS_URI: &str = "/consensus";
    /// Health URI is used to handle incoming Healthcheck requests.
    pub const HEALTH_URI: &str = "/health";
    /// Metrics URI is used to export metrics according to [Prometheus
    /// Guidance](https://prometheus.io/docs/instrumenting/writing_exporters/).
    pub const METRICS_URI: &str = "/metrics";
    /// The URI used for block synchronization.
    pub const BLOCK_SYNC_URI: &str = "/block";
    /// The web socket uri used to subscribe to block and transactions statuses.
    pub const SUBSCRIPTION_URI: &str = "/events";
    /// Get pending transactions.
    pub const PENDING_TRANSACTIONS_URI: &str = "/pending_transactions";
    /// Get pending transactions on leader.
    pub const PENDING_TRANSACTIONS_ON_LEADER_URI: &str = "/pending_transactions_on_leader";
    /// The URI for local config changing inspecting
    pub const CONFIGURATION_URI: &str = "/configure";
}

/// This module contains all configuration related logic.
pub mod config {
    use iroha_config::derive::Configurable;
    use serde::{Deserialize, Serialize};

    const DEFAULT_TORII_P2P_ADDR: &str = "127.0.0.1:1337";
    const DEFAULT_TORII_API_URL: &str = "127.0.0.1:8080";
    const DEFAULT_TORII_MAX_TRANSACTION_SIZE: usize = 2_usize.pow(15);
    const DEFAULT_TORII_MAX_INSTRUCTION_NUMBER: u64 = 2_u64.pow(12);
    const DEFAULT_TORII_MAX_SUMERAGI_MESSAGE_SIZE: usize = 2_usize.pow(12) * 4000;

    /// `ToriiConfiguration` provides an ability to define parameters such as `TORII_URL`.
    #[derive(Clone, Deserialize, Serialize, Debug, Configurable)]
    #[serde(rename_all = "UPPERCASE")]
    #[serde(default)]
    pub struct ToriiConfiguration {
        /// Torii URL for p2p communication for consensus and block synchronization purposes.
        pub torii_p2p_addr: String,
        /// Torii URL for client API.
        pub torii_api_url: String,
        /// Maximum number of bytes in raw transaction. Used to prevent from DOS attacks.
        pub torii_max_transaction_size: usize,
        /// Maximum number of bytes in raw message. Used to prevent from DOS attacks.
        pub torii_max_sumeragi_message_size: usize,
        /// Maximum number of instruction per transaction. Used to prevent from DOS attacks.
        pub torii_max_instruction_number: u64,
    }

    impl Default for ToriiConfiguration {
        fn default() -> Self {
            Self {
                torii_api_url: DEFAULT_TORII_API_URL.to_owned(),
                torii_p2p_addr: DEFAULT_TORII_P2P_ADDR.to_owned(),
                torii_max_transaction_size: DEFAULT_TORII_MAX_TRANSACTION_SIZE,
                torii_max_sumeragi_message_size: DEFAULT_TORII_MAX_SUMERAGI_MESSAGE_SIZE,
                torii_max_instruction_number: DEFAULT_TORII_MAX_INSTRUCTION_NUMBER,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::default_trait_access, clippy::restriction)]

    use std::{convert::TryInto, time::Duration};

    use futures::future::FutureExt;
    use iroha_actor::broker::Broker;
    use iroha_data_model::account::Id;
    use tokio::{sync::mpsc, time};

    use super::*;
    use crate::{
        config::Configuration,
        genesis::GenesisNetwork,
        queue::{Queue, QueueTrait},
        sumeragi::{Sumeragi, SumeragiTrait},
        wsv::World,
    };

    const CONFIGURATION_PATH: &str = "tests/test_config.json";
    const TRUSTED_PEERS_PATH: &str = "tests/test_trusted_peers.json";

    fn get_config() -> Configuration {
        Configuration::from_path(CONFIGURATION_PATH).expect("Failed to load configuration.")
    }

    async fn create_torii(
    ) -> Torii<Queue<World>, Sumeragi<Queue<World>, GenesisNetwork, World>, World> {
        let broker = Broker::new();
        let mut config = get_config();
        config
            .load_trusted_peers_from_path(TRUSTED_PEERS_PATH)
            .expect("Failed to load trusted peers.");
        let (events_sender, events_receiver) = mpsc::channel(100);
        let wsv = Arc::new(WorldStateView::new(World::with(
            ('a'..'z')
                .map(|name| name.to_string())
                .map(|name| (name.clone(), Domain::new(&name))),
            vec![],
        )));
        let queue = Queue::from_configuration(
            &config.queue_configuration,
            Arc::clone(&wsv),
            broker.clone(),
        )
        .start()
        .await
        .expect_running();
        let sumeragi = Sumeragi::from_configuration(
            &config.sumeragi_configuration,
            events_sender.clone(),
            Arc::clone(&wsv),
            AllowAll.into(),
            None,
            queue.clone(),
            broker.clone(),
        )
        .expect("Failed to initialize sumeragi.")
        .start()
        .await
        .expect_running();

        Torii::from_configuration(
            config.torii_configuration.clone(),
            wsv,
            System::new(&config),
            queue,
            sumeragi,
            (events_sender, events_receiver),
            broker,
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_start_torii() {
        let torii = create_torii().await;

        let result = time::timeout(Duration::from_millis(50), torii.start()).await;

        assert!(result.is_err() || result.unwrap().is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn torii_big_transaction() {
        let torii = create_torii().await;
        let state = Arc::new(RwLock::new(torii.create_state()));
        let id = Id {
            name: Default::default(),
            domain_name: Default::default(),
        };
        let max_transaction_size = state.read().await.config.torii_max_transaction_size;
        let instruction: Instruction = FailBox {
            message: "Fail message".to_owned(),
        }
        .into();

        let mut instruction_number = 32;

        let request = loop {
            let transaction = Transaction::new(
                vec![instruction.clone(); instruction_number],
                id.clone(),
                10_000,
            );
            let body: Vec<u8> = transaction.into();
            let request = RawHttpRequest {
                method: "POST".to_owned(),
                path: uri::INSTRUCTIONS_URI.to_owned(),
                version: HttpVersion::Http1_1,
                headers: Default::default(),
                body,
            };

            if request.body.len() <= max_transaction_size {
                instruction_number *= 2;
                continue;
            }
            break request;
        };

        let result =
            handle_instructions(state, Default::default(), Default::default(), request).await;
        match result {
            Err(Error::TxTooBig) => (),
            _ => panic!("Should be equal to TxTooBig: {:?}", result),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn torii_pagination() {
        let torii = create_torii().await;
        let state = Arc::new(RwLock::new(torii.create_state()));

        let keys = KeyPair::generate().expect("Failed to generate keys");

        let get_domains = |start, limit| {
            let query: VerifiedQueryRequest =
                QueryRequest::new(QueryBox::FindAllDomains(Default::default()))
                    .sign(&keys)
                    .expect("Failed to sign query with keys")
                    .try_into()
                    .expect("Failed to verify");

            let pagination = Pagination { start, limit };
            handle_queries(state.clone(), Default::default(), pagination, query).map(|result| {
                if let QueryResult(Value::Vec(domain)) = result.expect("Failed request with query")
                {
                    domain
                } else {
                    unreachable!()
                }
            })
        };

        assert_eq!(get_domains(None, None).await.len(), 25);
        assert_eq!(get_domains(Some(0), None).await.len(), 25);
        assert_eq!(get_domains(Some(15), Some(5)).await.len(), 5);
        assert_eq!(get_domains(None, Some(10)).await.len(), 10);
        assert_eq!(get_domains(Some(1), Some(15)).await.len(), 15);
    }
}
