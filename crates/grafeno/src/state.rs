//! A small key-value store per namespace for the state of features built
//! on the engine (what extraction has read, a review queue): `(namespace,
//! kind, key) -> bytes`, written in the same transactions as the graph,
//! backed up with it, dropped with its namespace, not exported.

use grafeno_core::Result;
use redb::{ReadableTable, TableDefinition};

use crate::db::{ReadTx, WriteTx};
use crate::store::storage;

pub(crate) type StateKey = (&'static str, &'static str, &'static str);

/// `(ns, kind, key) -> bytes`.
pub(crate) const STATE: TableDefinition<StateKey, &[u8]> = TableDefinition::new("state");

fn entries(
    table: &impl ReadableTable<StateKey, &'static [u8]>,
    ns: &str,
    kind: &str,
    prefix: &str,
    limit: usize,
) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    for item in table.range((ns, kind, prefix)..).map_err(storage)? {
        let (key, value) = item.map_err(storage)?;
        let (key_ns, key_kind, key) = key.value();
        if key_ns != ns || key_kind != kind || !key.starts_with(prefix) || out.len() >= limit {
            break;
        }
        out.push((key.to_owned(), value.value().to_vec()));
    }
    Ok(out)
}

impl ReadTx {
    pub fn state(&self, ns: &str, kind: &str, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .state
            .get((ns, kind, key))
            .map_err(storage)?
            .map(|guard| guard.value().to_vec()))
    }

    /// Up to `limit` entries of `kind` whose key starts with `prefix`, in
    /// key order.
    pub fn state_entries(
        &self,
        ns: &str,
        kind: &str,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        entries(&self.state, ns, kind, prefix, limit)
    }
}

impl WriteTx {
    pub fn state(&self, ns: &str, kind: &str, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .tx
            .open_table(STATE)
            .map_err(storage)?
            .get((ns, kind, key))
            .map_err(storage)?
            .map(|guard| guard.value().to_vec()))
    }

    pub fn state_entries(
        &self,
        ns: &str,
        kind: &str,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        entries(
            &self.tx.open_table(STATE).map_err(storage)?,
            ns,
            kind,
            prefix,
            limit,
        )
    }

    pub fn put_state(&mut self, ns: &str, kind: &str, key: &str, value: &[u8]) -> Result<()> {
        crate::namespace::ensure(&self.tx, ns)?;
        self.tx
            .open_table(STATE)
            .map_err(storage)?
            .insert((ns, kind, key), value)
            .map_err(storage)?;
        Ok(())
    }

    /// Removes an entry; whether it was there.
    pub fn delete_state(&mut self, ns: &str, kind: &str, key: &str) -> Result<bool> {
        Ok(self
            .tx
            .open_table(STATE)
            .map_err(storage)?
            .remove((ns, kind, key))
            .map_err(storage)?
            .is_some())
    }
}
