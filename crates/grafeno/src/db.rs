use std::collections::HashMap;
use std::path::Path as FsPath;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use grafeno_core::{Edge, EdgeId, Error, NewEdge, NewNode, Node, NodeId, Props, Result, Timestamp};
use redb::{
    Database, ReadOnlyTable, ReadTransaction, ReadableDatabase, ReadableTable,
    ReadableTableMetadata, TableDefinition, WriteTransaction,
};

use crate::algo::Shared;
use crate::index::{INDEX_ENTRIES, INDEXES, IndexKey};
use crate::memory::names_of;
use crate::stats::{
    NamespaceInfo, NamespaceStats, bump_edge_label, bump_namespace, bump_node_label, label_counts,
    namespaces,
};
use crate::store::{
    ADJ_IN, ADJ_OUT, AdjKey, AdjValue, CountKey, EDGE_LABEL_COUNTS, EDGES, HNSW_LINKS,
    LABEL_COUNTS, LabelKey, LinkKey, MEMORY, META, NAMES, NODE_LABELS, NODES, NS_COUNTS, POSTINGS,
    PostingKey, RecordKey, TEXT_LENGTHS, TEXT_STATS, TEXTS, VECTOR_META, VECTORS, adjacency,
    adjacency_value, check_label, check_namespace, decode, encode, label_page, nodes_with_label,
    read_record, scan_page, scan_records, storage,
};
use crate::{Hit, text, vector};

/// The version of the on-disk layout written by this build.
///
/// 1. Phases 1-3: graph, text and vector tables.
/// 2. Adds the `meta` table and the namespace and label counters.
/// 3. Adjacency entries carry the edge's times (`adjacency_out`/`_in`
///    replace `adj_out`/`_in`), and postings carry the text length
///    (`text_postings` replaces `postings`).
/// 4. Adds the `names` index (entity resolution) and the `memory` table
///    (importance and accesses).
/// 5. Adds property indexes (`indexes`, `index_entries`), empty until one
///    is declared.
pub const SCHEMA_VERSION: u64 = 5;

/// The tables replaced in version 3, by name (their types no longer matter).
const OLD_ADJ_OUT: TableDefinition<AdjKey, u128> = TableDefinition::new("adj_out");
const OLD_ADJ_IN: TableDefinition<AdjKey, u128> = TableDefinition::new("adj_in");
const OLD_POSTINGS: TableDefinition<PostingKey, u32> = TableDefinition::new("postings");

const SCHEMA_KEY: &str = "schema_version";

/// When a commit reaches the disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Durability {
    /// When `commit` returns (the default).
    #[default]
    Immediate,
    /// At the next [`Grafeno::flush`] (which a [`crate::Writer`] calls
    /// twice a second) or immediate commit. Much faster for many small
    /// writes; a crash loses the commits since the last flush, never part
    /// of one.
    Eventual,
}

/// A Grafeno database: one file (or memory) holding many namespaces.
///
/// It allows one writer and many readers at once; each reader sees a fixed
/// snapshot.
pub struct Grafeno {
    db: Database,
    eventual: AtomicBool,
    /// Commits with eventual durability since the last flush.
    unflushed: AtomicBool,
    /// The commit counter and the projections cached for algorithms.
    shared: Arc<Shared>,
}

impl Grafeno {
    /// Opens the file, creating it if it does not exist, and upgrades its
    /// layout if it was written by an older version.
    pub fn open(path: impl AsRef<FsPath>) -> Result<Self> {
        Self::init(Database::create(path).map_err(storage)?)
    }

    /// An in-memory database: for tests and short-lived agents.
    pub fn in_memory() -> Result<Self> {
        let db = Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .map_err(storage)?;
        Self::init(db)
    }

    fn init(db: Database) -> Result<Self> {
        let tx = db.begin_write().map_err(storage)?;
        // Tables must exist before a read transaction can open them.
        tx.open_table(NODES).map_err(storage)?;
        tx.open_table(EDGES).map_err(storage)?;
        tx.open_table(ADJ_OUT).map_err(storage)?;
        tx.open_table(ADJ_IN).map_err(storage)?;
        tx.open_table(NODE_LABELS).map_err(storage)?;
        tx.open_table(TEXTS).map_err(storage)?;
        tx.open_table(TEXT_LENGTHS).map_err(storage)?;
        tx.open_table(POSTINGS).map_err(storage)?;
        tx.open_table(TEXT_STATS).map_err(storage)?;
        tx.open_table(VECTORS).map_err(storage)?;
        tx.open_table(HNSW_LINKS).map_err(storage)?;
        tx.open_table(VECTOR_META).map_err(storage)?;
        tx.open_table(NS_COUNTS).map_err(storage)?;
        tx.open_table(LABEL_COUNTS).map_err(storage)?;
        tx.open_table(EDGE_LABEL_COUNTS).map_err(storage)?;
        tx.open_table(NAMES).map_err(storage)?;
        tx.open_table(MEMORY).map_err(storage)?;
        tx.open_table(INDEXES).map_err(storage)?;
        tx.open_table(INDEX_ENTRIES).map_err(storage)?;

        let stored = tx
            .open_table(META)
            .map_err(storage)?
            .get(SCHEMA_KEY)
            .map_err(storage)?
            .map(|guard| guard.value());
        let version = match stored {
            Some(version) => version,
            // Before version 2 there was no `meta` table: a database with
            // nodes but no version is from version 1.
            None if tx
                .open_table(NODES)
                .map_err(storage)?
                .len()
                .map_err(storage)?
                > 0 =>
            {
                1
            }
            None => SCHEMA_VERSION,
        };
        if version > SCHEMA_VERSION {
            return Err(Error::Invalid(format!(
                "the database uses layout version {version}, newer than this Grafeno \
                 (version {SCHEMA_VERSION}); upgrade Grafeno to open it"
            )));
        }
        if version < 2 {
            recount(&tx)?;
        }
        if version < 3 {
            migrate_to_3(&tx)?;
        }
        if version < 4 {
            migrate_to_4(&tx)?;
        }
        tx.open_table(META)
            .map_err(storage)?
            .insert(SCHEMA_KEY, SCHEMA_VERSION)
            .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(Self {
            db,
            eventual: AtomicBool::new(false),
            unflushed: AtomicBool::new(false),
            shared: Arc::default(),
        })
    }

