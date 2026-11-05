//! Community summaries on a graph with planted structure: teams of people
//! and notes, close inside, in departments of eight teams, with a little
//! noise across. Measures a full refresh, a refresh with nothing changed,
//! one after 1% new nodes, whether the summaries find the teams and the
//! departments, and global search over them.
//!
//! `cargo run --release -p grafeno-memory --example community_bench [teams] [team_size]`

use std::collections::HashMap;
use std::time::Instant;

use grafeno::{Direction, EdgeFilter, Grafeno, NewEdge, NewNode, NodeId};
use grafeno_memory::{
    COMMUNITY, CommunityOptions, MEMBER_OF, OverviewOptions, PART_OF, overview, refresh_communities,
};

const NS: &str = "org";

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// A made-up word for a team's topic: no two teams share one.
fn word(team: usize, k: usize) -> String {
    const SYLLABLES: [&str; 16] = [
        "ka", "lo", "mi", "nu", "pe", "ra", "si", "to", "ve", "zu", "ba", "do", "fe", "gi", "ho",
        "ju",
    ];
    let mut n = team * 4 + k + 4096;
    let mut out = String::new();
    while n > 0 {
        out.push_str(SYLLABLES[n % 16]);
        n /= 16;
    }
    out
}

fn main() {
    let args: Vec<usize> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let teams = args.first().copied().unwrap_or(400);
    let size = args.get(1).copied().unwrap_or(40);
    let db = Grafeno::in_memory().unwrap();
    let mut rng = Rng(7);
    let mut team_of: HashMap<NodeId, usize> = HashMap::new();
    let mut members: Vec<Vec<NodeId>> = vec![Vec::new(); teams];
    let started = Instant::now();
    let mut edges = 0usize;
    #[allow(clippy::needless_range_loop)]
    for team in 0..teams {
        db.write(|tx| {
            for k in 0..size {
                let id = if k % 2 == 0 {
                    tx.create_node(
                        NS,
                        NewNode::new("Person").prop("name", format!("p{team}_{k}")),
                    )?
                } else {
                    tx.create_node(
                        NS,
                        NewNode::new("Memory").text(format!(
                            "notes on {} and {} for the {} review",
                            word(team, 0),
                            word(team, 1),
                            word(team, rng.below(4))
                        )),
                    )?
                };
                members[team].push(id);
            }
            Ok(())
        })
        .unwrap();
        for id in &members[team] {
            team_of.insert(*id, team);
        }
    }
    let departments = teams.div_ceil(8);
    db.write(|tx| {
        for team in 0..teams {
            for &from in &members[team] {
                for _ in 0..3 {
                    let to = members[team][rng.below(size)];
                    if to != from {
                        tx.create_edge(NS, NewEdge::new(from, "works_with", to))?;
                        edges += 1;
                    }
                }
                // Now and then, someone in another team of the department;
                // rarely, anyone.
                if rng.below(4) == 0 {
                    let dept = team / 8;
                    let other = (dept * 8 + rng.below(8)).min(teams - 1);
                    let to = members[other][rng.below(size)];
                    tx.create_edge(NS, NewEdge::new(from, "knows", to))?;
                    edges += 1;
                }
                if rng.below(50) == 0 {
                    let to = members[rng.below(teams)][rng.below(size)];
                    tx.create_edge(NS, NewEdge::new(from, "knows", to))?;
                    edges += 1;
                }
            }
        }
        Ok(())
    })
    .unwrap();
    let nodes = teams * size;
    println!(
        "{nodes} nodes in {teams} teams of {size}, {departments} departments, {edges} edges (built in {:.1} s)",
        started.elapsed().as_secs_f64()
    );

    let options = CommunityOptions::default();
    let timed = |label: &str| {
        let started = Instant::now();
        let report = refresh_communities(&db, NS, &options).unwrap();
        let seconds = started.elapsed().as_secs_f64();
        println!(
            "| {label} | {seconds:.2} s | {} levels, {} summaries: {} created, {} updated, {} unchanged, {} removed |",
            report.levels,
            report.communities,
            report.created,
            report.updated,
            report.unchanged,
            report.removed
        );
        report
    };
    let first = timed("first refresh");
    timed("nothing changed");

    // 1% new people, each joining a team.
    let added = (nodes / 100).max(1);
    db.write(|tx| {
        for k in 0..added {
            let team = rng.below(teams);
            let id = tx.create_node(NS, NewNode::new("Person").prop("name", format!("new{k}")))?;
            for _ in 0..3 {
                tx.create_edge(
                    NS,
                    NewEdge::new(id, "works_with", members[team][rng.below(size)]),
                )?;
            }
            team_of.insert(id, team);
        }
        Ok(())
    })
    .unwrap();
    timed("after 1% new nodes");

    // Did the lowest summaries find the teams, and the level above the
    // departments? Purity: the share of nodes in their summary's majority.
    let read = db.begin_read().unwrap();
    let mut level_of: HashMap<NodeId, i64> = HashMap::new();
    for id in read.nodes_with_label(NS, COMMUNITY).unwrap() {
        let node = read.node(NS, id).unwrap().unwrap();
        level_of.insert(id, node.props["level"].as_i64().unwrap());
    }
    let mut by_level: HashMap<i64, usize> = HashMap::new();
    for level in level_of.values() {
        *by_level.entry(*level).or_default() += 1;
    }
    let mut levels: Vec<(i64, usize)> = by_level.into_iter().collect();
    levels.sort();
    println!("summaries per level: {levels:?}");
    let parent = |id: NodeId| {
        read.neighbors(NS, id, Direction::Out, &EdgeFilter::any().label(PART_OF))
            .unwrap()
            .first()
            .map(|n| n.node)
    };
    // Each node's summary at each level (its lowest, then the parents).
    let mut at_level: HashMap<i64, HashMap<NodeId, Vec<NodeId>>> = HashMap::new();
    for &node in team_of.keys() {
        let mut at = read
            .neighbors(
                NS,
                node,
                Direction::Out,
                &EdgeFilter::any().label(MEMBER_OF),
            )
            .unwrap()
            .first()
            .map(|n| n.node);
        while let Some(summary) = at {
            at_level
                .entry(level_of[&summary])
                .or_default()
                .entry(summary)
                .or_default()
                .push(node);
            at = parent(summary);
        }
    }
    let purity = |groups: &HashMap<NodeId, Vec<NodeId>>, truth: usize| {
        let (mut pure, mut total) = (0usize, 0usize);
        for nodes in groups.values() {
            let mut count: HashMap<usize, usize> = HashMap::new();
            for node in nodes {
                *count.entry(team_of[node] / truth).or_default() += 1;
            }
            pure += count.values().max().copied().unwrap_or(0);
            total += nodes.len();
        }
        (
            pure as f64 / total.max(1) as f64,
            total as f64 / team_of.len() as f64,
        )
    };
    for (level, _) in &levels {
        let groups = &at_level[level];
        let (teams_purity, coverage) = purity(groups, 1);
        let (departments_purity, _) = purity(groups, 8);
        println!(
            "| level {level} | {} summaries | purity against teams {teams_purity:.3}, against departments {departments_purity:.3} | coverage {coverage:.3} |",
            groups.len()
        );
    }

    // Global search: a team's topic word should find its summary first.
    let started = Instant::now();
    let (mut found, queries) = (0usize, 200.min(teams));
    for team in 0..queries {
        let hits = overview(
            &read,
            NS,
            Some(&word(team, 0)),
            &OverviewOptions {
                limit: 3,
                level: Some(0),
            },
        )
        .unwrap();
        if let Some(top) = hits.first() {
            let members = at_level[&0].get(&top.id).cloned().unwrap_or_default();
            let mine = members.iter().filter(|n| team_of[*n] == team).count();
            if mine * 2 > members.len() {
                found += 1;
            }
        }
    }
    println!(
        "global search: {found} of {queries} topics found their team's summary first, {:.2} ms a query",
        started.elapsed().as_secs_f64() * 1000.0 / queries as f64
    );
    let _ = first;
    assert!(read.check(NS).unwrap().is_ok());
}
