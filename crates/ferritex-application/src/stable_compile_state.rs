use ferritex_core::compilation::{CompilationSnapshot, DocumentState};
use ferritex_core::diagnostics::Diagnostic;

/// 最新の成功した compile の frozen read-only projection。
/// LSP の LiveAnalysisSnapshotFactory と preview が参照する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StableCompileState {
    pub snapshot: CompilationSnapshot,
    pub document_state: DocumentState,
    pub page_count: usize,
    pub success: bool,
    pub diagnostics: Vec<Diagnostic>,
}