    pub fn durability(&self) -> Durability {
        if self.eventual.load(Ordering::Relaxed) {
            Durability::Eventual
        } else {
            Durability::Immediate
        }
    }

    /// Sets the durability of later commits.
    pub fn set_durability(&self, durability: Durability) {
        self.eventual
            .store(durability == Durability::Eventual, Ordering::Relaxed);
    }

    /// Makes every commit so far durable. Cheap when there is nothing to do.
    pub fn flush(&self) -> Result<()> {
        if !self.unflushed.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        // An empty commit with immediate durability persists the ones before it.
        let tx = self.db.begin_write().map_err(storage)?;
        tx.commit().map_err(storage)
    }

    pub fn begin_read(&self) -> Result<ReadTx> {
        let (tx, generation) = self.shared.snapshot(|| self.db.begin_read());
        let tx = tx.map_err(storage)?;
        Ok(ReadTx {
            shared: self.shared.clone(),
            generation,
            nodes: tx.open_table(NODES).map_err(storage)?,
            edges: tx.open_table(EDGES).map_err(storage)?,
            out: tx.open_table(ADJ_OUT).map_err(storage)?,
            inn: tx.open_table(ADJ_IN).map_err(storage)?,
            labels: tx.open_table(NODE_LABELS).map_err(storage)?,
            texts: tx.open_table(TEXTS).map_err(storage)?,
            text_lengths: tx.open_table(TEXT_LENGTHS).map_err(storage)?,
            postings: tx.open_table(POSTINGS).map_err(storage)?,
            text_stats: tx.open_table(TEXT_STATS).map_err(storage)?,
            vectors: tx.open_table(VECTORS).map_err(storage)?,
            hnsw_links: tx.open_table(HNSW_LINKS).map_err(storage)?,
            vector_meta: tx.open_table(VECTOR_META).map_err(storage)?,
            ns_counts: tx.open_table(NS_COUNTS).map_err(storage)?,
            label_counts: tx.open_table(LABEL_COUNTS).map_err(storage)?,
            edge_label_counts: tx.open_table(EDGE_LABEL_COUNTS).map_err(storage)?,
            names: tx.open_table(NAMES).map_err(storage)?,
            memory: tx.open_table(MEMORY).map_err(storage)?,
            indexes: tx.open_table(INDEXES).map_err(storage)?,
            index_entries: tx.open_table(INDEX_ENTRIES).map_err(storage)?,
            _tx: tx,
        })
    }

    pub fn begin_write(&self) -> Result<WriteTx> {
        let mut tx = self.db.begin_write().map_err(storage)?;
        if self.eventual.load(Ordering::Relaxed) {
            tx.set_durability(redb::Durability::None).map_err(storage)?;
            self.unflushed.store(true, Ordering::Release);
        }
        Ok(WriteTx {
            tx,
            vectors: vector::Cache::default(),
            shared: self.shared.clone(),
        })
    }

    /// Runs `f` in a write transaction: commits if it returns `Ok`, discards
    /// everything if it returns `Err`.
    pub fn write<T>(&self, f: impl FnOnce(&mut WriteTx) -> Result<T>) -> Result<T> {
        let mut tx = self.begin_write()?;
        match f(&mut tx) {
            Ok(value) => {
                tx.commit()?;
                Ok(value)
            }
            Err(error) => {
                tx.abort()?;
                Err(error)
            }
        }
    }
}

/// Version 4: builds the names index from the node records. (The memory
/// table starts empty: every node has the default state.)
fn migrate_to_4(tx: &WriteTransaction) -> Result<()> {
    let mut names = tx.open_table(NAMES).map_err(storage)?;
    names.retain(|_, _| false).map_err(storage)?;
    for item in tx
        .open_table(NODES)
        .map_err(storage)?
        .iter()
        .map_err(storage)?
    {
        let (key, value) = item.map_err(storage)?;
        let (ns, id) = key.value();
        let node: Node = decode(value.value())?;
        for name in names_of(&node) {
            names.insert((ns, name.as_str(), id), ()).map_err(storage)?;
        }
    }
    Ok(())
}

