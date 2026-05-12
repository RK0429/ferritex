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

Stable nested fields:

- `output.pdfPath`: generated PDF path, or `null` when no PDF was produced
- `output.cacheDir`: cache directory path, or `null` when cache is disabled or no
  PDF was produced
- `output.syncTexPath`: generated SyncTeX sidecar path, or `null` when SyncTeX is
  disabled or no PDF was produced
- `output.sidecarPaths`: array of generated sidecar artifact paths
- `output.pageCount`: rendered page count, or `null` when unavailable
- `summary.elapsedMicros`: total elapsed duration in microseconds
- `summary.stageTotalMicros`: total measured stage duration in microseconds
- `summary.cache.status`: `disabled`, `hit`, or `miss`
- `summary.stageTimingsMicros`: per-stage timings in microseconds with
  `cacheLoad`, `sourceTreeLoad`, `parse`, `typeset`, `pdfRender`, and
  `cacheStore`; a stage value may be `null` when the stage did not run
- `summary.passCount`: typesetting pass count

Consumers must treat unrecognized additional fields in this object and nested
objects as additive metadata. The listed nested fields are stable; unlisted
nested fields are opaque unless documented by a later schema version or by an
explicit additive contract.

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
- `startedAt`: unsigned integer Unix timestamp in microseconds captured when
  the font task starts
- `finishedAt`: unsigned integer Unix timestamp in microseconds captured when
  the font task finishes; it is greater than or equal to `startedAt`
- `workerId`: unsigned integer identifier of the worker that emitted the trace

All listed fields are required. Consumers must treat unrecognized additional
fields as additive metadata.

## `ferritex.diagnostic.v1`

Produced as newline-delimited JSON on stderr when machine-readable diagnostic
output is enabled, including `--trace-font-tasks` failures in `batchmode`.

Required top-level fields:

- `schemaVersion`: always `ferritex.diagnostic.v1`
- `diagnostic`: structured diagnostic object with `severity`, `message`, and
  optional `file`, `line`, `column`, `context`, and `suggestion`
- `diagnostic.severity`: one of `Error`, `Warning`, or `Info`; casing is part
  of the `ferritex.diagnostic.v1` contract, so casing changes require a new
  schema version

## Human-Readable Channels

`batchmode` suppresses normal detailed diagnostics on stderr. For text output
failures, Ferritex may still emit one single-line failure summary so automation
has a human-readable failure boundary. `compile --format json` keeps the human
diagnostic channel silent and carries the machine-readable failure contract on
stdout.

`watch` and `preview` status output is human-readable only. Recompile status
lines include stable `event_id`, `revision`, and `duration_ms` fields so
external tools can correlate a recompile without relying on a JSON stream.
`preview` uses the published preview revision when available. If a recompile
fails after a previous successful compile, the previous PDF remains on disk and
the failed recompile is reported in logs; no stable JSON event is emitted for
this state.
`watch` and `preview` treat Ctrl+C/SIGINT as an intentional normal stop:
stderr includes `shutdown complete (exit=0)` and the process exits 0. External
non-SIGINT termination, such as SIGALRM from an alarm wrapper or SIGKILL, is
outside the normal-stop contract and may report the signal-derived exit status.

Invalid asset bundle archives are reported through an error diagnostic whose
message includes the archive extraction failure reason and the standard
suggestion to verify the asset bundle path and version.
