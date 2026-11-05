//! Online backups: a consistent copy of the whole database, written from a
//! read snapshot while writes go on.

use std::path::Path;

use grafeno_core::{Error, Result};
use redb::{
    Database, ReadTransaction, ReadableTable, TableDefinition, TableHandle, WriteTransaction,
};
use serde::Serialize;

use crate::access::PRINCIPALS;
use crate::constraints::CONSTRAINTS;
use crate::db::ReadTx;
use crate::index::{INDEX_ENTRIES, INDEXES};
use crate::merge::MERGES;
use crate::namespace::NAMESPACES;
use crate::resolve::MATCH_RULES;
use crate::state::STATE;
use crate::store::{
    ADJ_IN, ADJ_OUT, EDGE_LABEL_COUNTS, EDGES, HNSW_LINKS, LABEL_COUNTS, MEMORY, META, NAMES,
    NODE_LABELS, NODES, NS_COUNTS, POSTINGS, TEXT_LENGTHS, TEXT_STATS, TEXTS, VECTOR_META, VECTORS,
    storage,
};

/// What a backup copied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct BackupStats {
    pub tables: usize,
    pub entries: u64,
    pub bytes: u64,
}

fn copy<K: redb::Key + 'static, V: redb::Value + 'static>(
    from: &ReadTransaction,
    to: &WriteTransaction,
    definition: TableDefinition<K, V>,
) -> Result<u64> {
    let source = from.open_table(definition).map_err(storage)?;
    let mut target = to.open_table(definition).map_err(storage)?;
    let mut entries = 0;
    for item in source.iter().map_err(storage)? {
        let (key, value) = item.map_err(storage)?;
        target.insert(key.value(), value.value()).map_err(storage)?;
        entries += 1;
    }
    Ok(entries)
}

impl ReadTx {
    /// Writes a copy of the whole database, as this snapshot sees it, to a
    /// new file at `path` (which must not exist). Writers are not blocked;
    /// the copy is a valid database, opened like any other. It is written
    /// next to `path` and renamed into place when complete, so a failed
    /// backup leaves no partial file.
    pub fn backup(&self, path: &Path) -> Result<BackupStats> {
        if path.exists() {
            return Err(Error::Invalid(format!("{} exists already", path.display())));
        }
        let partial = path.with_extension("partial");
        let _ = std::fs::remove_file(&partial);
        let outcome = (|| {
            let db = Database::create(&partial).map_err(storage)?;
            let tx = db.begin_write().map_err(storage)?;
            let from = &self.tx;
            // A table added later and forgotten here would be lost: refuse.
            let known: Vec<&str> = vec![
                META.name(),
                NODES.name(),
                EDGES.name(),
                ADJ_OUT.name(),
                ADJ_IN.name(),
                NODE_LABELS.name(),
                TEXTS.name(),
                TEXT_LENGTHS.name(),
                POSTINGS.name(),
                TEXT_STATS.name(),
                VECTORS.name(),
                HNSW_LINKS.name(),
                VECTOR_META.name(),
                NS_COUNTS.name(),
                LABEL_COUNTS.name(),
                EDGE_LABEL_COUNTS.name(),
                NAMES.name(),
                MEMORY.name(),
                INDEXES.name(),
                INDEX_ENTRIES.name(),
                NAMESPACES.name(),
                PRINCIPALS.name(),
                MERGES.name(),
                MATCH_RULES.name(),
                STATE.name(),
                CONSTRAINTS.name(),
            ];
            for table in from.list_tables().map_err(storage)? {
                if !known.contains(&table.name()) {
                    return Err(Error::Storage(format!(
                        "the backup does not know the table {:?}",
                        table.name()
                    )));
                }
            }
            let mut entries = 0;
            entries += copy(from, &tx, META)?;
            entries += copy(from, &tx, NODES)?;
            entries += copy(from, &tx, EDGES)?;
            entries += copy(from, &tx, ADJ_OUT)?;
            entries += copy(from, &tx, ADJ_IN)?;
            entries += copy(from, &tx, NODE_LABELS)?;
            entries += copy(from, &tx, TEXTS)?;
            entries += copy(from, &tx, TEXT_LENGTHS)?;
            entries += copy(from, &tx, POSTINGS)?;
            entries += copy(from, &tx, TEXT_STATS)?;
            entries += copy(from, &tx, VECTORS)?;
            entries += copy(from, &tx, HNSW_LINKS)?;
            entries += copy(from, &tx, VECTOR_META)?;
            entries += copy(from, &tx, NS_COUNTS)?;
            entries += copy(from, &tx, LABEL_COUNTS)?;
            entries += copy(from, &tx, EDGE_LABEL_COUNTS)?;
            entries += copy(from, &tx, NAMES)?;
            entries += copy(from, &tx, MEMORY)?;
            entries += copy(from, &tx, INDEXES)?;
            entries += copy(from, &tx, INDEX_ENTRIES)?;
            entries += copy(from, &tx, NAMESPACES)?;
            entries += copy(from, &tx, PRINCIPALS)?;
            entries += copy(from, &tx, MERGES)?;
            entries += copy(from, &tx, MATCH_RULES)?;
            entries += copy(from, &tx, STATE)?;
            entries += copy(from, &tx, CONSTRAINTS)?;
            tx.commit().map_err(storage)?;
            drop(db);
            Ok(entries)
        })();
        let entries = match outcome {
            Ok(entries) => entries,
            Err(error) => {
                let _ = std::fs::remove_file(&partial);
                return Err(error);
            }
        };
        std::fs::rename(&partial, path).map_err(|error| {
            Error::Storage(format!("cannot move the backup into place: {error}"))
        })?;
        let bytes = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        Ok(BackupStats {
            tables: 26,
            entries,
            bytes,
        })
    }
}
