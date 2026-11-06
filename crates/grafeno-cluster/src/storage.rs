//! What a member's Raft keeps across restarts, in a redb file of its own
//! (the data file has one writer, the state machine): the hard state, the
//! log entries, and the last snapshot's metadata and file.

use std::path::{Path, PathBuf};

use grafeno_raft::{Entry, HardState, Index, SnapshotMeta};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

use crate::Error;

const LOG: TableDefinition<u64, &[u8]> = TableDefinition::new("raft_log");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("raft_meta");
const HARD: &str = "hard_state";
const SNAPSHOT: &str = "snapshot";
const SNAPSHOT_FILE: &str = "snapshot_file";

fn storage(error: impl std::fmt::Display) -> Error {
    Error::Storage(error.to_string())
}

fn json<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(value).map_err(storage)
}

fn parse<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(bytes).map_err(storage)
}

/// What was persisted, as loaded at start.
pub struct Loaded {
    pub hard: HardState,
    pub snapshot: Option<SnapshotMeta>,
    pub snapshot_file: Option<PathBuf>,
    pub log: Vec<Entry>,
}

impl Loaded {
    /// Nothing was ever persisted: a member that never started.
    pub fn is_new(&self) -> bool {
        self.hard == HardState::default() && self.snapshot.is_none() && self.log.is_empty()
    }
}

pub struct Storage {
    db: Database,
}

impl Storage {
    /// The Raft state in a file (created if missing).
    pub fn open(path: &Path) -> Result<Self, Error> {
        Self::init(Database::create(path).map_err(storage)?)
    }

    /// The Raft state in memory (tests; lost when dropped).
    pub fn in_memory() -> Result<Self, Error> {
        Self::init(
            Database::builder()
                .create_with_backend(redb::backends::InMemoryBackend::new())
                .map_err(storage)?,
        )
    }

    fn init(db: Database) -> Result<Self, Error> {
        let tx = db.begin_write().map_err(storage)?;
        tx.open_table(LOG).map_err(storage)?;
        tx.open_table(META).map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(Self { db })
    }

    pub fn load(&self) -> Result<Loaded, Error> {
        let tx = self.db.begin_read().map_err(storage)?;
        let meta = tx.open_table(META).map_err(storage)?;
        let get = |key: &str| -> Result<Option<Vec<u8>>, Error> {
            Ok(meta
                .get(key)
                .map_err(storage)?
                .map(|guard| guard.value().to_vec()))
        };
        let hard = get(HARD)?
            .map(|b| parse(&b))
            .transpose()?
            .unwrap_or_default();
        let snapshot = get(SNAPSHOT)?.map(|b| parse(&b)).transpose()?;
        let snapshot_file =
            get(SNAPSHOT_FILE)?.map(|b| PathBuf::from(String::from_utf8_lossy(&b).into_owned()));
        let mut log = Vec::new();
        for item in tx
            .open_table(LOG)
            .map_err(storage)?
            .iter()
            .map_err(storage)?
        {
            let (_, value) = item.map_err(storage)?;
            log.push(parse(value.value())?);
        }
        Ok(Loaded {
            hard,
            snapshot,
            snapshot_file,
            log,
        })
    }

    /// Persists what a `Ready` asks for, in one durable transaction.
    pub fn persist(
        &self,
        hard: Option<&HardState>,
        truncate_from: Option<Index>,
        entries: &[Entry],
        snapshot: Option<(&SnapshotMeta, &Path)>,
    ) -> Result<(), Error> {
        if hard.is_none() && truncate_from.is_none() && entries.is_empty() && snapshot.is_none() {
            return Ok(());
        }
        let tx = self.db.begin_write().map_err(storage)?;
        {
            let mut meta = tx.open_table(META).map_err(storage)?;
            let mut log = tx.open_table(LOG).map_err(storage)?;
            if let Some(hard) = hard {
                meta.insert(HARD, json(hard)?.as_slice()).map_err(storage)?;
            }
            if let Some((snapshot, file)) = snapshot {
                meta.insert(SNAPSHOT, json(snapshot)?.as_slice())
                    .map_err(storage)?;
                meta.insert(SNAPSHOT_FILE, file.to_string_lossy().as_bytes())
                    .map_err(storage)?;
                log.retain(|index, _| index > snapshot.index)
                    .map_err(storage)?;
            }
            if let Some(from) = truncate_from {
                log.retain(|index, _| index < from).map_err(storage)?;
            }
            for entry in entries {
                log.insert(entry.index, json(entry)?.as_slice())
                    .map_err(storage)?;
            }
        }
        tx.commit().map_err(storage)
    }
}
