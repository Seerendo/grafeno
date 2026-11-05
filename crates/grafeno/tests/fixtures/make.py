"""Builds old versions of Grafeno (one commit per layout version) and writes a
fixture database with each, compressed into this folder. The upgrade test
(crates/grafeno/tests/upgrade.rs) opens them with the current code.

    python crates/grafeno/tests/fixtures/make.py [version ...]

A new layout version adds its commit to COMMITS (and what it introduced to
EXTRAS). Needs git, Rust and the repository's history.
"""
import gzip
import os
import shutil
import subprocess
import sys

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), '..', '..', '..', '..'))
import tempfile
WORK = os.path.join(tempfile.gettempdir(), 'grafeno-fixtures')
TARGET = os.path.join(WORK, 'target')
OUT = os.path.join(REPO, 'crates', 'grafeno', 'tests', 'fixtures')

COMMITS = [(1, 'f99ab08'), (2, '0622a34'), (3, '74a2cf8'), (4, '5fdcdc4'), (5, '07256b3'), (6, '50fda41'), (7, '1d262d4'), (8, '8e473f5'), (9, '25a14fb')]

BASE = r'''
// Writes the fixture of this layout version (see crates/grafeno/tests/fixtures).
use grafeno::{Grafeno, NewEdge, NewNode, Timestamp};

const JAN_2020: i64 = 1_577_836_800_000_000;
const JAN_2022: i64 = 1_640_995_200_000_000;

fn main() {
    let path = std::env::args().nth(1).expect("output path");
    let db = Grafeno::open(&path).unwrap();
    db.write(|tx| {
        let ns = "fixture";
        let mut people = Vec::new();
        for i in 0..40u32 {
            let angle = i as f32;
            let id = tx.create_node(
                ns,
                NewNode::new("Person")
                    .prop("name", format!("Person {i}"))
                    .prop("age", 20 + i)
                    .text(format!("person {i} works on graphs and memory topic{}", i % 5))
                    .embedding(vec![angle.cos(), angle.sin(), 1.0]),
            )?;
            people.push(id);
        }
        let acme = tx.create_node(ns, NewNode::new("Company").prop("name", "Acme").text("Acme builds graph engines"))?;
        for (i, id) in people.iter().enumerate() {
            let mut fact = NewEdge::new(*id, "works_at", acme);
            if i % 2 == 0 {
                fact = fact
                    .valid_from(Timestamp::from_micros(JAN_2020))
                    .valid_to(Timestamp::from_micros(JAN_2022));
            }
            tx.create_edge(ns, fact)?;
            if i > 0 {
                tx.create_edge(ns, NewEdge::new(people[i - 1], "knows", *id))?;
            }
        }
        tx.create_node("other", NewNode::new("Note").prop("title", "elsewhere").text("another namespace"))?;
        EXTRA
        Ok(())
    })
    .unwrap();
    println!("wrote {path}");
}
'''

EXTRAS = {
    4: '''tx.set_importance(ns, people[0], 0.9)?;''',
    5: '''tx.set_importance(ns, people[0], 0.9)?;
        tx.create_index(ns, grafeno::IndexDef { name: "Person_age".into(), label: "Person".into(), key: "age".into(), unique: false })?;''',
    6: '''tx.set_importance(ns, people[0], 0.9)?;
        tx.create_index(ns, grafeno::IndexDef { name: "Person_age".into(), label: "Person".into(), key: "age".into(), unique: false })?;
        tx.create_namespace("empty", Some("kept empty"))?;''',
    7: '''tx.set_importance(ns, people[0], 0.9)?;
        tx.create_index(ns, grafeno::IndexDef { name: "Person_age".into(), label: "Person".into(), key: "age".into(), unique: false })?;
        tx.create_namespace("empty", Some("kept empty"))?;
        tx.create_principal(&grafeno::Principal { name: "reader".into(), secret_hash: "00".repeat(32), grants: vec![grafeno::Grant { namespace: "fixture".into(), role: grafeno::Role::Read }], owner: false, own_only: false, description: None, created_at: Timestamp::from_micros(JAN_2022) })?;''',
    8: '''tx.set_importance(ns, people[0], 0.9)?;
        tx.create_index(ns, grafeno::IndexDef { name: "Person_age".into(), label: "Person".into(), key: "age".into(), unique: false })?;
        tx.create_namespace("empty", Some("kept empty"))?;
        tx.create_principal(&grafeno::Principal { name: "reader".into(), secret_hash: "00".repeat(32), grants: vec![grafeno::Grant { namespace: "fixture".into(), role: grafeno::Role::Read }], owner: false, own_only: false, description: None, created_at: Timestamp::from_micros(JAN_2022) })?;''',
    9: '''tx.set_importance(ns, people[0], 0.9)?;
        tx.create_index(ns, grafeno::IndexDef::new("Person", "age"))?;
        tx.create_namespace("empty", Some("kept empty"))?;
        tx.create_principal(&grafeno::Principal { name: "reader".into(), secret_hash: "00".repeat(32), grants: vec![grafeno::Grant { namespace: "fixture".into(), role: grafeno::Role::Read }], owner: false, own_only: false, description: None, created_at: Timestamp::from_micros(JAN_2022) })?;''',
}


def run(*args, cwd=None, env=None):
    print('>', ' '.join(args), flush=True)
    subprocess.run(args, cwd=cwd, check=True, env=env)


def main():
    os.makedirs(WORK, exist_ok=True)
    os.makedirs(OUT, exist_ok=True)
    env = dict(os.environ, CARGO_TARGET_DIR=TARGET)
    only = {int(v) for v in sys.argv[1:]}
    for version, commit in COMMITS:
        if only and version not in only:
            continue
        tree = os.path.join(WORK, f'v{version}')
        if os.path.exists(tree):
            run('git', 'worktree', 'remove', '--force', tree, cwd=REPO)
        run('git', 'worktree', 'add', '--detach', tree, commit, cwd=REPO)
        example = os.path.join(tree, 'crates', 'grafeno', 'examples')
        os.makedirs(example, exist_ok=True)
        with open(os.path.join(example, 'fixture_gen.rs'), 'w', encoding='utf-8') as f:
            f.write(BASE.replace('EXTRA', EXTRAS.get(version, '')))
        db = os.path.join(WORK, f'layout-v{version}.redb')
        if os.path.exists(db):
            os.remove(db)
        run('cargo', 'run', '-q', '-p', 'grafeno', '--example', 'fixture_gen', '--', db, cwd=tree, env=env)
        with open(db, 'rb') as source, gzip.open(os.path.join(OUT, f'layout-v{version}.redb.gz'), 'wb', 9) as target:
            shutil.copyfileobj(source, target)
        run('git', 'worktree', 'remove', '--force', tree, cwd=REPO)
    print('done')


main()
