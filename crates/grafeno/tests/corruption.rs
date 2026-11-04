//! Damaged files: flipped bytes, zeroed pages and truncation. Verified with
//! `Grafeno::verify_file` and then opened, they must be refused, or read
//! back exactly as they were (redb repaired them, or the damage hit unused
//! space): never crash, never give different answers.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use grafeno::{Grafeno, NewEdge, NewNode, NodeId};

const NS: &str = "damaged";

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("grafeno-corrupt-{name}-{}.redb", NodeId::new()))
}

/// What happened when a damaged file was used.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// Refused by the file check or when opening.
    Refused,
    /// Opened, and a read or the check reported the damage.
    Detected,
    /// Opened, and everything read back the same as before.
    Intact,
    /// Opened and checked fine, but the content differs.
    Silent,
}

fn content(db: &Grafeno) -> grafeno::Result<String> {
    let mut out = Vec::new();
    db.begin_read()?.export(&[], &mut out)?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n"))
}

/// The way to use a file that may be damaged: verify it, then open it.
fn use_file(path: &Path, expected: &str) -> Outcome {
    if Grafeno::verify_file(path).is_err() {
        return Outcome::Refused;
    }
    let Ok(db) = Grafeno::open(path) else {
        return Outcome::Refused;
    };
    let reads = (|| -> grafeno::Result<bool> {
        let read = db.begin_read()?;
        let mut ok = true;
        for info in read.namespaces()? {
            ok &= read.check(&info.name)?.is_ok();
            read.stats(&info.name)?;
            read.search_text(&info.name, "damage words3", 10)?;
            read.search_vector(&info.name, &[1.0, 0.0, 1.0], 5)?;
        }
        Ok(ok)
    })();
    match reads {
        Err(_) | Ok(false) => Outcome::Detected,
        Ok(true) => match content(&db) {
            Err(_) => Outcome::Detected,
            Ok(found) if found == expected => Outcome::Intact,
            Ok(_) => Outcome::Silent,
        },
    }
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn damaged_files_never_crash() {
    let original = temp("original");
    let expected = {
        let db = Grafeno::open(&original).unwrap();
        db.write(|tx| {
            let mut previous = None;
            for i in 0..600 {
                let id = tx.create_node(
                    NS,
                    NewNode::new("Item")
                        .prop("i", i)
                        .text(format!("item {i} of the damage test with words{}", i % 7))
                        .embedding(vec![(i as f32).cos(), (i as f32).sin(), 1.0]),
                )?;
                if let Some(previous) = previous {
                    tx.create_edge(NS, NewEdge::new(previous, "next", id))?;
                }
                previous = Some(id);
            }
            Ok(())
        })
        .unwrap();
        content(&db).unwrap()
    };
    let bytes = std::fs::read(&original).unwrap();
    let mut random = Random(0x2545_F491_4F6C_DD1D);
    let mut outcomes = Vec::new();
    for round in 0..150 {
        let mut damaged = bytes.clone();
        let kind = round % 3;
        match kind {
            // A few flipped bytes anywhere.
            0 => {
                for _ in 0..1 + random.next() % 8 {
                    let at = (random.next() % damaged.len() as u64) as usize;
                    damaged[at] ^= 1 << (random.next() % 8);
                }
            }
            // A zeroed 4 KiB page.
            1 => {
                let page = (random.next() % (damaged.len() as u64 / 4096)) as usize * 4096;
                damaged[page..page + 4096].fill(0);
            }
            // Cut short.
            _ => {
                let length = (random.next() % damaged.len() as u64) as usize;
                damaged.truncate(length);
            }
        }
        let path = temp("copy");
        std::fs::write(&path, &damaged).unwrap();
        let outcome = catch_unwind(AssertUnwindSafe(|| use_file(&path, &expected)));
        let _ = std::fs::remove_file(&path);
        match outcome {
            Ok(outcome) => outcomes.push((round, kind, outcome)),
            Err(panic) => {
                let message = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap_or_default();
                panic!("round {round} (kind {kind}) crashed: {message}");
            }
        }
    }
    let count = |wanted: Outcome| outcomes.iter().filter(|(_, _, o)| *o == wanted).count();
    eprintln!(
        "refused {}, detected {}, intact {}, silent {}",
        count(Outcome::Refused),
        count(Outcome::Detected),
        count(Outcome::Intact),
        count(Outcome::Silent)
    );
    let silent: Vec<_> = outcomes
        .iter()
        .filter(|(_, _, o)| *o == Outcome::Silent)
        .collect();
    assert!(silent.is_empty(), "damage that went unnoticed: {silent:?}");
    let _ = std::fs::remove_file(&original);
}