/// Version 3: rebuilds the adjacency tables from the edge records and the
/// postings from the stored texts, then drops the old tables.
fn migrate_to_3(tx: &WriteTransaction) -> Result<()> {
    let mut out = tx.open_table(ADJ_OUT).map_err(storage)?;
    let mut inn = tx.open_table(ADJ_IN).map_err(storage)?;
    out.retain(|_, _| false).map_err(storage)?;
    inn.retain(|_, _| false).map_err(storage)?;
    for item in tx
        .open_table(EDGES)
        .map_err(storage)?
        .iter()
        .map_err(storage)?
    {
        let (key, value) = item.map_err(storage)?;
        let ns = key.value().0;
        let edge: Edge = decode(value.value())?;
        let (from, to, id) = (edge.from.as_u128(), edge.to.as_u128(), edge.id.as_u128());
        out.insert(
            (ns, from, edge.label.as_str(), id),
            adjacency_value(&edge, edge.to),
        )
        .map_err(storage)?;
        inn.insert(
            (ns, to, edge.label.as_str(), id),
            adjacency_value(&edge, edge.from),
        )
        .map_err(storage)?;
    }
    drop((out, inn));

    let mut postings = tx.open_table(POSTINGS).map_err(storage)?;
    postings.retain(|_, _| false).map_err(storage)?;
    for item in tx
        .open_table(TEXTS)
        .map_err(storage)?
        .iter()
        .map_err(storage)?
    {
        let (key, value) = item.map_err(storage)?;
        let (ns, id) = key.value();
        let terms = text::tokenize(value.value());
        let length = terms.len() as u32;
        for (term, frequency) in text::term_frequencies(&terms) {
            postings
                .insert((ns, term, id), (frequency, length))
                .map_err(storage)?;
        }
    }
    drop(postings);

    tx.delete_table(OLD_ADJ_OUT).map_err(storage)?;
    tx.delete_table(OLD_ADJ_IN).map_err(storage)?;
    tx.delete_table(OLD_POSTINGS).map_err(storage)?;
    Ok(())
}

/// Rebuilds every counter from the node and edge records.
fn recount(tx: &WriteTransaction) -> Result<()> {
    let mut namespaces: HashMap<String, (u64, u64)> = HashMap::new();
    let mut node_labels: HashMap<(String, String), u64> = HashMap::new();
    let mut edge_labels: HashMap<(String, String), u64> = HashMap::new();
    {
        let nodes = tx.open_table(NODES).map_err(storage)?;
        for item in nodes.iter().map_err(storage)? {
            let (key, value) = item.map_err(storage)?;
            let ns = key.value().0.to_owned();
            let node: Node = decode(value.value())?;
            namespaces.entry(ns.clone()).or_default().0 += 1;
            for label in node.labels {
                *node_labels.entry((ns.clone(), label)).or_default() += 1;
            }
        }
        let edges = tx.open_table(EDGES).map_err(storage)?;
        for item in edges.iter().map_err(storage)? {
            let (key, value) = item.map_err(storage)?;
            let ns = key.value().0.to_owned();
            let edge: Edge = decode(value.value())?;
            namespaces.entry(ns.clone()).or_default().1 += 1;
            *edge_labels.entry((ns, edge.label)).or_default() += 1;
        }
    }
    let mut table = tx.open_table(NS_COUNTS).map_err(storage)?;
    table.retain(|_, _| false).map_err(storage)?;
    for (ns, counts) in &namespaces {
        table.insert(ns.as_str(), *counts).map_err(storage)?;
    }
    drop(table);
    for (definition, counts) in [
        (LABEL_COUNTS, &node_labels),
        (EDGE_LABEL_COUNTS, &edge_labels),
    ] {
        let mut table = tx.open_table(definition).map_err(storage)?;
        table.retain(|_, _| false).map_err(storage)?;
        for ((ns, label), count) in counts {
            table
                .insert((ns.as_str(), label.as_str()), *count)
                .map_err(storage)?;
        }
    }
    Ok(())
}

impl Drop for Grafeno {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

/// A read transaction: a fixed snapshot of the database.
pub struct ReadTx {
    pub(crate) nodes: ReadOnlyTable<RecordKey, &'static [u8]>,
    pub(crate) edges: ReadOnlyTable<RecordKey, &'static [u8]>,
    pub(crate) out: ReadOnlyTable<AdjKey, AdjValue>,
    pub(crate) inn: ReadOnlyTable<AdjKey, AdjValue>,
    pub(crate) labels: ReadOnlyTable<LabelKey, ()>,
    pub(crate) texts: ReadOnlyTable<RecordKey, &'static str>,
    pub(crate) text_lengths: ReadOnlyTable<RecordKey, u32>,
    pub(crate) postings: ReadOnlyTable<PostingKey, (u32, u32)>,
    pub(crate) text_stats: ReadOnlyTable<&'static str, (u64, u64)>,
    pub(crate) vectors: ReadOnlyTable<RecordKey, &'static [u8]>,
    pub(crate) hnsw_links: ReadOnlyTable<LinkKey, &'static [u8]>,
    pub(crate) vector_meta: ReadOnlyTable<&'static str, &'static [u8]>,
    pub(crate) ns_counts: ReadOnlyTable<&'static str, (u64, u64)>,
    pub(crate) label_counts: ReadOnlyTable<CountKey, u64>,
    pub(crate) edge_label_counts: ReadOnlyTable<CountKey, u64>,
    pub(crate) names: ReadOnlyTable<LabelKey, ()>,
    pub(crate) memory: ReadOnlyTable<RecordKey, (f32, u32, i64)>,
    pub(crate) indexes: ReadOnlyTable<(&'static str, &'static str), &'static [u8]>,
    pub(crate) index_entries: ReadOnlyTable<IndexKey, ()>,
    pub(crate) shared: Arc<Shared>,
    /// The commit this snapshot sees, when known (for the projection cache).
    pub(crate) generation: Option<u64>,
    _tx: ReadTransaction,
}

impl ReadTx {
    pub fn node(&self, ns: &str, id: NodeId) -> Result<Option<Node>> {
        read_record(&self.nodes, ns, id.as_u128())
    }

    pub fn edge(&self, ns: &str, id: EdgeId) -> Result<Option<Edge>> {
        read_record(&self.edges, ns, id.as_u128())
    }

