//! A transport between members of one process, for tests: messages go
//! straight to the member they are for, unless the link is cut.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock, Weak};

use grafeno_raft::{Envelope, NodeId};

use crate::{Node, Transport};

/// The members of a cluster in one process, and which links are cut.
#[derive(Default)]
pub struct Hub {
    members: RwLock<HashMap<NodeId, Weak<Node>>>,
    cut: Mutex<HashSet<(NodeId, NodeId)>>,
}

impl Hub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn register(&self, node: &Arc<Node>) {
        self.members
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(node.id(), Arc::downgrade(node));
    }

    /// Cuts every link of `id` (both ways), or restores them.
    pub fn isolate(&self, id: NodeId, isolated: bool) {
        let members: Vec<NodeId> = self
            .members
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .copied()
            .collect();
        let mut cut = self.cut.lock().unwrap_or_else(|e| e.into_inner());
        for other in members {
            for link in [(id, other), (other, id)] {
                if isolated {
                    cut.insert(link);
                } else {
                    cut.remove(&link);
                }
            }
        }
    }

    /// A transport for the member `id` to send through.
    pub fn transport(self: &Arc<Self>) -> Arc<dyn Transport> {
        Arc::new(Link { hub: self.clone() })
    }
}

struct Link {
    hub: Arc<Hub>,
}

impl Transport for Link {
    fn send(&self, envelope: Envelope, snapshot: Option<Arc<Vec<u8>>>) {
        if self
            .hub
            .cut
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&(envelope.from, envelope.to))
        {
            return;
        }
        let target = self
            .hub
            .members
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&envelope.to)
            .and_then(Weak::upgrade);
        if let Some(node) = target {
            node.deliver(envelope, snapshot);
        }
    }
}
