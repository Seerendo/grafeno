//! Query execution: matching patterns, writes and the final projection.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use grafeno::{
    Direction, EdgeFilter, EdgeId, Grafeno, NewEdge, NewNode, Node, NodeId, Props, ReadTx, WriteTx,
};
use serde::Serialize;

use crate::QueryError;
use crate::ast::{
    Clause, Expr, Key, NodePattern, Pattern, Query, RelPattern, RemoveItem, Return, SetItem,
};
use crate::eval::{Context, Params, Row, Scope, eval, property, to_time, truth};
use crate::parser::parse;
use crate::value::{Value, equals, sort_order};

/// Variable-length paths (`*`) stop at this many hops.
pub const MAX_PATH_LENGTH: u32 = 32;

/// What a writing query changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct WriteStats {
    pub nodes_created: u64,
    pub nodes_deleted: u64,
    pub edges_created: u64,
    pub edges_deleted: u64,
    pub edges_retracted: u64,
    pub properties_set: u64,
    pub labels_added: u64,
    pub labels_removed: u64,
}

impl WriteStats {
    pub fn is_empty(&self) -> bool {
        *self == WriteStats::default()
    }
}

/// The result of a query: named columns, rows of values, and what changed.
#[derive(Clone, Debug)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub stats: WriteStats,
}

impl QueryResult {
    /// `{"columns": […], "rows": [[…]], "stats": {…}}`
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "columns": self.columns,
            "rows": self
                .rows
                .iter()
                .map(|row| row.iter().map(Value::to_json).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            "stats": self.stats,
        })
    }
}

/// Runs one statement on namespace `ns`. A statement with writes runs in
/// one write transaction: all of its changes or none.
pub fn execute(
    db: &Grafeno,
    ns: &str,
    query: &str,
    params: &Params,
) -> Result<QueryResult, QueryError> {
    let query = parse(query)?;
    validate(&query)?;
    if query.clauses.iter().any(Clause::writes) {
        // Holding the write lock first makes the snapshot the latest state.
        let mut tx = db.begin_write()?;
        let read = db.begin_read()?;
        match run(&query, &read, Some(&mut tx), ns, params) {
            Ok(result) => {
                tx.commit()?;
                Ok(result)
            }
            Err(error) => {
                tx.abort()?;
                Err(error)
            }
        }
    } else {
        run(&query, &db.begin_read()?, None, ns, params)
    }
}

/// Runs a read-only statement on an existing snapshot.
pub fn execute_read(
    read: &ReadTx,
    ns: &str,
    query: &str,
    params: &Params,
) -> Result<QueryResult, QueryError> {
    let query = parse(query)?;
    validate(&query)?;
    if query.clauses.iter().any(Clause::writes) {
        return Err(QueryError::runtime(
            "this query writes; it needs a writable database",
        ));
    }
    run(&query, read, None, ns, params)
}

/// Whether the statement writes (without running it).
pub fn is_write(query: &str) -> Result<bool, QueryError> {
    Ok(parse(query)?.clauses.iter().any(Clause::writes))
}

fn validate(query: &Query) -> Result<(), QueryError> {
    let mut wrote = false;
    for (index, clause) in query.clauses.iter().enumerate() {
        match clause {
            Clause::Match { .. } if wrote => {
                return Err(QueryError::runtime(
                    "MATCH after a write clause is not supported: match first, then write",
                ));
            }
            Clause::Return(_) if index + 1 != query.clauses.len() => {
                return Err(QueryError::runtime("RETURN must be the last clause"));
            }
            clause if clause.writes() => wrote = true,
            _ => {}
        }
    }
    let returns = matches!(query.clauses.last(), Some(Clause::Return(_)));
    if !returns && !wrote {
        return Err(QueryError::runtime(
            "the query needs RETURN, or a write clause (CREATE, SET, REMOVE, DELETE, RETRACT)",
        ));
    }
    Ok(())
}