    /// Every node of the namespace. Prefer [`ReadTx::nodes_page`] for large ones.
    pub fn nodes(&self, ns: &str) -> Result<Vec<Node>> {
        scan_records(&self.nodes, ns)
    }

    /// Every edge of the namespace, including retracted ones. Prefer
    /// [`ReadTx::edges_page`] for large ones.
    pub fn edges(&self, ns: &str) -> Result<Vec<Edge>> {
        scan_records(&self.edges, ns)
    }

    /// Up to `limit` nodes after the id `after`, in id (creation) order, only
    /// those with `label` if given. Pass the last id back as `after` to get
    /// the next page.
    pub fn nodes_page(
        &self,
        ns: &str,
        label: Option<&str>,
        after: Option<NodeId>,
        limit: usize,
    ) -> Result<Vec<Node>> {
        let after = after.map(NodeId::as_u128);
        match label {
            None => scan_page(&self.nodes, ns, after, limit, |_: &Node| true),
            Some(label) => label_page(&self.labels, ns, label, after, limit)?
                .into_iter()
                .map(|id| self.node(ns, id)?.ok_or(Error::NodeNotFound(id)))
                .collect(),
        }
    }

    /// Up to `limit` edges after the id `after`, in id order, only those with
    /// `label` if given (filtering scans the namespace's edges).
    pub fn edges_page(
        &self,
        ns: &str,
        label: Option<&str>,
        after: Option<EdgeId>,
        limit: usize,
    ) -> Result<Vec<Edge>> {
        scan_page(
            &self.edges,
            ns,
            after.map(EdgeId::as_u128),
            limit,
            |edge: &Edge| label.is_none_or(|label| edge.label == label),
        )
    }

    pub fn nodes_with_label(&self, ns: &str, label: &str) -> Result<Vec<NodeId>> {
        nodes_with_label(&self.labels, ns, label)
    }

    /// Every namespace that has nodes, in name order.
    pub fn namespaces(&self) -> Result<Vec<NamespaceInfo>> {
        namespaces(&self.ns_counts)
    }

    pub fn stats(&self, ns: &str) -> Result<NamespaceStats> {
        let (nodes, edges) = self
            .ns_counts
            .get(ns)
            .map_err(storage)?
            .map(|guard| guard.value())
            .unwrap_or((0, 0));
        let (texts, text_terms) = self
            .text_stats
            .get(ns)
            .map_err(storage)?
            .map(|guard| guard.value())
            .unwrap_or((0, 0));
        let meta = vector::read_meta(&self.vector_meta, ns)?;
        let vectors = meta.as_ref().map_or(0, |meta| meta.count());
        Ok(NamespaceStats {
            name: ns.to_owned(),
            nodes,
            edges,
            node_labels: label_counts(&self.label_counts, ns)?,
            edge_labels: label_counts(&self.edge_label_counts, ns)?,
            texts,
            text_terms,
            vectors,
            vector_dimension: meta.filter(|_| vectors > 0).map(|meta| meta.dimension()),
        })
    }

    /// The indexed text of a node.
    pub fn text(&self, ns: &str, id: NodeId) -> Result<Option<String>> {
        text::text(&self.texts, ns, id)
    }

    /// The stored (normalized) embedding of a node.
    pub fn embedding(&self, ns: &str, id: NodeId) -> Result<Option<Vec<f32>>> {
        vector::embedding(&self.vectors, ns, id)
    }

    /// Full-text search ranked by BM25, best first.
    pub fn search_text(&self, ns: &str, query: &str, limit: usize) -> Result<Vec<Hit>> {
        text::search(&self.postings, &self.text_stats, ns, query, limit)
    }

    /// Approximate nearest neighbors by cosine similarity, best first.
    pub fn search_vector(&self, ns: &str, query: &[f32], limit: usize) -> Result<Vec<Hit>> {
        self.search_vector_with(ns, query, limit, (limit * 4).max(64))
    }

    /// Like [`ReadTx::search_vector`], with the size `ef` of the candidate
    /// list: higher is slower and finds more of the true nearest neighbors.
    pub fn search_vector_with(
        &self,
        ns: &str,
        query: &[f32],
        limit: usize,
        ef: usize,
    ) -> Result<Vec<Hit>> {
        vector::search(
            &self.vectors,
            &self.hnsw_links,
            &self.vector_meta,
            ns,
            query,
            limit,
            ef,
        )
    }
}

/// A write transaction. Nothing is visible to others until `commit`.
pub struct WriteTx {
    pub(crate) tx: WriteTransaction,
    /// Decoded vectors and links, shared by the vector writes of this transaction.
    pub(crate) vectors: vector::Cache,
    pub(crate) shared: Arc<Shared>,
}

impl WriteTx {
    pub fn commit(self) -> Result<()> {
        let Self { tx, shared, .. } = self;
        shared.committing(|| tx.commit().map_err(storage))
    }

    pub fn abort(self) -> Result<()> {
        self.tx.abort().map_err(storage)
    }

    pub fn node(&self, ns: &str, id: NodeId) -> Result<Option<Node>> {
        let table = self.tx.open_table(NODES).map_err(storage)?;
        read_record(&table, ns, id.as_u128())
    }

    pub fn edge(&self, ns: &str, id: EdgeId) -> Result<Option<Edge>> {
        let table = self.tx.open_table(EDGES).map_err(storage)?;
        read_record(&table, ns, id.as_u128())
    }

