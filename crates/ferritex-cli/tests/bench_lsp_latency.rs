use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ferritex_bench::bench_fixtures_root;
use serde_json::{json, Value};

fn ferritex_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ferritex"))
}

fn write_lsp_message(writer: &mut impl Write, value: &Value) {
    let body = serde_json::to_vec(value).expect("serialize LSP message");
    write!(writer, "Content-Length: {}\r\n\r\n", body.len()).expect("write header");
    writer.write_all(&body).expect("write body");
    writer.flush().expect("flush body");
}

fn read_lsp_message(reader: &mut impl BufRead) -> Value {
    let mut content_length = None;

    loop {
        let mut header = String::new();
        reader.read_line(&mut header).expect("read LSP header");
        if header == "\r\n" {
            break;
        }
        if let Some(value) = header.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse::<usize>().expect("parse Content-Length"));
        }
    }

    let mut body = vec![0u8; content_length.expect("Content-Length header")];
    reader.read_exact(&mut body).expect("read LSP body");
    serde_json::from_slice(&body).expect("parse LSP body")
}

fn lsp_initialize(stdin: &mut impl Write, reader: &mut impl BufRead, root_uri: &str) {
    write_lsp_message(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {}
            }
        }),
    );
    let initialize = read_lsp_message(reader);
    assert_eq!(initialize["id"], 1);

    write_lsp_message(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }),
    );
}

fn lsp_did_open(stdin: &mut impl Write, uri: &str, text: &str) {
    write_lsp_message(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "latex",
                    "version": 1,
                    "text": text
                }
            }
        }),
    );
}

fn lsp_shutdown_exit(
    stdin: &mut impl Write,
    reader: &mut impl BufRead,
    child: &mut std::process::Child,
) {
    write_lsp_message(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 999,
            "method": "shutdown",
            "params": null
        }),
    );
    let shutdown = read_lsp_message(reader);
    assert_eq!(shutdown["id"], 999);
    assert_eq!(shutdown["result"], Value::Null);

    write_lsp_message(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }),
    );

    assert!(child.wait().expect("wait lsp").success());
}

fn measure_median(durations: &[Duration]) -> Duration {
    assert!(!durations.is_empty());
    let mut sorted = durations.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

#[test]
fn lsp_warm_diagnostics_latency() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("main.tex");
    let text = "\\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\n";
    std::fs::write(&tex_file, text).expect("write input file");
    let uri = format!("file://{}", tex_file.to_str().expect("utf-8 path"));
    let root_uri = format!("file://{}", dir.path().to_str().expect("utf-8 path"));

    let mut child = Command::new(ferritex_bin())
        .args(["lsp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ferritex lsp");
    let mut stdin = child.stdin.take().expect("lsp stdin");
    let stdout = child.stdout.take().expect("lsp stdout");
    let mut reader = BufReader::new(stdout);

    lsp_initialize(&mut stdin, &mut reader, &root_uri);
    lsp_did_open(&mut stdin, &uri, text);

    let initial_diagnostics = read_lsp_message(&mut reader);
    assert_eq!(
        initial_diagnostics["method"],
        "textDocument/publishDiagnostics"
    );

    let mut durations = Vec::with_capacity(5);
    for version in 2..=6 {
        let started_at = Instant::now();
        write_lsp_message(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didChange",
                "params": {
                    "textDocument": {
                        "uri": uri,
                        "version": version
                    },
                    "contentChanges": [
                        {
                            "text": text
                        }
                    ]
                }
            }),
        );

        let diagnostics = read_lsp_message(&mut reader);
        assert_eq!(diagnostics["method"], "textDocument/publishDiagnostics");
        durations.push(started_at.elapsed());
    }

    let median = measure_median(&durations);
    eprintln!("warm diagnostics median latency: {:?}", median);
    assert!(
        median <= Duration::from_millis(500),
        "warm diagnostics median latency exceeded threshold: {:?}",
        median
    );

    lsp_shutdown_exit(&mut stdin, &mut reader, &mut child);
}

