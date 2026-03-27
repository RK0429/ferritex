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

    pub fn affected_paths<'a, I>(&self, changed_paths: I) -> BTreeSet<PathBuf>
    where
        I: IntoIterator<Item = &'a PathBuf>,
    {
        let mut reverse_edges: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
        for (from, targets) in &self.edges {
            for target in targets {
                reverse_edges
                    .entry(target.clone())
                    .or_default()
                    .insert(from.clone());
            }
        }

        let mut affected = BTreeSet::new();
        let mut stack = Vec::new();
        for path in changed_paths {
            if affected.insert(path.clone()) {
                stack.push(path.clone());
            }
        }

        while let Some(path) = stack.pop() {
            if let Some(parents) = reverse_edges.get(&path) {
                for parent in parents {
                    if affected.insert(parent.clone()) {
                        stack.push(parent.clone());
                    }
                }
            }
        }

        affected
    }
}

#[cfg(test)]
mod tests {
    use super::DependencyGraph;

    #[test]
    fn affected_paths_include_transitive_parents() {
        let mut graph = DependencyGraph::default();
        let root = std::path::PathBuf::from("main.tex");
        let chapter = std::path::PathBuf::from("chapter.tex");
        let section = std::path::PathBuf::from("section.tex");
        let appendix = std::path::PathBuf::from("appendix.tex");
        graph.record_edge(root.clone(), chapter.clone());
        graph.record_edge(chapter.clone(), section.clone());
        graph.record_edge(root.clone(), appendix);

        let affected = graph.affected_paths([&section]);

        assert_eq!(
            affected.into_iter().collect::<Vec<_>>(),
            vec![chapter, root, section]
        );
    }
}
