//! `grafeno keys`: API keys, on a database file (the server stopped) or
//! through a running server (with its settings file and owner token).

use std::path::Path;

use grafeno::{Grafeno, Grant, Principal, Role, Timestamp};
use serde_json::{Value, json};

use crate::auth::{hash_secret, new_secret};
use crate::config::Config;

/// Where the keys live.
pub enum Target<'a> {
    File(&'a Path),
    /// The server of a settings file.
    Server {
        url: String,
        token: Option<String>,
    },
}

impl Target<'_> {
    pub fn server(config: &Path) -> Result<Target<'static>, String> {
        let config = Config::load(config).map_err(|error| error.to_string())?;
        Ok(Target::Server {
            url: config.local_url(),
            token: config.token,
        })
    }
}

/// Parses `namespace:role` (`vault:write`, `team-*:read`, `*:admin`).
pub fn parse_grant(text: &str) -> Result<Grant, String> {
    let (namespace, role) = text
        .rsplit_once(':')
        .ok_or_else(|| format!("a grant is namespace:role (vault:write), not {text:?}"))?;
    if namespace.is_empty() {
        return Err(format!("a grant needs a namespace: {text:?}"));
    }
    Ok(Grant {
        namespace: namespace.to_owned(),
        role: Role::parse(role).map_err(|error| error.to_string())?,
    })
}

fn http(
    url: &str,
    token: Option<&str>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .new_agent();
    let uri = format!("{url}{path}");
    let mut request = ureq::http::Request::builder().method(method).uri(&uri);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = request
        .header("content-type", "application/json")
        .body(body.map(|b| b.to_string()).unwrap_or_default())
        .map_err(|error| error.to_string())?;
    let mut response = agent
        .run(request)
        .map_err(|error| format!("cannot reach the server at {url}: {error}"))?;
    let status = response.status();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| error.to_string())?;
    let value: Value = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    };
    if status.is_success() {
        Ok(value)
    } else {
        Err(value["error"]["message"]
            .as_str()
            .map_or_else(|| format!("HTTP {status}"), str::to_owned))
    }
}

fn describe(key: &Value) -> String {
    let grants: Vec<String> = key["grants"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|grant| {
            format!(
                "{}:{}",
                grant["namespace"].as_str().unwrap_or("?"),
                grant["role"].as_str().unwrap_or("?")
            )
        })
        .collect();
    let mut what = if key["owner"] == true {
        "owner".to_owned()
    } else {
        grants.join(", ")
    };
    if key["own_only"] == true {
        what.push_str(", own records only");
    }
    format!("{:20} {}", key["name"].as_str().unwrap_or("?"), what)
}

pub fn list(target: &Target) -> Result<(), String> {
    let keys = match target {
        Target::File(path) => {
            let db = Grafeno::open(path).map_err(|error| error.to_string())?;
            let principals = db
                .begin_read()
                .and_then(|read| read.principals())
                .map_err(|e| e.to_string())?;
            principals
                .iter()
                .map(|p| json!({ "name": p.name, "grants": p.grants, "owner": p.owner, "own_only": p.own_only }))
                .collect()
        }
        Target::Server { url, token } => {
            match http(url, token.as_deref(), "GET", "/v1/keys", None)? {
                Value::Array(keys) => keys,
                _ => Vec::new(),
            }
        }
    };
    if keys.is_empty() {
        println!("No keys: only the server's token (the owner) works.");
    }
    for key in &keys {
        println!("{}", describe(key));
    }
    Ok(())
}

pub fn create(
    target: &Target,
    name: &str,
    grants: Vec<Grant>,
    owner: bool,
    own_only: bool,
    description: Option<String>,
) -> Result<(), String> {
    if grants.is_empty() && !owner {
        return Err("give at least one --grant namespace:role, or --owner".into());
    }
    let secret = match target {
        Target::File(path) => {
            let db = Grafeno::open(path).map_err(|error| error.to_string())?;
            let secret = new_secret();
            let principal = Principal {
                name: name.to_owned(),
                secret_hash: hash_secret(&secret),
                grants,
                owner,
                own_only,
                description,
                created_at: Timestamp::now(),
            };
            db.write(|tx| tx.create_principal(&principal))
                .map_err(|error| error.to_string())?;
            secret
        }
        Target::Server { url, token } => {
            let body = json!({ "name": name, "grants": grants, "owner": owner, "own_only": own_only, "description": description });
            let created = http(url, token.as_deref(), "POST", "/v1/keys", Some(body))?;
            created["secret"].as_str().unwrap_or_default().to_owned()
        }
    };
    println!("Created the key {name:?}. Its secret (shown only now):\n\n  {secret}\n");
    println!("Clients send it as `Authorization: Bearer {secret}`.");
    Ok(())
}

pub fn revoke(target: &Target, name: &str) -> Result<(), String> {
    let removed = match target {
        Target::File(path) => {
            let db = Grafeno::open(path).map_err(|error| error.to_string())?;
            db.write(|tx| tx.delete_principal(name))
                .map_err(|error| error.to_string())?
        }
        Target::Server { url, token } => {
            let path = format!("/v1/keys/{name}");
            http(url, token.as_deref(), "DELETE", &path, None).map(|_| true)?
        }
    };
    if removed {
        println!("Revoked the key {name:?}.");
        Ok(())
    } else {
        Err(format!("no key named {name:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_from_the_command_line() {
        let grant = parse_grant("team-*:write").unwrap();
        assert_eq!(
            (grant.namespace.as_str(), grant.role),
            ("team-*", Role::Write)
        );
        assert!(parse_grant("vault").is_err());
        assert!(parse_grant(":read").is_err());
        assert!(parse_grant("vault:root").is_err());
    }
}
