//! `grafeno roles` and `grafeno grants` (phase 14.3): roles and grants on
//! a database file (the server stopped) or through a running server. Each
//! command is a GQ access statement, so the rules are those of `CREATE
//! ROLE`, `GRANT`, `DENY` and `REVOKE`.

use std::sync::Arc;

use grafeno::Grafeno;
use serde_json::{Value, json};

use crate::keys::{Target, http};

/// A string literal of GQ.
pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// `--on`, as GQ: `server`, `namespace:team-*`, `nodes:Person@crm`,
/// `edges:works_at@crm` or `property:Person.email@crm`.
pub fn target(on: &str) -> Result<String, String> {
    if on.eq_ignore_ascii_case("server") {
        return Ok("SERVER".into());
    }
    let (kind, rest) = on
        .split_once(':')
        .ok_or_else(|| format!("--on {on:?}: server, namespace:NAME, nodes:Label@NAME, edges:label@NAME or property:Label.key@NAME"))?;
    let in_namespace = |rest: &str| -> Result<(String, String), String> {
        let (what, namespace) = rest
            .rsplit_once('@')
            .ok_or_else(|| format!("--on {on:?}: add @namespace"))?;
        Ok((what.to_owned(), quote(namespace)))
    };
    let label = |text: &str| -> Result<String, String> {
        if !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || c == '_') {
            Ok(text.to_owned())
        } else {
            Err(format!("{text:?} is not a label"))
        }
    };
    match kind.to_ascii_lowercase().as_str() {
        "namespace" => Ok(format!("NAMESPACE {}", quote(rest))),
        "nodes" | "edges" => {
            let (what, namespace) = in_namespace(rest)?;
            Ok(format!(
                "{} {} IN {namespace}",
                kind.to_ascii_uppercase(),
                label(&what)?
            ))
        }
        "property" => {
            let (what, namespace) = in_namespace(rest)?;
            let (owner, key) = what
                .split_once('.')
                .ok_or_else(|| format!("--on {on:?}: property:Label.key@namespace"))?;
            Ok(format!(
                "PROPERTY {}.{} IN {namespace}",
                label(owner)?,
                label(key)?
            ))
        }
        other => Err(format!("--on: unknown kind {other:?}")),
    }
}

/// Privileges as GQ lists them: `read,create` → `read, create`.
pub fn privileges(list: &str) -> Result<String, String> {
    let names: Vec<&str> = list
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if names.is_empty()
        || names
            .iter()
            .any(|n| !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
    {
        return Err(format!(
            "{list:?}: privileges are names such as read,create (grafeno grants catalog lists them)"
        ));
    }
    Ok(names.join(", "))
}

/// No limits, and everything allowed, for a database file opened here:
/// passwords set in GQ go through the policy and Argon2id.
pub fn local_limits() -> grafeno_query::Limits {
    let hasher: grafeno_query::PasswordHasher = Arc::new(|name: &str, password: &str| {
        crate::auth::check_password(name, password)?;
        Ok(crate::auth::hash_password(password))
    });
    grafeno_query::Limits {
        access: Some(grafeno_query::AccessContext::owner(Some(hasher))),
        ..grafeno_query::Limits::none()
    }
}

/// Runs an access statement and prints its rows.
pub fn run(target: &Target, statement: &str) -> Result<(), String> {
    let result = match target {
        Target::File(path) => {
            let db = Grafeno::open(path).map_err(|e| e.to_string())?;
            grafeno_query::execute_with(
                &db,
                "default",
                statement,
                &grafeno_query::Params::new(),
                &local_limits(),
            )
            .map_err(|e| e.to_string())?
            .to_json()
        }
        Target::Server { url, token } => http(
            url,
            token.as_deref(),
            "POST",
            "/v1/default/query",
            Some(json!({ "query": statement })),
        )?,
    };
    print(&result);
    Ok(())
}

fn cell(value: &Value) -> String {
    match value {
        Value::Null => "-".into(),
        Value::String(text) => text.clone(),
        Value::Array(list) if list.is_empty() => "-".into(),
        Value::Array(list) => list.iter().map(cell).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

fn print(result: &Value) {
    let columns: Vec<String> = result["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .map(cell)
        .collect();
    let rows: Vec<Vec<String>> = result["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| row.as_array().into_iter().flatten().map(cell).collect())
        .collect();
    if columns == ["result"] {
        for row in &rows {
            println!("{}", row.join(""));
        }
        return;
    }
    let widths: Vec<usize> = (0..columns.len())
        .map(|i| {
            rows.iter()
                .map(|row| row.get(i).map_or(0, |c| c.chars().count()))
                .chain([columns[i].len()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: &[String]| {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:width$}", width = widths[i]))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    println!("{}", line(&columns));
    for row in &rows {
        println!("{}", line(row));
    }
    if rows.is_empty() {
        println!("(none)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_and_privileges_become_gq() {
        assert_eq!(target("server").unwrap(), "SERVER");
        assert_eq!(target("namespace:team-*").unwrap(), "NAMESPACE 'team-*'");
        assert_eq!(target("nodes:Person@crm").unwrap(), "NODES Person IN 'crm'");
        assert_eq!(target("edges:works_at@*").unwrap(), "EDGES works_at IN '*'");
        assert_eq!(
            target("property:Person.email@crm").unwrap(),
            "PROPERTY Person.email IN 'crm'"
        );
        assert!(target("nodes:Person").is_err());
        assert!(target("nodes:Per son@crm").is_err());
        assert_eq!(quote("it's"), "'it\\'s'");
        assert_eq!(privileges("read, create").unwrap(), "read, create");
        assert!(privileges("read; DROP").is_err());
    }
}
