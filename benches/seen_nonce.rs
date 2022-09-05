#[macro_use]
extern crate criterion;

use std::sync::Arc;
use criterion::Criterion;
use flurry::HashSet;
use rand::thread_rng;
use snarkvm::dpc::testnet2::Testnet2;
use snarkvm::prelude::Network;
use snarkvm::utilities::UniformRand;

fn fake_nonce() -> String {
    let nonce: <Testnet2 as Network>::PoSWNonce = UniformRand::rand(&mut thread_rng());
    nonce.to_string()
}

fn seen_nonce_benchmark(c: &mut Criterion) {
    let nonce_seen = Arc::new(HashSet::with_capacity(10 << 20));
    c.bench_function("seen_nonce", |b| b.iter(|| nonce_seen.pin().insert(fake_nonce())));
}

criterion_group!(nonce,seen_nonce_benchmark);
criterion_main!(nonce);
