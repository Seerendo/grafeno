//! The cost of the audit log (phase 14.5): appending events one write each
//! (a refused request, a failed sign-in), many in one write, and reading
//! them back; and what a change of an account costs with its event.
//!
//! `cargo run --release -p grafeno --example audit_bench [events]`

use std::time::Instant;

use grafeno::{Account, AccountKind, Grafeno, NewAuditEvent, Outcome, Timestamp};

fn event(i: usize) -> NewAuditEvent {
    NewAuditEvent::new("refused")
        .target(format!("GET /v1/ns{}/nodes", i % 100))
        .outcome(Outcome::Refused)
        .detail(serde_json::json!({ "code": "unauthorized" }))
}

fn main() {
    let events: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(100_000);
    let db = Grafeno::in_memory().unwrap();

    let one_each = events / 10;
    let started = Instant::now();
    for i in 0..one_each {
        db.write(|tx| tx.audit(event(i)).map(|_| ())).unwrap();
    }
    let single = one_each as f64 / started.elapsed().as_secs_f64();

    let started = Instant::now();
    for chunk in 0..(events / 1000) {
        db.write(|tx| {
            for i in 0..1000 {
                tx.audit(event(chunk * 1000 + i))?;
            }
            Ok(())
        })
        .unwrap();
    }
    let batched = (events / 1000 * 1000) as f64 / started.elapsed().as_secs_f64();

    let started = Instant::now();
    let read = db.begin_read().unwrap();
    let mut since = 0;
    let mut seen = 0;
    loop {
        let page = read.audit(since, 1000).unwrap();
        if page.events.is_empty() {
            break;
        }
        seen += page.events.len();
        since = page.events.last().unwrap().seq;
    }
    let reading = seen as f64 / started.elapsed().as_secs_f64();
    drop(read);

    // A change of an account, which writes its event in the same
    // transaction.
    let changes = 2_000;
    let started = Instant::now();
    for i in 0..changes {
        db.write(|tx| {
            tx.put_account(&Account {
                name: format!("a{}", i % 50),
                kind: AccountKind::Service,
                password_hash: None,
                grants: Vec::new(),
                roles: Vec::new(),
                superuser: false,
                own_only: false,
                disabled: i % 2 == 0,
                expires_at: None,
                attributes: Default::default(),
                description: None,
                created_at: Timestamp::now(),
            })
        })
        .unwrap();
    }
    let account_us = started.elapsed().as_secs_f64() * 1e6 / changes as f64;

    println!("measure,value,unit");
    println!("events,{seen},");
    println!("appended one write each,{single:.0},events/s");
    println!("appended a thousand a write,{batched:.0},events/s");
    println!("read back,{reading:.0},events/s");
    println!("a change of an account with its event,{account_us:.0},us");
}
