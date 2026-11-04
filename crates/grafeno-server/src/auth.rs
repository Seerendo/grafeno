//! Who is calling, and what it may do.
//!
//! The server's static token (from `grafeno.toml`) is the owner. API keys
//! are principals stored in the database ([`grafeno::Principal`]): each has
//! grants per namespace (read, write, admin), may be an owner, and may be
//! limited to changing what it recorded itself. A server with neither a
//! token nor keys is open, as on a developer's machine.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use grafeno::{Actor, Grafeno, Principal, Role};
use sha2::{Digest, Sha256};

/// The hex SHA-256 of a secret, as stored.
pub fn hash_secret(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A new secret: `gfk_` and 40 random hex digits.
pub fn new_secret() -> String {
    let mut bytes = [0u8; 20];
    getrandom::fill(&mut bytes).expect("the system has randomness");
    format!(
        "gfk_{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

/// The keys of a database, by the hash of their secret.
#[derive(Default)]
pub struct Keyring {
    by_hash: RwLock<HashMap<String, Arc<Principal>>>,
}

impl Keyring {
    pub fn load(db: &Grafeno) -> grafeno::Result<Self> {
        let keyring = Self::default();
        keyring.reload(db)?;
        Ok(keyring)
    }

    /// Reads the keys again, after one was created or revoked.
    pub fn reload(&self, db: &Grafeno) -> grafeno::Result<()> {
        let principals = db.begin_read()?.principals()?;
        let map = principals
            .into_iter()
            .map(|principal| (principal.secret_hash.clone(), Arc::new(principal)))
            .collect();
        *self.by_hash.write().expect("not poisoned") = map;
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.by_hash.read().expect("not poisoned").is_empty()
    }

    pub fn find(&self, secret: &str) -> Option<Arc<Principal>> {
        self.by_hash
            .read()
            .expect("not poisoned")
            .get(&hash_secret(secret))
            .cloned()
    }
}

/// The caller of a request.
#[derive(Clone, Debug)]
pub enum Caller {
    /// The static token, or an open server: everything.
    Owner,
    Key(Arc<Principal>),
}

impl Caller {
    pub fn may(&self, ns: &str, role: Role) -> bool {
        match self {
            Caller::Owner => true,
            Caller::Key(principal) => principal.may(ns, role),
        }
    }

    pub fn is_owner(&self) -> bool {
        match self {
            Caller::Owner => true,
            Caller::Key(principal) => principal.owner,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Caller::Owner => "owner",
            Caller::Key(principal) => &principal.name,
        }
    }

    /// Who the engine records as the writer (nobody for the owner token).
    pub fn actor(&self) -> Option<Actor> {
        match self {
            Caller::Owner => None,
            Caller::Key(principal) => Some(Actor {
                name: principal.name.clone(),
                own_only: principal.own_only,
            }),
        }
    }

    /// Why a request is refused: `role` on `ns` is missing.
    pub fn refusal(&self, ns: &str, role: Role) -> String {
        let role = match role {
            Role::Read => "read",
            Role::Write => "write",
            Role::Admin => "admin",
        };
        format!(
            "the key {:?} has no {role} access to the namespace {ns:?}",
            self.name()
        )
    }
}

tokio::task_local! {
    /// The caller of the request being served.
    pub static CALLER: Caller;
}

/// The caller of the current request (the owner outside of one: tests,
/// tools).
pub fn current() -> Caller {
    CALLER.try_with(Clone::clone).unwrap_or(Caller::Owner)
}

/// What a request under `/v1` needs, from its method and path.
#[derive(Debug, PartialEq, Eq)]
pub enum Need {
    /// Any caller the server accepts.
    Caller,
    /// The owner (keys, import).
    Owner,
    /// A role on a namespace; the handler may ask for more (writing
    /// queries, writing algorithms).
    Namespace(String, Role),
}

fn decode(segment: &str) -> String {
    let hex = |byte: u8| (byte as char).to_digit(16).map(|digit| digit as u8);
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(high * 16 + low);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What `method` on `path` (relative to `/v1`) needs.
pub fn need(method: &str, path: &str) -> Need {
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    match parts.as_slice() {
        ["algorithms"] | ["whoami"] => Need::Caller,
        // Listing filters what the caller may read; creating checks the
        // name in the handler.
        ["namespaces"] => Need::Caller,
        ["namespaces", ns] => Need::Namespace(
            decode(ns),
            if method == "GET" {
                Role::Read
            } else {
                Role::Admin
            },
        ),
        // Export checks each namespace it writes out.
        ["export"] => Need::Caller,
        ["import"] | ["keys", ..] | ["backup"] => Need::Owner,
        [ns, rest @ ..] => {
            let ns = decode(ns);
            let reads = matches!(
                rest,
                ["k-hop"]
                    | ["path"]
                    | ["subgraph"]
                    | ["search", ..]
                    | ["recall"]
                    | ["context"]
                    | ["query"]
                    | ["algo", ..]
            );
            let role = if rest == ["rebuild"] || rest == ["compact"] {
                Role::Admin
            } else if method == "GET" || (method == "POST" && reads) {
                Role::Read
            } else {
                Role::Write
            };
            Need::Namespace(ns, role)
        }
        [] => Need::Caller,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_need_roles() {
        let ns = |name: &str, role| Need::Namespace(name.into(), role);
        assert_eq!(need("GET", "/vault/nodes"), ns("vault", Role::Read));
        assert_eq!(need("POST", "/vault/nodes"), ns("vault", Role::Write));
        assert_eq!(need("POST", "/vault/search/text"), ns("vault", Role::Read));
        assert_eq!(need("POST", "/vault/query"), ns("vault", Role::Read));
        assert_eq!(need("POST", "/vault/merge"), ns("vault", Role::Write));
        assert_eq!(need("POST", "/vault/rebuild"), ns("vault", Role::Admin));
        assert_eq!(need("DELETE", "/vault/edges/x"), ns("vault", Role::Write));
        assert_eq!(
            need("DELETE", "/namespaces/team%2Da"),
            ns("team-a", Role::Admin)
        );
        assert_eq!(need("GET", "/namespaces/demo"), ns("demo", Role::Read));
        assert_eq!(need("POST", "/import"), Need::Owner);
        assert_eq!(need("DELETE", "/keys/ana"), Need::Owner);
        assert_eq!(need("GET", "/namespaces"), Need::Caller);
    }

    #[test]
    fn secrets_are_hashed() {
        let secret = new_secret();
        assert!(secret.starts_with("gfk_") && secret.len() == 44);
        assert_eq!(hash_secret(&secret).len(), 64);
        assert_ne!(hash_secret(&secret), hash_secret(&new_secret()));
    }
}