fn run(
    query: &Query,
    read: &ReadTx,
    mut tx: Option<&mut WriteTx>,
    ns: &str,
    params: &Params,
) -> Result<QueryResult, QueryError> {
    let mut rows: Vec<Row> = vec![Row::new()];
    let mut variables: Vec<String> = Vec::new();
    let mut stats = WriteStats::default();
    let mut deleted_nodes = HashSet::new();
    let mut deleted_edges = HashSet::new();

    for clause in &query.clauses {
        match clause {
            Clause::Match {
                patterns,
                valid_at,
                known_at,
                filter,
            } => {
                let ctx = Context {
                    read,
                    write: None,
                    ns,
                    params,
                };
                let empty = Row::new();
                let mut base = EdgeFilter::any();
                if let Some(at) = valid_at {
                    base = base.valid_at(to_time(&eval(&ctx, at, Scope::Row(&empty))?)?);
                }
                if let Some(at) = known_at {
                    base = base.known_at(to_time(&eval(&ctx, at, Scope::Row(&empty))?)?);
                }
                for pattern in patterns {
                    remember_variables(pattern, &mut variables);
                    rows = match_pattern(&ctx, rows, pattern, &base)?;
                }
                if let Some(filter) = filter {
                    let mut kept = Vec::with_capacity(rows.len());
                    for row in rows {
                        if truth(&eval(&ctx, filter, Scope::Row(&row))?)? == Some(true) {
                            kept.push(row);
                        }
                    }
                    rows = kept;
                }
            }
            Clause::Return(ret) => {
                let ctx = Context {
                    read,
                    write: tx.as_deref(),
                    ns,
                    params,
                };
                let (columns, rows) = project(&ctx, rows, ret, &variables)?;
                return Ok(QueryResult {
                    columns,
                    rows,
                    stats,
                });
            }
            write => {
                let tx = tx
                    .as_deref_mut()
                    .expect("writing queries run with a transaction");
                for row in &mut rows {
                    match write {
                        Clause::Create(patterns) => {
                            for pattern in patterns {
                                remember_variables(pattern, &mut variables);
                                create(read, tx, ns, params, row, pattern, &mut stats)?;
                            }
                        }
                        Clause::Set(items) => {
                            for item in items {
                                set(read, tx, ns, params, row, item, &mut stats)?;
                            }
                        }
                        Clause::Remove(items) => {
                            for item in items {
                                remove(tx, ns, row, item, &mut stats)?;
                            }
                        }
                        Clause::Delete(exprs) => {
                            for expr in exprs {
                                let value = {
                                    let ctx = Context {
                                        read,
                                        write: Some(&*tx),
                                        ns,
                                        params,
                                    };
                                    eval(&ctx, expr, Scope::Row(row))?
                                };
                                match value {
                                    Value::Null => {}
                                    Value::Node(node) => {
                                        if deleted_nodes.insert(node.id)
                                            && tx.node(ns, node.id)?.is_some()
                                        {
                                            tx.delete_node(ns, node.id)?;
                                            stats.nodes_deleted += 1;
                                        }
                                    }
                                    Value::Edge(edge) => {
                                        if deleted_edges.insert(edge.id)
                                            && tx.edge(ns, edge.id)?.is_some()
                                        {
                                            tx.delete_edge(ns, edge.id)?;
                                            stats.edges_deleted += 1;
                                        }
                                    }
                                    other => {
                                        return Err(QueryError::runtime(format!(
                                            "DELETE needs nodes or edges, got a {}",
                                            other.type_name()
                                        )));
                                    }
                                }
                            }
                        }
                        Clause::Retract(exprs) => {
                            for expr in exprs {
                                let value = {
                                    let ctx = Context {
                                        read,
                                        write: Some(&*tx),
                                        ns,
                                        params,
                                    };
                                    eval(&ctx, expr, Scope::Row(row))?
                                };
                                match value {
                                    Value::Null => {}
                                    Value::Edge(edge) => {
                                        if tx.edge(ns, edge.id)?.is_some_and(|e| !e.is_retracted())
                                        {
                                            tx.retract_edge(ns, edge.id)?;
                                            stats.edges_retracted += 1;
                                        }
                                    }
                                    other => {
                                        return Err(QueryError::runtime(format!(
                                            "RETRACT needs edges, got a {}",
                                            other.type_name()
                                        )));
                                    }
                                }
                            }
                        }
                        Clause::Match { .. } | Clause::Return(_) => unreachable!("not a write"),
                    }
                }
            }
        }
    }
    Ok(QueryResult {
        columns: Vec::new(),
        rows: Vec::new(),
        stats,
    })
}

fn remember_variables(pattern: &Pattern, variables: &mut Vec<String>) {
    let names = std::iter::once(pattern.start.var.as_ref())
        .chain(
            pattern
                .steps
                .iter()
                .flat_map(|(rel, node)| [rel.var.as_ref(), node.var.as_ref()]),
        )
        .flatten();
    for name in names {
        if !variables.contains(name) {
            variables.push(name.clone());
        }
    }
}

