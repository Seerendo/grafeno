//! Extraction on synthetic memories whose mentions are known: how fast,
//! how incremental, and how right.
//!
//! `cargo run --release -p grafeno-memory --example extract_bench [entities] [memories]`
//!
//! Entities are people and companies with generated names (some people
//! share a name: namesakes). Each memory mentions two to four of them by
//! name, sometimes with an email, a hashtag, a date and an amount, plus,
//! now and then, a new name that recurs across memories (to become an
//! entity) or appears once (to wait for review). Because the generator
//! knows what each memory mentions, every link can be checked.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Instant;

use grafeno::{Direction, EdgeFilter, Grafeno, NewEdge, NewNode, NodeId};
use grafeno_memory::{ExtractOptions, MENTIONS, extract, review_queue};

const NS: &str = "bench";
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
];
const COMPANY: &[&str] = &[
    "Andina",
    "Pacífico",
    "Altiplano",
    "Horizonte",
    "Cóndor",
    "Amazonas",
    "Inca",
    "Quinoa",
    "Nevado",
    "Sol",
    "Volcán",
    "Laguna",
    "Cumbre",
    "Puerto",
    "Selva",
    "Desierto",
];
const KIND: &[&str] = &[
    "Logística",
    "Seguros",
    "Minera",
    "Textil",
    "Software",
    "Agro",
    "Energía",
    "Salud",
];
const VERBS: &[&str] = &[
    "habló con",
    "se reunió con",
    "envió el informe a",
    "llamó a",
    "trabaja con",
    "presentó la propuesta a",
    "firmó el contrato con",
    "revisó el pedido de",
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
}