    /// [`ReadTx::nodes_page`] as written so far in this transaction.
    pub fn nodes_page(
        &self,
        ns: &str,
        label: Option<&str>,
        after: Option<NodeId>,
        limit: usize,
    ) -> Result<Vec<Node>> {
        let after = after.map(NodeId::as_u128);
        let nodes = self.tx.open_table(NODES).map_err(storage)?;
        match label {
            None => scan_page(&nodes, ns, after, limit, |_: &Node| true),
            Some(label) => {
                let labels = self.tx.open_table(NODE_LABELS).map_err(storage)?;
                label_page(&labels, ns, label, after, limit)?
                    .into_iter()
                    .map(|id| read_record(&nodes, ns, id.as_u128())?.ok_or(Error::NodeNotFound(id)))
                    .collect()
            }
        }
    }

    /// The indexed text of a node, as written so far in this transaction.
    pub fn text(&self, ns: &str, id: NodeId) -> Result<Option<String>> {
        let table = self.tx.open_table(TEXTS).map_err(storage)?;
        text::text(&table, ns, id)
    }

    /// The stored (normalized) embedding of a node, as written so far in
    /// this transaction.
    pub fn embedding(&self, ns: &str, id: NodeId) -> Result<Option<Vec<f32>>> {
        let table = self.tx.open_table(VECTORS).map_err(storage)?;
        vector::embedding(&table, ns, id)
    }

    pub fn create_node(&mut self, ns: &str, new: NewNode) -> Result<NodeId> {
        let id = NodeId::new();
        self.upsert_node(ns, id, new)?;
        Ok(id)
    }

    /// Creates the node with the given id, or replaces it entirely if it
    /// exists: labels, properties, sources, text and embedding (a missing
    /// text or embedding is removed). Its edges and `created_at` are kept.
    /// Returns whether the node was created.
    ///
    /// This lets a client keep its own ids and sync idempotently.
    pub fn upsert_node(&mut self, ns: &str, id: NodeId, new: NewNode) -> Result<bool> {
        check_namespace(ns)?;
        let labels = normalize_labels(new.labels)?;
        let now = Timestamp::now();
        let existing = self.node(ns, id)?;
        let node = Node {
            id,
            labels,
            props: new.props,
            sources: new.sources,
            created_at: existing.as_ref().map_or(now, |old| old.created_at),
            updated_at: now,
        };
        self.put_node(ns, &node, existing.as_ref())?;
        self.replace_indexes(ns, id, new.text.as_deref(), new.embedding.as_deref())?;
        Ok(existing.is_none())
    }

    /// Writes a node exactly as given, timestamps included, with its text and
    /// embedding (replacing any previous ones). Used to import exports.
    pub fn restore_node(
        &mut self,
        ns: &str,
        mut node: Node,
        text: Option<&str>,
        embedding: Option<&[f32]>,
    ) -> Result<()> {
        check_namespace(ns)?;
        node.labels = normalize_labels(node.labels)?;
        let existing = self.node(ns, node.id)?;
        self.put_node(ns, &node, existing.as_ref())?;
        self.replace_indexes(ns, node.id, text, embedding)
    }

    fn replace_indexes(
        &mut self,
        ns: &str,
        id: NodeId,
        text: Option<&str>,
        embedding: Option<&[f32]>,
    ) -> Result<()> {
        match text {
            Some(text) => text::index(&self.tx, ns, id, text)?,
            None => {
                text::remove(&self.tx, ns, id)?;
            }
        }
        match embedding {
            Some(embedding) => vector::insert(&self.tx, &mut self.vectors, ns, id, embedding)?,
            None => {
                vector::remove(&self.tx, &mut self.vectors, ns, id)?;
            }
        }
        Ok(())
    }

    /// Merges `props` into the node; a `null` value removes the property.
    pub fn set_node_props(&mut self, ns: &str, id: NodeId, props: Props) -> Result<()> {
        let mut node = self.node(ns, id)?.ok_or(Error::NodeNotFound(id))?;
        let previous = node.clone();
        merge_props(&mut node.props, props);
        node.updated_at = Timestamp::now();
        self.put_node(ns, &node, Some(&previous))
    }

    pub fn add_label(&mut self, ns: &str, id: NodeId, label: &str) -> Result<()> {
        check_label(label)?;
        let mut node = self.node(ns, id)?.ok_or(Error::NodeNotFound(id))?;
        if let Err(at) = node
            .labels
            .binary_search_by(|item| item.as_str().cmp(label))
        {
            let previous = node.clone();
            node.labels.insert(at, label.to_owned());
            node.updated_at = Timestamp::now();
            self.put_node(ns, &node, Some(&previous))?;
        }
        Ok(())
    }

    pub fn remove_label(&mut self, ns: &str, id: NodeId, label: &str) -> Result<()> {
        let mut node = self.node(ns, id)?.ok_or(Error::NodeNotFound(id))?;
        if let Ok(at) = node
            .labels
            .binary_search_by(|item| item.as_str().cmp(label))
        {
            if node.labels.len() == 1 {
                return Err(Error::Invalid("a node needs at least one label".into()));
            }
            let previous = node.clone();
            node.labels.remove(at);
            node.updated_at = Timestamp::now();
            self.put_node(ns, &node, Some(&previous))?;
        }
        Ok(())
    }

    /// Indexes `text` for full-text search, replacing the previous text.
    pub fn set_text(&mut self, ns: &str, id: NodeId, text: &str) -> Result<()> {
        self.node(ns, id)?.ok_or(Error::NodeNotFound(id))?;
        text::index(&self.tx, ns, id, text)
    }

    /// Indexes the texts of many nodes at once, replacing any they had: for
    /// loading, several times faster than one [`WriteTx::set_text`] each.
    pub fn set_texts(&mut self, ns: &str, texts: &[(NodeId, String)]) -> Result<()> {
        for (node, _) in texts {
            self.node(ns, *node)?.ok_or(Error::NodeNotFound(*node))?;
        }
        text::index_many(&self.tx, ns, texts)
    }