#[test]
fn lsp_warm_completion_latency() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("main.tex");
    let text = "\\documentclass{article}\n\\begin{document}\n\\sec\n\\end{document}\n";
    std::fs::write(&tex_file, text).expect("write input file");
    let uri = format!("file://{}", tex_file.to_str().expect("utf-8 path"));
    let root_uri = format!("file://{}", dir.path().to_str().expect("utf-8 path"));

    let mut child = Command::new(ferritex_bin())
        .args(["lsp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ferritex lsp");
    let mut stdin = child.stdin.take().expect("lsp stdin");
    let stdout = child.stdout.take().expect("lsp stdout");
    let mut reader = BufReader::new(stdout);

    lsp_initialize(&mut stdin, &mut reader, &root_uri);
    lsp_did_open(&mut stdin, &uri, text);

    let initial_diagnostics = read_lsp_message(&mut reader);
    assert_eq!(
        initial_diagnostics["method"],
        "textDocument/publishDiagnostics"
    );

    let mut durations = Vec::with_capacity(5);
    for request_id in 2..=6 {
        let started_at = Instant::now();
        write_lsp_message(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "textDocument/completion",
                "params": {
                    "textDocument": { "uri": uri },
                    "position": { "line": 2, "character": 4 }
                }
            }),
        );

        let completion = read_lsp_message(&mut reader);
        assert_eq!(completion["id"], request_id);
        assert!(completion["result"].is_array());
        durations.push(started_at.elapsed());
    }

    let median = measure_median(&durations);
    eprintln!("warm completion median latency: {:?}", median);
    assert!(
        median <= Duration::from_millis(100),
        "warm completion median latency exceeded threshold: {:?}",
        median
    );

    lsp_shutdown_exit(&mut stdin, &mut reader, &mut child);
}

#[test]
fn lsp_warm_definition_latency() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("main.tex");
    let text =
        "\\documentclass{article}\n\\newcommand{\\foo}{bar}\n\\begin{document}\n\\foo\n\\end{document}\n";
    std::fs::write(&tex_file, text).expect("write input file");
    let uri = format!("file://{}", tex_file.to_str().expect("utf-8 path"));
    let root_uri = format!("file://{}", dir.path().to_str().expect("utf-8 path"));

    let mut child = Command::new(ferritex_bin())
        .args(["lsp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ferritex lsp");
    let mut stdin = child.stdin.take().expect("lsp stdin");
    let stdout = child.stdout.take().expect("lsp stdout");
    let mut reader = BufReader::new(stdout);

    lsp_initialize(&mut stdin, &mut reader, &root_uri);
    lsp_did_open(&mut stdin, &uri, text);

    let initial_diagnostics = read_lsp_message(&mut reader);
    assert_eq!(
        initial_diagnostics["method"],
        "textDocument/publishDiagnostics"
    );

    let mut durations = Vec::with_capacity(5);
    for request_id in 2..=6 {
        let started_at = Instant::now();
        write_lsp_message(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "textDocument/definition",
                "params": {
                    "textDocument": { "uri": uri },
                    "position": { "line": 3, "character": 1 }
                }
            }),
        );

        let definition = read_lsp_message(&mut reader);
        assert_eq!(definition["id"], request_id);
        assert!(!definition["result"].is_null());
        durations.push(started_at.elapsed());
    }

    let median = measure_median(&durations);
    eprintln!("warm definition median latency: {:?}", median);
    assert!(
        median <= Duration::from_millis(200),
        "warm definition median latency exceeded threshold: {:?}",
        median
    );

    lsp_shutdown_exit(&mut stdin, &mut reader, &mut child);
}

