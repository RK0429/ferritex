# Ferritex

A Rust-based high-performance TeX compiler.

## Status

The current build covers a non-trivial docs-aligned subset rather than a placeholder shell. `compile` resolves `\\input` / `\\include` / `\\InputIfFileExists` across the current file, project root, configured overlay roots, and asset bundles; expands `\\def` / `\\gdef` / `\\edef`, `\\expandafter`, `\\noexpand`, `\\csname`, `\\newcommand`, `\\newenvironment`, and group-scoped definitions; supports conditionals (`\\if`, `\\ifx`, `\\ifcat`, `\\ifnum`, `\\ifdim`, `\\ifcase`) plus e-TeX `\\numexpr` / `\\dimexpr`; and handles the extended register families (`count`, `dimen`, `skip`, `muskip`, `toks`) with local/global rollback.

The PDF path now includes multi-pass refs/`\\pageref`, TOC/LOF/LOT/index generation, equation/align-style math blocks, `hyperref` metadata and link annotations, `graphicx` PNG/JPEG embedding, bibliography `.bbl` readback, fontspec named-font resolution across project/overlay/bundle/host catalogs, outline-derived document partition planning, deterministic layout-stage commit ordering for parallel page rendering, and TrueType embedding with subsetting plus ToUnicode maps. `watch`, `preview`, and `lsp` are all live; LSP serves diagnostics, completion, definition, hover, and code actions from the latest stable compile state. `--synctex` now emits a `.synctex` sidecar with forward/inverse search data for the current line-based trace model.

Warm incremental recompilation is functional and meets the `REQ-NF-002` target: on the current test configuration (`FTX-BENCH-001`, 1000-section staged input), release-mode hot incremental 5-run median is **66ms** (no-ref) / **70ms** (with-ref), well under the 100ms threshold (2026-04-05 measurement). The optimization path included Step 1 (changed-paths fast path + split cache), Step 2 (block checkpoint reuse + suffix rebuild), Step 3 (per-page payload reuse), and Cache I/O Final (v8 binary cache with bincode, `CacheIndexSnapshot`, `BackgroundCacheWriter`, cached subtree fast path). Cross-reference convergence after page-shifting changes produces byte-identical output to a fresh compile. See `docs/design-incremental-100ms-optimization.md` for the full design and benchmark history.

Bundle distribution Wave 3 is now wired into CI: `scripts/build_bundle_archive.sh` reproducibly emits `FTX-ASSET-BUNDLE-001.tar.gz`, and `bundle-ci.yml` uploads the archive artifact, downloads it in a dependent job, and runs `bundle_archive_smoke_proof` against the downloaded archive under `--reproducible`.

Stage timing instrumentation (Step 0) is live: `StageTiming` in `CompileJobService` measures cache_load, source_tree_load, parse, typeset, pdf_render, and cache_store individually. This instrumentation provided the quantitative foundation for the `REQ-NF-002` optimization steps (now complete).

TikZ support has been expanded with xcolor-standard named colors (19 total), mm/in length units, ellipse path operations, `\path` command routing, arc paths, line width presets, and `\foreach` loops (simple lists, numeric ranges with `...`, step inference, nested loops).

Multicol MVP is implemented: `\begin{multicols}{N}` / `\end{multicols}` environment parsing, `\columnbreak` directives, and N-column layout typesetting with automatic column width calculation and side-by-side column placement via `VListItem::MulticolRegion`. Known limitation: a multicol region cannot span page boundaries — the entire region is placed on a single page.

`REQ-NF-002` (differential compile median < 100ms) is achieved. The remaining gaps against `docs/requirements.md` are tracked in `docs/planning_report.md` §3.

## Quick start

```sh
cargo build
cargo run -- compile hello.tex   # emits hello.pdf for a minimal LaTeX document
cargo run -- watch hello.tex     # polls and recompiles on source changes
cargo run -- preview hello.tex   # serves the current PDF on a loopback preview URL
cargo run -- lsp                 # starts an LSP server over stdio
cargo run -- compile hello.tex --reproducible  # disables host-font fallback
cargo run -- compile hello.tex --synctex       # also emits hello.synctex
cargo run -- compile hello.tex --asset-bundle builtin:basic  # uses the built-in basic asset bundle
```

## Crate layout

| Crate | Role |
|---|---|
| `ferritex-cli` | CLI binary (`compile` / `watch` / `preview` / `lsp` subcommands) |
| `ferritex-application` | Application services for compile/watch/lsp/preview orchestration, runtime options, scheduler, diagnostics/snapshot services |
| `ferritex-core` | Domain modules and shared model for parser, typesetting, PDF, policy, compilation, diagnostics, kernel utilities |
| `ferritex-infra` | OS/FS/network adapters (file access gate, shell command gateway, asset bundle loader, loopback preview transport, polling watcher) |
| `ferritex-bench` | Benchmark harness scaffold for `FTX-BENCH-001`, partition parallelism, and bundle bootstrap smoke |

Dependency direction: `cli → application + core + infra`, `application → core`, `infra → application + core`.

## Testing

```sh
cargo test --workspace                  # unit + integration + E2E
cargo clippy -- -D warnings             # lint
python3 scripts/check_architecture.py   # crate dependency + context boundary checks
```
