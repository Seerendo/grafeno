//! Graph algorithms in GQ: `CALL algo.<name>(…, {settings}) YIELD …`
//! procedures and the `algo.*` pair functions.
//!
//! Procedures run on a projection of the namespace (`grafeno::algo`),
//! which read queries share through a cache until the next commit. Every
//! procedure takes a map of settings as its last argument; these choose
//! the projection:
//!
//! - `labels`: nodes with these labels (a string or a list); every node
//!   by default.
//! - `edgeLabels`: edges with these labels; every edge by default.
//! - `weight`: the edge property with each edge's weight (1 without it).
//! - `direction`: `'out'`, `'in'` or `'both'`.
//! - `validAt`, `knownAt`: the time travel of `MATCH`.
//!
//! Results stream as rows, so `RETURN` aggregates them (stats) and `SET`
//! stores them (`CALL algo.pageRank() YIELD node, score SET node.rank =
//! score`).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use grafeno::algo::{
    self, FastRp, LinkMetric, Louvain, PageRank, Projection, ProjectionSpec, SimilarityMetric,
    WeightedPath,
};
use grafeno::{Direction, EdgeFilter, NodeId, Timestamp};

use crate::QueryError;
use crate::eval::{Context, to_time};
use crate::exec::uuid_from_text;
use crate::procedures::Procedure;
use crate::value::Value;

pub(crate) const ALGORITHMS: &[Procedure] = &[
    Procedure {
        name: "algo.shortestPath",
        args: "source, target, {weight, direction: 'out', …}",
        columns: &["nodes", "edges", "cost"],
        description: "The cheapest path (Dijkstra): the fewest hops without a weight.",
    },
    Procedure {
        name: "algo.astar",
        args: "source, target, {weight, latitude, longitude | x, y, …}",
        columns: &["nodes", "edges", "cost"],
        description: "The cheapest path with A*, guided by the straight-line distance between node coordinates (km for latitude/longitude).",
    },
    Procedure {
        name: "algo.kShortestPaths",
        args: "source, target, k, {weight, …}",
        columns: &["rank", "nodes", "edges", "cost"],
        description: "The k cheapest loopless paths (Yen), cheapest first.",
    },
    Procedure {
        name: "algo.allShortestPaths",
        args: "source, target, {weight, limit: 100, …}",
        columns: &["nodes", "edges", "cost"],
        description: "Every cheapest path between two nodes.",
    },
    Procedure {
        name: "algo.distances",
        args: "source, {weight, …}",
        columns: &["node", "cost"],
        description: "The cost of the cheapest path from source to every node it reaches, nearest first.",
    },
    Procedure {
        name: "algo.degree",
        args: "{direction: 'both', weight, …}",
        columns: &["node", "score"],
        description: "Degree centrality: edges per node (or the sum of their weights), highest first.",
    },
    Procedure {
        name: "algo.pageRank",
        args: "{damping: 0.85, iterations: 20, tolerance: 1e-7, sources, weight, …}",
        columns: &["node", "score"],
        description: "PageRank (personalized with sources), highest first; scores add up to 1.",
    },
    Procedure {
        name: "algo.betweenness",
        args: "{sample, seed, weight, direction: 'out', …}",
        columns: &["node", "score"],
        description: "Betweenness centrality (Brandes): shortest paths through each node, highest first. sample estimates it from that many sources.",
    },
    Procedure {
        name: "algo.closeness",
        args: "{harmonic: false, weight, direction: 'out', …}",
        columns: &["node", "score"],
        description: "Closeness centrality (Wasserman-Faust, or harmonic), highest first.",
    },
    Procedure {
        name: "algo.wcc",
        args: "{…}",
        columns: &["node", "component"],
        description: "Weakly connected components.",
    },
    Procedure {
        name: "algo.scc",
        args: "{…}",
        columns: &["node", "component"],
        description: "Strongly connected components (following edge directions).",
    },
    Procedure {
        name: "algo.labelPropagation",
        args: "{iterations: 10, seed, weight, …}",
        columns: &["node", "community"],
        description: "Communities by label propagation.",
    },
    Procedure {
        name: "algo.louvain",
        args: "{resolution: 1, levels: 10, iterations: 10, weight, …}",
        columns: &["node", "community", "modularity"],
        description: "Communities by modularity (Louvain).",
    },
    Procedure {
        name: "algo.triangles",
        args: "{…}",
        columns: &["node", "triangles", "coefficient"],
        description: "Triangles per node and the local clustering coefficient.",
    },
    Procedure {
        name: "algo.nodeSimilarity",
        args: "{metric: 'jaccard', topK: 10, cutoff: 0, node, direction: 'out', …}",
        columns: &["node", "other", "score"],
        description: "Most similar nodes by shared neighbors (jaccard, overlap or cosine), best first per node.",
    },
    Procedure {
        name: "algo.knn",
        args: "{topK: 10, cutoff: 0, node, labels}",
        columns: &["node", "other", "score"],
        description: "Nearest nodes by embedding (cosine), from the vector index.",
    },
    Procedure {
        name: "algo.linkPrediction",
        args: "node, {metric: 'adamicAdar', limit: 10, …}",
        columns: &["other", "score"],
        description: "Likely new edges of a node: nodes two hops away, scored by commonNeighbors, adamicAdar, resourceAllocation, preferentialAttachment, totalNeighbors or jaccard.",
    },
    Procedure {
        name: "algo.fastRP",
        args: "{dimension: 128, iterationWeights: [0, 1, 1], selfInfluence: 0, normalizationStrength: 0, seed: 42, …}",
        columns: &["node", "embedding"],
        description: "Structural embeddings (FastRP); SET n.@embedding = embedding stores them.",
    },
];

