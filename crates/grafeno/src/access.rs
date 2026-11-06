//! Principals: named API keys with grants per namespace, stored in the
//! database (only a hash of each secret). Servers check them; the engine
//! records who wrote what ([`crate::Actor`]).

use grafeno_core::{Error, Result, Timestamp};
use redb::{ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::db::{ReadTx, WriteTx};
use crate::store::{decode, encode, storage};

/// `name -> Principal` (JSON).
pub(crate) const PRINCIPALS: TableDefinition<&str, &[u8]> = TableDefinition::new("principals");

/// What a grant allows, each role including the ones before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Read and search.
    Read,
    /// Also write nodes and facts, merge and archive.
    Write,
    /// Also indexes, rebuilds, imports, and creating, describing and
    /// dropping the namespace.
    Admin,
}

impl Role {
    pub fn parse(text: &str) -> Result<Self> {
        match text.to_ascii_lowercase().as_str() {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "admin" => Ok(Self::Admin),
            other => Err(Error::Invalid(format!(
                "unknown role {other:?} (read, write, admin)"
            ))),
        }
    }
}

/// A role on the namespaces a pattern names: `vault` (that one), `team-*`
/// (every name starting with `team-`) or `*` (all).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub namespace: String,
    pub role: Role,
}

impl Grant {
    pub fn covers(&self, ns: &str) -> bool {
        match self.namespace.strip_suffix('*') {
            Some(prefix) => ns.starts_with(prefix),
            None => self.namespace == ns,
        }
    }
}

/// A named API key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    pub name: String,
    /// The hex SHA-256 of the secret; the secret itself is never stored.
    pub secret_hash: String,
    #[serde(default)]
    pub grants: Vec<Grant>,
    /// Every right on every namespace, and managing keys.
    #[serde(default)]
    pub owner: bool,
    /// May change only nodes and facts it recorded itself.
    #[serde(default)]
    pub own_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: Timestamp,
}

impl Principal {
    /// The highest role it has on `ns`.
    pub fn role_in(&self, ns: &str) -> Option<Role> {
        if self.owner {
            return Some(Role::Admin);
        }
        self.grants
            .iter()
            .filter(|grant| grant.covers(ns))
            .map(|grant| grant.role)
            .max()
    }

    pub fn may(&self, ns: &str, role: Role) -> bool {
        self.role_in(ns).is_some_and(|have| have >= role)
    }
}

/// Checks a principal's name: it ends up in provenance and in URLs.
pub fn check_principal_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@'));
    if valid {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "a key name has 1 to 64 letters, digits, '-', '_', '.' or '@', not {name:?}"
        )))
    }
}

fn all(table: &impl ReadableTable<&'static str, &'static [u8]>) -> Result<Vec<Principal>> {
    let mut out = Vec::new();
    for item in table.iter().map_err(storage)? {
        let (_, value) = item.map_err(storage)?;
        out.push(decode(value.value())?);
    }
    Ok(out)
}

impl ReadTx {
    /// Every principal, in name order.
    pub fn principals(&self) -> Result<Vec<Principal>> {
        all(&self.principals)
    }
}

impl WriteTx {
    pub fn principals(&self) -> Result<Vec<Principal>> {
        all(&self.tx.open_table(PRINCIPALS).map_err(storage)?)
    }

    /// Adds a principal. Fails with [`Error::Conflict`] if the name is taken.
    pub fn create_principal(&mut self, principal: &Principal) -> Result<()> {
        check_principal_name(&principal.name)?;
        self.note(crate::replication::Touch::Principal(principal.name.clone()));
        let mut table = self.tx.open_table(PRINCIPALS).map_err(storage)?;
        if table
            .get(principal.name.as_str())
            .map_err(storage)?
            .is_some()
        {
            return Err(Error::Conflict(format!(
                "a key named {:?} exists",
                principal.name
            )));
        }
        table
            .insert(principal.name.as_str(), encode(principal)?.as_slice())
            .map_err(storage)?;
        Ok(())
    }

    /// Removes a principal; its secret stops working. Returns whether it
    /// existed. What it recorded keeps its name.
    pub fn delete_principal(&mut self, name: &str) -> Result<bool> {
        self.note(crate::replication::Touch::Principal(name.to_owned()));
        let mut table = self.tx.open_table(PRINCIPALS).map_err(storage)?;
        Ok(table.remove(name).map_err(storage)?.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(grants: &[(&str, Role)]) -> Principal {
        Principal {
            name: "agent".into(),
            secret_hash: String::new(),
            grants: grants
                .iter()
                .map(|(namespace, role)| Grant {
                    namespace: (*namespace).into(),
                    role: *role,
                })
                .collect(),
            owner: false,
            own_only: false,
            description: None,
            created_at: Timestamp::from_micros(0),
        }
    }

    #[test]
    fn grants_and_roles() {
        let p = principal(&[
            ("vault", Role::Write),
            ("team-*", Role::Read),
            ("team-ops", Role::Admin),
        ]);
        assert!(p.may("vault", Role::Read) && p.may("vault", Role::Write));
        assert!(!p.may("vault", Role::Admin));
        assert!(p.may("team-a", Role::Read) && !p.may("team-a", Role::Write));
        assert!(p.may("team-ops", Role::Admin), "the highest grant wins");
        assert!(
            !p.may("vault2", Role::Read),
            "exact names do not match prefixes"
        );
        assert!(!p.may("team", Role::Read));
        assert_eq!(p.role_in("other"), None);
        let all = principal(&[("*", Role::Read)]);
        assert!(all.may("anything", Role::Read));
        assert!(check_principal_name("ana@agents").is_ok());
        assert!(check_principal_name("a b").is_err());
        assert!(Role::parse("WRITE").is_ok() && Role::parse("root").is_err());
    }
}