// ---------------------------------------------------------------------------
// FTX-LSP-BENCH-001 warm trace (REQ-NF-004)
//
// Benchmark profile `FTX-LSP-BENCH-001` is defined in docs/requirements.md:161
// as replaying a diagnostics/completion/definition LSP trace against the same
// 100-page academic paper template used by `FTX-BENCH-001`, from a warm state
// (cache and Stable Compile State already built) constructed by:
//   (1) running one full compile without --no-cache,
//   (2) starting `ferritex lsp` and completing the `initialize` handshake,
//   (3) sending `textDocument/didOpen` and waiting for the initial Stable
//       Compile State (signalled by the first publishDiagnostics).
// The trace must include at least 5 samples per operation with cursor
// positions distributed across document regions. REQ-NF-004 specifies
// thresholds: diagnostics < 500ms, completion < 100ms, definition < 200ms,
// measured as the median of 5 replays after 1 warm-up replay.
// ---------------------------------------------------------------------------

struct TraceCursor {
    label: &'static str,
    line: u32,
    completion_character: u32,
    definition_character: u32,
}

fn find_line_index(text: &str, needle: &str) -> u32 {
    let idx = text
        .find(needle)
        .unwrap_or_else(|| panic!("fixture missing expected marker: {needle:?}"));
    text[..idx].matches('\n').count() as u32
}

fn character_in_line(line_text: &str, after: &str) -> u32 {
    let idx = line_text
        .find(after)
        .unwrap_or_else(|| panic!("line missing expected marker {after:?}: {line_text}"));
    (idx + after.len()) as u32
}

fn stage_lsp_bench_001_fixture() -> (tempfile::TempDir, PathBuf, String) {
    let fixtures_root = bench_fixtures_root();
    let bench_src = fixtures_root.join("bench/ftx_bench_001.tex");
    let pixel_src = fixtures_root.join("bench/pixel.png");
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_dst = dir.path().join("ftx_bench_001.tex");
    std::fs::copy(&bench_src, &tex_dst).expect("copy bench fixture");
    std::fs::copy(&pixel_src, dir.path().join("pixel.png")).expect("copy pixel asset");
    let text = std::fs::read_to_string(&tex_dst).expect("read bench fixture");
    (dir, tex_dst, text)
}

fn run_warm_full_compile(tex_file: &Path) {
    // Warm-state step 1 per FTX-LSP-BENCH-001: run one full compile without
    // --no-cache to materialize the asset bundle, font cache, and on-disk
    // compile cache before the LSP server opens the same document. A
    // warning-only exit (code 1) is tolerated because the built-in basic
    // bundle used by `ferritex lsp` emits a WinAnsiEncoding warning for
    // the `∑` glyph in the fixture; the subsequent LSP measurement is the
    // actual REQ-NF-004 signal.
    let output = Command::new(ferritex_bin())
        .arg("compile")
        .arg(tex_file)
        .output()
        .expect("run ferritex compile for warm state");
    let exit_code = output.status.code().unwrap_or(-1);
    assert!(
        exit_code == 0 || exit_code == 1,
        "warm-state ferritex compile failed (exit={exit_code}): stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn ftx_lsp_bench_cursors(text: &str) -> Vec<TraceCursor> {
    // Three logical regions of the fixture:
    //   * Overview section body (maps to pages 1-2)               → "early"
    //   * Benchmark Cycle body inside the \@for expansion         → "middle"
    //     (the macro expands 48 times, proxying pages 31-70)
    //   * Cycle Validation Notes inside the same expansion        → "late"
    //     (same \@for expansion also covers pages 71-100)
    // Each cursor exposes both a completion column (right after a partial
    // `\ref{sec:` context so the completion provider returns label
    // candidates) and a definition column (over a complete `\ref{sec:...}`
    // argument so the definition provider resolves it to a label target).
    let lines: Vec<&str> = text.lines().collect();

    let early_line = find_line_index(text, "internal references to Section");
    let early_completion = character_in_line(lines[early_line as usize], "\\ref{sec:m");
    let early_definition = character_in_line(lines[early_line as usize], "\\ref{sec:me");

    let middle_line = find_line_index(text, "Cycle \\the\\loopcount\\ revisits Section");
    let middle_completion = character_in_line(lines[middle_line as usize], "\\ref{sec:o");
    let middle_definition = character_in_line(lines[middle_line as usize], "\\ref{sec:ov");

    let late_line = find_line_index(text, "This validation page cross-references Section");
    let late_completion = character_in_line(lines[late_line as usize], "\\ref{sec:m");
    let late_definition = character_in_line(lines[late_line as usize], "\\ref{sec:me");

    vec![
        TraceCursor {
            label: "early(pages 1-30)",
            line: early_line,
            completion_character: early_completion,
            definition_character: early_definition,
        },
        TraceCursor {
            label: "middle(pages 31-70)",
            line: middle_line,
            completion_character: middle_completion,
            definition_character: middle_definition,
        },
        TraceCursor {
            label: "late(pages 71-100)",
            line: late_line,
            completion_character: late_completion,
            definition_character: late_definition,
        },
    ]
}

fn measure_trace_replay_diagnostics(
    stdin: &mut impl Write,
    reader: &mut impl BufRead,
    uri: &str,
    text: &str,
    version: i64,
) -> Duration {
    let started_at = Instant::now();
    write_lsp_message(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }],
            },
        }),
    );
    let response = read_lsp_message(reader);
    assert_eq!(response["method"], "textDocument/publishDiagnostics");
    started_at.elapsed()
}