// ---------------------------------------------------------------- settings

/// The settings map of a procedure, checked for unknown keys.
struct Settings {
    procedure: &'static str,
    map: BTreeMap<String, Value>,
    used: RefCell<BTreeSet<&'static str>>,
}

impl Settings {
    fn new(procedure: &'static str, value: Option<&Value>) -> Result<Self, QueryError> {
        let map = match value {
            None | Some(Value::Null) => BTreeMap::new(),
            Some(Value::Map(map)) => map.clone(),
            Some(other) => {
                return Err(QueryError::runtime(format!(
                    "{procedure}() takes its settings as a map ({{key: value}}), not a {}",
                    other.type_name()
                )));
            }
        };
        Ok(Self {
            procedure,
            map,
            used: RefCell::default(),
        })
    }

    fn get(&self, key: &'static str) -> Option<&Value> {
        self.used.borrow_mut().insert(key);
        self.map.get(key).filter(|value| !value.is_null())
    }

    fn fail(&self, key: &str, wanted: &str, got: &Value) -> QueryError {
        QueryError::runtime(format!(
            "{}() needs {key} to be {wanted}, not {}",
            self.procedure,
            got.to_json()
        ))
    }

    fn float(&self, key: &'static str, default: f64) -> Result<f64, QueryError> {
        match self.get(key) {
            None => Ok(default),
            Some(Value::Int(n)) => Ok(*n as f64),
            Some(Value::Float(f)) if f.is_finite() => Ok(*f),
            Some(other) => Err(self.fail(key, "a number", other)),
        }
    }

    fn count(&self, key: &'static str, default: usize) -> Result<usize, QueryError> {
        match self.get(key) {
            None => Ok(default),
            Some(Value::Int(n)) if *n >= 0 => Ok(*n as usize),
            Some(other) => Err(self.fail(key, "a whole number, 0 or more", other)),
        }
    }

    fn optional_count(&self, key: &'static str) -> Result<Option<usize>, QueryError> {
        Ok(match self.get(key) {
            None => None,
            Some(_) => Some(self.count(key, 0)?),
        })
    }

    fn flag(&self, key: &'static str, default: bool) -> Result<bool, QueryError> {
        match self.get(key) {
            None => Ok(default),
            Some(Value::Bool(b)) => Ok(*b),
            Some(other) => Err(self.fail(key, "true or false", other)),
        }
    }

