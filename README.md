# Ferritex

A Rust-based high-performance TeX compiler.

## Status

The current build covers a non-trivial docs-aligned subset rather than a placeholder shell. `compile` resolves `\\input` / `\\include` / `\\InputIfFileExists` across the current file, project root (the directory that contains the top-level input file), configured overlay roots, and asset bundles; any read outside those roots — including absolute-path `\\input{...}` — is denied and surfaced as an `input file access denied` error (exit code 2). Use `--overlay` to declare additional allowed read roots. The compiler expands `\\def` / `\\gdef` / `\\edef`, `\\expandafter`, `\\noexpand`, `\\csname`, `\\newcommand`, `\\newenvironment`, and group-scoped definitions; supports conditionals (`\\if`, `\\ifx`, `\\ifcat`, `\\ifnum`, `\\ifdim`, `\\ifcase`) plus e-TeX `\\numexpr` / `\\dimexpr`; and handles the extended register families (`count`, `dimen`, `skip`, `muskip`, `toks`) with local/global rollback.

The PDF path now includes multi-pass refs/`\\pageref`, TOC/LOF/LOT/index generation, equation/align-style math blocks, `hyperref` metadata and link annotations, `graphicx` PNG/JPEG embedding, bibliography `.bbl` readback, fontspec named-font resolution across project/overlay/bundle/host catalogs, outline-derived document partition planning, deterministic layout-stage commit ordering for parallel page rendering, and TrueType embedding with subsetting plus ToUnicode maps. `watch`, `preview`, and `lsp` are all live; LSP serves diagnostics, completion, definition, hover, and code actions from the latest stable compile state. `--synctex` now emits a `.synctex` sidecar with forward/inverse search data for the current line-based trace model.

Warm incremental recompilation is functional and clears the 100ms threshold that `REQ-NF-002` defines. The figure comes from a reliability-limited proxy measurement rather than the canonical `REQ-NF-002` fixture: the `incremental_stage_timing_*` tests edit one paragraph in a 1000-section staged input (an `FTX-STRESS-2000`-class stress proxy) and report a release-mode hot incremental 5-run median of **66ms** (no-ref) / **70ms** (with-ref) on this machine (2026-04-05). `REQ-NF-002` itself is defined against the canonical `FTX-BENCH-001` 100-page academic fixture (`ftx_bench_001.tex`), so treat the 66ms/70ms proxy figures as a point-in-time indicator of `REQ-NF-002` progress, not as formal validation on the canonical fixture. The optimization path included Step 1 (changed-paths fast path + split cache), Step 2 (block checkpoint reuse + suffix rebuild), Step 3 (per-page payload reuse), and Cache I/O Final (v8 binary cache with bincode, `CacheIndexSnapshot`, `BackgroundCacheWriter`, cached subtree fast path). Cross-reference convergence after page-shifting changes produces byte-identical output to a fresh compile. See `docs/design-incremental-100ms-optimization.md` for the full design and benchmark history.

Bundle distribution Wave 3 is now wired into CI: `scripts/build_bundle_archive.sh` reproducibly emits `FTX-ASSET-BUNDLE-001.tar.gz`, and `bundle-ci.yml` uploads the archive artifact, downloads it in a dependent job, and runs `bundle_archive_smoke_proof` against the downloaded archive under `--reproducible`.

Stage timing instrumentation (Step 0) is live: `StageTiming` in `CompileJobService` measures cache_load, source_tree_load, parse, typeset, pdf_render, and cache_store individually. This instrumentation provided the quantitative foundation for the `REQ-NF-002` optimization steps (now complete).

TikZ support has been expanded with xcolor-standard named colors (19 total), mm/in length units, ellipse path operations, `\path` command routing, arc paths, line width presets, and `\foreach` loops (simple lists, numeric ranges with `...`, step inference, nested loops).

Multicol MVP is implemented: `\begin{multicols}{N}` / `\end{multicols}` environment parsing, `\columnbreak` directives, and N-column layout typesetting with automatic column width calculation and side-by-side column placement via `VListItem::MulticolRegion`. Known limitation: a multicol region cannot span page boundaries — the entire region is placed on a single page.

All Must requirements in `docs/requirements.md` are satisfied, with `REQ-NF-002` (differential compile median < 100ms) in a reliability-limited state: the 66ms/70ms proxy measurement on a 1000-section staged input (`FTX-STRESS-2000`-class, not the canonical `FTX-BENCH-001` 100-page fixture) is well under the 100ms threshold, but canonical validation on `FTX-BENCH-001` is tracked as an open item in `docs/planning_report.md`. `REQ-NF-004` (LSP response latency) is backed by the `FTX-LSP-BENCH-001` warm 100-page trace in `crates/ferritex-cli/tests/bench_lsp_latency.rs::ftx_lsp_bench_001_warm_trace_meets_req_nf_004_thresholds` (release-mode medians: diagnostics 35.69ms / completion 124.88µs / definition 129.25µs, all well under the 500ms / 100ms / 200ms thresholds). Parity evidence across 5 categories (layout-core / navigation / bibliography / embedded-assets / tikz) is fully passing. Cross-platform CI (Linux/macOS/Windows) produces byte-identical output under `--reproducible`. Remaining non-blocker items (REQ-NF-003 memory profiling, REQ-NF-001a pdfLaTeX relative speed) are tracked as future tasks in `docs/planning_report.md`.

