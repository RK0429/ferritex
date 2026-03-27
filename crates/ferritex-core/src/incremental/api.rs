use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RecompilationScope {
    FullDocument,
    LocalRegion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyNode {
    pub path: PathBuf,
    pub content_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DependencyGraph {
    pub nodes: BTreeMap<PathBuf, DependencyNode>,
    pub edges: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
}

impl DependencyGraph {
    pub fn record_node(&mut self, path: PathBuf, content_hash: u64) {
        self.nodes
            .insert(path.clone(), DependencyNode { path, content_hash });
    }

    pub fn record_edge(&mut self, from: PathBuf, to: PathBuf) {
        self.edges.entry(from).or_default().insert(to);
    }
}
