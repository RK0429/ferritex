# Ferritex JSON Contracts

Ferritex versioned machine-readable JSON payloads use a top-level
`schemaVersion` field. New versioned JSON surfaces must use the same field name
unless an explicitly documented external protocol requires otherwise.

## `ferritex.compileResult.v1`

Produced by `ferritex compile --format json` on stdout as exactly one JSON
object.

Required top-level fields:

- `schemaVersion`: always `ferritex.compileResult.v1`
- `command`: always `compile`
- `classification`: `success`, `warning`, or `error`
- `exitCode`: process exit code
- `success`: boolean success flag
- `output`: generated artifact paths and page count
- `summary`: timing and cache summary
- `diagnostics`: structured diagnostics

## `ferritex.previewBootstrap.v1`

Produced by `ferritex preview --bootstrap-format json` for the initial preview
bootstrap only. Continuous preview logs and events are not a stable JSON or
NDJSON stream.

Required top-level fields:

- `schemaVersion`: always `ferritex.previewBootstrap.v1`
- `command`: always `preview`
- `sessionId`: preview session identifier
- `urls`: loopback document and event URLs
- `target`: input target information
- `output`: initial PDF path and page count
- `summary`: compile timing and cache summary

## `ferritex.perfEvidence.v1`

Produced by `ferritex perf-evidence --output-dir <DIR>` as
`ferritex-perf-evidence.json`.

Required top-level fields:

- `schemaVersion`: always `ferritex.perfEvidence.v1`
- `fixture`: evidence fixture source and path
- `command`: template command plus actual per-run invocations
- `config`: warmup and measured run counts
- `results`: measured run records
- `summary`: successful run count and median duration
- `failures`: failed warmup or measured run records

## `ferritex.fontTaskTrace.v1`

Produced as newline-delimited JSON on stderr only when
`--trace-font-tasks` is set.

Required top-level fields:

- `schemaVersion`: always `ferritex.fontTaskTrace.v1`
- `fontTaskId`: public font task identifier
- `fontAsset`: font asset identifier
- `phase`: task phase

## `ferritex.diagnostic.v1`

Produced as newline-delimited JSON on stderr when machine-readable diagnostic
output is enabled, including `--trace-font-tasks` failures in `batchmode`.

Required top-level fields:

- `schemaVersion`: always `ferritex.diagnostic.v1`
- `diagnostic`: structured diagnostic object with `severity`, `message`, and
  optional `file`, `line`, `column`, `context`, and `suggestion`

## Human-Readable Channels

`batchmode` suppresses normal detailed diagnostics on stderr. For text output
failures, Ferritex may still emit one single-line failure summary so automation
has a human-readable failure boundary. `compile --format json` keeps the human
diagnostic channel silent and carries the machine-readable failure contract on
stdout.

`watch` status output is human-readable only. If a recompile fails after a
previous successful compile, the previous PDF remains on disk and the failed
recompile is reported in logs; no stable JSON event is emitted for this state.
`watch` and `preview` treat Ctrl+C/SIGINT as an intentional normal stop:
stderr includes `shutdown complete (exit=0)` and the process exits 0. External
non-SIGINT termination, such as SIGALRM from an alarm wrapper or SIGKILL, is
outside the normal-stop contract and may report the signal-derived exit status.

Invalid asset bundle archives are reported through an error diagnostic whose
message includes the archive extraction failure reason and the standard
suggestion to verify the asset bundle path and version.
