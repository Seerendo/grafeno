//! The cost of passwords (phase 14.1): Argon2id with the parameters the
//! server uses (19 MiB, 2 passes, 1 lane), as hashes made and checked one
//! at a time, and as sign-ins a second on one core and on every core.
//!
//! `cargo run --release -p grafeno-server --example auth_bench [checks]`

use std::time::Instant;

use grafeno_server::auth::{hash_password, verify_password};

fn main() {
    let checks: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(50);
    let started = Instant::now();
    let hashes: Vec<String> = (0..checks)
        .map(|i| hash_password(&format!("passphrase number {i}")))
        .collect();
    let hash_ms = started.elapsed().as_secs_f64() * 1000.0 / checks as f64;
    let started = Instant::now();
    for (i, hash) in hashes.iter().enumerate() {
        assert!(verify_password(hash, &format!("passphrase number {i}")));
    }
    let verify_ms = started.elapsed().as_secs_f64() * 1000.0 / checks as f64;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let per_thread = checks.div_ceil(threads).max(4);
    let started = Instant::now();
    std::thread::scope(|scope| {
        for t in 0..threads {
            let hash = &hashes[t % hashes.len()];
            let password = format!("passphrase number {}", t % hashes.len());
            scope.spawn(move || {
                for _ in 0..per_thread {
                    assert!(verify_password(hash, &password));
                }
            });
        }
    });
    let parallel = (threads * per_thread) as f64 / started.elapsed().as_secs_f64();
    println!("measure,value,unit");
    println!("hash,{hash_ms:.1},ms");
    println!("check,{verify_ms:.1},ms");
    println!("sign-ins on one core,{:.0},per second", 1000.0 / verify_ms);
    println!("sign-ins on {threads} threads,{parallel:.0},per second");
    println!("memory per check,19,MiB");
}
