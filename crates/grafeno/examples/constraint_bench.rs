//! The cost of an edge constraint on writes, and whether it keeps its
//! promise: people change jobs many times, and each must have exactly one
//! current employer, with the earlier ones kept in valid time.
//!
//! `cargo run --release -p grafeno --example constraint_bench [people] [changes]`

use std::time::Instant;

use grafeno::{
    Constraint, Direction, EdgeFilter, Grafeno, NewEdge, NewNode, NodeId, OnConflict, Scope,
    Timestamp,
};

const NS: &str = "jobs";

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

fn run(people: usize, changes: usize, constrained: bool) -> (f64, Grafeno, Vec<NodeId>) {
    let db = Grafeno::in_memory().unwrap();
    let (persons, companies) = db
        .write(|tx| {
            if constrained {
                tx.create_constraint(
                    NS,
                    Constraint::Edge {
                        name: "one_employer".into(),
                        label: "works_at".into(),
                        scope: Scope::Source,
                        on_conflict: OnConflict::Invalidate,
                    },
                )?;
            }
            let persons = (0..people)
                .map(|_| tx.create_node(NS, NewNode::new("Person")))
                .collect::<grafeno::Result<Vec<_>>>()?;
            let companies = (0..500)
                .map(|_| tx.create_node(NS, NewNode::new("Company")))
                .collect::<grafeno::Result<Vec<_>>>()?;
            Ok((persons, companies))
        })
        .unwrap();
    let mut rng = Rng(42);
    let started = Instant::now();
    let base = Timestamp::parse("2000-01-01").unwrap().as_micros();
    for chunk in 0..changes.div_ceil(1_000) {
        db.write(|tx| {
            for k in 0..1_000.min(changes - chunk * 1_000) {
                let step = (chunk * 1_000 + k) as i64;
                let person = persons[rng.below(people)];
                let company = companies[rng.below(companies.len())];
                // Changes arrive in time order, an hour apart (all past).
                let when = Timestamp::from_micros(base + step * 3_600_000_000);
                tx.create_edge(
                    NS,
                    NewEdge::new(person, "works_at", company).valid_from(when),
                )?;
            }
            Ok(())
        })
        .unwrap();
    }
    (started.elapsed().as_secs_f64(), db, persons)
}

fn main() {
    let args: Vec<usize> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let people = args.first().copied().unwrap_or(10_000);
    let changes = args.get(1).copied().unwrap_or(50_000);
    let (plain, _, _) = run(people, changes, false);
    let (constrained, db, persons) = run(people, changes, true);
    println!("{changes} job changes for {people} people (transactions of 1 000)");
    println!(
        "| without a constraint | {plain:.2} s | {:.0} facts/s |",
        changes as f64 / plain
    );
    println!(
        "| one employer per person | {constrained:.2} s | {:.0} facts/s |",
        changes as f64 / constrained
    );
    let read = db.begin_read().unwrap();
    let now = Timestamp::now();
    let (mut exactly_one, mut more, mut history) = (0, 0, 0);
    for person in &persons {
        // Employers, not edges: the same company twice is the same fact.
        let current = read
            .neighbors(
                NS,
                *person,
                Direction::Out,
                &EdgeFilter::any().label("works_at").valid_at(now),
            )
            .unwrap()
            .into_iter()
            .map(|n| n.node)
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let all = read
            .neighbors(
                NS,
                *person,
                Direction::Out,
                &EdgeFilter::any().label("works_at"),
            )
            .unwrap()
            .len();
        match current {
            0 => {}
            1 => exactly_one += 1,
            _ => more += 1,
        }
        history += all.saturating_sub(current);
    }
    let events = read.constraint_events(NS, usize::MAX).unwrap().len();
    println!(
        "people with one current employer: {exactly_one}, with more: {more}; {history} earlier jobs kept in valid time; {events} invalidations recorded"
    );
    assert!(read.check(NS).unwrap().is_ok());
}
