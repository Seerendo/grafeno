//! Query execution: a pipeline of clauses over rows of variables.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use grafeno::{
    Direction, EdgeFilter, EdgeId, Grafeno, NewEdge, NewNode, Node, NodeId, Props, ReadTx,
    Timestamp, WriteTx,
};
use serde::Serialize;

use crate::QueryError;
use crate::ast::{
    Clause, Expr, Key, Mode, NodePattern, Pattern, ProcedureCall, Projection, Query, RelPattern,
    RemoveItem, SetItem, Statement,
};
use crate::eval::{Context, Params, Row, Scope, eval, property, to_time, truth};
use crate::parser::parse;
use crate::procedures;
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

/// One step of a query's plan, with what it did when the query ran under
/// `PROFILE`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlanStep {
    pub step: usize,
    pub operation: String,
    pub details: String,
    /// Rows after the step (`PROFILE` only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    /// Milliseconds the step took (`PROFILE` only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<f64>,
}

/// The result of a query: named columns, rows of values, and what changed.
#[derive(Clone, Debug)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub stats: WriteStats,
    /// The steps and what each did, under `PROFILE`.
    pub profile: Option<Vec<PlanStep>>,
}

impl QueryResult {
    /// `{"columns": […], "rows": [[…]], "stats": {…}}`, plus `"profile"`
    /// under `PROFILE`.
    pub fn to_json(&self) -> serde_json::Value {
        let mut json = serde_json::json!({
            "columns": self.columns,
            "rows": self
                .rows
                .iter()
                .map(|row| row.iter().map(Value::to_json).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            "stats": self.stats,
        });
        if let Some(profile) = &self.profile {
            json["profile"] = serde_json::to_value(profile).unwrap_or_default();
        }
        json
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
    let statement = parse(query)?;
    validate(&statement)?;
    if statement.mode == Mode::Explain {
        return Ok(explain(&statement));
    }
    if statement.writes() {
        // Holding the write lock first makes the snapshot the latest state.
        let mut tx = db.begin_write()?;
        let read = db.begin_read()?;
        let outcome = Runner::new(&read, Some(&mut tx), ns, params, statement.mode).run(&statement);
        match outcome {
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
        let read = db.begin_read()?;
        Runner::new(&read, None, ns, params, statement.mode).run(&statement)
    }
}

/// Runs a read-only statement on an existing snapshot.
pub fn execute_read(
    read: &ReadTx,
    ns: &str,
    query: &str,
    params: &Params,
) -> Result<QueryResult, QueryError> {
    let statement = parse(query)?;
    validate(&statement)?;
    if statement.mode == Mode::Explain {
        return Ok(explain(&statement));
    }
    if statement.writes() {
        return Err(QueryError::runtime(
            "this query writes; it needs a writable database",
        ));
    }
    Runner::new(read, None, ns, params, statement.mode).run(&statement)
}

/// Whether the statement writes when run (`EXPLAIN` never does).
pub fn is_write(query: &str) -> Result<bool, QueryError> {
    let statement = parse(query)?;
    Ok(statement.mode != Mode::Explain && statement.writes())
}

fn validate(statement: &Statement) -> Result<(), QueryError> {
    if statement
        .union_all
        .windows(2)
        .any(|pair| pair[0] != pair[1])
    {
        return Err(QueryError::runtime(
            "a statement cannot mix UNION and UNION ALL",
        ));
    }
    let union = statement.parts.len() > 1;
    for part in &statement.parts {
        let last = part.clauses.len() - 1;
        for (index, clause) in part.clauses.iter().enumerate() {
            match clause {
                Clause::Return(_) if index != last => {
                    return Err(QueryError::runtime("RETURN must be the last clause"));
                }
                Clause::With(_) if index == last => {
                    return Err(QueryError::runtime(
                        "a query cannot end with WITH: add RETURN, or a write",
                    ));
                }
                Clause::With(projection) => {
                    for item in &projection.items {
                        if !item.aliased && !matches!(item.expr, Expr::Var(_)) {
                            return Err(QueryError::runtime(format!(
                                "WITH needs a name for `{}`: add AS name",
                                item.alias
                            )));
                        }
                    }
                }
                _ => {}
            }
        }
        let ends = &part.clauses[last];
        if union && !matches!(ends, Clause::Return(_)) {
            return Err(QueryError::runtime(
                "each part of a UNION must end with RETURN",
            ));
        }
        if !matches!(ends, Clause::Return(_) | Clause::Call(_)) && !ends.writes() {
            return Err(QueryError::runtime(format!(
                "a query cannot end with {}: add RETURN, or end with a write (CREATE, MERGE, SET, REMOVE, DELETE, RETRACT, FOREACH)",
                ends.name()
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- runner

struct Runner<'a> {
    read: &'a ReadTx,
    tx: Option<&'a mut WriteTx>,
    ns: &'a str,
    params: &'a Params,
    stats: WriteStats,
    deleted_nodes: HashSet<NodeId>,
    deleted_edges: HashSet<EdgeId>,
    profile: Option<Vec<PlanStep>>,
}

/// The rows flowing between clauses and the variables they bind, in order.
struct Flow {
    rows: Vec<Row>,
    vars: Vec<String>,
}

impl Flow {
    fn bind(&mut self, names: impl IntoIterator<Item = impl AsRef<str>>) {
        for name in names {
            let name = name.as_ref();
            if !self.vars.iter().any(|var| var == name) {
                self.vars.push(name.to_owned());
            }
        }
    }
}

impl<'a> Runner<'a> {
    fn new(
        read: &'a ReadTx,
        tx: Option<&'a mut WriteTx>,
        ns: &'a str,
        params: &'a Params,
        mode: Mode,
    ) -> Self {
        Self {
            read,
            tx,
            ns,
            params,
            stats: WriteStats::default(),
            deleted_nodes: HashSet::new(),
            deleted_edges: HashSet::new(),
            profile: (mode == Mode::Profile).then(Vec::new),
        }
    }

    fn ctx<'b>(&'b self, edges: &'b EdgeFilter) -> Context<'b> {
        Context {
            read: self.read,
            write: self.tx.as_deref(),
            ns: self.ns,
            params: self.params,
            edges,
        }
    }

    fn run(mut self, statement: &Statement) -> Result<QueryResult, QueryError> {
        let mut columns: Option<Vec<String>> = None;
        let mut rows: Vec<Vec<Value>> = Vec::new();
        for (index, part) in statement.parts.iter().enumerate() {
            let (part_columns, part_rows) = self.run_query(part)?;
            match &columns {
                Some(existing) if *existing != part_columns => {
                    return Err(QueryError::runtime(format!(
                        "every part of a UNION must return the same columns: {} and {}",
                        existing.join(", "),
                        part_columns.join(", ")
                    )));
                }
                _ => columns = Some(part_columns),
            }
            rows.extend(part_rows);
            if index > 0 && !statement.union_all[index - 1] {
                let started = Instant::now();
                let mut seen = HashSet::new();
                rows.retain(|row| seen.insert(row.iter().map(Value::key).collect::<Vec<_>>()));
                self.record("UNION", "distinct rows of both parts", rows.len(), started);
            }
        }
        Ok(QueryResult {
            columns: columns.unwrap_or_default(),
            rows,
            stats: self.stats,
            profile: self.profile,
        })
    }

    fn record(
        &mut self,
        operation: &str,
        details: impl Into<String>,
        rows: usize,
        started: Instant,
    ) {
        if let Some(profile) = &mut self.profile {
            profile.push(PlanStep {
                step: profile.len() + 1,
                operation: operation.to_owned(),
                details: details.into(),
                rows: Some(rows as u64),
                time_ms: Some(started.elapsed().as_secs_f64() * 1000.0),
            });
        }
    }

    fn run_query(&mut self, query: &Query) -> Result<(Vec<String>, Vec<Vec<Value>>), QueryError> {
        let mut flow = Flow {
            rows: vec![Row::new()],
            vars: Vec::new(),
        };
        let mut bound: HashSet<String> = HashSet::new();
        for clause in &query.clauses {
            let started = Instant::now();
            let details = self.profile.is_some().then(|| describe(clause, &bound));
            if let Clause::Return(projection) = clause {
                let edges = EdgeFilter::any();
                let ctx = self.ctx(&edges);
                let (columns, rows) = project(&ctx, flow.rows, projection, &flow.vars)?;
                self.record("RETURN", details.unwrap_or_default(), rows.len(), started);
                return Ok((columns, rows));
            }
            flow = self.apply(clause, flow)?;
            bound.extend(flow.vars.iter().cloned());
            self.record(
                clause.name(),
                details.unwrap_or_default(),
                flow.rows.len(),
                started,
            );
        }
        // A query that ends with CALL returns what the procedure yields.
        if let Some(Clause::Call(call)) = query.clauses.last() {
            let columns = yielded(call)?
                .into_iter()
                .map(|(_, alias)| alias)
                .collect::<Vec<_>>();
            let rows = flow
                .rows
                .into_iter()
                .map(|row| {
                    columns
                        .iter()
                        .map(|column| row.get(column).cloned().unwrap_or(Value::Null))
                        .collect()
                })
                .collect();
            return Ok((columns, rows));
        }
        Ok((Vec::new(), Vec::new()))
    }

    /// Runs one clause (not RETURN) on the rows.
    fn apply(&mut self, clause: &Clause, mut flow: Flow) -> Result<Flow, QueryError> {
        match clause {
            Clause::Match {
                optional,
                patterns,
                valid_at,
                known_at,
                filter,
            } => {
                let new_vars: Vec<String> = patterns
                    .iter()
                    .flat_map(Pattern::variables)
                    .map(str::to_owned)
                    .collect();
                // `VALID AT at` may name a variable (one time per row, as
                // with `temporal.series`); otherwise it is computed once.
                let per_row = [valid_at, known_at]
                    .into_iter()
                    .flatten()
                    .any(mentions_variables);
                let shared = if per_row {
                    EdgeFilter::any()
                } else {
                    self.time_filter(valid_at.as_ref(), known_at.as_ref(), &Row::new())?
                };
                let matched = |rows: Vec<Row>| -> Result<Vec<Row>, QueryError> {
                    let run =
                        |edges: &EdgeFilter, rows: Vec<Row>| -> Result<Vec<Row>, QueryError> {
                            let ctx = self.ctx(edges);
                            let mut rows = rows;
                            for pattern in patterns {
                                rows = match_pattern(&ctx, rows, pattern)?;
                            }
                            if let Some(filter) = filter {
                                rows = keep(&ctx, rows, filter)?;
                            }
                            Ok(rows)
                        };
                    if !per_row {
                        return run(&shared, rows);
                    }
                    let mut out = Vec::new();
                    for row in rows {
                        let edges = self.time_filter(valid_at.as_ref(), known_at.as_ref(), &row)?;
                        out.extend(run(&edges, vec![row])?);
                    }
                    Ok(out)
                };
                flow.rows = if *optional {
                    let mut out = Vec::new();
                    for row in flow.rows {
                        let found = matched(vec![row.clone()])?;
                        if found.is_empty() {
                            let mut row = row;
                            for var in &new_vars {
                                row.entry(var.clone()).or_insert(Value::Null);
                            }
                            out.push(row);
                        } else {
                            out.extend(found);
                        }
                    }
                    out
                } else {
                    matched(flow.rows)?
                };
                flow.bind(&new_vars);
            }
            Clause::Unwind { list, var } => {
                let edges = EdgeFilter::any();
                let ctx = self.ctx(&edges);
                let mut out = Vec::new();
                for row in flow.rows {
                    let items = match eval(&ctx, list, Scope::Row(&row))? {
                        Value::List(items) => items,
                        Value::Null => Vec::new(),
                        other => vec![other],
                    };
                    for item in items {
                        let mut next = row.clone();
                        next.insert(var.clone(), item);
                        out.push(next);
                    }
                }
                flow.rows = out;
                flow.bind([var]);
            }
            Clause::With(projection) => {
                let edges = EdgeFilter::any();
                let ctx = self.ctx(&edges);
                let (columns, values) = project(&ctx, flow.rows, projection, &flow.vars)?;
                let mut rows: Vec<Row> = values
                    .into_iter()
                    .map(|values| columns.iter().cloned().zip(values).collect())
                    .collect();
                if let Some(filter) = &projection.filter {
                    rows = keep(&ctx, rows, filter)?;
                }
                flow = Flow {
                    rows,
                    vars: columns,
                };
            }
            Clause::Call(call) => {
                let yields = yielded(call)?;
                let edges = EdgeFilter::any();
                let ctx = self.ctx(&edges);
                let procedure = procedures::find(&call.name)?;
                let mut out = Vec::new();
                for row in flow.rows {
                    let args = call
                        .args
                        .iter()
                        .map(|arg| eval(&ctx, arg, Scope::Row(&row)))
                        .collect::<Result<Vec<_>, _>>()?;
                    for result in procedures::call(&ctx, &call.name, &args)? {
                        let mut next = row.clone();
                        for (column, alias) in &yields {
                            let at = procedure
                                .columns
                                .iter()
                                .position(|name| name.eq_ignore_ascii_case(column))
                                .expect("checked by yielded()");
                            next.insert(alias.clone(), result[at].clone());
                        }
                        out.push(next);
                    }
                }
                if let Some(filter) = &call.filter {
                    out = keep(&ctx, out, filter)?;
                }
                flow.rows = out;
                flow.bind(yields.iter().map(|(_, alias)| alias));
            }
            Clause::Foreach { var, list, body } => {
                for row in &flow.rows {
                    let items = {
                        let edges = EdgeFilter::any();
                        let ctx = self.ctx(&edges);
                        match eval(&ctx, list, Scope::Row(row))? {
                            Value::List(items) => items,
                            Value::Null => Vec::new(),
                            other => {
                                return Err(QueryError::runtime(format!(
                                    "FOREACH needs a list, got a {}",
                                    other.type_name()
                                )));
                            }
                        }
                    };
                    for item in items {
                        let mut inner = row.clone();
                        inner.insert(var.clone(), item);
                        let mut inner = Flow {
                            rows: vec![inner],
                            vars: flow.vars.iter().cloned().chain([var.clone()]).collect(),
                        };
                        for clause in body {
                            inner = self.apply(clause, inner)?;
                        }
                    }
                }
            }
            Clause::Merge {
                pattern,
                on_create,
                on_match,
            } => {
                let mut out = Vec::new();
                for row in flow.rows {
                    // Only facts true now count as existing.
                    let edges = EdgeFilter::any().valid_at(Timestamp::now());
                    let found = {
                        let ctx = self.ctx(&edges);
                        match_pattern(&ctx, vec![row.clone()], pattern)?
                    };
                    if found.is_empty() {
                        let mut row = row;
                        self.create(&mut row, pattern, true)?;
                        for item in on_create {
                            self.set(&mut row, item)?;
                        }
                        out.push(row);
                    } else {
                        for mut row in found {
                            for item in on_match {
                                self.set(&mut row, item)?;
                            }
                            out.push(row);
                        }
                    }
                }
                flow.rows = out;
                flow.bind(pattern.variables());
            }
            Clause::Create(patterns) => {
                for row in &mut flow.rows {
                    for pattern in patterns {
                        self.create(row, pattern, false)?;
                    }
                }
                flow.bind(patterns.iter().flat_map(Pattern::variables));
            }
            Clause::Set(items) => {
                for row in &mut flow.rows {
                    for item in items {
                        self.set(row, item)?;
                    }
                }
            }
            Clause::Remove(items) => {
                for row in &mut flow.rows {
                    for item in items {
                        self.remove(row, item)?;
                    }
                }
            }
            Clause::Delete(exprs) => {
                for row in &flow.rows {
                    for expr in exprs {
                        self.delete(row, expr)?;
                    }
                }
            }
            Clause::Retract(exprs) => {
                for row in &flow.rows {
                    for expr in exprs {
                        self.retract(row, expr)?;
                    }
                }
            }
            Clause::Return(_) => unreachable!("RETURN ends the query"),
        }
        Ok(flow)
    }

    fn time_filter(
        &self,
        valid_at: Option<&Expr>,
        known_at: Option<&Expr>,
        row: &Row,
    ) -> Result<EdgeFilter, QueryError> {
        let any = EdgeFilter::any();
        let ctx = self.ctx(&any);
        let mut filter = EdgeFilter::any();
        if let Some(at) = valid_at {
            filter = filter.valid_at(to_time(&eval(&ctx, at, Scope::Row(row))?)?);
        }
        if let Some(at) = known_at {
            filter = filter.known_at(to_time(&eval(&ctx, at, Scope::Row(row))?)?);
        }
        Ok(filter)
    }

    fn tx(&mut self) -> &mut WriteTx {
        self.tx
            .as_deref_mut()
            .expect("writing queries run with a write transaction")
    }

    fn eval(&self, row: &Row, expr: &Expr) -> Result<Value, QueryError> {
        let edges = EdgeFilter::any();
        eval(&self.ctx(&edges), expr, Scope::Row(row))
    }
}

/// Whether an expression reads a variable of the row.
fn mentions_variables(expr: &Expr) -> bool {
    let mut found = false;
    expr.visit(&mut |inner| found |= matches!(inner, Expr::Var(_)));
    found
}

fn keep(ctx: &Context, rows: Vec<Row>, filter: &Expr) -> Result<Vec<Row>, QueryError> {
    let mut kept = Vec::with_capacity(rows.len());
    for row in rows {
        if truth(&eval(ctx, filter, Scope::Row(&row))?)? == Some(true) {
            kept.push(row);
        }
    }
    Ok(kept)
}

/// The `(column, alias)` pairs a CALL binds.
fn yielded(call: &ProcedureCall) -> Result<Vec<(String, String)>, QueryError> {
    let procedure = procedures::find(&call.name)?;
    match &call.yields {
        None => Ok(procedure
            .columns
            .iter()
            .map(|column| ((*column).to_owned(), (*column).to_owned()))
            .collect()),
        Some(items) => {
            for (column, _) in items {
                if !procedure
                    .columns
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(column))
                {
                    return Err(QueryError::runtime(format!(
                        "{}() yields {}, not {column}",
                        procedure.name,
                        procedure.columns.join(", ")
                    )));
                }
            }
            Ok(items.clone())
        }
    }
}

/// Whether a pattern predicate matches from `row` at least once.
pub(crate) fn exists(
    ctx: &Context,
    row: &Row,
    patterns: &[Pattern],
    filter: Option<&Expr>,
) -> Result<bool, QueryError> {
    let mut rows = vec![row.clone()];
    for pattern in patterns {
        rows = match_pattern(ctx, rows, pattern)?;
        if rows.is_empty() {
            return Ok(false);
        }
    }
    match filter {
        None => Ok(true),
        Some(filter) => {
            for row in &rows {
                if truth(&eval(ctx, filter, Scope::Row(row))?)? == Some(true) {
                    return Ok(true);
                }
            }
            Ok(false)
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

fn match_pattern(ctx: &Context, rows: Vec<Row>, pattern: &Pattern) -> Result<Vec<Row>, QueryError> {
    let nodes = pattern.nodes();
    let mut out = Vec::new();
    for row in rows {
        let anchor = choose_anchor(&nodes, |var| row.contains_key(var));
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

/// How the start of a pattern is found, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Anchor {
    Scan,
    Label,
    LabelAndProperties,
    Id,
    Bound,
}

fn anchor_kind(node: &NodePattern, is_bound: &impl Fn(&str) -> bool) -> Anchor {
    if node.var.as_deref().is_some_and(is_bound) {
        Anchor::Bound
    } else if node
        .props
        .iter()
        .any(|(key, _)| *key == Key::Meta("id".into()))
    {
        Anchor::Id
    } else if !node.labels.is_empty() && !node.props.is_empty() {
        Anchor::LabelAndProperties
    } else if !node.labels.is_empty() {
        Anchor::Label
    } else {
        Anchor::Scan
    }
}

/// The node to start from: a bound variable, then an `@id`, then labels
/// with properties, then labels.
fn choose_anchor(nodes: &[&NodePattern], is_bound: impl Fn(&str) -> bool) -> usize {
    let mut best = 0;
    for (index, node) in nodes.iter().enumerate() {
        if anchor_kind(node, &is_bound) > anchor_kind(nodes[best], &is_bound) {
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
        ctx.nodes(pattern.labels.first().map(String::as_str))?
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

fn expand(
    ctx: &Context,
    state: Partial,
    from: NodeId,
    rel: &RelPattern,
    direction: Direction,
    target: &NodePattern,
    target_index: usize,
) -> Result<Vec<Partial>, QueryError> {
    let mut filter = ctx.edges.clone();
    for label in &rel.labels {
        filter = filter.label(label);
    }
    let mut out = Vec::new();
    let Some((min, max)) = rel.length else {
        for (edge_id, other) in ctx.neighbor_ids(from, direction, &filter)? {
            if state.used.contains(&edge_id) {
                continue;
            }
            let edge = if rel.var.is_some() || !rel.props.is_empty() {
                let Some(edge) = ctx.edge(edge_id)? else {
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
        for (edge_id, other) in ctx.neighbor_ids(current, direction, &filter)? {
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
                if let Some(edge) = ctx.edge(edge_id)? {
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

impl Runner<'_> {
    /// Creates the parts of `pattern` that are not bound in `row`. MERGE
    /// (`undirected`) may create an undirected edge, from left to right.
    fn create(
        &mut self,
        row: &mut Row,
        pattern: &Pattern,
        undirected: bool,
    ) -> Result<(), QueryError> {
        let ns = self.ns;
        let mut ids = Vec::new();
        for node in pattern.nodes() {
            if let Some(var) = &node.var
                && let Some(bound) = row.get(var)
            {
                match bound {
                    Value::Node(existing) => {
                        ids.push(existing.id);
                        continue;
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
                    "CREATE and MERGE need at least one label for each new node",
                ));
            }
            let mut new = NewNode {
                labels: node.labels.clone(),
                ..NewNode::default()
            };
            let mut props = Vec::new();
            for (key, expr) in &node.props {
                let value = self.eval(row, expr)?;
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
                        "sources" if !value.is_null() => {
                            new.sources = ids_from(&value, "@sources")?
                        }
                        "embedding" | "sources" => {}
                        other => {
                            return Err(QueryError::runtime(format!(
                                "a new node can get @text, @embedding and @sources, not @{other}"
                            )));
                        }
                    },
                }
            }
            new.props = to_props(props)?;
            let tx = self.tx();
            let id = tx.create_node(ns, new)?;
            self.stats.nodes_created += 1;
            if let Some(var) = &node.var {
                let created = self.tx().node(ns, id)?.expect("just created");
                row.insert(var.clone(), Value::Node(Arc::new(created)));
            }
            ids.push(id);
        }

        for (index, (rel, _)) in pattern.steps.iter().enumerate() {
            let [label] = rel.labels.as_slice() else {
                return Err(QueryError::runtime("a new edge needs exactly one label"));
            };
            if rel.length.is_some() {
                return Err(QueryError::runtime(
                    "CREATE and MERGE cannot use variable-length edges",
                ));
            }
            // An undirected edge is created from left to right.
            let (from, to) = match rel.direction {
                Direction::Out | Direction::Both => (ids[index], ids[index + 1]),
                Direction::In => (ids[index + 1], ids[index]),
            };
            if rel.direction == Direction::Both && !undirected {
                return Err(QueryError::runtime(
                    "CREATE needs a direction for each edge (`->` or `<-`)",
                ));
            }
            if let Some(var) = &rel.var
                && row.contains_key(var)
            {
                return Err(QueryError::runtime(format!("`{var}` is already bound")));
            }
            let mut edge = NewEdge::new(from, label.clone(), to);
            let mut props = Vec::new();
            for (key, expr) in &rel.props {
                let value = self.eval(row, expr)?;
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
                                "a new edge can get @valid_from, @valid_to and @sources, not @{other}"
                            )));
                        }
                    },
                }
            }
            edge.props = to_props(props)?;
            let id = self.tx().create_edge(ns, edge)?;
            self.stats.edges_created += 1;
            if let Some(var) = &rel.var {
                let created = self.tx().edge(ns, id)?.expect("just created");
                row.insert(var.clone(), Value::Edge(Arc::new(created)));
            }
        }
        Ok(())
    }

    /// Puts the current record of `var` back into the row after a write.
    fn refresh(&mut self, row: &mut Row, var: &str) -> Result<(), QueryError> {
        let ns = self.ns;
        let value = match target_of(row, var)? {
            Some(Target::Node(id)) => self
                .tx()
                .node(ns, id)?
                .map_or(Value::Null, |node| Value::Node(Arc::new(node))),
            Some(Target::Edge(id)) => self
                .tx()
                .edge(ns, id)?
                .map_or(Value::Null, |edge| Value::Edge(Arc::new(edge))),
            None => return Ok(()),
        };
        row.insert(var.to_owned(), value);
        Ok(())
    }

    fn set_props(&mut self, target: &Target, props: Props) -> Result<(), QueryError> {
        let ns = self.ns;
        match target {
            Target::Node(id) => self.tx().set_node_props(ns, *id, props)?,
            Target::Edge(id) => self.tx().set_edge_props(ns, *id, props)?,
        }
        Ok(())
    }

    fn set(&mut self, row: &mut Row, item: &SetItem) -> Result<(), QueryError> {
        let ns = self.ns;
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
                let value = self.eval(row, value)?;
                self.set_props(&target, to_props([(name.clone(), value)])?)?;
                self.stats.properties_set += 1;
            }
            SetItem::Property {
                key: Key::Meta(name),
                value,
                ..
            } => {
                let value = self.eval(row, value)?;
                let tx = self.tx();
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
                    (Target::Node(id), "importance", Value::Int(_) | Value::Float(_)) => {
                        let importance = match value {
                            Value::Int(i) => i as f32,
                            Value::Float(f) => f as f32,
                            _ => unreachable!(),
                        };
                        tx.set_importance(ns, *id, importance)?
                    }
                    (Target::Edge(id), "valid_to", value) => {
                        tx.invalidate_edge(ns, *id, to_time(value)?)?
                    }
                    (Target::Node(_), other, _) => {
                        return Err(QueryError::runtime(format!(
                            "SET can change @text, @embedding and @importance of a node, not @{other} (labels: SET n:Label)"
                        )));
                    }
                    (Target::Edge(_), other, _) => {
                        return Err(QueryError::runtime(format!(
                            "SET can change @valid_to of an edge (it invalidates the fact), not @{other}"
                        )));
                    }
                }
                self.stats.properties_set += 1;
            }
            SetItem::Merge { map, .. } | SetItem::Replace { map, .. } => {
                let Value::Map(entries) = self.eval(row, map)? else {
                    return Err(QueryError::runtime("SET with `+=` or `=` needs a map"));
                };
                let mut props = Props::new();
                if matches!(item, SetItem::Replace { .. }) {
                    let current = match &target {
                        Target::Node(id) => self.tx().node(ns, *id)?.map(|n| n.props),
                        Target::Edge(id) => self.tx().edge(ns, *id)?.map(|e| e.props),
                    };
                    for key in current.unwrap_or_default().keys() {
                        props.insert(key.clone(), serde_json::Value::Null);
                    }
                }
                self.stats.properties_set += entries.len() as u64;
                props.extend(to_props(entries)?);
                self.set_props(&target, props)?;
            }
            SetItem::Labels { labels, .. } => {
                let Target::Node(id) = target else {
                    return Err(QueryError::runtime("only nodes have labels"));
                };
                for label in labels {
                    if !self
                        .tx()
                        .node(ns, id)?
                        .is_some_and(|node| node.has_label(label))
                    {
                        self.tx().add_label(ns, id, label)?;
                        self.stats.labels_added += 1;
                    }
                }
            }
        }
        self.refresh(row, var)
    }

    fn remove(&mut self, row: &mut Row, item: &RemoveItem) -> Result<(), QueryError> {
        let ns = self.ns;
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
                self.set_props(&target, to_props([(name.clone(), Value::Null)])?)?;
                self.stats.properties_set += 1;
            }
            RemoveItem::Property {
                key: Key::Meta(name),
                ..
            } => match (&target, name.as_str()) {
                (Target::Node(id), "text") => {
                    self.tx().remove_text(ns, *id)?;
                }
                (Target::Node(id), "embedding") => {
                    self.tx().remove_embedding(ns, *id)?;
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
                    if self
                        .tx()
                        .node(ns, id)?
                        .is_some_and(|node| node.has_label(label))
                    {
                        self.tx().remove_label(ns, id, label)?;
                        self.stats.labels_removed += 1;
                    }
                }
            }
        }
        self.refresh(row, var)
    }

    fn delete(&mut self, row: &Row, expr: &Expr) -> Result<(), QueryError> {
        let ns = self.ns;
        let values = match self.eval(row, expr)? {
            Value::List(items) => items,
            other => vec![other],
        };
        for value in values {
            match value {
                Value::Null => {}
                Value::Node(node) => {
                    if self.deleted_nodes.insert(node.id) && self.tx().node(ns, node.id)?.is_some()
                    {
                        self.tx().delete_node(ns, node.id)?;
                        self.stats.nodes_deleted += 1;
                    }
                }
                Value::Edge(edge) => {
                    if self.deleted_edges.insert(edge.id) && self.tx().edge(ns, edge.id)?.is_some()
                    {
                        self.tx().delete_edge(ns, edge.id)?;
                        self.stats.edges_deleted += 1;
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
        Ok(())
    }

    fn retract(&mut self, row: &Row, expr: &Expr) -> Result<(), QueryError> {
        let ns = self.ns;
        let values = match self.eval(row, expr)? {
            Value::List(items) => items,
            other => vec![other],
        };
        for value in values {
            match value {
                Value::Null => {}
                Value::Edge(edge) => {
                    if self
                        .tx()
                        .edge(ns, edge.id)?
                        .is_some_and(|e| !e.is_retracted())
                    {
                        self.tx().retract_edge(ns, edge.id)?;
                        self.stats.edges_retracted += 1;
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
        Ok(())
    }
}

// ---------------------------------------------------------------- projection

/// Evaluates a RETURN or WITH: grouping when there are aggregates, then
/// DISTINCT, ORDER BY, SKIP and LIMIT.
fn project(
    ctx: &Context,
    rows: Vec<Row>,
    projection: &Projection,
    variables: &[String],
) -> Result<(Vec<String>, Vec<Vec<Value>>), QueryError> {
    let mut items: Vec<(Expr, String)> = Vec::new();
    if projection.star {
        if variables.is_empty() && projection.items.is_empty() {
            return Err(QueryError::runtime("`*` needs variables in scope"));
        }
        items.extend(
            variables
                .iter()
                .map(|name| (Expr::Var(name.clone()), name.clone())),
        );
    }
    for item in &projection.items {
        items.push((item.expr.clone(), item.alias.clone()));
    }
    let columns: Vec<String> = items.iter().map(|(_, alias)| alias.clone()).collect();
    let mut seen_columns = HashSet::new();
    for column in &columns {
        if !seen_columns.insert(column) {
            return Err(QueryError::runtime(format!(
                "the column `{column}` appears twice: rename one with AS"
            )));
        }
    }
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

    if projection.distinct {
        let mut seen = HashSet::new();
        output.retain(|(values, _)| seen.insert(values.iter().map(Value::key).collect::<Vec<_>>()));
    }

    if !projection.order.is_empty() {
        let mut keyed = Vec::with_capacity(output.len());
        for (values, mut source) in output {
            for (column, value) in columns.iter().zip(&values) {
                source.insert(column.clone(), value.clone());
            }
            let mut keys = Vec::with_capacity(projection.order.len());
            for (expr, _) in &projection.order {
                // An ORDER BY item that repeats a projected item (aggregates
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
            for ((left, right), (_, descending)) in a.iter().zip(b).zip(&projection.order) {
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
    let skip = count(&projection.skip, "SKIP")?.unwrap_or(0);
    let limit = count(&projection.limit, "LIMIT")?.unwrap_or(usize::MAX);
    let rows = output
        .into_iter()
        .skip(skip)
        .take(limit)
        .map(|(values, _)| values)
        .collect();
    Ok((columns, rows))
}

// ---------------------------------------------------------------- EXPLAIN

/// The plan as a table: one row per step.
fn explain(statement: &Statement) -> QueryResult {
    let mut steps: Vec<(String, String)> = Vec::new();
    for (index, part) in statement.parts.iter().enumerate() {
        if index > 0 {
            let all = statement.union_all[index - 1];
            steps.push((
                if all { "UNION ALL" } else { "UNION" }.into(),
                if all {
                    "rows of both parts".into()
                } else {
                    "distinct rows of both parts".into()
                },
            ));
        }
        let mut bound = HashSet::new();
        for clause in &part.clauses {
            steps.push((clause.name().into(), describe(clause, &bound)));
            bound.extend(clause_variables(clause));
        }
    }
    QueryResult {
        columns: vec!["step".into(), "operation".into(), "details".into()],
        rows: steps
            .into_iter()
            .enumerate()
            .map(|(index, (operation, details))| {
                vec![
                    Value::Int(index as i64 + 1),
                    Value::Str(operation),
                    Value::Str(details),
                ]
            })
            .collect(),
        stats: WriteStats::default(),
        profile: None,
    }
}

/// Variables a clause binds, for planning the clauses after it.
fn clause_variables(clause: &Clause) -> Vec<String> {
    match clause {
        Clause::Match { patterns, .. } | Clause::Create(patterns) => patterns
            .iter()
            .flat_map(Pattern::variables)
            .map(str::to_owned)
            .collect(),
        Clause::Merge { pattern, .. } => {
            pattern.variables().into_iter().map(str::to_owned).collect()
        }
        Clause::Unwind { var, .. } => vec![var.clone()],
        Clause::With(projection) => projection
            .items
            .iter()
            .map(|item| item.alias.clone())
            .collect(),
        Clause::Call(call) => yielded(call)
            .map(|items| items.into_iter().map(|(_, alias)| alias).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn node_text(node: &NodePattern) -> String {
    let mut text = String::from("(");
    text.push_str(node.var.as_deref().unwrap_or(""));
    for label in &node.labels {
        text.push(':');
        text.push_str(label);
    }
    if !node.props.is_empty() {
        let keys: Vec<String> = node
            .props
            .iter()
            .map(|(key, _)| match key {
                Key::Prop(name) => name.clone(),
                Key::Meta(name) => format!("@{name}"),
            })
            .collect();
        text.push_str(&format!(" {{{}}}", keys.join(", ")));
    }
    text.push(')');
    text
}

fn rel_text(rel: &RelPattern, direction: Direction) -> String {
    let mut inner = rel.var.clone().unwrap_or_default();
    if !rel.labels.is_empty() {
        inner.push(':');
        inner.push_str(&rel.labels.join("|"));
    }
    if let Some((min, max)) = rel.length {
        inner.push_str(&match max {
            Some(max) => format!("*{min}..{max}"),
            None => format!("*{min}.."),
        });
    }
    let body = if inner.is_empty() {
        String::new()
    } else {
        format!("[{inner}]")
    };
    match direction {
        Direction::Out => format!("-{body}->"),
        Direction::In => format!("<-{body}-"),
        Direction::Both => format!("-{body}-"),
    }
}

/// How a pattern is matched: where it starts, then each expansion.
fn describe_pattern(pattern: &Pattern, bound: &HashSet<String>) -> String {
    let nodes = pattern.nodes();
    let is_bound = |var: &str| bound.contains(var);
    let anchor = choose_anchor(&nodes, is_bound);
    let start = nodes[anchor];
    let how = match anchor_kind(start, &is_bound) {
        Anchor::Bound => "the bound node",
        Anchor::Id => "lookup by @id",
        Anchor::LabelAndProperties => "label scan, then property filter",
        Anchor::Label => "label scan",
        Anchor::Scan => "scan of every node",
    };
    let mut parts = vec![format!("start at {} ({how})", node_text(start))];
    for step in anchor..pattern.steps.len() {
        let (rel, target) = &pattern.steps[step];
        parts.push(format!(
            "expand {}{}",
            rel_text(rel, rel.direction),
            node_text(target)
        ));
    }
    for step in (0..anchor).rev() {
        let (rel, _) = &pattern.steps[step];
        parts.push(format!(
            "expand {}{}",
            rel_text(rel, reverse(rel.direction)),
            node_text(nodes[step])
        ));
    }
    parts.join(", ")
}

fn describe(clause: &Clause, bound: &HashSet<String>) -> String {
    let projection_text = |projection: &Projection| {
        let mut parts = Vec::new();
        if projection.star {
            parts.push("*".to_owned());
        }
        parts.extend(projection.items.iter().map(|item| item.alias.clone()));
        let mut text = parts.join(", ");
        if projection
            .items
            .iter()
            .any(|item| item.expr.has_aggregate())
        {
            text.push_str("; aggregates");
        }
        if projection.distinct {
            text.push_str("; distinct");
        }
        if !projection.order.is_empty() {
            text.push_str("; sort");
        }
        if projection.skip.is_some() || projection.limit.is_some() {
            text.push_str("; skip/limit");
        }
        if projection.filter.is_some() {
            text.push_str("; filter");
        }
        text
    };
    match clause {
        Clause::Match {
            patterns,
            valid_at,
            known_at,
            filter,
            ..
        } => {
            let mut bound = bound.clone();
            let mut parts = Vec::new();
            for pattern in patterns {
                parts.push(describe_pattern(pattern, &bound));
                bound.extend(pattern.variables().into_iter().map(str::to_owned));
            }
            let mut text = parts.join("; ");
            if valid_at.is_some() {
                text.push_str("; edges valid at a time");
            }
            if known_at.is_some() {
                text.push_str("; edges known at a time");
            }
            if filter.is_some() {
                text.push_str("; filter (WHERE)");
            }
            text
        }
        Clause::Unwind { var, .. } => format!("one row per item, as {var}"),
        Clause::With(projection) | Clause::Return(projection) => projection_text(projection),
        Clause::Call(call) => {
            let columns = yielded(call)
                .map(|items| {
                    items
                        .into_iter()
                        .map(|(_, alias)| alias)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let mut text = format!("{}() yields {columns}", call.name);
            if call.filter.is_some() {
                text.push_str("; filter (WHERE)");
            }
            text
        }
        Clause::Create(patterns) => format!("{} pattern(s)", patterns.len()),
        Clause::Merge {
            pattern,
            on_create,
            on_match,
        } => format!(
            "match or create: {}; {} ON CREATE, {} ON MATCH item(s)",
            describe_pattern(pattern, bound),
            on_create.len(),
            on_match.len()
        ),
        Clause::Set(items) => format!("{} item(s)", items.len()),
        Clause::Remove(items) => format!("{} item(s)", items.len()),
        Clause::Delete(items) | Clause::Retract(items) => format!("{} value(s)", items.len()),
        Clause::Foreach { var, body, .. } => format!(
            "for each {var}: {}",
            body.iter().map(Clause::name).collect::<Vec<_>>().join(", ")
        ),
    }
}