    /// Removes the node's text from the index. Returns whether it had any.
    pub fn remove_text(&mut self, ns: &str, id: NodeId) -> Result<bool> {
        text::remove(&self.tx, ns, id)
    }

    /// Indexes `embedding` for vector search, replacing the previous one. All
    /// embeddings of a namespace must have the same dimension.
    pub fn set_embedding(&mut self, ns: &str, id: NodeId, embedding: &[f32]) -> Result<()> {
        self.node(ns, id)?.ok_or(Error::NodeNotFound(id))?;
        vector::insert(&self.tx, &mut self.vectors, ns, id, embedding)
    }

    /// Removes the node's embedding from the index. Returns whether it had one.
    pub fn remove_embedding(&mut self, ns: &str, id: NodeId) -> Result<bool> {
        vector::remove(&self.tx, &mut self.vectors, ns, id)
    }

    /// Deletes the node, its edges and its indexed text and embedding, without
    /// keeping history.
    pub fn delete_node(&mut self, ns: &str, id: NodeId) -> Result<()> {
        let node = self.node(ns, id)?.ok_or(Error::NodeNotFound(id))?;
        let mut edges: Vec<EdgeId> = Vec::new();
        for definition in [ADJ_OUT, ADJ_IN] {
            let table = self.tx.open_table(definition).map_err(storage)?;
            edges.extend(
                adjacency(&table, ns, id, None)?
                    .into_iter()
                    .map(|entry| entry.edge),
            );
        }
        edges.sort();
        edges.dedup();
        for edge in edges {
            self.delete_edge(ns, edge)?;
        }
        text::remove(&self.tx, ns, id)?;
        vector::remove(&self.tx, &mut self.vectors, ns, id)?;
        self.index_node(ns, None, Some(&node))?;
        let mut names = self.tx.open_table(NAMES).map_err(storage)?;
        for name in names_of(&node) {
            names
                .remove((ns, name.as_str(), id.as_u128()))
                .map_err(storage)?;
        }
        drop(names);
        self.tx
            .open_table(MEMORY)
            .map_err(storage)?
            .remove((ns, id.as_u128()))
            .map_err(storage)?;
        let mut labels = self.tx.open_table(NODE_LABELS).map_err(storage)?;
        for label in &node.labels {
            labels
                .remove((ns, label.as_str(), id.as_u128()))
                .map_err(storage)?;
        }
        drop(labels);
        for label in &node.labels {
            bump_node_label(&self.tx, ns, label, -1)?;
        }
        let mut nodes = self.tx.open_table(NODES).map_err(storage)?;
        nodes.remove((ns, id.as_u128())).map_err(storage)?;
        drop(nodes);
        bump_namespace(&self.tx, ns, -1, 0)
    }

    pub fn create_edge(&mut self, ns: &str, new: NewEdge) -> Result<EdgeId> {
        let edge = Edge {
            id: EdgeId::new(),
            label: new.label,
            from: new.from,
            to: new.to,
            props: new.props,
            sources: new.sources,
            valid_from: new.valid_from,
            valid_to: new.valid_to,
            recorded_at: Timestamp::now(),
            expired_at: None,
        };
        let id = edge.id;
        self.restore_edge(ns, edge)?;
        Ok(id)
    }

    /// Writes an edge exactly as given, timestamps included, replacing any
    /// edge with its id. Both ends must exist. Used to import exports.
    pub fn restore_edge(&mut self, ns: &str, edge: Edge) -> Result<()> {
        check_namespace(ns)?;
        check_label(&edge.label)?;
        check_interval(edge.valid_from, edge.valid_to)?;
        for end in [edge.from, edge.to] {
            if self.node(ns, end)?.is_none() {
                return Err(Error::NodeNotFound(end));
            }
        }
        if self.edge(ns, edge.id)?.is_some() {
            self.delete_edge(ns, edge.id)?;
        }
        self.put_edge(ns, &edge)?;
        bump_namespace(&self.tx, ns, 0, 1)?;
        bump_edge_label(&self.tx, ns, &edge.label, 1)
    }

    /// Merges `props` into the edge; a `null` value removes the property.
    pub fn set_edge_props(&mut self, ns: &str, id: EdgeId, props: Props) -> Result<()> {
        let mut edge = self.edge(ns, id)?.ok_or(Error::EdgeNotFound(id))?;
        merge_props(&mut edge.props, props);
        self.put_edge(ns, &edge)
    }

    /// The fact stopped being true at `at`: the world changed, not what we know.
    pub fn invalidate_edge(&mut self, ns: &str, id: EdgeId, at: Timestamp) -> Result<()> {
        let mut edge = self.edge(ns, id)?.ok_or(Error::EdgeNotFound(id))?;
        check_interval(edge.valid_from, Some(at))?;
        edge.valid_to = Some(at);
        self.put_edge(ns, &edge)
    }

    /// The fact was a mistake. It stays in history, visible with
    /// `EdgeFilter::known_at` at an earlier instant.
    pub fn retract_edge(&mut self, ns: &str, id: EdgeId) -> Result<()> {
        let mut edge = self.edge(ns, id)?.ok_or(Error::EdgeNotFound(id))?;
        if edge.expired_at.is_none() {
            edge.expired_at = Some(Timestamp::now());
            self.put_edge(ns, &edge)?;
        }
        Ok(())
    }

