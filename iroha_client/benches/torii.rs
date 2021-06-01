#![allow(missing_docs, clippy::cast_precision_loss, clippy::restriction)]

use std::thread;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use iroha::{config::Configuration, prelude::*};
use iroha_client::{
    client::{asset, Client},
    config::Configuration as ClientConfiguration,
};
use iroha_data_model::prelude::*;
use test_network::{Network, Peer as TestPeer};

const CONFIGURATION_PATH: &str = "tests/test_config.json";
const CLIENT_CONFIGURATION_PATH: &str = "tests/test_client_config.json";
const GENESIS_PATH: &str = "tests/genesis.json";
const MINIMUM_SUCCESS_REQUEST_RATIO: f32 = 0.9;

const DOMAIN_NAME: &str = "domain";
const ACCOUNT_NAME: &str = "account";

fn create_peer() -> TestPeer {
    let mut configuration =
        Configuration::from_path(CONFIGURATION_PATH).expect("Failed to load configuration.");
    configuration.genesis_configuration.genesis_block_path = Some(GENESIS_PATH.to_string());
    let peer = TestPeer::new().expect("Failed to create peer");
    configuration.sumeragi_configuration.trusted_peers.peers =
        std::iter::once(peer.id.clone()).collect();

    drop(peer.start_with_config(configuration));
    peer
}

fn setup_network() -> Vec<Instruction> {
    let account_id = AccountId::new(ACCOUNT_NAME, DOMAIN_NAME);
    let asset_definition_id = AssetDefinitionId::new("xor", DOMAIN_NAME);

    let create_domain = RegisterBox::new(IdentifiableBox::from(Domain::new(DOMAIN_NAME)));
    let create_account = RegisterBox::new(IdentifiableBox::from(NewAccount::with_signatory(
        account_id,
        KeyPair::generate()
            .expect("Failed to generate KeyPair.")
            .public_key,
    )));
    let create_asset = RegisterBox::new(IdentifiableBox::from(AssetDefinition::new_quantity(
        asset_definition_id,
    )));
    vec![
        create_domain.into(),
        create_asset.into(),
        create_account.into(),
    ]
}

fn setup_bench() -> Client {
    let peer = create_peer();

    let mut client_config = ClientConfiguration::from_path(CLIENT_CONFIGURATION_PATH)
        .expect("Failed to load configuration.");
    client_config.torii_api_url = peer.api_address;
    let mut client = Client::new(&client_config);
    let _ = client
        .submit_all(setup_network())
        .expect("Failed to create role.");
    thread::sleep(std::time::Duration::from_millis(500));
    client
}

fn setup_bench_network() -> Client {
    let (_, mut client) = Network::start_test(4, 10);

    let _ = client
        .submit_all(setup_network())
        .expect("Failed to create role.");
    thread::sleep(std::time::Duration::from_millis(500));
    client
}

fn query_requests(criterion: &mut Criterion) {
    let mut iroha_client = setup_bench();
    let mut group = criterion.benchmark_group("query-reqeuests");

    let request = asset::by_account_id(AccountId::new(ACCOUNT_NAME, DOMAIN_NAME));

    let (mut success_count, mut failures_count) = (0, 0);

    let _dropable = group.throughput(Throughput::Bytes(Vec::from(&request).len() as u64));

    let _dropable2 = group.bench_function("query", |b| {
        b.iter(|| match iroha_client.request(request.clone()) {
            Ok(assets) => {
                assert!(!assets.is_empty());
                success_count += 1;
            }
            Err(e) => {
                eprintln!("Query failed: {}", e);
                failures_count += 1;
            }
        });
    });

    println!(
        "Success count: {}, Failures count: {}",
        success_count, failures_count
    );
    group.finish();
    if (failures_count + success_count) > 0 {
        assert!(
            success_count as f32 / (failures_count + success_count) as f32
                > MINIMUM_SUCCESS_REQUEST_RATIO
        );
    }
}

fn instruction_submits(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("instruction-requests");
    let mut iroha_client = setup_bench();

    let account_id = AccountId::new(ACCOUNT_NAME, DOMAIN_NAME);
    let asset_definition_id = AssetDefinitionId::new("xor", DOMAIN_NAME);

    let mut success_count = 0;
    let mut failures_count = 0;

    let _ = group.bench_function("instructions", |b| {
        b.iter(|| {
            let quantity: u32 = 200;
            let mint_asset = MintBox::new(
                Value::U32(quantity),
                IdBox::AssetId(AssetId::new(
                    asset_definition_id.clone(),
                    account_id.clone(),
                )),
            );
            match iroha_client.submit(mint_asset) {
                Ok(_) => success_count += 1,
                Err(e) => {
                    eprintln!("Failed to execute instruction: {}", e);
                    failures_count += 1;
                }
            };
        })
    });

    println!(
        "Success count: {}, Failures count: {}",
        success_count, failures_count
    );
    group.finish();
    if (failures_count + success_count) > 0 {
        assert!(
            success_count as f32 / (failures_count + success_count) as f32
                > MINIMUM_SUCCESS_REQUEST_RATIO
        );
    }
}

fn instruction_submits_network(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("instruction-requests-network");
    let mut iroha_client = setup_bench_network();

    let account_id = AccountId::new(ACCOUNT_NAME, DOMAIN_NAME);
    let asset_definition_id = AssetDefinitionId::new("xor", DOMAIN_NAME);

    let mut success_count = 0;
    let mut failures_count = 0;

    let _ = group.bench_function("instructions", |b| {
        b.iter(|| {
            let quantity: u32 = 200;
            let mint_asset = MintBox::new(
                Value::U32(quantity),
                IdBox::AssetId(AssetId::new(
                    asset_definition_id.clone(),
                    account_id.clone(),
                )),
            );
            match iroha_client.submit(mint_asset) {
                Ok(_) => success_count += 1,
                Err(e) => {
                    eprintln!("Failed to execute instruction: {}", e);
                    failures_count += 1;
                }
            };
        })
    });

    println!(
        "Success count: {}, Failures count: {}",
        success_count, failures_count
    );
    group.finish();
    if (failures_count + success_count) > 0 {
        assert!(
            success_count as f32 / (failures_count + success_count) as f32
                > MINIMUM_SUCCESS_REQUEST_RATIO
        );
    }
}

criterion_group!(
    instructions,
    //instruction_submits,
    instruction_submits_network
);
criterion_group!(queries, query_requests);
criterion_main!(instructions, queries);
