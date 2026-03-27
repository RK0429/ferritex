# Ferritex

A Rust-based high-performance TeX compiler.

## Status

The current build covers a non-trivial docs-aligned subset rather than a placeholder shell. `compile` resolves `\\input` / `\\include` / `\\InputIfFileExists` across the current file, project root, configured overlay roots, and asset bundles; expands `\\def` / `\\gdef` / `\\edef`, `\\expandafter`, `\\noexpand`, `\\csname`, `\\newcommand`, `\\newenvironment`, and group-scoped definitions; supports conditionals (`\\if`, `\\ifx`, `\\ifcat`, `\\ifnum`, `\\ifdim`, `\\ifcase`) plus e-TeX `\\numexpr` / `\\dimexpr`; and handles the extended register families (`count`, `dimen`, `skip`, `muskip`, `toks`) with local/global rollback.

The PDF path now includes multi-pass refs/`\\pageref`, TOC/LOF/LOT/index generation, equation/align-style math blocks, `hyperref` metadata and link annotations, `graphicx` PNG/JPEG embedding, bibliography `.bbl` readback, fontspec named-font resolution across project/overlay/bundle/host catalogs, and TrueType embedding with subsetting plus ToUnicode maps. `watch`, `preview`, and `lsp` are all live; LSP serves diagnostics, completion, definition, hover, and code actions from the latest stable compile state. `--synctex` now emits a `.synctex` sidecar with forward/inverse search data for the current line-based trace model.

The largest remaining gaps against `docs/requirements.md` are incremental compilation and persistent cache, document-partition/commit-barrier parallelism, `tikz`/`pgf`, and higher-fidelity source-span tracking for SyncTeX.

## Quick start

```sh
cargo build
cargo run -- compile hello.tex   # emits hello.pdf for a minimal LaTeX document
cargo run -- watch hello.tex     # polls and recompiles on source changes
cargo run -- preview hello.tex   # serves the current PDF on a loopback preview URL
cargo run -- lsp                 # starts an LSP server over stdio
cargo run -- compile hello.tex --reproducible  # disables host-font fallback
cargo run -- compile hello.tex --synctex       # also emits hello.synctex
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
cargo test --workspace                  # unit + integration + E2E
cargo clippy -- -D warnings             # lint
python3 scripts/check_architecture.py   # crate dependency + context boundary checks
```