    fn text(&self, key: &'static str) -> Result<Option<String>, QueryError> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Str(s)) => Ok(Some(s.clone())),
            Some(other) => Err(self.fail(key, "a string", other)),
        }
    }

    fn texts(&self, key: &'static str) -> Result<Vec<String>, QueryError> {
        match self.get(key) {
            None => Ok(Vec::new()),
            Some(Value::Str(s)) => Ok(vec![s.clone()]),
            Some(Value::List(items)) => items
                .iter()
                .map(|item| match item {
                    Value::Str(s) => Ok(s.clone()),
                    other => Err(self.fail(key, "a string or a list of strings", other)),
                })
                .collect(),
            Some(other) => Err(self.fail(key, "a string or a list of strings", other)),
        }
    }

    fn time(&self, key: &'static str) -> Result<Option<Timestamp>, QueryError> {
        self.get(key).map(to_time).transpose()
    }

    fn seed(&self) -> Result<u64, QueryError> {
        match self.get("seed") {
            None => Ok(42),
            Some(Value::Int(n)) => Ok(*n as u64),
            Some(other) => Err(self.fail("seed", "an integer", other)),
        }
    }

    fn direction(&self, default: Direction) -> Result<Direction, QueryError> {
        match self
            .text("direction")?
            .as_deref()
            .map(str::to_ascii_lowercase)
        {
            None => Ok(default),
            Some(d) if d == "out" || d == "outgoing" => Ok(Direction::Out),
            Some(d) if d == "in" || d == "incoming" => Ok(Direction::In),
            Some(d) if d == "both" || d == "undirected" => Ok(Direction::Both),
            Some(d) => Err(self.fail("direction", "'out', 'in' or 'both'", &Value::Str(d))),
        }
    }

    fn spec(&self) -> Result<ProjectionSpec, QueryError> {
        Ok(ProjectionSpec {
            labels: self.texts("labels")?,
            edge_labels: self.texts("edgeLabels")?,
            valid_at: self.time("validAt")?,
            known_at: self.time("knownAt")?,
            weight: self.text("weight")?,
        })
    }

    /// Fails on keys no setting read: a typo would otherwise be ignored.
    fn finish(&self) -> Result<(), QueryError> {
        let used = self.used.borrow();
        if let Some(unknown) = self.map.keys().find(|key| !used.contains(key.as_str())) {
            return Err(QueryError::runtime(format!(
                "{}() has no setting {unknown} (it takes {})",
                self.procedure,
                used.iter().copied().collect::<Vec<_>>().join(", ")
            )));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- helpers

fn node_id(procedure: &str, value: &Value) -> Result<NodeId, QueryError> {
    match value {
        Value::Node(node) => Ok(node.id),
        Value::Str(text) => uuid_from_text(text).ok_or_else(|| {
            QueryError::runtime(format!("{procedure}(): {text:?} is not a node id"))
        }),
        other => Err(QueryError::runtime(format!(
            "{procedure}() needs a node (or its id), not a {}",
            other.type_name()
        ))),
    }
}

fn position(procedure: &str, p: &Projection, id: NodeId) -> Result<u32, QueryError> {
    p.position(id).ok_or_else(|| {
        QueryError::runtime(format!(
            "{procedure}(): node {id} is not in the projection (check labels)"
        ))
    })
}

fn node_value(ctx: &Context, p: &Projection, at: u32) -> Result<Value, QueryError> {
    Ok(ctx
        .node(p.id(at))?
        .map_or(Value::Null, |node| Value::Node(Arc::new(node))))
}

fn path_row(ctx: &Context, p: &Projection, path: &WeightedPath) -> Result<Vec<Value>, QueryError> {
    let nodes = path
        .nodes
        .iter()
        .map(|&at| node_value(ctx, p, at))
        .collect::<Result<Vec<_>, _>>()?;
    let mut edges = Vec::with_capacity(path.edges.len());
    for &id in &path.edges {
        edges.push(
            ctx.edge(id)?
                .map_or(Value::Null, |edge| Value::Edge(Arc::new(edge))),
        );
    }
    Ok(vec![
        Value::List(nodes),
        Value::List(edges),
        Value::Float(path.cost),
    ])
}

/// One row per node with its score, highest first.
fn ranked(ctx: &Context, p: &Projection, scores: &[f64]) -> Result<Vec<Vec<Value>>, QueryError> {
    let mut order: Vec<u32> = (0..p.len() as u32).collect();
    order.sort_by(|a, b| {
        scores[*b as usize]
            .total_cmp(&scores[*a as usize])
            .then(a.cmp(b))
    });
    order
        .into_iter()
        .map(|at| {
            Ok(vec![
                node_value(ctx, p, at)?,
                Value::Float(scores[at as usize]),
            ])
        })
        .collect()
}

/// One row per node with its group, in node order.
fn grouped(ctx: &Context, p: &Projection, groups: &[u32]) -> Result<Vec<Vec<Value>>, QueryError> {
    (0..p.len() as u32)
        .map(|at| {
            Ok(vec![
                node_value(ctx, p, at)?,
                Value::Int(i64::from(groups[at as usize])),
            ])
        })
        .collect()
}

fn haversine_km((lat1, lon1): (f64, f64), (lat2, lon2): (f64, f64)) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6371.0088 * a.sqrt().asin()
}

// ---------------------------------------------------------------- running

/// Runs `algo.<name>`: one row per result, in the order of its columns.
pub(crate) fn call(
    ctx: &Context,
    procedure: &'static Procedure,
    args: &[Value],
) -> Result<Vec<Vec<Value>>, QueryError> {
    let name = procedure.name;
    // Positional arguments before the settings map.
    let positional = match name {
        "algo.shortestPath" | "algo.astar" | "algo.allShortestPaths" => 2,
        "algo.kShortestPaths" => 3,
        "algo.distances" | "algo.linkPrediction" => 1,
        _ => 0,
    };
    if args.len() < positional || args.len() > positional + 1 {
        return Err(QueryError::runtime(format!(
            "{name}() takes {}",
            procedure.args
        )));
    }
    let settings = Settings::new(name, args.get(positional))?;
    let spec = settings.spec()?;
    let stop = ctx.budget.stopper();
    let stopped = |error: grafeno::Error| -> QueryError {
        if stop() {
            ctx.budget
                .tick()
                .err()
                .unwrap_or_else(|| QueryError::limit("the query was stopped"))
        } else {
            error.into()
        }
    };
    let project = |spec: &ProjectionSpec| -> Result<Arc<Projection>, QueryError> {
        let p = match ctx.write {
            Some(tx) => tx.projection(ctx.ns, spec)?,
            None => ctx.read.projection(ctx.ns, spec)?,
        };
        ctx.budget.tick()?;
        Ok(p)
    };
    let endpoints = |p: &Projection| -> Result<(u32, u32), QueryError> {
        let from = position(name, p, node_id(name, &args[0])?)?;
        let to = position(name, p, node_id(name, &args[1])?)?;
        Ok((from, to))
    };
    Ok(match name {
        "algo.shortestPath" | "algo.astar" => {
            let direction = settings.direction(Direction::Out)?;
            let coordinates = if name == "algo.astar" {
                match (
                    settings.text("latitude")?,
                    settings.text("longitude")?,
                    settings.text("x")?,
                    settings.text("y")?,
                ) {
                    (Some(lat), Some(lon), None, None) => Some((lat, lon, true)),
                    (None, None, Some(x), Some(y)) => Some((x, y, false)),
                    _ => {
                        return Err(QueryError::runtime(
                            "algo.astar() needs the coordinate properties: latitude and longitude, or x and y",
                        ));
                    }
                }
            } else {
                None
            };
            settings.finish()?;
            let p = project(&spec)?;
            let (from, to) = endpoints(&p)?;
            let path = match coordinates {
                None => algo::shortest_path(&p, from, to, direction, &stop).map_err(stopped)?,
                Some((a, b, spherical)) => {
                    let cache: RefCell<HashMap<u32, Option<(f64, f64)>>> = RefCell::default();
                    let point = |at: u32| -> Option<(f64, f64)> {
                        if let Some(known) = cache.borrow().get(&at) {
                            return *known;
                        }
                        let found = ctx.node(p.id(at)).ok().flatten().and_then(|node| {
                            Some((node.props.get(&a)?.as_f64()?, node.props.get(&b)?.as_f64()?))
                        });
                        cache.borrow_mut().insert(at, found);
                        found
                    };
                    let goal = point(to);
                    let estimate = |at: u32| match (point(at), goal) {
                        (Some(here), Some(there)) if spherical => haversine_km(here, there),
                        (Some((x1, y1)), Some((x2, y2))) => (x1 - x2).hypot(y1 - y2),
                        _ => 0.0,
                    };
                    algo::astar(&p, from, to, direction, &estimate, &stop).map_err(stopped)?
                }
            };
            match path {
                Some(path) => vec![path_row(ctx, &p, &path)?],
                None => Vec::new(),
            }
        }
        "algo.kShortestPaths" => {
            let k = match &args[2] {
                Value::Int(k) if *k > 0 => *k as usize,
                other => {
                    return Err(QueryError::runtime(format!(
                        "algo.kShortestPaths() needs k, a positive integer, not {}",
                        other.to_json()
                    )));
                }
            };
            let direction = settings.direction(Direction::Out)?;
            settings.finish()?;
            let p = project(&spec)?;
            let (from, to) = endpoints(&p)?;
            let paths =
                algo::k_shortest_paths(&p, from, to, k, direction, &stop).map_err(stopped)?;
            let mut rows = Vec::new();
            for (rank, path) in paths.iter().enumerate() {
                let mut row = vec![Value::Int(rank as i64 + 1)];
                row.extend(path_row(ctx, &p, path)?);
                rows.push(row);
            }
            rows
        }
        "algo.allShortestPaths" => {
            let direction = settings.direction(Direction::Out)?;
            let limit = settings.count("limit", 100)?;
            settings.finish()?;
            let p = project(&spec)?;
            let (from, to) = endpoints(&p)?;
            algo::all_shortest_paths(&p, from, to, direction, limit, &stop)
                .map_err(stopped)?
                .iter()
                .map(|path| path_row(ctx, &p, path))
                .collect::<Result<_, _>>()?
        }
        "algo.distances" => {
            let direction = settings.direction(Direction::Out)?;
            settings.finish()?;
            let p = project(&spec)?;
            let from = position(name, &p, node_id(name, &args[0])?)?;
            let costs = algo::distances(&p, from, direction, &stop).map_err(stopped)?;
            let mut order: Vec<u32> = (0..p.len() as u32)
                .filter(|at| costs[*at as usize].is_finite())
                .collect();
            order.sort_by(|a, b| {
                costs[*a as usize]
                    .total_cmp(&costs[*b as usize])
                    .then(a.cmp(b))
            });
            order
                .into_iter()
                .map(|at| {
                    Ok(vec![
                        node_value(ctx, &p, at)?,
                        Value::Float(costs[at as usize]),
                    ])
                })
                .collect::<Result<_, QueryError>>()?
        }
        "algo.degree" => {
            let direction = settings.direction(Direction::Both)?;
            settings.finish()?;
            let p = project(&spec)?;
            ranked(ctx, &p, &algo::degree(&p, direction, p.is_weighted()))?
        }
        "algo.pageRank" => {
            let defaults = PageRank::default();
            let direction = settings.direction(Direction::Out)?;
            let damping = settings.float("damping", defaults.damping)?;
            if !(0.0..1.0).contains(&damping) {
                return Err(QueryError::runtime(
                    "algo.pageRank() needs damping from 0 to less than 1",
                ));
            }
            let max_iterations = settings.count("iterations", defaults.max_iterations)?;
            let tolerance = settings.float("tolerance", defaults.tolerance)?;
            let sources = match settings.get("sources") {
                None => Vec::new(),
                Some(Value::List(items)) => items.clone(),
                Some(single) => vec![single.clone()],
            };
            settings.finish()?;
            let p = project(&spec)?;
            let sources = sources
                .iter()
                .map(|value| position(name, &p, node_id(name, value)?))
                .collect::<Result<Vec<_>, _>>()?;
            let page_rank = PageRank {
                damping,
                max_iterations,
                tolerance,
                direction,
                sources,
            };
            let (scores, _) = algo::page_rank(&p, &page_rank, &stop).map_err(stopped)?;
            ranked(ctx, &p, &scores)?
        }
        "algo.betweenness" => {
            let direction = settings.direction(Direction::Out)?;
            let sample = settings.optional_count("sample")?;
            let seed = settings.seed()?;
            settings.finish()?;
            let p = project(&spec)?;
            let scores = algo::betweenness(&p, direction, sample, seed, &stop).map_err(stopped)?;
            ranked(ctx, &p, &scores)?
        }
        "algo.closeness" => {
            let direction = settings.direction(Direction::Out)?;
            let harmonic = settings.flag("harmonic", false)?;
            settings.finish()?;
            let p = project(&spec)?;
            let scores = algo::closeness(&p, direction, harmonic, &stop).map_err(stopped)?;
            ranked(ctx, &p, &scores)?
        }
        "algo.wcc" | "algo.scc" => {
            settings.finish()?;
            let p = project(&spec)?;
            let groups = if name == "algo.wcc" {
                algo::weakly_connected(&p)
            } else {
                algo::strongly_connected(&p)
            };
            grouped(ctx, &p, &groups)?
        }
        "algo.labelPropagation" => {
            let iterations = settings.count("iterations", 10)?;
            let seed = settings.seed()?;
            settings.finish()?;
            let p = project(&spec)?;
            let (groups, _) =
                algo::label_propagation(&p, iterations, seed, &stop).map_err(stopped)?;
            grouped(ctx, &p, &groups)?
        }
        "algo.louvain" => {
            let defaults = Louvain::default();
            let louvain = Louvain {
                resolution: settings.float("resolution", defaults.resolution)?,
                max_levels: settings.count("levels", defaults.max_levels)?,
                max_iterations: settings.count("iterations", defaults.max_iterations)?,
                tolerance: settings.float("tolerance", defaults.tolerance)?,
            };
            settings.finish()?;
            let p = project(&spec)?;
            let (groups, modularity, _) = algo::louvain(&p, &louvain, &stop).map_err(stopped)?;
            let mut rows = grouped(ctx, &p, &groups)?;
            for row in &mut rows {
                row.push(Value::Float(modularity));
            }
            rows
        }
        "algo.triangles" => {
            settings.finish()?;
            let p = project(&spec)?;
            let found = algo::triangles(&p, &stop).map_err(stopped)?;
            (0..p.len() as u32)
                .map(|at| {
                    Ok(vec![
                        node_value(ctx, &p, at)?,
                        Value::Int(found.per_node[at as usize] as i64),
                        Value::Float(found.coefficient[at as usize]),
                    ])
                })
                .collect::<Result<_, QueryError>>()?
        }
        "algo.nodeSimilarity" => {
            let metric =
                SimilarityMetric::parse(settings.text("metric")?.as_deref().unwrap_or("jaccard"))?;
            let direction = settings.direction(Direction::Out)?;
            let top_k = settings.count("topK", 10)?;
            let cutoff = settings.float("cutoff", 0.0)?;
            let node = settings.get("node").cloned();
            settings.finish()?;
            let p = project(&spec)?;
            let source = node
                .map(|value| position(name, &p, node_id(name, &value)?))
                .transpose()?;
            let pairs =
                algo::similar_neighbors(&p, metric, direction, top_k, cutoff, source, &stop)
                    .map_err(stopped)?;
            pairs
                .into_iter()
                .map(|(a, b, score)| {
                    Ok(vec![
                        node_value(ctx, &p, a)?,
                        node_value(ctx, &p, b)?,
                        Value::Float(score),
                    ])
                })
                .collect::<Result<_, QueryError>>()?
        }
        "algo.knn" => {
            let top_k = settings.count("topK", 10)?;
            let cutoff = settings.float("cutoff", 0.0)?;
            let node = settings.get("node").cloned();
            settings.finish()?;
            // Only the nodes: no edge has an empty label.
            let p = project(&ProjectionSpec {
                labels: spec.labels.clone(),
                edge_labels: vec![String::new()],
                ..ProjectionSpec::default()
            })?;
            let sources: Vec<u32> = match node {
                Some(value) => vec![position(name, &p, node_id(name, &value)?)?],
                None => (0..p.len() as u32).collect(),
            };
            // Extra candidates make up for hits outside the labels.
            let ask = if spec.labels.is_empty() {
                top_k + 1
            } else {
                (top_k + 1) * 4
            };
            let mut rows = Vec::new();
            for (i, at) in sources.into_iter().enumerate() {
                if i % 64 == 0 {
                    ctx.budget.tick()?;
                }
                let id = p.id(at);
                let Some(embedding) = ctx.embedding(id)? else {
                    continue;
                };
                let hits = ctx.read.search_vector(ctx.ns, &embedding, ask)?;
                let mut kept = 0;
                for hit in hits {
                    let score = f64::from(hit.score);
                    if hit.node == id || score < cutoff || kept == top_k {
                        continue;
                    }
                    let Some(other) = p.position(hit.node) else {
                        continue;
                    };
                    kept += 1;
                    rows.push(vec![
                        node_value(ctx, &p, at)?,
                        node_value(ctx, &p, other)?,
                        Value::Float(score),
                    ]);
                }
            }
            rows
        }
        "algo.linkPrediction" => {
            let metric =
                LinkMetric::parse(settings.text("metric")?.as_deref().unwrap_or("adamicAdar"))?;
            let limit = settings.count("limit", 10)?;
            settings.finish()?;
            let p = project(&spec)?;
            let node = position(name, &p, node_id(name, &args[0])?)?;
            algo::predict_links(&p, node, metric, limit, &stop)
                .map_err(stopped)?
                .into_iter()
                .map(|(other, score)| Ok(vec![node_value(ctx, &p, other)?, Value::Float(score)]))
                .collect::<Result<_, QueryError>>()?
        }
        "algo.fastRP" => {
            let defaults = FastRp::default();
            let iteration_weights = match settings.get("iterationWeights") {
                None => defaults.iteration_weights.clone(),
                Some(Value::List(items)) => items
                    .iter()
                    .map(|item| match item {
                        Value::Int(n) => Ok(*n as f64),
                        Value::Float(f) => Ok(*f),
                        other => Err(settings.fail("iterationWeights", "a list of numbers", other)),
                    })
                    .collect::<Result<_, _>>()?,
                Some(other) => {
                    return Err(settings.fail("iterationWeights", "a list of numbers", other));
                }
            };
            let fast_rp = FastRp {
                dimension: settings.count("dimension", defaults.dimension)?,
                iteration_weights,
                self_influence: settings.float("selfInfluence", defaults.self_influence)?,
                normalization_strength: settings
                    .float("normalizationStrength", defaults.normalization_strength)?,
                seed: settings.seed()?,
                direction: settings.direction(Direction::Both)?,
            };
            settings.finish()?;
            let p = project(&spec)?;
            let vectors = algo::fast_rp(&p, &fast_rp, &stop).map_err(stopped)?;
            vectors
                .into_iter()
                .enumerate()
                .map(|(at, vector)| {
                    Ok(vec![
                        node_value(ctx, &p, at as u32)?,
                        Value::List(
                            vector
                                .into_iter()
                                .map(|x| Value::Float(f64::from(x)))
                                .collect(),
                        ),
                    ])
                })
                .collect::<Result<_, QueryError>>()?
        }
        other => unreachable!("algorithm {other} without a body"),
    })
}

// ---------------------------------------------------------------- functions

/// The distinct neighbors of a node in both directions, sorted.
fn neighbor_set(ctx: &Context, id: NodeId) -> Result<Vec<NodeId>, QueryError> {
    let mut set: Vec<NodeId> = ctx
        .neighbor_ids(id, Direction::Both, &EdgeFilter::any())?
        .into_iter()
        .map(|(_, other)| other)
        .filter(|other| *other != id)
        .collect();
    set.sort_unstable();
    set.dedup();
    Ok(set)
}

/// The `algo.*` functions, by lowercase name, or `None` when `name` is
/// not one of them.
pub(crate) fn function(
    ctx: &Context,
    name: &str,
    args: &[Value],
) -> Option<Result<Value, QueryError>> {
    let link = match name {
        "algo.commonneighbors" => Some(LinkMetric::CommonNeighbors),
        "algo.adamicadar" => Some(LinkMetric::AdamicAdar),
        "algo.resourceallocation" => Some(LinkMetric::ResourceAllocation),
        "algo.preferentialattachment" => Some(LinkMetric::PreferentialAttachment),
        "algo.totalneighbors" => Some(LinkMetric::TotalNeighbors),
        _ => None,
    };
    let similarity = match name {
        "algo.jaccard" => Some(SimilarityMetric::Jaccard),
        "algo.overlap" => Some(SimilarityMetric::Overlap),
        _ => None,
    };
    if link.is_none() && similarity.is_none() && name != "algo.cosine" {
        return None;
    }
    Some((|| {
        let [a, b] = args else {
            return Err(QueryError::runtime(format!("{name}() takes two arguments")));
        };
        if name == "algo.cosine" {
            let vector = |value: &Value| -> Result<Vec<f64>, QueryError> {
                let Value::List(items) = value else {
                    return Err(QueryError::runtime(
                        "algo.cosine() compares two lists of numbers",
                    ));
                };
                items
                    .iter()
                    .map(|item| match item {
                        Value::Int(n) => Ok(*n as f64),
                        Value::Float(f) => Ok(*f),
                        _ => Err(QueryError::runtime(
                            "algo.cosine() compares two lists of numbers",
                        )),
                    })
                    .collect()
            };
            let (x, y) = (vector(a)?, vector(b)?);
            if x.len() != y.len() {
                return Err(QueryError::runtime(format!(
                    "algo.cosine() needs lists of the same length, got {} and {}",
                    x.len(),
                    y.len()
                )));
            }
            let dot: f64 = x.iter().zip(&y).map(|(p, q)| p * q).sum();
            let norm = x.iter().map(|p| p * p).sum::<f64>().sqrt()
                * y.iter().map(|q| q * q).sum::<f64>().sqrt();
            return Ok(if norm > 0.0 {
                Value::Float(dot / norm)
            } else {
                Value::Null
            });
        }
        let (a, b) = (
            neighbor_set(ctx, node_id(name, a)?)?,
            neighbor_set(ctx, node_id(name, b)?)?,
        );
        Ok(Value::Float(match (link, similarity) {
            (Some(metric), _) => {
                let mut degrees = HashMap::new();
                let needs_degrees = matches!(
                    metric,
                    LinkMetric::AdamicAdar | LinkMetric::ResourceAllocation
                );
                if needs_degrees {
                    for w in a.iter().filter(|w| b.binary_search(w).is_ok()) {
                        degrees.insert(*w, neighbor_set(ctx, *w)?.len());
                    }
                }
                algo::link_score(&a, &b, metric, |w| degrees.get(&w).copied().unwrap_or(0))
            }
            (None, Some(metric)) => algo::neighbor_similarity(&a, &b, metric),
            (None, None) => unreachable!("checked above"),
        }))
    })())
}