    /// Deletes the outgoing edges of `from` (only those with `label`, if
    /// given), without keeping history. Returns how many were deleted.
    ///
    /// Useful to replace derived edges, such as a note's links, on each sync.
    pub fn delete_edges_from(
        &mut self,
        ns: &str,
        from: NodeId,
        label: Option<&str>,
    ) -> Result<usize> {
        let edges = {
            let table = self.tx.open_table(ADJ_OUT).map_err(storage)?;
            adjacency(&table, ns, from, label)?
        };
        for entry in &edges {
            self.delete_edge(ns, entry.edge)?;
        }
        Ok(edges.len())
    }

    /// Deletes the edge without keeping history.
    pub fn delete_edge(&mut self, ns: &str, id: EdgeId) -> Result<()> {
        let edge = self.edge(ns, id)?.ok_or(Error::EdgeNotFound(id))?;
        let (from, to, edge_id) = (edge.from.as_u128(), edge.to.as_u128(), id.as_u128());
        let label = edge.label.as_str();
        self.tx
            .open_table(ADJ_OUT)
            .map_err(storage)?
            .remove((ns, from, label, edge_id))
            .map_err(storage)?;
        self.tx
            .open_table(ADJ_IN)
            .map_err(storage)?
            .remove((ns, to, label, edge_id))
            .map_err(storage)?;
        self.tx
            .open_table(EDGES)
            .map_err(storage)?
            .remove((ns, edge_id))
            .map_err(storage)?;
        bump_namespace(&self.tx, ns, 0, -1)?;
        bump_edge_label(&self.tx, ns, label, -1)
    }

    /// Writes the node record and brings the label index, the names index and
    /// the counters from `previous` (`None`: a new node) to `node`.
    pub(crate) fn put_node(
        &mut self,
        ns: &str,
        node: &Node,
        previous: Option<&Node>,
    ) -> Result<()> {
        let bytes = encode(node)?;
        let id = node.id.as_u128();
        self.tx
            .open_table(NODES)
            .map_err(storage)?
            .insert((ns, id), bytes.as_slice())
            .map_err(storage)?;

        self.index_node(ns, Some(node), previous)?;

        let old_names = previous.map(names_of).unwrap_or_default();
        let new_names = names_of(node);
        let mut names = self.tx.open_table(NAMES).map_err(storage)?;
        for name in old_names.difference(&new_names) {
            names.remove((ns, name.as_str(), id)).map_err(storage)?;
        }
        for name in new_names.difference(&old_names) {
            names.insert((ns, name.as_str(), id), ()).map_err(storage)?;
        }
        drop(names);

        let old_labels = previous.map(|old| old.labels.as_slice());
        let old = old_labels.unwrap_or_default();
        let removed: Vec<&String> = old.iter().filter(|l| !node.labels.contains(l)).collect();
        let added: Vec<&String> = node.labels.iter().filter(|l| !old.contains(l)).collect();
        let mut index = self.tx.open_table(NODE_LABELS).map_err(storage)?;
        for label in &removed {
            index.remove((ns, label.as_str(), id)).map_err(storage)?;
        }
        for label in &added {
            index
                .insert((ns, label.as_str(), id), ())
                .map_err(storage)?;
        }
        drop(index);
        for label in removed {
            bump_node_label(&self.tx, ns, label, -1)?;
        }
        for label in added {
            bump_node_label(&self.tx, ns, label, 1)?;
        }
        if old_labels.is_none() {
            bump_namespace(&self.tx, ns, 1, 0)?;
        }
        Ok(())
    }

