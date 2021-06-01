#![allow(missing_docs, clippy::cast_precision_loss, clippy::restriction)]

use std::thread;

use iroha::prelude::*;
use iroha_client::client::Client;
use iroha_data_model::prelude::*;
use test_network::Network;

const DOMAIN_NAME: &str = "domain";
const ACCOUNT_NAME: &str = "account";
const MINIMUM_SUCCESS_REQUEST_RATIO: f32 = 0.9;

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

fn setup_bench_network(maximum_transactions_in_block: u32) -> Client {
    let (_, mut client) = Network::start_test(4, maximum_transactions_in_block);

    let _ = client
        .submit_all(setup_network())
        .expect("Failed to create role.");
    thread::sleep(std::time::Duration::from_millis(500));
    client
}

#[test]
fn overflow_4_peers_1_tx() {
    overflow_4_peers(1)
}

#[test]
fn overflow_4_peers_5_tx() {
    overflow_4_peers(5)
}

#[test]
fn overflow_4_peers_10_tx() {
    overflow_4_peers(10)
}

#[test]
fn overflow_4_peers_20_tx() {
    overflow_4_peers(20)
}

#[test]
fn overflow_4_peers_40_tx() {
    overflow_4_peers(40)
}

fn overflow_4_peers(maximum_transactions_in_block: u32) {
    let mut iroha_client = setup_bench_network(maximum_transactions_in_block);

    let account_id = AccountId::new(ACCOUNT_NAME, DOMAIN_NAME);
    let asset_definition_id = AssetDefinitionId::new("xor", DOMAIN_NAME);

    let (mut success_count, mut failures_count) = (0, 0);

    for _ in 0..10000 {
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
    }

    let mut get_num_txs = || {
        iroha_client
            .get_pending_txs()
            .map(|txs| txs.len())
            .unwrap_or(usize::MAX)
    };

    let mut len = get_num_txs();

    while len != 0 {
        iroha_logger::error!("Pending {} txs. Sleeping...", len);
        len = get_num_txs();
        thread::sleep(std::time::Duration::from_millis(50));
    }

    iroha_logger::error!(
        "Success count: {}, Failures count: {}",
        success_count,
        failures_count
    );

    if (failures_count + success_count) > 0 {
        assert!(
            success_count as f32 / (failures_count + success_count) as f32
                > MINIMUM_SUCCESS_REQUEST_RATIO
        );
    }
}