// ---------------------------------------------------------------- matching

#[derive(Clone)]
struct Partial {
    row: Row,
    /// Edges used by this match: an edge appears at most once per pattern.
    used: Vec<EdgeId>,
    /// The node bound at each position of the pattern.
    nodes: Vec<Option<NodeId>>,
}

fn match_pattern(
    ctx: &Context,
    rows: Vec<Row>,
    pattern: &Pattern,
    base: &EdgeFilter,
) -> Result<Vec<Row>, QueryError> {
    let nodes = pattern.nodes();
    let mut out = Vec::new();
    for row in rows {
        let anchor = choose_anchor(&nodes, &row);
        for node in candidates(ctx, nodes[anchor], &row)? {
            let mut start = Partial {
                row: row.clone(),
                used: Vec::new(),
                nodes: vec![None; nodes.len()],
            };
            start.nodes[anchor] = Some(node.id);
            if let Some(var) = &nodes[anchor].var {
                start.row.insert(var.clone(), Value::Node(node));
            }
            let mut states = vec![start];
            // Right of the anchor, in the written direction…
            for step in anchor..pattern.steps.len() {
                let (rel, target) = &pattern.steps[step];
                let mut next = Vec::new();
                for state in states {
                    let from = state.nodes[step].expect("bound while extending");
                    next.extend(expand(
                        ctx,
                        state,
                        from,
                        rel,
                        rel.direction,
                        target,
                        step + 1,
                        base,
                    )?);
                }
                states = next;
            }
            // …and left of it, against the written direction.
            for step in (0..anchor).rev() {
                let (rel, _) = &pattern.steps[step];
                let mut next = Vec::new();
                for state in states {
                    let from = state.nodes[step + 1].expect("bound while extending");
                    next.extend(expand(
                        ctx,
                        state,
                        from,
                        rel,
                        reverse(rel.direction),
                        nodes[step],
                        step,
                        base,
                    )?);
                }
                states = next;
            }
            out.extend(states.into_iter().map(|state| state.row));
        }
    }
    Ok(out)
}

fn reverse(direction: Direction) -> Direction {
    match direction {
        Direction::Out => Direction::In,
        Direction::In => Direction::Out,
        Direction::Both => Direction::Both,
    }
}

/// The node to start from: a bound variable, then an `@id`, then labels
/// with properties, then labels.
fn choose_anchor(nodes: &[&NodePattern], row: &Row) -> usize {
    let score = |node: &NodePattern| {
        if node.var.as_ref().is_some_and(|var| row.contains_key(var)) {
            4
        } else if node
            .props
            .iter()
            .any(|(key, _)| *key == Key::Meta("id".into()))
        {
            3
        } else if !node.labels.is_empty() && !node.props.is_empty() {
            2
        } else if !node.labels.is_empty() {
            1
        } else {
            0
        }
    };
    let mut best = 0;
    for (index, node) in nodes.iter().enumerate() {
        if score(node) > score(nodes[best]) {
            best = index;
        }
    }
    best
}

