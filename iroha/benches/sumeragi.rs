use criterion::*;
use iroha::{peer::PeerId, sumeragi::Sumeragi};

const N_PEERS: usize = 255;

fn get_n_peers(n: usize) -> Vec<PeerId> {
    (0..n)
        .map(|i| PeerId {
            address: format!("127.0.0.{}", i),
            public_key: [0u8; 32],
        })
        .collect()
}

fn sort_hash_combine(criterion: &mut Criterion) {
    let mut peers: Vec<PeerId> = get_n_peers(N_PEERS);
    criterion.bench_function("sort_hash_combine", |b| {
        b.iter(|| Sumeragi::sort_peers(&mut peers, Some([0u8; 32])));
    });
}

fn sort_with_rand(criterion: &mut Criterion) {
    let mut peers: Vec<PeerId> = get_n_peers(N_PEERS);
    criterion.bench_function("sort_with_rand", |b| {
        b.iter(|| Sumeragi::sort_peers_with_rand(&mut peers, Some([0u8; 32])));
    });
}

criterion_group!(benches, sort_hash_combine, sort_with_rand);
criterion_main!(benches);
