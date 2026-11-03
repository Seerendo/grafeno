//! The syntax tree of a query.

use grafeno::Direction;

use crate::Value;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Query {
    pub(crate) clauses: Vec<Clause>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Clause {
    Match {
        patterns: Vec<Pattern>,
        valid_at: Option<Expr>,
        known_at: Option<Expr>,
        filter: Option<Expr>,
    },
    Create(Vec<Pattern>),
    Set(Vec<SetItem>),
    Remove(Vec<RemoveItem>),
    Delete(Vec<Expr>),
    Retract(Vec<Expr>),
    Return(Return),
}

impl Clause {
    pub(crate) fn writes(&self) -> bool {
        matches!(
            self,
            Clause::Create(_)
                | Clause::Set(_)
                | Clause::Remove(_)
                | Clause::Delete(_)
                | Clause::Retract(_)
        )
    }
}

/// `(a)-[r]->(b)-[s]-(c)`: a start node and the steps after it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pattern {
    pub(crate) start: NodePattern,
    pub(crate) steps: Vec<(RelPattern, NodePattern)>,
}

impl Pattern {
    pub(crate) fn nodes(&self) -> Vec<&NodePattern> {
        std::iter::once(&self.start)
            .chain(self.steps.iter().map(|(_, node)| node))
            .collect()
    }
}

/// A property key: a regular property, or a pseudo-property such as `@text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Prop(String),
    Meta(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NodePattern {
    pub(crate) var: Option<String>,
    pub(crate) labels: Vec<String>,
    pub(crate) props: Vec<(Key, Expr)>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RelPattern {
    pub(crate) var: Option<String>,
    /// Any of these labels (`[:a|b]`); empty means any label.
    pub(crate) labels: Vec<String>,
    pub(crate) props: Vec<(Key, Expr)>,
    /// From the left node's point of view.
    pub(crate) direction: Direction,
    /// `*min..max` (`max` `None`: up to the engine's limit).
    pub(crate) length: Option<(u32, Option<u32>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SetItem {
    /// `n.key = value` (a `null` value removes the property).
    Property { var: String, key: Key, value: Expr },
    /// `n += {…}`
    Merge { var: String, map: Expr },
    /// `n = {…}`: replaces every property.
    Replace { var: String, map: Expr },
    /// `n:Label:Other`
    Labels { var: String, labels: Vec<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RemoveItem {
    Property { var: String, key: Key },
    Labels { var: String, labels: Vec<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Return {
    pub(crate) distinct: bool,
    /// `None` for `RETURN *`.
    pub(crate) items: Option<Vec<ReturnItem>>,
    pub(crate) order: Vec<(Expr, bool)>,
    pub(crate) skip: Option<Expr>,
    pub(crate) limit: Option<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReturnItem {
    pub(crate) expr: Expr,
    /// The column name: the `AS` alias, or the expression as written.
    pub(crate) alias: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    Or,
    Xor,
    And,
    Eq,
    Neq,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Contains,
    StartsWith,
    EndsWith,
    In,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnOp {
    Not,
    Neg,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Expr {
    Literal(Value),
    Param(String),
    Var(String),
    Property(Box<Expr>, Key),
    Index(Box<Expr>, Box<Expr>),
    List(Vec<Expr>),
    Map(Vec<(String, Expr)>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `x IS NULL` (`negated`: `IS NOT NULL`).
    IsNull {
        expr: Box<Expr>,
        negated: bool,
    },
    /// `n:Label:Other`
    HasLabels(Box<Expr>, Vec<String>),
    /// A function call; `name` is lowercase.
    Call {
        name: String,
        distinct: bool,
        args: Vec<Expr>,
    },
    /// `count(*)`
    CountStar,
}

pub(crate) const AGGREGATES: &[&str] = &["count", "sum", "avg", "min", "max", "collect"];

impl Expr {
    pub(crate) fn has_aggregate(&self) -> bool {
        match self {
            Expr::CountStar => true,
            Expr::Call { name, args, .. } => {
                AGGREGATES.contains(&name.as_str()) || args.iter().any(Expr::has_aggregate)
            }
            Expr::Property(inner, _) | Expr::Unary(_, inner) | Expr::HasLabels(inner, _) => {
                inner.has_aggregate()
            }
            Expr::IsNull { expr, .. } => expr.has_aggregate(),
            Expr::Index(a, b) | Expr::Binary(_, a, b) => a.has_aggregate() || b.has_aggregate(),
            Expr::List(items) => items.iter().any(Expr::has_aggregate),
            Expr::Map(entries) => entries.iter().any(|(_, value)| value.has_aggregate()),
            Expr::Literal(_) | Expr::Param(_) | Expr::Var(_) => false,
        }
    }
}
