//! Databases written by every past layout version (fixtures made with the
//! code of that version, see `tests/fixtures/make.py`) open, upgrade and
//! answer like new.

use std::io::Read;
use std::path::PathBuf;

use grafeno::{Direction, EdgeFilter, Grafeno, NewNode, NodeId, Timestamp};

const NS: &str = "fixture";

/// Decompresses the fixture of `version` into a temporary file.
fn fixture(version: u64) -> PathBuf {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tests/fixtures/layout-v{version}.redb.gz"));
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(&source).unwrap())
        .read_to_end(&mut bytes)
        .unwrap();
    let path =
        std::env::temp_dir().join(format!("grafeno-upgrade-v{version}-{}.redb", NodeId::new()));
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Everything a fixture holds, read back after the upgrade.
fn verify(db: &Grafeno, version: u64) {
    let read = db.begin_read().unwrap();
    let stats = read.stats(NS).unwrap();
    assert_eq!((stats.nodes, stats.edges), (41, 79), "v{version}");
    assert_eq!(stats.node_labels["Person"], 40);
    assert_eq!(stats.edge_labels["works_at"], 40);
    assert_eq!((stats.texts, stats.vectors), (41, 40));
    for ns in [NS, "other"] {
        let report = read.check(ns).unwrap();
        assert!(report.is_ok(), "v{version} {ns}: {:#?}", report.problems);
    }
    let mut names: Vec<String> = read
        .namespaces()
        .unwrap()
        .into_iter()
        .map(|info| info.name)
        .collect();
    names.sort();
    let mut expected = vec!["fixture", "other"];
    if version >= 6 {
        expected.insert(0, "empty");
    }
    assert_eq!(names, expected, "v{version}");

    // Text, vectors, names, traversals and valid time all work.
    assert_eq!(read.search_text(NS, "topic3", 50).unwrap().len(), 8);
    let acme = read.search_text(NS, "engines", 1).unwrap()[0].node;
    assert_eq!(
        read.node(NS, acme).unwrap().unwrap().explicit_name(),
        Some("Acme")
    );
    let nearest = read.search_vector(NS, &[1.0, 0.0, 1.0], 1).unwrap()[0].node;
    assert_eq!(
        read.node(NS, nearest).unwrap().unwrap().explicit_name(),
        Some("Person 0")
    );
    let seven = read.find_by_name(NS, "person 7").unwrap();
    assert_eq!(seven.len(), 1, "v{version}: the names index");
    let at = |date: &str| {
        EdgeFilter::any()
            .label("works_at")
            .valid_at(Timestamp::parse(date).unwrap())
    };
    let in_2021 = read
        .neighbors(NS, acme, Direction::In, &at("2021-06-01"))
        .unwrap();
    let in_2023 = read
        .neighbors(NS, acme, Direction::In, &at("2023-06-01"))
        .unwrap();
    assert_eq!((in_2021.len(), in_2023.len()), (40, 20), "v{version}");
    let knows = read
        .k_hop(
            NS,
            seven[0],
            2,
            Direction::Both,
            &EdgeFilter::any().label("knows"),
        )
        .unwrap();
    assert_eq!(knows.len(), 4);

    let first = read.find_by_name(NS, "person 0").unwrap()[0];
    let node = read.node(NS, first).unwrap().unwrap();
    let importance = read.memory(NS, &node).unwrap().importance;
    if version >= 4 {
        assert!((importance - 0.9).abs() < 1e-6, "v{version}: {importance}");
    }
    if version >= 5 {
        let indexes = read.indexes(NS).unwrap();
        assert_eq!(indexes.len(), 1);
        assert_eq!(indexes[0].entries, 40);
    }
    if version >= 7 {
        // Principals became service accounts with a key each (layout 15).
        let accounts = read.accounts().unwrap();
        let expected = match version {
            15 => 2,
            14 => 3,
            _ => 1,
        };
        assert_eq!(accounts.len(), expected, "v{version}");
        let reader = read.account("reader").unwrap().expect("the reader");
        assert_eq!(reader.kind, grafeno::AccountKind::Service);
        assert!(
            reader.may("fixture", grafeno::Role::Read)
                && !reader.may("fixture", grafeno::Role::Write)
        );
        let key = read.api_key("reader").unwrap().expect("its key");
        assert_eq!(
            (key.account.as_str(), key.secret_hash.as_str()),
            ("reader", "00".repeat(32).as_str())
        );
        if version == 15 {
            let ana = read.account("ana").unwrap().unwrap();
            assert_eq!(ana.kind, grafeno::AccountKind::User);
            assert!(ana.own_only && ana.password_hash.is_some());
            assert!(
                read.api_key("ana-ro")
                    .unwrap()
                    .unwrap()
                    .scope
                    .unwrap()
                    .read_only
            );
            assert_eq!(read.sessions(Some("ana")).unwrap().len(), 1);
            assert!(
                read.roles().unwrap().is_empty(),
                "roles start empty (layout 16)"
            );
        } else if version >= 14 {
            assert!(read.account("admin-key").unwrap().unwrap().superuser);
            let agent = read.account("agent").unwrap().unwrap();
            assert!(agent.own_only && agent.may("fixture", grafeno::Role::Write));
            assert_eq!(read.api_keys().unwrap().len(), 3);
        }
        if version < 15 {
            assert!(read.sessions(None).unwrap().is_empty());
        }
    }
    assert!(read.merges(NS, None).unwrap().is_empty());
    if version >= 11 {
        assert_eq!(read.constraints(NS).unwrap().len(), 1, "v{version}");
    }
    // Before layout 12 the change log starts empty and fills from the first
    // commit after; a layout 12 file already has the fixture's writes.
    let last = read.changes(NS, 0, 10).unwrap().last;
    if version >= 12 {
        assert!(last > 0, "v{version}");
    } else {
        assert_eq!(last, 0, "v{version}");
    }
}

#[test]
fn every_past_layout_upgrades() {
    for version in 1..=15 {
        let path = fixture(version);
        {
            let db = Grafeno::open(&path).unwrap_or_else(|e| panic!("v{version}: {e}"));
            verify(&db, version);
            // It is a normal database now: it takes writes.
            db.write(|tx| tx.create_node(NS, NewNode::new("Note").text("after the upgrade")))
                .unwrap();
        }
        // Reopening upgrades nothing more and loses nothing.
        let db = Grafeno::open(&path).unwrap();
        let read = db.begin_read().unwrap();
        assert_eq!(read.stats(NS).unwrap().nodes, 42, "v{version}");
        // One more entry than the fixture wrote (none before layout 12).
        let logged = if version >= 12 { 121 } else { 1 };
        assert_eq!(read.changes(NS, 0, 10).unwrap().last, logged, "v{version}");
        assert!(read.check(NS).unwrap().is_ok());
        assert_eq!(read.search_text(NS, "upgrade", 5).unwrap().len(), 1);
        drop((read, db));
        std::fs::remove_file(&path).unwrap();
    }
}
