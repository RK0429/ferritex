use std::collections::{BTreeSet, HashSet};

use crate::typesetting::DocumentLayoutFragment;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StageOrder {
    MacroSessionDelta,
    DocumentReferenceBibliography,
    LayoutPageNumberMerge,
    ArtifactEmissionCacheMetadata,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StageCommitPayload {
    MacroSession(MacroSessionPayload),
    DocumentReference(DocumentReferencePayload),
    LayoutMerge(LayoutMergePayload),
    ArtifactCache(ArtifactCachePayload),
}

impl StageCommitPayload {
    pub fn stage_order(&self) -> StageOrder {
        match self {
            Self::MacroSession(_) => StageOrder::MacroSessionDelta,
            Self::DocumentReference(_) => StageOrder::DocumentReferenceBibliography,
            Self::LayoutMerge(_) => StageOrder::LayoutPageNumberMerge,
            Self::ArtifactCache(_) => StageOrder::ArtifactEmissionCacheMetadata,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MacroSessionPayload {
    pub macro_writes: Vec<String>,
    pub register_updates: Vec<RegisterUpdate>,
    pub catcode_changes: Vec<CatcodeChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterUpdate {
    pub kind: RegisterUpdateKind,
    pub index: u16,
    pub value: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterUpdateKind {
    Count,
    Dimen,
    Skip,
    Muskip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatcodeChange {
    pub character: char,
    pub new_catcode: u8,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DocumentReferencePayload {
    pub label_updates: Vec<(String, String)>,
    pub citation_updates: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct LayoutMergePayload {
    pub fragments: Vec<DocumentLayoutFragment>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ArtifactCachePayload {
    pub artifact_records: Vec<String>,
    pub cache_entries: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommitEntry {
    pub partition_id: String,
    pub payload: StageCommitPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AuthorityKey {
    MacroRegister(String),
    Label(String),
    Citation(String),
    TocNavigation(String),
    ArtifactSlot(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityKeyCollision {
    pub key: AuthorityKey,
    pub stage: StageOrder,
    pub partition_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CommitBarrier {
    pass_number: u32,
    pending: Vec<CommitEntry>,
    collisions: Vec<AuthorityKeyCollision>,
}

impl CommitBarrier {
    pub fn new(pass_number: u32) -> Self {
        Self {
            pass_number,
            pending: Vec::new(),
            collisions: Vec::new(),
        }
    }

    pub fn pass_number(&self) -> u32 {
        self.pass_number
    }

    pub fn commit(&mut self, entry: CommitEntry) {
        let stage = entry.payload.stage_order();
        let entry_authority_keys = authority_keys(&entry.payload);

        for key in entry_authority_keys {
            let mut partition_ids = BTreeSet::from([entry.partition_id.clone()]);
            for pending_entry in &self.pending {
                if pending_entry.partition_id == entry.partition_id
                    || pending_entry.payload.stage_order() != stage
                {
                    continue;
                }

                if authority_keys(&pending_entry.payload).contains(&key) {
                    let _ = partition_ids.insert(pending_entry.partition_id.clone());
                }
            }

            if partition_ids.len() > 1 {
                self.record_collision(key, stage, partition_ids.into_iter().collect());
            }
        }

        self.pending.push(entry);
    }

    pub fn has_collisions(&self) -> bool {
        !self.collisions.is_empty()
    }

    pub fn collisions(&self) -> &[AuthorityKeyCollision] {
        &self.collisions
    }

    pub fn into_ordered(mut self) -> Vec<CommitEntry> {
        self.pending.sort_by(|left, right| {
            left.payload
                .stage_order()
                .cmp(&right.payload.stage_order())
                .then_with(|| left.partition_id.cmp(&right.partition_id))
        });
        self.pending
    }

    fn record_collision(
        &mut self,
        key: AuthorityKey,
        stage: StageOrder,
        partition_ids: Vec<String>,
    ) {
        if let Some(existing) = self
            .collisions
            .iter_mut()
            .find(|collision| collision.key == key && collision.stage == stage)
        {
            let merged = existing
                .partition_ids
                .iter()
                .cloned()
                .chain(partition_ids)
                .collect::<BTreeSet<_>>();
            existing.partition_ids = merged.into_iter().collect();
            return;
        }

        self.collisions.push(AuthorityKeyCollision {
            key,
            stage,
            partition_ids,
        });
    }
}

fn authority_keys(payload: &StageCommitPayload) -> HashSet<AuthorityKey> {
    match payload {
        StageCommitPayload::MacroSession(payload) => {
            payload
                .macro_writes
                .iter()
                .map(|name| AuthorityKey::MacroRegister(format!("macro:{name}")))
                .chain(payload.register_updates.iter().map(|update| {
                    AuthorityKey::MacroRegister(format!(
                        "{}:{}",
                        register_update_prefix(update.kind),
                        update.index
                    ))
                }))
                .chain(payload.catcode_changes.iter().map(|change| {
                    AuthorityKey::MacroRegister(format!("catcode:{}", change.character))
                }))
                .collect()
        }
        StageCommitPayload::DocumentReference(payload) => payload
            .label_updates
            .iter()
            .map(|(label, _)| AuthorityKey::Label(label.clone()))
            .chain(
                payload
                    .citation_updates
                    .iter()
                    .map(|(citation, _)| AuthorityKey::Citation(citation.clone())),
            )
            .collect(),
        StageCommitPayload::LayoutMerge(payload) => payload
            .fragments
            .iter()
            .map(|fragment| AuthorityKey::TocNavigation(fragment.partition_id.clone()))
            .collect(),
        StageCommitPayload::ArtifactCache(payload) => payload
            .artifact_records
            .iter()
            .cloned()
            .map(AuthorityKey::ArtifactSlot)
            .chain(
                payload
                    .cache_entries
                    .iter()
                    .cloned()
                    .map(AuthorityKey::ArtifactSlot),
            )
            .collect(),
    }
}

fn register_update_prefix(kind: RegisterUpdateKind) -> &'static str {
    match kind {
        RegisterUpdateKind::Count => "count",
        RegisterUpdateKind::Dimen => "dimen",
        RegisterUpdateKind::Skip => "skip",
        RegisterUpdateKind::Muskip => "muskip",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AuthorityKey, CommitBarrier, CommitEntry, DocumentReferencePayload, LayoutMergePayload,
        MacroSessionPayload, RegisterUpdate, RegisterUpdateKind, StageCommitPayload, StageOrder,
    };
    use crate::typesetting::DocumentLayoutFragment;

    #[test]
    fn commit_barrier_orders_entries_by_stage_then_partition() {
        let mut barrier = CommitBarrier::new(2);
        barrier.commit(CommitEntry {
            partition_id: "section:0002".to_string(),
            payload: StageCommitPayload::LayoutMerge(LayoutMergePayload {
                fragments: vec![fragment("section:0002")],
            }),
        });
        barrier.commit(CommitEntry {
            partition_id: "section:0003".to_string(),
            payload: StageCommitPayload::DocumentReference(DocumentReferencePayload {
                label_updates: vec![("sec:refs".to_string(), "3".to_string())],
                citation_updates: Vec::new(),
            }),
        });
        barrier.commit(CommitEntry {
            partition_id: "section:0001".to_string(),
            payload: StageCommitPayload::LayoutMerge(LayoutMergePayload {
                fragments: vec![fragment("section:0001")],
            }),
        });

        let ordered = barrier
            .into_ordered()
            .into_iter()
            .map(|entry| (entry.payload.stage_order(), entry.partition_id))
            .collect::<Vec<_>>();

        assert_eq!(
            ordered,
            vec![
                (
                    StageOrder::DocumentReferenceBibliography,
                    "section:0003".to_string(),
                ),
                (
                    StageOrder::LayoutPageNumberMerge,
                    "section:0001".to_string(),
                ),
                (
                    StageOrder::LayoutPageNumberMerge,
                    "section:0002".to_string(),
                ),
            ]
        );
    }

    #[test]
    fn commit_barrier_records_authority_key_collisions() {
        let mut barrier = CommitBarrier::new(4);
        barrier.commit(CommitEntry {
            partition_id: "section:a".to_string(),
            payload: StageCommitPayload::MacroSession(MacroSessionPayload {
                macro_writes: vec!["foo".to_string()],
                register_updates: vec![RegisterUpdate {
                    kind: RegisterUpdateKind::Count,
                    index: 3,
                    value: 1,
                }],
                catcode_changes: Vec::new(),
            }),
        });
        barrier.commit(CommitEntry {
            partition_id: "section:b".to_string(),
            payload: StageCommitPayload::MacroSession(MacroSessionPayload {
                macro_writes: vec!["foo".to_string()],
                register_updates: vec![RegisterUpdate {
                    kind: RegisterUpdateKind::Count,
                    index: 8,
                    value: 2,
                }],
                catcode_changes: Vec::new(),
            }),
        });

        assert!(barrier.has_collisions());
        assert_eq!(
            barrier.collisions(),
            &[super::AuthorityKeyCollision {
                key: AuthorityKey::MacroRegister("macro:foo".to_string()),
                stage: StageOrder::MacroSessionDelta,
                partition_ids: vec!["section:a".to_string(), "section:b".to_string()],
            }]
        );
    }

    #[test]
    fn commit_barrier_ignores_distinct_authority_keys() {
        let mut barrier = CommitBarrier::new(5);
        barrier.commit(CommitEntry {
            partition_id: "section:a".to_string(),
            payload: StageCommitPayload::DocumentReference(DocumentReferencePayload {
                label_updates: vec![("sec:intro".to_string(), "1".to_string())],
                citation_updates: Vec::new(),
            }),
        });
        barrier.commit(CommitEntry {
            partition_id: "section:b".to_string(),
            payload: StageCommitPayload::DocumentReference(DocumentReferencePayload {
                label_updates: vec![("sec:methods".to_string(), "2".to_string())],
                citation_updates: vec![("knuth84".to_string(), "[1]".to_string())],
            }),
        });

        assert!(!barrier.has_collisions());
        assert!(barrier.collisions().is_empty());
    }

    #[test]
    fn commit_barrier_preserves_pass_number() {
        let barrier = CommitBarrier::new(3);

        assert_eq!(barrier.pass_number(), 3);
    }

    fn fragment(partition_id: &str) -> DocumentLayoutFragment {
        DocumentLayoutFragment {
            partition_id: partition_id.to_string(),
            ..DocumentLayoutFragment::default()
        }
    }
}
