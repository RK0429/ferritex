//! Core domain types and algorithms for Ferritex.
//!
//! The stable downstream integration surface is the documented API-oriented
//! modules and re-exports, especially each context's `api` module and the
//! explicit re-exports from that context root. The broad top-level module tree
//! remains public so the workspace crates can compose the current modular
//! monolith without crate-splitting churn.
//!
//! Treat direct access to non-`api` submodules as unstable implementation
//! detail unless the item is explicitly re-exported by the context root. Those
//! internal paths may change while the domain boundaries are still evolving.

/// Asset identity and resolver-facing API. Non-`api` internals are unstable.
pub mod assets;
/// Bibliography parsing and formatting API. Non-`api` internals are unstable.
pub mod bibliography;
/// Compilation state, partitions, sessions, and snapshots.
pub mod compilation;
/// Structured diagnostics shared by the runtime crates.
pub mod diagnostics;
/// Font metrics and loading primitives. Non-`api` internals are unstable.
pub mod font;
/// Graphics scene and image primitives. Non-`api` internals are unstable.
pub mod graphics;
/// Incremental recompilation API and dependency graph types.
pub mod incremental;
/// Stable IDs, source spans, dimensions, and other kernel primitives.
pub mod kernel;
/// Parser and macro-engine API. Non-`api` internals are unstable.
pub mod parser;
/// PDF rendering API and payload types. Non-`api` internals are unstable.
pub mod pdf;
/// Execution, file access, and artifact publication policy facade.
pub mod policy;
/// SyncTeX source/layout trace API.
pub mod synctex;
/// Typesetting API and layout model. Non-`api` internals are unstable.
pub mod typesetting;