fn measure_trace_replay_completion(
    stdin: &mut impl Write,
    reader: &mut impl BufRead,
    uri: &str,
    cursor: &TraceCursor,
    request_id: i64,
) -> Duration {
    let started_at = Instant::now();
    write_lsp_message(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": cursor.line, "character": cursor.completion_character },
            },
        }),
    );
    let response = read_lsp_message(reader);
    assert_eq!(response["id"], request_id);
    let items = response["result"].as_array().unwrap_or_else(|| {
        panic!(
            "completion result should be array at {} line={} char={}",
            cursor.label, cursor.line, cursor.completion_character
        )
    });
    assert!(
        !items.is_empty(),
        "completion should return label candidates at {} line={} char={}",
        cursor.label,
        cursor.line,
        cursor.completion_character
    );
    started_at.elapsed()
}

fn measure_trace_replay_definition(
    stdin: &mut impl Write,
    reader: &mut impl BufRead,
    uri: &str,
    cursor: &TraceCursor,
    request_id: i64,
) -> Duration {
    let started_at = Instant::now();
    write_lsp_message(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": cursor.line, "character": cursor.definition_character },
            },
        }),
    );
    let response = read_lsp_message(reader);
    assert_eq!(response["id"], request_id);
    assert!(
        !response["result"].is_null(),
        "definition should resolve at {} line={} char={}",
        cursor.label,
        cursor.line,
        cursor.definition_character
    );
    started_at.elapsed()
}

