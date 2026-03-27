use std::collections::BTreeMap;

use ferritex_core::compilation::{CompilationSnapshot, DocumentState, IndexEntry};
use ferritex_core::diagnostics::Diagnostic;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossReferenceSectionEntry {
    pub level: u8,
    pub number: String,
    pub title: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossReferenceCaptionEntry {
    pub kind: String,
    pub number: String,
    pub caption: String,
}

/// 差分コンパイル時に前回の相互参照状態を warm start するための seed。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossReferenceSeed {
    pub labels: BTreeMap<String, String>,
    pub section_entries: Vec<CrossReferenceSectionEntry>,
    pub figure_entries: Vec<CrossReferenceCaptionEntry>,
    pub table_entries: Vec<CrossReferenceCaptionEntry>,
    pub bibliography: BTreeMap<String, String>,
    pub page_labels: BTreeMap<String, u32>,
    pub index_entries: Vec<IndexEntry>,
}

/// 最新の成功した compile の frozen read-only projection。
/// LSP の LiveAnalysisSnapshotFactory と preview が参照する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StableCompileState {
    pub snapshot: CompilationSnapshot,
    pub document_state: DocumentState,
    pub cross_reference_seed: CrossReferenceSeed,
    pub page_count: usize,
    pub success: bool,
    pub diagnostics: Vec<Diagnostic>,
}
