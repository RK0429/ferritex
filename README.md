# Ferritex

A Rust-based high-performance TeX compiler.

## Status

Early docs-aligned slice. `compile` accepts a minimal LaTeX document shape, recursively resolves `\\input` / `\\include` with current-file-relative lookup, runs a simple parse -> typeset -> PDF pipeline, and emits a PDF containing the expanded document body text instead of a placeholder artifact. `watch` refreshes its polled path set from the latest successful compile state, so newly introduced input dependencies are picked up on the next cycle. `preview` starts a loopback-only preview server and publishes the compiled PDF over HTTP. `lsp` speaks stdio JSON-RPC, answers `initialize`, recompiles on open/change, merges compile diagnostics with buffer-local analysis, and augments label / citation completion and definition lookups with the latest stable compile state. Full TeX/LaTeX compatibility, real macro expansion, bibliography toolchains, and production-grade typesetting are still unimplemented.

## Quick start

```sh
cargo build
cargo run -- compile hello.tex   # emits hello.pdf for a minimal LaTeX document
cargo run -- watch hello.tex     # polls and recompiles on source changes
cargo run -- preview hello.tex   # serves the current PDF on a loopback preview URL
cargo run -- lsp                 # starts an LSP server over stdio
```

## Crate layout

| Crate | Role |
|---|---|
| `ferritex-cli` | CLI binary (`compile` / `watch` / `preview` / `lsp` subcommands) |
| `ferritex-application` | Application services for compile/watch/lsp/preview orchestration, runtime options, scheduler, diagnostics/snapshot services |
| `ferritex-core` | Domain modules and shared model for parser, typesetting, PDF, policy, compilation, diagnostics, kernel utilities |
| `ferritex-infra` | OS/FS/network adapters (file access gate, shell command gateway, asset bundle loader, loopback preview transport, polling watcher) |
| `ferritex-bench` | Benchmark harness (placeholder) |

Dependency direction: `cli → application + core + infra`, `application → core`, `infra → application + core`.

## Testing

```sh
cargo test                              # unit + integration + E2E
cargo clippy -- -D warnings             # lint
python3 scripts/check_architecture.py   # crate dependency + context boundary checks
```
