use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