/// Replays the FTX-LSP-BENCH-001 warm LSP trace against the 100-page
/// `ftx_bench_001.tex` fixture and asserts REQ-NF-004 thresholds.
///
/// Gated with `#[ignore]` because running a full compile plus 18 LSP round
/// trips against a 100-page document is an order of magnitude slower than
/// the minimal-document smoke tests above and is intended to be invoked
/// explicitly (e.g. `cargo test -p ferritex-cli --test bench_lsp_latency --
/// --ignored ftx_lsp_bench_001_warm_trace_meets_req_nf_004_thresholds`).
#[test]
#[ignore = "FTX-LSP-BENCH-001 warm 100-page LSP trace; run explicitly via --ignored"]
fn ftx_lsp_bench_001_warm_trace_meets_req_nf_004_thresholds() {
    let (dir, tex_file, text) = stage_lsp_bench_001_fixture();
    let uri = format!("file://{}", tex_file.to_str().expect("utf-8 path"));
    let root_uri = format!("file://{}", dir.path().to_str().expect("utf-8 path"));

    // Warm step (1): full compile once; materialises the on-disk cache, the
    // asset bundle mmap, and font resolution state.
    run_warm_full_compile(&tex_file);

    // Warm step (2): start `ferritex lsp` and complete the initialize
    // handshake.
    let mut child = Command::new(ferritex_bin())
        .args(["lsp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ferritex lsp");
    let mut stdin = child.stdin.take().expect("lsp stdin");
    let stdout = child.stdout.take().expect("lsp stdout");
    let mut reader = BufReader::new(stdout);

    lsp_initialize(&mut stdin, &mut reader, &root_uri);

    // Warm step (3): didOpen and wait for the initial publishDiagnostics
    // (the first Stable Compile State signal for this document).
    lsp_did_open(&mut stdin, &uri, &text);
    let initial = read_lsp_message(&mut reader);
    assert_eq!(
        initial["method"],
        "textDocument/publishDiagnostics",
        "LSP must publish initial diagnostics before measured replay"
    );

    let cursors = ftx_lsp_bench_cursors(&text);

    // Trace replay: 1 warmup round then 5 measured rounds rotating across
    // the three logical regions (covers pages 1-30 / 31-70 / 71-100 per
    // FTX-LSP-BENCH-001 via the \@for macro expansion in the fixture).
    // 1 + 5 = 6 rounds × 3 operation types = 18 trace events.
    const MEASURED_ROUNDS: usize = 5;
    let total_rounds = 1 + MEASURED_ROUNDS;

    let mut diagnostics_samples = Vec::with_capacity(MEASURED_ROUNDS);
    let mut completion_samples = Vec::with_capacity(MEASURED_ROUNDS);
    let mut definition_samples = Vec::with_capacity(MEASURED_ROUNDS);

    for round in 0..total_rounds {
        let cursor = &cursors[round % cursors.len()];

        let diagnostic_latency = measure_trace_replay_diagnostics(
            &mut stdin,
            &mut reader,
            &uri,
            &text,
            (2 + round) as i64,
        );
        let completion_latency = measure_trace_replay_completion(
            &mut stdin,
            &mut reader,
            &uri,
            cursor,
            (1_000 + round) as i64,
        );
        let definition_latency = measure_trace_replay_definition(
            &mut stdin,
            &mut reader,
            &uri,
            cursor,
            (2_000 + round) as i64,
        );

        if round == 0 {
            eprintln!(
                "FTX-LSP-BENCH-001 warm-up round at {}: diagnostics={:?} completion={:?} definition={:?}",
                cursor.label, diagnostic_latency, completion_latency, definition_latency
            );
        } else {
            eprintln!(
                "FTX-LSP-BENCH-001 measured round {} at {}: diagnostics={:?} completion={:?} definition={:?}",
                round, cursor.label, diagnostic_latency, completion_latency, definition_latency
            );
            diagnostics_samples.push(diagnostic_latency);
            completion_samples.push(completion_latency);
            definition_samples.push(definition_latency);
        }
    }

    lsp_shutdown_exit(&mut stdin, &mut reader, &mut child);
    drop(dir);

    let diagnostics_median = measure_median(&diagnostics_samples);
    let completion_median = measure_median(&completion_samples);
    let definition_median = measure_median(&definition_samples);

    eprintln!(
        "FTX-LSP-BENCH-001 medians (5 replays): diagnostics={:?} (REQ-NF-004 < 500ms), completion={:?} (REQ-NF-004 < 100ms), definition={:?} (REQ-NF-004 < 200ms)",
        diagnostics_median, completion_median, definition_median
    );

    assert!(
        diagnostics_median <= Duration::from_millis(500),
        "FTX-LSP-BENCH-001 diagnostics median exceeded REQ-NF-004 threshold (500ms): {:?}",
        diagnostics_median
    );
    assert!(
        completion_median <= Duration::from_millis(100),
        "FTX-LSP-BENCH-001 completion median exceeded REQ-NF-004 threshold (100ms): {:?}",
        completion_median
    );
    assert!(
        definition_median <= Duration::from_millis(200),
        "FTX-LSP-BENCH-001 definition median exceeded REQ-NF-004 threshold (200ms): {:?}",
        definition_median
    );
}