fn main() {
    let args: Vec<usize> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let entities = args.first().copied().unwrap_or(5_000);
    let memories = args.get(1).copied().unwrap_or(20_000);
    let mut rng = Rng(0xBEEF);
    let db = Grafeno::in_memory().unwrap();

    // Entities: four people for each company; names drawn from lists, so
    // some people share a name.
    let mut names: Vec<String> = Vec::new();
    let mut ids: Vec<NodeId> = Vec::new();
    let mut emails: Vec<Option<String>> = Vec::new();
    let mut employer: Vec<Option<usize>> = Vec::new();
    db.write(|tx| {
        let companies = entities / 5;
        for c in 0..companies {
            let name = format!(
                "{} {} {}",
                COMPANY[c % COMPANY.len()],
                KIND[(c / COMPANY.len()) % KIND.len()],
                if c >= COMPANY.len() * KIND.len() {
                    format!("{}", c / (COMPANY.len() * KIND.len()))
                } else {
                    String::new()
                }
            )
            .trim()
            .to_owned();
            ids.push(tx.create_node(NS, NewNode::new("Company").prop("name", name.as_str()))?);
            names.push(name);
            emails.push(None);
            employer.push(None);
        }
        for p in companies..entities {
            let name = format!(
                "{} {}",
                FIRST[rng.below(FIRST.len())],
                LAST[rng.below(LAST.len())]
            );
            let email = format!("p{p}@correo.pe");
            let id = tx.create_node(
                NS,
                NewNode::new("Person")
                    .prop("name", name.as_str())
                    .prop("email", email.as_str()),
            )?;
            let company = rng.below(companies);
            tx.create_edge(NS, NewEdge::new(id, "works_at", ids[company]))?;
            ids.push(id);
            names.push(name);
            emails.push(Some(email));
            employer.push(Some(company));
        }
        tx.create_index(NS, grafeno::IndexDef::new("Person", "email"))?;
        Ok(())
    })
    .unwrap();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    for name in &names {
        *by_name.entry(grafeno::normalize_name(name)).or_default() += 1;
    }
    let shared = names
        .iter()
        .filter(|n| by_name[&grafeno::normalize_name(n)] > 1)
        .count();
    println!(
        "{} entities ({} companies), {} of them share their name with another",
        entities,
        entities / 5,
        shared
    );

    // Memories, with what each mentions.
    let new_names: Vec<String> = (0..200)
        .map(|i| format!("Proyecto {} {}", COMPANY[i % COMPANY.len()], i))
        .collect();
    let mut truth: Vec<(NodeId, Vec<usize>)> = Vec::new();
    let started = Instant::now();
    for chunk in 0..memories.div_ceil(2_000) {
        let count = 2_000.min(memories - chunk * 2_000);
        let batch: Vec<(String, Vec<usize>)> = (0..count)
            .map(|_| {
                let people: Vec<usize> = (0..1 + rng.below(3))
                    .map(|_| entities / 5 + rng.below(entities - entities / 5))
                    .collect();
                let mut text = String::new();
                let mut mentioned = Vec::new();
                for (k, &p) in people.iter().enumerate() {
                    let company = employer[p].unwrap();
                    if k > 0 {
                        text.push_str(" Después, ");
                    }
                    text.push_str(&format!(
                        "{} {} {}",
                        names[p],
                        VERBS[rng.below(VERBS.len())],
                        names[company]
                    ));
                    mentioned.push(p);
                    mentioned.push(company);
                    if rng.below(4) == 0 {
                        text.push_str(&format!(
                            " (escribió desde {})",
                            emails[p].as_ref().unwrap()
                        ));
                    }
                    text.push('.');
                }
                if rng.below(3) == 0 {
                    text.push_str(&format!(
                        " #{}",
                        ["ventas", "soporte", "contratos", "logistica"][rng.below(4)]
                    ));
                }
                if rng.below(3) == 0 {
                    text.push_str(&format!(" El pago fue de USD {}.", 100 + rng.below(9_900)));
                }
                if rng.below(10) == 0 {
                    text.push_str(&format!(
                        " Hablaron del {}.",
                        new_names[rng.below(new_names.len())]
                    ));
                }
                (text, mentioned)
            })
            .collect();
        let made = db
            .write(|tx| {
                batch
                    .iter()
                    .map(|(text, _)| tx.create_node(NS, NewNode::new("Memory").text(text.as_str())))
                    .collect::<grafeno::Result<Vec<_>>>()
            })
            .unwrap();
        truth.extend(made.into_iter().zip(batch.into_iter().map(|(_, m)| m)));
    }
    println!(
        "{memories} memories written in {:.1} s",
        started.elapsed().as_secs_f64()
    );

    let options = ExtractOptions::default();
    let started = Instant::now();
    let report = extract(&db, NS, &options).unwrap();
    let first = started.elapsed().as_secs_f64();
    println!(
        "\nfirst run: {} texts in {first:.1} s ({:.0} texts/s), {} mentions, {} linked, {} created, {} queued",
        report.read,
        report.read as f64 / first,
        report.mentions,
        report.linked,
        report.created,
        report.queued
    );
    println!("found: {:?}", report.found);

    let started = Instant::now();
    let again = extract(&db, NS, &options).unwrap();
    println!(
        "second run, nothing changed: {} read, {} unchanged, {:.2} s",
        again.read,
        again.unchanged,
        started.elapsed().as_secs_f64()
    );
    let changed: Vec<NodeId> = truth.iter().step_by(100).map(|(id, _)| *id).collect();
    db.write(|tx| {
        for id in &changed {
            let text = tx.text(NS, *id)?.unwrap_or_default();
            tx.set_text(NS, *id, &format!("{text} Nota añadida."))?;
        }
        Ok(())
    })
    .unwrap();
    let started = Instant::now();
    let again = extract(&db, NS, &options).unwrap();
    println!(
        "third run, 1% changed: {} read, {} unchanged, {:.2} s",
        again.read,
        again.unchanged,
        started.elapsed().as_secs_f64()
    );

    // Quality: the entities each memory links against what it mentions.
    let read = db.begin_read().unwrap();
    let entity: HashMap<NodeId, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let (mut right, mut wrong, mut expected, mut found_named) = (0, 0, 0, 0);
    let mut ambiguous_expected = 0;
    for (memory, mentioned) in &truth {
        let linked: HashSet<usize> = read
            .neighbors(
                NS,
                *memory,
                Direction::Out,
                &EdgeFilter::any().label(MENTIONS),
            )
            .unwrap()
            .into_iter()
            .filter_map(|n| entity.get(&n.node).copied())
            .collect();
        let wanted: BTreeSet<usize> = mentioned.iter().copied().collect();
        for w in &wanted {
            expected += 1;
            if by_name[&grafeno::normalize_name(&names[*w])] > 1 {
                ambiguous_expected += 1;
            }
            if linked.contains(w) {
                right += 1;
            }
        }
        for l in &linked {
            found_named += 1;
            if !wanted.contains(l) {
                wrong += 1;
            }
        }
    }
    let queue = review_queue(&read, NS, usize::MAX).unwrap();
    let ambiguous = queue.iter().filter(|i| i.kind == "ambiguous").count();
    let phrases = queue.iter().filter(|i| i.kind == "phrase").count();
    let made_from_phrases = read
        .nodes_page(NS, Some("Entity"), None, usize::MAX)
        .unwrap()
        .iter()
        .filter(|n| n.display_name().starts_with("Proyecto"))
        .count();
    println!(
        "\nentity links: {right} of {expected} mentions linked to the right entity ({:.3} recall); {wrong} of {found_named} links wrong ({:.4} precision)",
        right as f64 / expected as f64,
        1.0 - wrong as f64 / found_named.max(1) as f64
    );
    println!(
        "{ambiguous_expected} mentions were of a shared name; {ambiguous} names wait as ambiguous, {phrases} phrases wait; {made_from_phrases} of the recurring new names became entities"
    );
}