    /// Writes the edge record and its adjacency entries (no counters).
    pub(crate) fn put_edge(&mut self, ns: &str, edge: &Edge) -> Result<()> {
        let bytes = encode(edge)?;
        let (from, to, id) = (edge.from.as_u128(), edge.to.as_u128(), edge.id.as_u128());
        let label = edge.label.as_str();
        self.tx
            .open_table(EDGES)
            .map_err(storage)?
            .insert((ns, id), bytes.as_slice())
            .map_err(storage)?;
        self.tx
            .open_table(ADJ_OUT)
            .map_err(storage)?
            .insert((ns, from, label, id), adjacency_value(edge, edge.to))
            .map_err(storage)?;
        self.tx
            .open_table(ADJ_IN)
            .map_err(storage)?
            .insert((ns, to, label, id), adjacency_value(edge, edge.from))
            .map_err(storage)?;
        Ok(())
    }
}

fn normalize_labels(labels: Vec<String>) -> Result<Vec<String>> {
    if labels.is_empty() {
        return Err(Error::Invalid("a node needs at least one label".into()));
    }
    labels.iter().try_for_each(|label| check_label(label))?;
    let mut labels = labels;
    labels.sort();
    labels.dedup();
    Ok(labels)
}

fn merge_props(target: &mut Props, changes: Props) {
    for (key, value) in changes {
        if value.is_null() {
            target.remove(&key);
        } else {
            target.insert(key, value);
        }
    }
}

pub(crate) fn check_interval(from: Option<Timestamp>, to: Option<Timestamp>) -> Result<()> {
    if let (Some(from), Some(to)) = (from, to)
        && to <= from
    {
        return Err(Error::Invalid("valid_to must be after valid_from".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafeno_core::NewNode;

    #[test]
    fn version_1_databases_get_counters() {
        let path = std::env::temp_dir().join(format!("grafeno-migration-{}.redb", NodeId::new()));
        {
            let db = Grafeno::open(&path).unwrap();
            db.write(|tx| {
                let a = tx.create_node("ns", NewNode::new("Person").label("Admin"))?;
                let b = tx.create_node("ns", NewNode::new("Person"))?;
                tx.create_edge("ns", NewEdge::new(a, "knows", b))?;
                tx.create_node("other", NewNode::new("Note"))?;
                Ok(())
            })
            .unwrap();
        }
        {
            // Turn it into a version 1 database: no `meta` and no counters.
            let db = Database::create(&path).unwrap();
            let tx = db.begin_write().unwrap();
            tx.delete_table(META).unwrap();
            tx.delete_table(NS_COUNTS).unwrap();
            tx.delete_table(LABEL_COUNTS).unwrap();
            tx.delete_table(EDGE_LABEL_COUNTS).unwrap();
            tx.commit().unwrap();
        }
        {
            let db = Grafeno::open(&path).unwrap();
            let read = db.begin_read().unwrap();
            let names: Vec<(String, u64, u64)> = read
                .namespaces()
                .unwrap()
                .into_iter()
                .map(|info| (info.name, info.nodes, info.edges))
                .collect();
            assert_eq!(names, vec![("ns".into(), 2, 1), ("other".into(), 1, 0)]);
            let stats = read.stats("ns").unwrap();
            assert_eq!(stats.node_labels["Person"], 2);
            assert_eq!(stats.node_labels["Admin"], 1);
            assert_eq!(stats.edge_labels["knows"], 1);
        }
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn version_2_databases_get_new_adjacency_and_postings() {
        let path = std::env::temp_dir().join(format!("grafeno-v2-{}.redb", NodeId::new()));
        let (a, b) = {
            let db = Grafeno::open(&path).unwrap();
            db.write(|tx| {
                let a = tx.create_node("ns", NewNode::new("Note").text("alpha beta beta"))?;
                let b = tx.create_node("ns", NewNode::new("Note").text("gamma"))?;
                let edge = tx.create_edge("ns", NewEdge::new(a, "links_to", b))?;
                tx.create_edge("ns", NewEdge::new(b, "links_to", a))?;
                tx.retract_edge("ns", edge)?;
                Ok((a, b))
            })
            .unwrap()
        };
        {
            // Turn it into a version 2 database: the old tables, no new ones.
            let db = Database::create(&path).unwrap();
            let tx = db.begin_write().unwrap();
            tx.delete_table(ADJ_OUT).unwrap();
            tx.delete_table(ADJ_IN).unwrap();
            tx.delete_table(POSTINGS).unwrap();
            tx.open_table(OLD_ADJ_OUT)
                .unwrap()
                .insert(("ns", 1, "x", 2), 3)
                .unwrap();
            tx.open_table(OLD_ADJ_IN).unwrap();
            tx.open_table(OLD_POSTINGS)
                .unwrap()
                .insert(("ns", "alpha", 1), 1)
                .unwrap();
            tx.open_table(META).unwrap().insert(SCHEMA_KEY, 2).unwrap();
            tx.commit().unwrap();
        }
        {
            let db = Grafeno::open(&path).unwrap();
            let read = db.begin_read().unwrap();
            let report = read.check("ns").unwrap();
            assert!(report.is_ok(), "{:?}", report.problems);
            let out = read
                .neighbors("ns", b, crate::Direction::Out, &crate::EdgeFilter::any())
                .unwrap();
            assert_eq!(out[0].node, a);
            let retracted = read
                .neighbors("ns", a, crate::Direction::Out, &crate::EdgeFilter::any())
                .unwrap();
            assert!(
                retracted.is_empty(),
                "the retraction survives the migration"
            );
            assert_eq!(read.search_text("ns", "beta", 5).unwrap()[0].node, a);
        }
        {
            let db = Database::create(&path).unwrap();
            let tx = db.begin_read().unwrap();
            let names: Vec<String> = tx
                .list_tables()
                .unwrap()
                .map(|t| redb::TableHandle::name(&t).to_owned())
                .collect();
            for old in ["adj_out", "adj_in", "postings"] {
                assert!(!names.iter().any(|name| name == old), "{old} was dropped");
            }
            assert_eq!(
                tx.open_table(META)
                    .unwrap()
                    .get(SCHEMA_KEY)
                    .unwrap()
                    .unwrap()
                    .value(),
                SCHEMA_VERSION
            );
        }
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn version_3_databases_get_a_names_index() {
        let path = std::env::temp_dir().join(format!("grafeno-v3-{}.redb", NodeId::new()));
        let ana = {
            let db = Grafeno::open(&path).unwrap();
            db.write(|tx| tx.create_node("ns", NewNode::new("Person").prop("name", "Ana García")))
                .unwrap()
        };
        {
            let db = Database::create(&path).unwrap();
            let tx = db.begin_write().unwrap();
            tx.delete_table(NAMES).unwrap();
            tx.delete_table(MEMORY).unwrap();
            tx.open_table(META).unwrap().insert(SCHEMA_KEY, 3).unwrap();
            tx.commit().unwrap();
        }
        {
            let db = Grafeno::open(&path).unwrap();
            let read = db.begin_read().unwrap();
            assert_eq!(read.find_by_name("ns", "ana garcia").unwrap(), vec![ana]);
            assert!(read.check("ns").unwrap().is_ok());
        }
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn newer_layouts_are_refused() {
        let path = std::env::temp_dir().join(format!("grafeno-future-{}.redb", NodeId::new()));
        drop(Grafeno::open(&path).unwrap());
        {
            let db = Database::create(&path).unwrap();
            let tx = db.begin_write().unwrap();
            tx.open_table(META)
                .unwrap()
                .insert(SCHEMA_KEY, SCHEMA_VERSION + 1)
                .unwrap();
            tx.commit().unwrap();
        }
        assert!(matches!(Grafeno::open(&path), Err(Error::Invalid(_))));
        std::fs::remove_file(&path).unwrap();
    }
}
