use std::path::Path as FsPath;

use grafeno_core::{Edge, EdgeId, Error, NewEdge, NewNode, Node, NodeId, Props, Result, Timestamp};
use redb::{Database, ReadOnlyTable, ReadTransaction, ReadableDatabase, WriteTransaction};

use crate::store::{
    ADJ_IN, ADJ_OUT, AdjKey, EDGES, HNSW_LINKS, LabelKey, LinkKey, NODE_LABELS, NODES, POSTINGS,
    PostingKey, RecordKey, TEXT_LENGTHS, TEXT_STATS, TEXTS, VECTOR_META, VECTORS, adjacency,
    check_label, check_namespace, encode, nodes_with_label, read_record, scan_records, storage,
};
use crate::{Hit, text, vector};

/// A Grafeno database: one file (or memory) holding many namespaces.
///
/// It allows one writer and many readers at once; each reader sees a fixed
/// snapshot.
pub struct Grafeno {
    db: Database,
}

impl Grafeno {
    /// Opens the file, creating it if it does not exist.
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
        // Tables must exist before a read transaction can open them.
        let tx = db.begin_write().map_err(storage)?;
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
        tx.commit().map_err(storage)?;
        Ok(Self { db })
    }

    pub fn begin_read(&self) -> Result<ReadTx> {
        let tx = self.db.begin_read().map_err(storage)?;
        Ok(ReadTx {
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
            _tx: tx,
        })
    }

    pub fn begin_write(&self) -> Result<WriteTx> {
        Ok(WriteTx {
            tx: self.db.begin_write().map_err(storage)?,
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

/// A read transaction: a fixed snapshot of the database.
pub struct ReadTx {
    pub(crate) nodes: ReadOnlyTable<RecordKey, &'static [u8]>,
    pub(crate) edges: ReadOnlyTable<RecordKey, &'static [u8]>,
    pub(crate) out: ReadOnlyTable<AdjKey, u128>,
    pub(crate) inn: ReadOnlyTable<AdjKey, u128>,
    pub(crate) labels: ReadOnlyTable<LabelKey, ()>,
    texts: ReadOnlyTable<RecordKey, &'static str>,
    text_lengths: ReadOnlyTable<RecordKey, u32>,
    postings: ReadOnlyTable<PostingKey, u32>,
    text_stats: ReadOnlyTable<&'static str, (u64, u64)>,
    vectors: ReadOnlyTable<RecordKey, &'static [u8]>,
    hnsw_links: ReadOnlyTable<LinkKey, &'static [u8]>,
    vector_meta: ReadOnlyTable<&'static str, &'static [u8]>,
    _tx: ReadTransaction,
}

impl ReadTx {
    pub fn node(&self, ns: &str, id: NodeId) -> Result<Option<Node>> {
        read_record(&self.nodes, ns, id.as_u128())
    }

    pub fn edge(&self, ns: &str, id: EdgeId) -> Result<Option<Edge>> {
        read_record(&self.edges, ns, id.as_u128())
    }

    pub fn nodes(&self, ns: &str) -> Result<Vec<Node>> {
        scan_records(&self.nodes, ns)
    }

    /// Every edge of the namespace, including retracted ones.
    pub fn edges(&self, ns: &str) -> Result<Vec<Edge>> {
        scan_records(&self.edges, ns)
    }

    pub fn nodes_with_label(&self, ns: &str, label: &str) -> Result<Vec<NodeId>> {
        nodes_with_label(&self.labels, ns, label)
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
        text::search(
            &self.postings,
            &self.text_lengths,
            &self.text_stats,
            ns,
            query,
            limit,
        )
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
    tx: WriteTransaction,
}

impl WriteTx {
    pub fn commit(self) -> Result<()> {
        self.tx.commit().map_err(storage)
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

    pub fn create_node(&mut self, ns: &str, new: NewNode) -> Result<NodeId> {
        check_namespace(ns)?;
        if new.labels.is_empty() {
            return Err(Error::Invalid("a node needs at least one label".into()));
        }
        new.labels.iter().try_for_each(|label| check_label(label))?;
        let mut labels = new.labels;
        labels.sort();
        labels.dedup();
        let now = Timestamp::now();
        let node = Node {
            id: NodeId::new(),
            labels,
            props: new.props,
            sources: new.sources,
            created_at: now,
            updated_at: now,
        };
        self.put_node(ns, &node)?;
        if let Some(text) = &new.text {
            text::index(&self.tx, ns, node.id, text)?;
        }
        if let Some(embedding) = &new.embedding {
            vector::insert(&self.tx, ns, node.id, embedding)?;
        }
        Ok(node.id)
    }

    /// Merges `props` into the node; a `null` value removes the property.
    pub fn set_node_props(&mut self, ns: &str, id: NodeId, props: Props) -> Result<()> {
        let mut node = self.node(ns, id)?.ok_or(Error::NodeNotFound(id))?;
        merge_props(&mut node.props, props);
        node.updated_at = Timestamp::now();
        self.put_node(ns, &node)
    }

    pub fn add_label(&mut self, ns: &str, id: NodeId, label: &str) -> Result<()> {
        check_label(label)?;
        let mut node = self.node(ns, id)?.ok_or(Error::NodeNotFound(id))?;
        if let Err(at) = node
            .labels
            .binary_search_by(|item| item.as_str().cmp(label))
        {
            node.labels.insert(at, label.to_owned());
            node.updated_at = Timestamp::now();
            self.put_node(ns, &node)?;
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
            node.labels.remove(at);
            node.updated_at = Timestamp::now();
            self.put_node(ns, &node)?;
            let mut labels = self.tx.open_table(NODE_LABELS).map_err(storage)?;
            labels.remove((ns, label, id.as_u128())).map_err(storage)?;
        }
        Ok(())
    }

    /// Indexes `text` for full-text search, replacing the previous text.
    pub fn set_text(&mut self, ns: &str, id: NodeId, text: &str) -> Result<()> {
        self.node(ns, id)?.ok_or(Error::NodeNotFound(id))?;
        text::index(&self.tx, ns, id, text)
    }

    /// Removes the node's text from the index. Returns whether it had any.
    pub fn remove_text(&mut self, ns: &str, id: NodeId) -> Result<bool> {
        text::remove(&self.tx, ns, id)
    }

    /// Indexes `embedding` for vector search, replacing the previous one. All
    /// embeddings of a namespace must have the same dimension.
    pub fn set_embedding(&mut self, ns: &str, id: NodeId, embedding: &[f32]) -> Result<()> {
        self.node(ns, id)?.ok_or(Error::NodeNotFound(id))?;
        vector::insert(&self.tx, ns, id, embedding)
    }

    /// Removes the node's embedding from the index. Returns whether it had one.
    pub fn remove_embedding(&mut self, ns: &str, id: NodeId) -> Result<bool> {
        vector::remove(&self.tx, ns, id)
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
                    .map(|(edge, _)| edge),
            );
        }
        edges.sort();
        edges.dedup();
        for edge in edges {
            self.delete_edge(ns, edge)?;
        }
        text::remove(&self.tx, ns, id)?;
        vector::remove(&self.tx, ns, id)?;
        let mut labels = self.tx.open_table(NODE_LABELS).map_err(storage)?;
        for label in &node.labels {
            labels
                .remove((ns, label.as_str(), id.as_u128()))
                .map_err(storage)?;
        }
        drop(labels);
        let mut nodes = self.tx.open_table(NODES).map_err(storage)?;
        nodes.remove((ns, id.as_u128())).map_err(storage)?;
        Ok(())
    }

    pub fn create_edge(&mut self, ns: &str, new: NewEdge) -> Result<EdgeId> {
        check_namespace(ns)?;
        check_label(&new.label)?;
        check_interval(new.valid_from, new.valid_to)?;
        for end in [new.from, new.to] {
            if self.node(ns, end)?.is_none() {
                return Err(Error::NodeNotFound(end));
            }
        }
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
        self.put_edge(ns, &edge)?;
        Ok(edge.id)
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
        Ok(())
    }

    fn put_node(&mut self, ns: &str, node: &Node) -> Result<()> {
        let bytes = encode(node)?;
        let id = node.id.as_u128();
        let mut nodes = self.tx.open_table(NODES).map_err(storage)?;
        nodes.insert((ns, id), bytes.as_slice()).map_err(storage)?;
        drop(nodes);
        let mut labels = self.tx.open_table(NODE_LABELS).map_err(storage)?;
        for label in &node.labels {
            labels
                .insert((ns, label.as_str(), id), ())
                .map_err(storage)?;
        }
        Ok(())
    }

    fn put_edge(&mut self, ns: &str, edge: &Edge) -> Result<()> {
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
            .insert((ns, from, label, id), to)
            .map_err(storage)?;
        self.tx
            .open_table(ADJ_IN)
            .map_err(storage)?
            .insert((ns, to, label, id), from)
            .map_err(storage)?;
        Ok(())
    }
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

fn check_interval(from: Option<Timestamp>, to: Option<Timestamp>) -> Result<()> {
    if let (Some(from), Some(to)) = (from, to)
        && to <= from
    {
        return Err(Error::Invalid("valid_to must be after valid_from".into()));
    }
    Ok(())
}