## Quick start

### 1. Build

```sh
cargo build --release
```

### 2. Prepare the asset bundle

Ferritex uses a pre-indexed asset bundle (`FTX-ASSET-BUNDLE-001`) for class, package, and font assets instead of depending on a TeX Live installation at runtime.

```sh
# Generate the bundle archive from the bundled fixtures
bash scripts/build_bundle_archive.sh tmp/FTX-ASSET-BUNDLE-001.tar.gz

# Extract the archive
mkdir -p tmp/bundle
tar -xzf tmp/FTX-ASSET-BUNDLE-001.tar.gz -C tmp/bundle
```

### 3. Compile a document

```sh
# Compile with the asset bundle (recommended)
cargo run --release -- compile hello.tex --asset-bundle tmp/bundle/FTX-ASSET-BUNDLE-001

# Reproducible mode disables host-font fallback for deterministic output
cargo run --release -- compile hello.tex --asset-bundle tmp/bundle/FTX-ASSET-BUNDLE-001 --reproducible

# SyncTeX output for editor forward/inverse search
cargo run --release -- compile hello.tex --synctex
```

### 4. Other subcommands

```sh
cargo run --release -- watch hello.tex     # polls and recompiles on source changes
cargo run --release -- preview hello.tex   # serves the current PDF on a loopback preview URL
cargo run --release -- lsp                 # starts an LSP server over stdio
```

## LSP (Language Server Protocol)

`ferritex lsp` starts a Language Server Protocol server over stdio. It speaks JSON-RPC 2.0, so each message must use `Content-Length: <N>\r\n\r\n<N bytes of UTF-8 JSON>` framing. Oversize or malformed frames are treated as fatal session errors and end the current session.

The handshake follows the standard LSP sequence: the client sends an `initialize` request, the server replies with an `InitializeResult`, and the client then sends an `initialized` notification before issuing `textDocument/*` requests.

Minimal `publishDiagnostics` notification payload:

```json
{
  "jsonrpc": "2.0",
  "method": "textDocument/publishDiagnostics",
  "params": {
    "uri": "file:///workspace/hello.tex",
    "diagnostics": [
      {
        "range": {
          "start": { "line": 0, "character": 0 },
          "end": { "line": 0, "character": 5 }
        },
        "severity": 1,
        "message": "Undefined control sequence"
      }
    ]
  }
}
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

## Completion status

All 48 functional requirements (REQ-FUNC-001–048) and 6 of 11 non-functional requirements are fully verified. `REQ-NF-002` clears its 100ms threshold on a reliability-limited proxy measurement, and the remaining 4 non-functional items are non-blockers:

| Requirement | Fixture | Status | Notes |
|---|---|---|---|
| REQ-NF-002 (differential compile median < 100ms) | Canonical: `FTX-BENCH-001` (100-page academic fixture, `ftx_bench_001.tex`). Proxy used so far: 1000-section staged input (`FTX-STRESS-2000`-class, `incremental_stage_timing_*` tests) | Reliability-limited | Proxy 5-run median 66ms (no-ref) / 70ms (with-ref) on this machine (2026-04-05) is well under 100ms, but the canonical `FTX-BENCH-001` measurement has not been performed. Treat as a point-in-time indicator, not formal validation |
| REQ-NF-001 (full compile < 1.0s) | `FTX-BENCH-001` (100-page academic fixture, `ftx_bench_001.tex`) | Infra ready | Benchmark harness measures and logs via `full_bench_docs_protocol_median_and_timing_proof`; CI assert deferred to avoid flaky results from runner variance |
| REQ-NF-001a (50x vs pdfLaTeX) | `FTX-BENCH-001` (100-page academic fixture) | Managed risk | Tracked in `docs/requirements.md` §5 as open item. Local measurement shows 54.89x (2026-04-05) |
| REQ-NF-003 (memory < 1 GiB) | Heavy partition fixture `crates/ferritex-bench/fixtures/corpus/partition-article/heavy_sections_independent.tex` | Deferred | Should priority. The heavy fixture run at `--jobs 16` observed RSS around 1133 MiB, so `ferritex compile --help` now warns that high parallelism can increase peak RSS. The observation does **not** apply to `FTX-BENCH-001` |
| REQ-NF-010 (error messages) | N/A | Design complete | `Diagnostic` struct covers file/line/message/context/suggestion; exhaustive path coverage is future work |

`REQ-NF-004` (LSP response latency) previously sat on this list as "minimal-input only"; it is now fully backed by the `FTX-LSP-BENCH-001` warm 100-page trace described above. Run it with:

```sh
cargo test --release -p ferritex-cli --test bench_lsp_latency -- --ignored ftx_lsp_bench_001_warm_trace_meets_req_nf_004_thresholds
```

The test is gated with `#[ignore]` so the default `cargo test` run stays fast; invoke it explicitly when recording REQ-NF-004 evidence.

Formal verification (proptest, miri, etc.) is not required by the current requirements and is out of scope.

See `docs/planning_report.md` for the full implementation history and `tmp/ferritex-completion-audit.md` for the detailed audit.

## Testing

```sh
cargo test --workspace                  # unit + integration + E2E
cargo clippy -- -D warnings             # lint
python3 scripts/check_architecture.py   # crate dependency + context boundary checks
```
