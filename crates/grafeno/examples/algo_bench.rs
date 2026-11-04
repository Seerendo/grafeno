//! Times graph algorithms on a random graph with communities.
//!
//! ```bash
//! cargo run --release -p grafeno --example algo_bench -- 100000 5
//! ```
//!
//! Arguments: nodes, and edges per node (default 100 000 and 5). Nodes fall
//! in communities of 100; 90% of edges stay inside one.

use std::time::Instant;

use grafeno::algo::{self, FastRp, Louvain, PageRank, ProjectionSpec};
use grafeno::{Direction, Grafeno, NewEdge, NewNode, NodeId};

const NS: &str = "bench";

fn main() -> grafeno::Result<()> {
    let mut args = std::env::args().skip(1);
    let nodes: usize = args.next().map_or(100_000, |n| n.parse().expect("nodes"));
    let per_node: usize = args
        .next()
        .map_or(5, |n| n.parse().expect("edges per node"));
    let db = Grafeno::in_memory()?;
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut random = move |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    let started = Instant::now();
    let ids: Vec<NodeId> = db.write(|tx| {
        (0..nodes)
            .map(|i| tx.create_node(NS, NewNode::new("Node").prop("i", i as i64)))
            .collect()
    })?;
    db.write(|tx| {
        for (i, &from) in ids.iter().enumerate() {
            for _ in 0..per_node {
                let to = if random(10) < 9 {
                    (i / 100) * 100 + random(100)
                } else {
                    random(nodes)
                };
                let to = ids[to.min(nodes - 1)];
                tx.create_edge(
                    NS,
                    NewEdge::new(from, "link", to).prop("w", 1 + random(9) as i64),
                )?;
            }
        }
        Ok(())
    })?;
    println!(
        "{nodes} nodes, {} edges: built in {:.1} s\n",
        nodes * per_node,
        started.elapsed().as_secs_f64()
    );
    println!("| Step | Time |\n| --- | --- |");
    let time =
        |name: &str, f: &mut dyn FnMut() -> grafeno::Result<String>| -> grafeno::Result<()> {
            let started = Instant::now();
            let note = f()?;
            println!(
                "| {name} | {:.0} ms {note} |",
                started.elapsed().as_secs_f64() * 1e3
            );
            Ok(())
        };
    let spec = ProjectionSpec::default();
    let weighted = ProjectionSpec {
        weight: Some("w".into()),
        ..ProjectionSpec::default()
    };
    let mut p = None;
    time("Projection", &mut || {
        p = Some(db.begin_read()?.projection(NS, &spec)?);
        Ok(String::new())
    })?;
    time("Projection again (cached)", &mut || {
        db.begin_read()?.projection(NS, &spec)?;
        Ok(String::new())
    })?;
    let mut w = None;
    time("Projection with weights", &mut || {
        w = Some(db.begin_read()?.projection(NS, &weighted)?);
        Ok(String::new())
    })?;
    let (p, w) = (p.expect("built"), w.expect("built"));
    let never = &algo::never;
    time("Dijkstra, one pair", &mut || {
        let path = algo::shortest_path(&w, 0, (nodes - 1) as u32, Direction::Out, never)?;
        Ok(format!("({} hops)", path.map_or(0, |p| p.edges.len())))
    })?;
    time("Yen, 5 paths", &mut || {
        let paths = algo::k_shortest_paths(&w, 0, (nodes - 1) as u32, 5, Direction::Out, never)?;
        Ok(format!("({} found)", paths.len()))
    })?;
    time("PageRank, 20 rounds", &mut || {
        algo::page_rank(&p, &PageRank::default(), never)?;
        Ok(String::new())
    })?;
    time("Weak components", &mut || {
        let groups = algo::weakly_connected(&p);
        Ok(format!("({})", groups.iter().max().map_or(0, |m| m + 1)))
    })?;
    time("Label propagation", &mut || {
        let (groups, _) = algo::label_propagation(&p, 10, 42, never)?;
        Ok(format!(
            "({} communities)",
            groups.iter().max().map_or(0, |m| m + 1)
        ))
    })?;
    time("Louvain", &mut || {
        let (groups, q, _) = algo::louvain(&p, &Louvain::default(), never)?;
        Ok(format!(
            "({} communities, modularity {q:.3})",
            groups.iter().max().map_or(0, |m| m + 1)
        ))
    })?;
    time("Triangles", &mut || {
        Ok(format!("({})", algo::triangles(&p, never)?.total))
    })?;
    time("Betweenness, 100 sampled sources", &mut || {
        algo::betweenness(&p, Direction::Both, Some(100), 42, never)?;
        Ok(String::new())
    })?;
    time("FastRP, 64 dimensions", &mut || {
        let settings = FastRp {
            dimension: 64,
            ..FastRp::default()
        };
        algo::fast_rp(&p, &settings, never)?;
        Ok(String::new())
    })?;
    time("Node similarity, top 10", &mut || {
        let pairs = algo::similar_neighbors(
            &p,
            algo::SimilarityMetric::Jaccard,
            Direction::Both,
            10,
            0.0,
            None,
            never,
        )?;
        Ok(format!("({} pairs)", pairs.len()))
    })?;
    Ok(())
}
