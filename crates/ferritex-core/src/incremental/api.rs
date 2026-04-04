use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::compilation::{
    slugify_partition_title, DocumentPartitionPlan, DocumentWorkUnit, PartitionKind,
    PartitionLocator, SectionOutlineEntry,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RecompilationScope {
    FullDocument,
    LocalRegion,
    BlockLevel {
        affected_partitions: Vec<String>,
        references_affected: bool,
        pagination_affected: bool,
    },
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DocumentPartitionPlanner;

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

impl DocumentPartitionPlanner {
    pub fn plan(
        primary_input: &Path,
        document_class: &str,
        section_entries: &[SectionOutlineEntry],
    ) -> DocumentPartitionPlan {
        let fallback_partition_id = DocumentPartitionPlan::fallback_partition_id_for(primary_input);
        let Some((level, kind)) = partition_strategy(document_class, section_entries) else {
            return DocumentPartitionPlan {
                fallback_partition_id: fallback_partition_id.clone(),
                work_units: vec![DocumentWorkUnit {
                    partition_id: fallback_partition_id,
                    kind: PartitionKind::Document,
                    locator: PartitionLocator {
                        entry_file: primary_input.to_path_buf(),
                        level: 0,
                        ordinal: 0,
                        title: "document".to_string(),
                    },
                    title: "document".to_string(),
                }],
            };
        };

        let work_units = section_entries
            .iter()
            .filter(|entry| entry.level == level)
            .enumerate()
            .map(|(ordinal, entry)| {
                let title = entry.display_title();
                let slug = slugify_partition_title(&title);
                let partition_prefix = match kind {
                    PartitionKind::Chapter => "chapter",
                    PartitionKind::Section => "section",
                    PartitionKind::Document => "document",
                };

                DocumentWorkUnit {
                    partition_id: format!("{partition_prefix}:{:04}:{slug}", ordinal + 1),
                    kind,
                    locator: PartitionLocator {
                        entry_file: primary_input.to_path_buf(),
                        level: entry.level,
                        ordinal,
                        title: title.clone(),
                    },
                    title,
                }
            })
            .collect::<Vec<_>>();

        DocumentPartitionPlan {
            fallback_partition_id,
            work_units,
        }
    }
}

fn partition_strategy(
    document_class: &str,
    section_entries: &[SectionOutlineEntry],
) -> Option<(u8, PartitionKind)> {
    let prefers_chapters = matches!(document_class, "book" | "report");
    if prefers_chapters && section_entries.iter().any(|entry| entry.level == 0) {
        return Some((0, PartitionKind::Chapter));
    }
    if section_entries.iter().any(|entry| entry.level == 1) {
        return Some((1, PartitionKind::Section));
    }
    if section_entries.iter().any(|entry| entry.level == 0) {
        return Some((0, PartitionKind::Section));
    }
    None
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::compilation::SectionOutlineEntry;

    use super::{DependencyGraph, DocumentPartitionPlanner, PartitionKind, RecompilationScope};

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

    #[test]
    fn recompilation_scope_block_level_carries_partition_metadata() {
        let scope = RecompilationScope::BlockLevel {
            affected_partitions: vec!["chapter:0001:intro".to_string()],
            references_affected: true,
            pagination_affected: false,
        };

        match scope {
            RecompilationScope::BlockLevel {
                affected_partitions,
                references_affected,
                pagination_affected,
            } => {
                assert_eq!(affected_partitions, vec!["chapter:0001:intro".to_string()]);
                assert!(references_affected);
                assert!(!pagination_affected);
            }
            other => panic!("expected BlockLevel scope, got {other:?}"),
        }
    }

    #[test]
    fn partition_planner_prefers_chapters_for_book_documents() {
        let plan = DocumentPartitionPlanner::plan(
            Path::new("book.tex"),
            "book",
            &[
                SectionOutlineEntry {
                    level: 0,
                    number: "1".to_string(),
                    title: "Intro".to_string(),
                },
                SectionOutlineEntry {
                    level: 1,
                    number: "1.1".to_string(),
                    title: "Background".to_string(),
                },
                SectionOutlineEntry {
                    level: 0,
                    number: "2".to_string(),
                    title: "Results".to_string(),
                },
            ],
        );

        assert_eq!(plan.work_units.len(), 2);
        assert_eq!(plan.work_units[0].kind, PartitionKind::Chapter);
        assert_eq!(plan.work_units[0].partition_id, "chapter:0001:1-intro");
        assert_eq!(plan.work_units[1].partition_id, "chapter:0002:2-results");
    }

    #[test]
    fn partition_planner_uses_sections_for_article_documents() {
        let plan = DocumentPartitionPlanner::plan(
            Path::new("paper.tex"),
            "article",
            &[
                SectionOutlineEntry {
                    level: 1,
                    number: "1".to_string(),
                    title: "Intro".to_string(),
                },
                SectionOutlineEntry {
                    level: 2,
                    number: "1.1".to_string(),
                    title: "Motivation".to_string(),
                },
                SectionOutlineEntry {
                    level: 1,
                    number: "2".to_string(),
                    title: "Method".to_string(),
                },
            ],
        );

        assert_eq!(plan.work_units.len(), 2);
        assert_eq!(plan.work_units[0].kind, PartitionKind::Section);
        assert_eq!(plan.work_units[0].partition_id, "section:0001:1-intro");
        assert_eq!(plan.work_units[1].partition_id, "section:0002:2-method");
    }

    #[test]
    fn partition_planner_falls_back_to_whole_document() {
        let plan = DocumentPartitionPlanner::plan(Path::new("main.tex"), "article", &[]);

        assert_eq!(plan.work_units.len(), 1);
        assert_eq!(plan.work_units[0].kind, PartitionKind::Document);
        assert_eq!(plan.work_units[0].partition_id, "document:0000:main");
        assert_eq!(plan.fallback_partition_id, "document:0000:main");
        assert_eq!(
            plan.work_units[0].locator.entry_file,
            PathBuf::from("main.tex")
        );
    }
}
