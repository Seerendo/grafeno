//! `grafeno users`: accounts (14.1), on a database file (the server
//! stopped) or through a running server (with its settings file and token).
//! Passwords are asked at a prompt that does not echo, or read from
//! standard input with `--password-stdin`.

use grafeno::{Account, AccountKind, Grafeno, Grant, Timestamp};
use serde_json::{Value, json};

use crate::auth::{check_password, hash_password};
use crate::keys::{Target, http};

/// A password from standard input (one line) or a prompt, asked twice.
pub fn read_password(name: &str, from_stdin: bool) -> Result<String, String> {
    let password = if from_stdin {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        line.trim_end_matches(['\r', '\n']).to_owned()
    } else {
        let first = rpassword::prompt_password(format!("Password for {name}: "))
            .map_err(|e| e.to_string())?;
        let again = rpassword::prompt_password("Again: ").map_err(|e| e.to_string())?;
        if first != again {
            return Err("the passwords differ".into());
        }
        first
    };
    check_password(name, &password)?;
    Ok(password)
}

fn open(path: &std::path::Path) -> Result<Grafeno, String> {
    Grafeno::open(path).map_err(|e| e.to_string())
}

fn describe(account: &Value) -> String {
    let grants: Vec<String> = account["grants"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|g| {
            format!(
                "{}:{}",
                g["namespace"].as_str().unwrap_or("?"),
                g["role"].as_str().unwrap_or("?")
            )
        })
        .collect();
    let mut what = if account["superuser"] == true {
        "superuser".to_owned()
    } else {
        grants.join(", ")
    };
    if account["own_only"] == true {
        what.push_str(", own records only");
    }
    if account["disabled"] == true {
        what.push_str(" (disabled)");
    }
    format!(
        "{:20} {:8} {}",
        account["name"].as_str().unwrap_or("?"),
        account["kind"].as_str().unwrap_or("?"),
        what
    )
}

pub fn list(target: &Target) -> Result<(), String> {
    let accounts: Vec<Value> = match target {
        Target::File(path) => open(path)?
            .begin_read()
            .and_then(|read| read.accounts())
            .map_err(|e| e.to_string())?
            .iter()
            .map(|a| json!({ "name": a.name, "kind": a.kind, "grants": a.grants, "superuser": a.superuser, "own_only": a.own_only, "disabled": a.disabled }))
            .collect(),
        Target::Server { url, token } => match http(url, token.as_deref(), "GET", "/v1/accounts", None)? {
            Value::Array(list) => list,
            _ => Vec::new(),
        },
    };
    if accounts.is_empty() {
        println!("No accounts: only the server's token works (or the server is open).");
    }
    for account in &accounts {
        println!("{}", describe(account));
    }
    Ok(())
}

pub struct NewAccount {
    pub name: String,
    pub service: bool,
    pub superuser: bool,
    pub grants: Vec<Grant>,
    pub own_only: bool,
    pub password: Option<String>,
}

pub fn create(target: &Target, new: NewAccount) -> Result<(), String> {
    let kind = if new.service {
        AccountKind::Service
    } else {
        AccountKind::User
    };
    match target {
        Target::File(path) => {
            let account = Account {
                name: new.name.clone(),
                kind,
                password_hash: new.password.as_deref().map(hash_password),
                grants: new.grants,
                roles: Vec::new(),
                superuser: new.superuser,
                own_only: new.own_only,
                disabled: false,
                expires_at: None,
                description: None,
                created_at: Timestamp::now(),
            };
            open(path)?
                .write(|tx| tx.create_account(&account))
                .map_err(|e| e.to_string())?;
        }
        Target::Server { url, token } => {
            let body = json!({ "name": new.name, "kind": kind, "password": new.password, "grants": new.grants,
                               "superuser": new.superuser, "own_only": new.own_only });
            http(url, token.as_deref(), "POST", "/v1/accounts", Some(body))?;
        }
    }
    let what = match (new.service, new.superuser) {
        (true, _) => "service account (give it keys: grafeno keys create --account)",
        (false, true) => "superuser",
        (false, false) => "user",
    };
    println!("Created the {what} {:?}.", new.name);
    Ok(())
}

/// Changes an account: its password, or whether it is disabled.
pub fn change(
    target: &Target,
    name: &str,
    password: Option<String>,
    disabled: Option<bool>,
) -> Result<(), String> {
    match target {
        Target::File(path) => {
            open(path)?
                .write(|tx| {
                    let mut account = tx
                        .account(name)?
                        .ok_or_else(|| grafeno::Error::Invalid(format!("no account {name:?}")))?;
                    if let Some(password) = &password {
                        account.password_hash = Some(hash_password(password));
                    }
                    if let Some(disabled) = disabled {
                        account.disabled = disabled;
                    }
                    tx.put_account(&account)?;
                    // A new password or a disabled account ends its sessions.
                    for session in tx.all_sessions()?.into_iter().filter(|s| s.account == name) {
                        tx.delete_session(&session.id)?;
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
        }
        Target::Server { url, token } => {
            let mut body = json!({});
            if let Some(password) = password {
                body["password"] = json!(password);
            }
            if let Some(disabled) = disabled {
                body["disabled"] = json!(disabled);
            }
            http(
                url,
                token.as_deref(),
                "PATCH",
                &format!("/v1/accounts/{name}"),
                Some(body),
            )?;
        }
    }
    println!("Changed the account {name:?}.");
    Ok(())
}

pub fn delete(target: &Target, name: &str) -> Result<(), String> {
    let removed = match target {
        Target::File(path) => open(path)?
            .write(|tx| tx.delete_account(name))
            .map_err(|e| e.to_string())?,
        Target::Server { url, token } => http(
            url,
            token.as_deref(),
            "DELETE",
            &format!("/v1/accounts/{name}"),
            None,
        )
        .map(|_| true)?,
    };
    if removed {
        println!("Deleted the account {name:?}, its keys and its sessions.");
        Ok(())
    } else {
        Err(format!("no account {name:?}"))
    }
}