fn candidates(
    ctx: &Context,
    pattern: &NodePattern,
    row: &Row,
) -> Result<Vec<Arc<Node>>, QueryError> {
    if let Some(var) = &pattern.var
        && let Some(bound) = row.get(var)
    {
        return match bound {
            Value::Node(node) if node_fits(ctx, node, pattern, row)? => Ok(vec![node.clone()]),
            Value::Node(_) | Value::Null => Ok(Vec::new()),
            other => Err(QueryError::runtime(format!(
                "`{var}` is a {}, not a node",
                other.type_name()
            ))),
        };
    }
    let found: Vec<Node> = if let Some((_, expr)) = pattern
        .props
        .iter()
        .find(|(key, _)| *key == Key::Meta("id".into()))
    {
        match eval(ctx, expr, Scope::Row(row))? {
            Value::Str(text) => match uuid_from_text(&text) {
                Some(id) => ctx.node(id)?.into_iter().collect(),
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    } else {
        let label = pattern.labels.first().map(String::as_str);
        ctx.read.nodes_page(ctx.ns, label, None, usize::MAX)?
    };
    let mut out = Vec::new();
    for node in found {
        let node = Arc::new(node);
        if node_fits(ctx, &node, pattern, row)? {
            out.push(node);
        }
    }
    Ok(out)
}

fn uuid_from_text(text: &str) -> Option<NodeId> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok().map(NodeId::from_u128)
}

fn props_fit(
    ctx: &Context,
    target: &Value,
    props: &[(Key, Expr)],
    row: &Row,
) -> Result<bool, QueryError> {
    for (key, expr) in props {
        let expected = eval(ctx, expr, Scope::Row(row))?;
        let actual = property(ctx, target, key)?;
        if equals(&actual, &expected) != Some(true) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn node_fits(
    ctx: &Context,
    node: &Arc<Node>,
    pattern: &NodePattern,
    row: &Row,
) -> Result<bool, QueryError> {
    if !pattern.labels.iter().all(|label| node.has_label(label)) {
        return Ok(false);
    }
    props_fit(ctx, &Value::Node(node.clone()), &pattern.props, row)
}

/// The node at `id` if it fits `pattern` (and the row's binding of its
/// variable, if any).
fn target_node(
    ctx: &Context,
    id: NodeId,
    pattern: &NodePattern,
    row: &Row,
) -> Result<Option<Arc<Node>>, QueryError> {
    if let Some(var) = &pattern.var
        && let Some(bound) = row.get(var)
    {
        return match bound {
            Value::Node(node) if node.id == id && node_fits(ctx, node, pattern, row)? => {
                Ok(Some(node.clone()))
            }
            _ => Ok(None),
        };
    }
    let Some(node) = ctx.node(id)? else {
        return Ok(None);
    };
    let node = Arc::new(node);
    Ok(node_fits(ctx, &node, pattern, row)?.then_some(node))
}

#[allow(clippy::too_many_arguments)]
fn expand(
    ctx: &Context,
    state: Partial,
    from: NodeId,
    rel: &RelPattern,
    direction: Direction,
    target: &NodePattern,
    target_index: usize,
    base: &EdgeFilter,
) -> Result<Vec<Partial>, QueryError> {
    let mut filter = base.clone();
    for label in &rel.labels {
        filter = filter.label(label);
    }
    let mut out = Vec::new();
    let Some((min, max)) = rel.length else {
        for (edge_id, other) in ctx.read.neighbor_ids(ctx.ns, from, direction, &filter)? {
            if state.used.contains(&edge_id) {
                continue;
            }
            let edge = if rel.var.is_some() || !rel.props.is_empty() {
                let Some(edge) = ctx.read.edge(ctx.ns, edge_id)? else {
                    continue;
                };
                let edge = Arc::new(edge);
                if !props_fit(ctx, &Value::Edge(edge.clone()), &rel.props, &state.row)? {
                    continue;
                }
                if let Some(var) = &rel.var
                    && let Some(bound) = state.row.get(var)
                    && !matches!(bound, Value::Edge(existing) if existing.id == edge_id)
                {
                    continue;
                }
                Some(edge)
            } else {
                None
            };
            let Some(node) = target_node(ctx, other, target, &state.row)? else {
                continue;
            };
            let mut next = state.clone();
            next.used.push(edge_id);
            next.nodes[target_index] = Some(other);
            if let Some(var) = &target.var {
                next.row.insert(var.clone(), Value::Node(node));
            }
            if let (Some(var), Some(edge)) = (&rel.var, edge) {
                next.row.insert(var.clone(), Value::Edge(edge));
            }
            out.push(next);
        }
        return Ok(out);
    };

    if !rel.props.is_empty() {
        return Err(QueryError::runtime(
            "properties on a variable-length edge pattern are not supported",
        ));
    }
    let max = max.unwrap_or(MAX_PATH_LENGTH).min(MAX_PATH_LENGTH);
    // Breadth-first: each node is reached once, at its shortest distance.
    let mut parent: HashMap<NodeId, (NodeId, EdgeId)> = HashMap::new();
    let mut distance = HashMap::from([(from, 0u32)]);
    let mut queue = VecDeque::from([from]);
    let mut reached = Vec::new();
    if min == 0 {
        reached.push(from);
    }
    while let Some(current) = queue.pop_front() {
        let depth = distance[&current];
        if depth == max {
            continue;
        }
        for (edge_id, other) in ctx.read.neighbor_ids(ctx.ns, current, direction, &filter)? {
            if distance.contains_key(&other) {
                continue;
            }
            distance.insert(other, depth + 1);
            parent.insert(other, (current, edge_id));
            if depth + 1 >= min {
                reached.push(other);
            }
            queue.push_back(other);
        }
    }
    for end in reached {
        let Some(node) = target_node(ctx, end, target, &state.row)? else {
            continue;
        };
        let mut next = state.clone();
        next.nodes[target_index] = Some(end);
        if let Some(var) = &rel.var {
            let mut edges = Vec::new();
            let mut current = end;
            while let Some(&(previous, edge_id)) = parent.get(&current) {
                if let Some(edge) = ctx.read.edge(ctx.ns, edge_id)? {
                    edges.push(Value::Edge(Arc::new(edge)));
                }
                current = previous;
            }
            edges.reverse();
            next.row.insert(var.clone(), Value::List(edges));
        }
        if let Some(var) = &target.var {
            next.row.insert(var.clone(), Value::Node(node));
        }
        out.push(next);
    }
    Ok(out)
}

// ---------------------------------------------------------------- writes

fn eval_in(
    read: &ReadTx,
    tx: &WriteTx,
    ns: &str,
    params: &Params,
    row: &Row,
    expr: &Expr,
) -> Result<Value, QueryError> {
    let ctx = Context {
        read,
        write: Some(tx),
        ns,
        params,
    };
    eval(&ctx, expr, Scope::Row(row))
}

fn to_props(entries: impl IntoIterator<Item = (String, Value)>) -> Result<Props, QueryError> {
    let mut props = Props::new();
    for (key, value) in entries {
        props.insert(key, value.to_property().map_err(QueryError::runtime)?);
    }
    Ok(props)
}

fn ids_from(value: &Value, what: &str) -> Result<Vec<NodeId>, QueryError> {
    let Value::List(items) = value else {
        return Err(QueryError::runtime(format!("{what} needs a list of ids")));
    };
    items
        .iter()
        .map(|item| match item {
            Value::Str(text) => uuid_from_text(text)
                .ok_or_else(|| QueryError::runtime(format!("invalid id {text:?}"))),
            Value::Node(node) => Ok(node.id),
            other => Err(QueryError::runtime(format!(
                "{what} needs ids, got a {}",
                other.type_name()
            ))),
        })
        .collect()
}

fn embedding_from(value: &Value) -> Result<Vec<f32>, QueryError> {
    let Value::List(items) = value else {
        return Err(QueryError::runtime("@embedding needs a list of numbers"));
    };
    items
        .iter()
        .map(|item| match item {
            Value::Int(i) => Ok(*i as f32),
            Value::Float(f) => Ok(*f as f32),
            other => Err(QueryError::runtime(format!(
                "@embedding needs numbers, got a {}",
                other.type_name()
            ))),
        })
        .collect()
}

fn create(
    read: &ReadTx,
    tx: &mut WriteTx,
    ns: &str,
    params: &Params,
    row: &mut Row,
    pattern: &Pattern,
    stats: &mut WriteStats,
) -> Result<(), QueryError> {
    let mut ids = Vec::new();
    for node in pattern.nodes() {
        if let Some(var) = &node.var
            && let Some(bound) = row.get(var)
        {
            match bound {
                Value::Node(existing) if node.labels.is_empty() && node.props.is_empty() => {
                    ids.push(existing.id);
                    continue;
                }
                Value::Node(_) => {
                    return Err(QueryError::runtime(format!(
                        "`{var}` is already bound: CREATE cannot add labels or properties to it (use SET)"
                    )));
                }
                other => {
                    return Err(QueryError::runtime(format!(
                        "`{var}` is a {}, not a node",
                        other.type_name()
                    )));
                }
            }
        }
        if node.labels.is_empty() {
            return Err(QueryError::runtime(
                "CREATE needs at least one label for each new node",
            ));
        }
        let mut new = NewNode {
            labels: node.labels.clone(),
            ..NewNode::default()
        };
        let mut props = Vec::new();
        for (key, expr) in &node.props {
            let value = eval_in(read, tx, ns, params, row, expr)?;
            match key {
                Key::Prop(_) if value.is_null() => {}
                Key::Prop(name) => props.push((name.clone(), value)),
                Key::Meta(name) => match name.as_str() {
                    "text" => match value {
                        Value::Str(text) => new.text = Some(text),
                        Value::Null => {}
                        other => {
                            return Err(QueryError::runtime(format!(
                                "@text needs a string, got a {}",
                                other.type_name()
                            )));
                        }
                    },
                    "embedding" if !value.is_null() => {
                        new.embedding = Some(embedding_from(&value)?)
                    }
                    "sources" if !value.is_null() => new.sources = ids_from(&value, "@sources")?,
                    "embedding" | "sources" => {}
                    other => {
                        return Err(QueryError::runtime(format!(
                            "CREATE can set @text, @embedding and @sources, not @{other}"
                        )));
                    }
                },
            }
        }
        new.props = to_props(props)?;
        let id = tx.create_node(ns, new)?;
        stats.nodes_created += 1;
        if let Some(var) = &node.var {
            let created = tx.node(ns, id)?.expect("just created");
            row.insert(var.clone(), Value::Node(Arc::new(created)));
        }
        ids.push(id);
    }

    for (index, (rel, _)) in pattern.steps.iter().enumerate() {
        let [label] = rel.labels.as_slice() else {
            return Err(QueryError::runtime(
                "CREATE needs exactly one label for each edge",
            ));
        };
        if rel.length.is_some() {
            return Err(QueryError::runtime(
                "CREATE cannot use variable-length edges",
            ));
        }
        let (from, to) = match rel.direction {
            Direction::Out => (ids[index], ids[index + 1]),
            Direction::In => (ids[index + 1], ids[index]),
            Direction::Both => {
                return Err(QueryError::runtime(
                    "CREATE needs a direction for each edge (`->` or `<-`)",
                ));
            }
        };
        if let Some(var) = &rel.var
            && row.contains_key(var)
        {
            return Err(QueryError::runtime(format!("`{var}` is already bound")));
        }
        let mut edge = NewEdge::new(from, label.clone(), to);
        let mut props = Vec::new();
        for (key, expr) in &rel.props {
            let value = eval_in(read, tx, ns, params, row, expr)?;
            match key {
                Key::Prop(_) if value.is_null() => {}
                Key::Prop(name) => props.push((name.clone(), value)),
                Key::Meta(_) if value.is_null() => {}
                Key::Meta(name) => match name.as_str() {
                    "valid_from" => edge.valid_from = Some(to_time(&value)?),
                    "valid_to" => edge.valid_to = Some(to_time(&value)?),
                    "sources" => edge.sources = ids_from(&value, "@sources")?,
                    other => {
                        return Err(QueryError::runtime(format!(
                            "CREATE can set @valid_from, @valid_to and @sources on edges, not @{other}"
                        )));
                    }
                },
            }
        }
        edge.props = to_props(props)?;
        let id = tx.create_edge(ns, edge)?;
        stats.edges_created += 1;
        if let Some(var) = &rel.var {
            let created = tx.edge(ns, id)?.expect("just created");
            row.insert(var.clone(), Value::Edge(Arc::new(created)));
        }
    }
    Ok(())
}

enum Target {
    Node(NodeId),
    Edge(EdgeId),
}

fn target_of(row: &Row, var: &str) -> Result<Option<Target>, QueryError> {
    match row.get(var) {
        Some(Value::Node(node)) => Ok(Some(Target::Node(node.id))),
        Some(Value::Edge(edge)) => Ok(Some(Target::Edge(edge.id))),
        Some(Value::Null) => Ok(None),
        Some(other) => Err(QueryError::runtime(format!(
            "`{var}` is a {}, not a node or edge",
            other.type_name()
        ))),
        None => Err(QueryError::runtime(format!("unknown variable `{var}`"))),
    }
}

/// Puts the current record of `var` back into the row after a write.
fn refresh(tx: &WriteTx, ns: &str, row: &mut Row, var: &str) -> Result<(), QueryError> {
    let value = match target_of(row, var)? {
        Some(Target::Node(id)) => tx
            .node(ns, id)?
            .map_or(Value::Null, |node| Value::Node(Arc::new(node))),
        Some(Target::Edge(id)) => tx
            .edge(ns, id)?
            .map_or(Value::Null, |edge| Value::Edge(Arc::new(edge))),
        None => return Ok(()),
    };
    row.insert(var.to_owned(), value);
    Ok(())
}

fn set_props(tx: &mut WriteTx, ns: &str, target: &Target, props: Props) -> Result<(), QueryError> {
    match target {
        Target::Node(id) => tx.set_node_props(ns, *id, props)?,
        Target::Edge(id) => tx.set_edge_props(ns, *id, props)?,
    }
    Ok(())
}

fn set(
    read: &ReadTx,
    tx: &mut WriteTx,
    ns: &str,
    params: &Params,
    row: &mut Row,
    item: &SetItem,
    stats: &mut WriteStats,
) -> Result<(), QueryError> {
    let var = match item {
        SetItem::Property { var, .. }
        | SetItem::Merge { var, .. }
        | SetItem::Replace { var, .. }
        | SetItem::Labels { var, .. } => var,
    };
    let Some(target) = target_of(row, var)? else {
        return Ok(());
    };
    match item {
        SetItem::Property {
            key: Key::Prop(name),
            value,
            ..
        } => {
            let value = eval_in(read, tx, ns, params, row, value)?;
            set_props(tx, ns, &target, to_props([(name.clone(), value)])?)?;
            stats.properties_set += 1;
        }
        SetItem::Property {
            key: Key::Meta(name),
            value,
            ..
        } => {
            let value = eval_in(read, tx, ns, params, row, value)?;
            match (&target, name.as_str(), &value) {
                (Target::Node(id), "text", Value::Str(text)) => tx.set_text(ns, *id, text)?,
                (Target::Node(id), "text", Value::Null) => {
                    tx.remove_text(ns, *id)?;
                }
                (Target::Node(id), "embedding", Value::Null) => {
                    tx.remove_embedding(ns, *id)?;
                }
                (Target::Node(id), "embedding", value) => {
                    tx.set_embedding(ns, *id, &embedding_from(value)?)?
                }
                (Target::Edge(id), "valid_to", value) => {
                    tx.invalidate_edge(ns, *id, to_time(value)?)?
                }
                (Target::Node(_), other, _) => {
                    return Err(QueryError::runtime(format!(
                        "SET can change @text and @embedding of a node, not @{other} (labels: SET n:Label)"
                    )));
                }
                (Target::Edge(_), other, _) => {
                    return Err(QueryError::runtime(format!(
                        "SET can change @valid_to of an edge (it invalidates the fact), not @{other}"
                    )));
                }
            }
            stats.properties_set += 1;
        }
        SetItem::Merge { map, .. } | SetItem::Replace { map, .. } => {
            let Value::Map(entries) = eval_in(read, tx, ns, params, row, map)? else {
                return Err(QueryError::runtime("SET with `+=` or `=` needs a map"));
            };
            let mut props = Props::new();
            if matches!(item, SetItem::Replace { .. }) {
                let current = match &target {
                    Target::Node(id) => tx.node(ns, *id)?.map(|n| n.props),
                    Target::Edge(id) => tx.edge(ns, *id)?.map(|e| e.props),
                };
                for key in current.unwrap_or_default().keys() {
                    props.insert(key.clone(), serde_json::Value::Null);
                }
            }
            stats.properties_set += entries.len() as u64;
            props.extend(to_props(entries)?);
            set_props(tx, ns, &target, props)?;
        }
        SetItem::Labels { labels, .. } => {
            let Target::Node(id) = target else {
                return Err(QueryError::runtime("only nodes have labels"));
            };
            for label in labels {
                if !tx.node(ns, id)?.is_some_and(|node| node.has_label(label)) {
                    tx.add_label(ns, id, label)?;
                    stats.labels_added += 1;
                }
            }
        }
    }
    refresh(tx, ns, row, var)
}

fn remove(
    tx: &mut WriteTx,
    ns: &str,
    row: &mut Row,
    item: &RemoveItem,
    stats: &mut WriteStats,
) -> Result<(), QueryError> {
    let var = match item {
        RemoveItem::Property { var, .. } | RemoveItem::Labels { var, .. } => var,
    };
    let Some(target) = target_of(row, var)? else {
        return Ok(());
    };
    match item {
        RemoveItem::Property {
            key: Key::Prop(name),
            ..
        } => {
            set_props(tx, ns, &target, to_props([(name.clone(), Value::Null)])?)?;
            stats.properties_set += 1;
        }
        RemoveItem::Property {
            key: Key::Meta(name),
            ..
        } => match (&target, name.as_str()) {
            (Target::Node(id), "text") => {
                tx.remove_text(ns, *id)?;
            }
            (Target::Node(id), "embedding") => {
                tx.remove_embedding(ns, *id)?;
            }
            (_, other) => {
                return Err(QueryError::runtime(format!(
                    "REMOVE can drop @text and @embedding of a node, not @{other}"
                )));
            }
        },
        RemoveItem::Labels { labels, .. } => {
            let Target::Node(id) = target else {
                return Err(QueryError::runtime("only nodes have labels"));
            };
            for label in labels {
                if tx.node(ns, id)?.is_some_and(|node| node.has_label(label)) {
                    tx.remove_label(ns, id, label)?;
                    stats.labels_removed += 1;
                }
            }
        }
    }
    refresh(tx, ns, row, var)
}

// ---------------------------------------------------------------- projection

fn project(
    ctx: &Context,
    rows: Vec<Row>,
    ret: &Return,
    variables: &[String],
) -> Result<(Vec<String>, Vec<Vec<Value>>), QueryError> {
    let items: Vec<(Expr, String)> = match &ret.items {
        Some(items) => items
            .iter()
            .map(|item| (item.expr.clone(), item.alias.clone()))
            .collect(),
        None => {
            if variables.is_empty() {
                return Err(QueryError::runtime("RETURN * needs variables to return"));
            }
            variables
                .iter()
                .map(|name| (Expr::Var(name.clone()), name.clone()))
                .collect()
        }
    };
    let columns: Vec<String> = items.iter().map(|(_, alias)| alias.clone()).collect();
    let aggregated = items.iter().any(|(expr, _)| expr.has_aggregate());

    // Each output row keeps a row of variables (plus the aliases) for ORDER BY.
    let mut output: Vec<(Vec<Value>, Row)> = Vec::new();
    if aggregated {
        let mut groups: Vec<Vec<Row>> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for row in rows {
            let mut key = String::new();
            for (expr, _) in items.iter().filter(|(expr, _)| !expr.has_aggregate()) {
                key.push_str(&eval(ctx, expr, Scope::Row(&row))?.key());
                key.push('\u{1}');
            }
            match index.get(&key) {
                Some(&at) => groups[at].push(row),
                None => {
                    index.insert(key, groups.len());
                    groups.push(vec![row]);
                }
            }
        }
        let grouped = items.iter().any(|(expr, _)| !expr.has_aggregate());
        if groups.is_empty() && !grouped {
            // Aggregates over nothing still give one row: count(*) = 0.
            groups.push(Vec::new());
        }
        for group in groups {
            let values = items
                .iter()
                .map(|(expr, _)| eval(ctx, expr, Scope::Group(&group)))
                .collect::<Result<Vec<_>, _>>()?;
            let source = group.into_iter().next().unwrap_or_default();
            output.push((values, source));
        }
    } else {
        for row in rows {
            let values = items
                .iter()
                .map(|(expr, _)| eval(ctx, expr, Scope::Row(&row)))
                .collect::<Result<Vec<_>, _>>()?;
            output.push((values, row));
        }
    }

    if ret.distinct {
        let mut seen = HashSet::new();
        output.retain(|(values, _)| {
            let key: Vec<String> = values.iter().map(Value::key).collect();
            seen.insert(key.join("\u{1}"))
        });
    }

    if !ret.order.is_empty() {
        let mut keyed = Vec::with_capacity(output.len());
        for (values, mut source) in output {
            for (column, value) in columns.iter().zip(&values) {
                source.insert(column.clone(), value.clone());
            }
            let mut keys = Vec::with_capacity(ret.order.len());
            for (expr, _) in &ret.order {
                // An ORDER BY item that repeats a RETURN item (aggregates
                // included) uses its value.
                let key = match items.iter().position(|(item, _)| item == expr) {
                    Some(at) => values[at].clone(),
                    None => eval(ctx, expr, Scope::Row(&source))?,
                };
                keys.push(key);
            }
            keyed.push((keys, values));
        }
        keyed.sort_by(|(a, _), (b, _)| {
            for ((left, right), (_, descending)) in a.iter().zip(b).zip(&ret.order) {
                let ordering = sort_order(left, right);
                let ordering = if *descending {
                    ordering.reverse()
                } else {
                    ordering
                };
                if !ordering.is_eq() {
                    return ordering;
                }
            }
            std::cmp::Ordering::Equal
        });
        output = keyed
            .into_iter()
            .map(|(_, values)| (values, Row::new()))
            .collect();
    }

    let empty = Row::new();
    let count = |expr: &Option<Expr>, what: &str| -> Result<Option<usize>, QueryError> {
        let Some(expr) = expr else { return Ok(None) };
        match eval(ctx, expr, Scope::Row(&empty))? {
            Value::Int(n) if n >= 0 => Ok(Some(n as usize)),
            other => Err(QueryError::runtime(format!(
                "{what} needs a non-negative integer, got {}",
                other.to_json()
            ))),
        }
    };
    let skip = count(&ret.skip, "SKIP")?.unwrap_or(0);
    let limit = count(&ret.limit, "LIMIT")?.unwrap_or(usize::MAX);
    let rows = output
        .into_iter()
        .skip(skip)
        .take(limit)
        .map(|(values, _)| values)
        .collect();
    Ok((columns, rows))
}
