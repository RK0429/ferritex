//! Application services for Ferritex runtime orchestration.
//!
//! Downstream integrators should treat the documented service modules exported
//! from this crate root as the application facade: compile job orchestration,
//! execution policy construction, preview sessions, runtime options, stable
//! compile state, LSP capability snapshots, and the `ports` module for adapter
//! boundaries.
//!
//! The remaining broad module exports are public for current workspace wiring
//! and tests. Unless a module is named in the facade above or its Rustdoc
//! explicitly says otherwise, consider direct use of its internals unstable.

/// Compile-cache implementation for the application layer; unstable outside the workspace.
pub mod compile_cache;
/// Stable facade for document compilation use cases.
pub mod compile_job_service;
/// Stable facade for constructing execution policies from runtime options.
pub mod execution_policy_factory;
/// Stable facade for live analysis and LSP-facing snapshots.
pub mod live_analysis_snapshot;
/// Stable facade for LSP capability behavior.
pub mod lsp_capability_service;
/// Open-document buffer and store used by editor/LSP workflows; unstable outside the workspace.
pub mod open_document_store;
/// Stable adapter boundary traits and DTOs for infrastructure crates.
pub mod ports;
/// Stable facade for preview session orchestration.
pub mod preview_session_service;
/// Recompile scheduling primitives used by watch workflows; unstable outside the workspace.
pub mod recompile_scheduler;
/// Stable facade for CLI/runtime option mapping.
pub mod runtime_options;
/// Stable facade for compile state snapshots exposed to clients.
pub mod stable_compile_state;
/// Workspace-level job scheduling primitives used by watch workflows; unstable outside the workspace.
pub mod workspace_job_scheduler;
