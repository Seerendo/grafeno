//! Entity resolution on synthetic people: speed of a fuzzy index against
//! comparing with every node, and the quality of a match rule's decisions.
//!
//! `cargo run --release -p grafeno --example resolve_bench [people]`
//!
//! The data: `people` persons (first and last names from lists, a birth
//! date, a city). Then two kinds of probes, each a record to look up:
//!
//! - **duplicates**: a person's record with realistic damage (a typo, two
//!   letters swapped, a dropped accent, the words in another order, an
//!   initial for the first name, day and month swapped in the date);
//! - **namesakes**: an existing person's exact name with another birth date
//!   and city (a different person who happens to share the name).
//!
//! A duplicate should be found as `same` (or at least `review`); a namesake
//! should not be called `same`.

use std::time::{Duration, Instant};

use grafeno::textsim::Measure;
use grafeno::{Decision, FieldRule, Grafeno, IndexDef, MatchRule, NewNode, NodeId, Probe};
use serde_json::json;

const NS: &str = "people";
const FIRST: &[&str] = &[
    "José",
    "María",
    "Juan",
    "Ana",
    "Luis",
    "Carmen",
    "Carlos",
    "Rosa",
    "Jorge",
    "Lucía",
    "Miguel",
    "Elena",
    "Pedro",
    "Sofía",
    "Andrés",
    "Isabel",
    "Javier",
    "Patricia",
    "Fernando",
    "Gabriela",
    "Ricardo",
    "Valeria",
    "Diego",
    "Camila",
    "Alejandro",
    "Daniela",
    "Roberto",
    "Mónica",
    "Sergio",
    "Natalia",
    "Raúl",
    "Paula",
    "Héctor",
    "Teresa",
    "Iván",
    "Claudia",
    "John",
    "Mary",
    "James",
    "Linda",
    "Robert",
    "Susan",
    "Michael",
    "Karen",
    "William",
    "Nancy",
];
const LAST: &[&str] = &[
    "García",
    "Rodríguez",
    "Martínez",
    "López",
    "González",
    "Pérez",
    "Sánchez",
    "Ramírez",
    "Torres",
    "Flores",
    "Rivera",
    "Gómez",
    "Díaz",
    "Cruz",
    "Morales",
    "Ortiz",
    "Gutiérrez",
    "Chávez",
    "Ramos",
    "Herrera",
    "Medina",
    "Castillo",
    "Jiménez",
    "Vargas",
    "Romero",
    "Álvarez",
    "Mendoza",
    "Ruiz",
    "Aguilar",
    "Quispe",
    "Mamani",
    "Huamán",
    "Rojas",
    "Vásquez",
    "Castro",
    "Smith",
    "Johnson",
    "Williams",
    "Brown",
    "Jones",
    "Miller",
    "Davis",
    "Wilson",
    "Taylor",
];
const CITIES: &[&str] = &[
    "Lima",
    "Cusco",
    "Arequipa",
    "Quito",
    "Bogotá",
    "Madrid",
    "Sevilla",
    "Santiago",
    "La Paz",
    "Trujillo",
    "Piura",
    "Chiclayo",
    "Medellín",
    "Valencia",
    "Austin",
    "Denver",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

#[derive(Clone)]
struct Person {
    name: String,
    born: String,
    city: String,
}

fn person(rng: &mut Rng) -> Person {
    // Two last names half the time, as in Spanish-speaking countries.
    let mut name = format!("{} {}", rng.pick(FIRST), rng.pick(LAST));
    if rng.below(2) == 0 {
        name = format!("{name} {}", rng.pick(LAST));
    }
    Person {
        name,
        born: format!(
            "{}-{:02}-{:02}",
            1940 + rng.below(65),
            1 + rng.below(12),
            1 + rng.below(28)
        ),
        city: rng.pick(CITIES).to_owned(),
    }
}

/// One or two kinds of damage a record goes through when retyped.
fn damage(rng: &mut Rng, p: &Person) -> (Person, &'static str) {
    let mut out = p.clone();
    let chars: Vec<char> = p.name.chars().collect();
    let kind = rng.below(6);
    let what = match kind {
        0 => {
            // A typo: one letter replaced.
            let at = 1 + rng.below(chars.len() - 2);
            let mut c = chars.clone();
            if c[at] != ' ' {
                c[at] = "aeioulnrst".chars().nth(rng.below(10)).unwrap();
            }
            out.name = c.into_iter().collect();
            "typo"
        }
        1 => {
            // Two neighbors swapped.
            let at = 1 + rng.below(chars.len() - 3);
            let mut c = chars.clone();
            if c[at] != ' ' && c[at + 1] != ' ' {
                c.swap(at, at + 1);
            }
            out.name = c.into_iter().collect();
            "swapped letters"
        }
        2 => {
            out.name = grafeno::normalize_name(&p.name).to_uppercase();
            "no accents, upper case"
        }
        3 => {
            // Last names first.
            let words: Vec<&str> = p.name.split(' ').collect();
            out.name = format!("{} {}", words[1..].join(" "), words[0]);
            "word order"
        }
        4 => {
            // An initial for the first name.
            let words: Vec<&str> = p.name.split(' ').collect();
            out.name = format!("{}. {}", &words[0][..1], words[1..].join(" "));
            "initial"
        }
        _ => {
            // Day and month swapped (only when it gives a valid date).
            let parts: Vec<&str> = p.born.split('-').collect();
            if parts[2].parse::<u32>().unwrap() <= 12 {
                out.born = format!("{}-{}-{}", parts[0], parts[2], parts[1]);
            }
            "day and month swapped"
        }
    };
    if rng.below(4) == 0 {
        out.city = String::new();
    }
    (out, what)
}

fn rule() -> MatchRule {
    MatchRule {
        label: "Person".into(),
        strong_keys: Vec::new(),
        fields: vec![
            FieldRule {
                key: "name".into(),
                measure: Measure::Dice { n: 3 },
                weight: 2.0,
            },
            FieldRule {
                key: "name".into(),
                measure: Measure::Tokens,
                weight: 0.5,
            },
            FieldRule {
                key: "born_on".into(),
                // Birth dates: a near miss is a different person; a swapped
                // day and month still scores 0.8.
                measure: Measure::Date { days: 30.0 },
                weight: 2.0,
            },
            FieldRule {
                key: "city".into(),
                measure: Measure::Exact,
                weight: 0.5,
            },
        ],
        neighbors: 0.0,
        same: 0.85,
        review: 0.7,
    }
}

fn percentile(sorted: &[Duration], q: f64) -> f64 {
    sorted[((q * sorted.len() as f64) as usize).min(sorted.len() - 1)].as_secs_f64() * 1e3
}

fn main() {
    let count: usize = std::env::args()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(100_000);
    let mut rng = Rng(0xC0FFEE);
    let people: Vec<Person> = (0..count).map(|_| person(&mut rng)).collect();
    let db = Grafeno::in_memory().unwrap();
    let started = Instant::now();
    let mut ids: Vec<NodeId> = Vec::with_capacity(count);
    for chunk in people.chunks(10_000) {
        ids.extend(
            db.write(|tx| {
                chunk
                    .iter()
                    .map(|p| {
                        let mut node = NewNode::new("Person")
                            .prop("name", p.name.as_str())
                            .prop("born_on", p.born.as_str());
                        if !p.city.is_empty() {
                            node = node.prop("city", p.city.as_str());
                        }
                        tx.create_node(NS, node)
                    })
                    .collect::<grafeno::Result<Vec<_>>>()
            })
            .unwrap(),
        );
    }
    println!(
        "{count} people written in {:.1} s",
        started.elapsed().as_secs_f64()
    );
    let started = Instant::now();
    let entries = db
        .write(|tx| tx.create_index(NS, IndexDef::fuzzy("Person", "name")))
        .unwrap();
    println!(
        "fuzzy index on name: {entries} trigram entries ({:.1} per person) in {:.1} s",
        entries as f64 / count as f64,
        started.elapsed().as_secs_f64()
    );
    db.write(|tx| tx.set_match_rule(NS, &rule())).unwrap();
    let read = db.begin_read().unwrap();

    // Speed: the fuzzy index against comparing with every node.
    let queries: Vec<(usize, Person)> = (0..300)
        .map(|_| {
            let at = rng.below(count);
            (at, damage(&mut rng, &people[at]).0)
        })
        .collect();
    let mut indexed = Vec::new();
    let mut found_top1 = 0;
    for (at, q) in &queries {
        let started = Instant::now();
        let hits = read
            .fuzzy_search(NS, "Person", "name", &q.name, Measure::Dice { n: 3 }, 10)
            .unwrap();
        indexed.push(started.elapsed());
        // Names repeat (namesakes): count the true person's name being
        // among the hits.
        let name = &people[*at].name;
        if hits.iter().any(|hit| {
            read.node(NS, hit.node)
                .unwrap()
                .is_some_and(|n| n.prop("name") == Some(&json!(name)))
        }) {
            found_top1 += 1;
        }
    }
    indexed.sort();
    let mut scanned = Vec::new();
    for (_, q) in queries.iter().take(30) {
        let started = Instant::now();
        let mut best = 0.0f64;
        for p in &people {
            best = best.max(Measure::Dice { n: 3 }.compare(&q.name, &p.name));
        }
        std::hint::black_box(best);
        scanned.push(started.elapsed());
    }
    scanned.sort();
    println!("\n| search for a damaged name, top 10 | p50 ms | p99 ms |");
    println!("| --- | --- | --- |");
    println!(
        "| fuzzy index | {:.2} | {:.2} |",
        percentile(&indexed, 0.5),
        percentile(&indexed, 0.99)
    );
    println!(
        "| compare with every node | {:.0} | {:.0} |",
        percentile(&scanned, 0.5),
        percentile(&scanned, 0.99)
    );
    println!(
        "the true name is in the top 10 for {found_top1} of {} damaged names",
        queries.len()
    );

    // Quality: duplicates and namesakes through the match rule.
    let mut kinds: std::collections::BTreeMap<&str, [usize; 4]> = Default::default();
    let mut latencies = Vec::new();
    let duplicates = 2_000;
    let mut same_right = 0;
    let mut same_total = 0;
    for _ in 0..duplicates {
        let at = rng.below(count);
        let (q, what) = damage(&mut rng, &people[at]);
        let mut props = json!({ "name": q.name, "born_on": q.born });
        if !q.city.is_empty() {
            props["city"] = json!(q.city);
        }
        let probe = Probe {
            label: "Person".into(),
            props: props.as_object().unwrap().clone().into_iter().collect(),
            neighbors: Vec::new(),
        };
        let started = Instant::now();
        let found = read.find_matches(NS, &probe, None, 3, None).unwrap();
        latencies.push(started.elapsed());
        let entry = kinds.entry(what).or_default();
        entry[0] += 1;
        match found.first() {
            Some(m) if m.node == ids[at] && m.decision == Decision::Same => entry[1] += 1,
            Some(m) if m.node == ids[at] && m.decision == Decision::Review => entry[2] += 1,
            _ => entry[3] += 1,
        }
        if let Some(m) = found.first().filter(|m| m.decision == Decision::Same) {
            same_total += 1;
            if m.node == ids[at] {
                same_right += 1;
            }
        }
    }
    let namesakes = 2_000;
    let mut namesake_same = 0;
    let mut namesake_review = 0;
    for _ in 0..namesakes {
        let at = rng.below(count);
        let other = person(&mut rng);
        let probe = Probe {
            label: "Person".into(),
            props: json!({ "name": people[at].name, "born_on": other.born, "city": other.city })
                .as_object()
                .unwrap()
                .clone()
                .into_iter()
                .collect(),
            neighbors: Vec::new(),
        };
        let started = Instant::now();
        let found = read.find_matches(NS, &probe, None, 3, None).unwrap();
        latencies.push(started.elapsed());
        match found.first().map(|m| m.decision) {
            Some(Decision::Same) => {
                namesake_same += 1;
                same_total += 1;
            }
            Some(Decision::Review) => namesake_review += 1,
            _ => {}
        }
    }
    latencies.sort();
    println!("\n| duplicate kind | records | same | review | missed |");
    println!("| --- | --- | --- | --- | --- |");
    let mut totals = [0usize; 4];
    for (what, [n, same, review, missed]) in &kinds {
        println!("| {what} | {n} | {same} | {review} | {missed} |");
        totals[0] += n;
        totals[1] += same;
        totals[2] += review;
        totals[3] += missed;
    }
    println!(
        "| all | {} | {} | {} | {} |",
        totals[0], totals[1], totals[2], totals[3]
    );
    println!(
        "\nnamesakes: {namesakes} records, {namesake_same} called same, {namesake_review} to review"
    );
    println!(
        "precision of `same`: {:.3} ({same_right} right of {same_total}); recall at `same`: {:.3}, at `review` or better: {:.3}",
        same_right as f64 / same_total.max(1) as f64,
        totals[1] as f64 / totals[0] as f64,
        (totals[1] + totals[2]) as f64 / totals[0] as f64,
    );
    println!(
        "find_matches per record: p50 {:.2} ms, p99 {:.2} ms",
        percentile(&latencies, 0.5),
        percentile(&latencies, 0.99)
    );
}
