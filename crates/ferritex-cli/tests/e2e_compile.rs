use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn ferritex_bin() -> Command {
    let bin = env!("CARGO_BIN_EXE_ferritex");
    Command::new(bin)
}

#[test]
fn compile_nonexistent_file_exits_with_code_2() {
    let output = ferritex_bin()
        .args(["compile", "nonexistent.tex"])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("input file not found"));
}

#[test]
fn compile_existing_file_writes_pdf_with_document_content() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("hello.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nHello, Ferritex!\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.trim().is_empty());

    let pdf_file = dir.path().join("hello.pdf");
    let pdf = std::fs::read_to_string(&pdf_file).expect("read output pdf");
    assert!(pdf.starts_with("%PDF-1.4"));
    assert!(pdf.contains("Hello, Ferritex!"));
    assert!(!pdf.contains("Ferritex placeholder PDF"));
}

#[test]
fn compile_expands_def_macro_into_pdf_output() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("macro.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\def\\hello{Hello, Macro!}\n\\begin{document}\n\\hello\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("macro.pdf")).expect("read output pdf");
    assert!(pdf.contains("Hello, Macro!"));
}

#[test]
fn compile_applies_catcode_changes_during_parsing() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("catcode.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\catcode`\\@=11\n\\def\\make@title{Catcode parsing works}\n\\begin{document}\n\\make@title\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("catcode.pdf")).expect("read output pdf");
    assert!(pdf.contains("Catcode parsing works"));
}

#[test]
fn compile_respects_group_scoped_macros() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("scoped.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n{\\def\\local{Scoped }\\local}\\local\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("scoped.pdf")).expect("read output pdf");
    assert!(pdf.contains("Scoped "));
    assert!(pdf.contains("\\\\local"));
}

#[test]
fn compile_resolves_nested_input_files() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let main = dir.path().join("main.tex");
    let chapter_dir = dir.path().join("chapters");
    let section_dir = chapter_dir.join("sections");
    std::fs::create_dir_all(&section_dir).expect("create source tree");
    std::fs::write(
        &main,
        "\\documentclass{article}\n\\begin{document}\n\\input{chapters/intro}\n\\end{document}\n",
    )
    .expect("write main");
    std::fs::write(
        chapter_dir.join("intro.tex"),
        "Intro line.\n\\input{sections/detail}\n",
    )
    .expect("write intro");
    std::fs::write(section_dir.join("detail.tex"), "Nested detail.\n").expect("write detail");

    let output = ferritex_bin()
        .args(["compile", main.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("main.pdf")).expect("read output pdf");
    assert!(pdf.contains("Intro line."));
    assert!(pdf.contains("Nested detail."));
}

#[test]
fn compile_resolves_project_root_fallback_from_nested_input() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let project_root = dir.path().join("project");
    let src_dir = project_root.join("src");
    let section_dir = src_dir.join("chapters");
    let shared_dir = project_root.join("shared");
    std::fs::create_dir_all(&section_dir).expect("create source tree");
    std::fs::create_dir_all(&shared_dir).expect("create shared tree");

    std::fs::write(
        src_dir.join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\n\\input{chapters/section}\n\\end{document}\n",
    )
    .expect("write main");
    std::fs::write(section_dir.join("section.tex"), "\\input{shared/macros}\n")
        .expect("write nested section");
    std::fs::write(shared_dir.join("macros.tex"), "Project root fallback.\n")
        .expect("write shared macros");

    let output = ferritex_bin()
        .current_dir(&project_root)
        .args(["compile", "src/main.tex"])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(src_dir.join("main.pdf")).expect("read output pdf");
    assert!(pdf.contains("Project root fallback."));
}

#[test]
fn compile_resolves_tex_input_from_asset_bundle_outside_project_root() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let project_root = dir.path().join("project");
    let bundle_root = dir.path().join("bundle");
    std::fs::create_dir_all(project_root.join("src")).expect("create source tree");
    std::fs::create_dir_all(bundle_root.join("texmf")).expect("create bundle texmf");
    std::fs::write(
        bundle_root.join("manifest.json"),
        r#"{"name":"default","version":"2026.03.18","min_ferritex_version":"0.1.0"}"#,
    )
    .expect("write bundle manifest");
    std::fs::write(
        bundle_root.join("texmf/bundled.tex"),
        "Bundled from asset bundle.\n",
    )
    .expect("write bundled tex input");
    std::fs::write(
        project_root.join("src/main.tex"),
        "\\documentclass{article}\n\\begin{document}\n\\input{bundled}\n\\end{document}\n",
    )
    .expect("write main");

    let output = ferritex_bin()
        .current_dir(&project_root)
        .args([
            "compile",
            "src/main.tex",
            "--asset-bundle",
            bundle_root.to_str().expect("utf-8 bundle path"),
        ])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(project_root.join("src/main.pdf")).expect("read output pdf");
    assert!(pdf.contains("Bundled from asset bundle."));
}

#[test]
fn compile_rejects_commented_out_documentclass() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("broken.tex");
    std::fs::write(
        &tex_file,
        "% \\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("missing \\documentclass declaration"));
    assert!(!dir.path().join("broken.pdf").exists());
}

#[test]
fn compile_rejects_trailing_content_after_end_document() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("trailing.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\nTrailing\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected content after \\end{document}"));
    assert!(!dir.path().join("trailing.pdf").exists());
}

#[test]
fn compile_with_missing_asset_bundle_reports_validation_error() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("hello.tex");
    std::fs::write(&tex_file, "\\documentclass{article}\n").expect("write input file");

    let output = ferritex_bin()
        .args([
            "compile",
            tex_file.to_str().expect("utf-8 path"),
            "--asset-bundle",
            dir.path()
                .join("missing-bundle")
                .to_str()
                .expect("utf-8 path"),
        ])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("bundle not found"));
}

#[test]
fn watch_writes_initial_pdf_and_recompiles_on_change() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("hello.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\n",
    )
    .expect("write input file");

    let mut child = ferritex_bin()
        .args(["watch", tex_file.to_str().expect("utf-8 path")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ferritex watch");

    let pdf_file = dir.path().join("hello.pdf");
    wait_until(
        || pdf_file.exists(),
        Duration::from_secs(2),
        "watch should emit the initial PDF",
    );
    let initial_modified = std::fs::metadata(&pdf_file)
        .expect("initial metadata")
        .modified()
        .expect("initial modified time");

    thread::sleep(Duration::from_millis(20));
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nUpdated\n\\end{document}\n",
    )
    .expect("rewrite input file");

    wait_until(
        || {
            std::fs::metadata(&pdf_file)
                .and_then(|metadata| metadata.modified())
                .map(|modified| modified > initial_modified)
                .unwrap_or(false)
        },
        Duration::from_secs(2),
        "watch should recompile after a source change",
    );

    child.kill().expect("kill watch process");
    child.wait().expect("wait for watch process");
}

#[test]
fn watch_refreshes_dependency_set_after_new_input_is_added() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("main.tex");
    let appendix = dir.path().join("appendix.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nInitial\n\\end{document}\n",
    )
    .expect("write input file");

    let mut child = ferritex_bin()
        .args(["watch", tex_file.to_str().expect("utf-8 path")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ferritex watch");

    let pdf_file = dir.path().join("main.pdf");
    wait_until(
        || pdf_file.exists(),
        Duration::from_secs(2),
        "watch should emit the initial PDF",
    );
    let initial_modified = std::fs::metadata(&pdf_file)
        .expect("initial metadata")
        .modified()
        .expect("initial modified time");

    thread::sleep(Duration::from_millis(20));
    std::fs::write(&appendix, "Appendix v1\n").expect("write appendix");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\input{appendix}\n\\end{document}\n",
    )
    .expect("rewrite main");

    wait_until(
        || {
            std::fs::metadata(&pdf_file)
                .and_then(|metadata| metadata.modified())
                .map(|modified| modified > initial_modified)
                .unwrap_or(false)
        },
        Duration::from_secs(2),
        "watch should recompile after adding a new input dependency",
    );
    let after_main_change = std::fs::metadata(&pdf_file)
        .expect("updated metadata")
        .modified()
        .expect("updated modified time");

    thread::sleep(Duration::from_millis(20));
    std::fs::write(&appendix, "Appendix v2\n").expect("rewrite appendix");

    wait_until(
        || {
            std::fs::metadata(&pdf_file)
                .and_then(|metadata| metadata.modified())
                .map(|modified| modified > after_main_change)
                .unwrap_or(false)
        },
        Duration::from_secs(2),
        "watch should pick up changes from a newly discovered dependency",
    );

    child.kill().expect("kill watch process");
    child.wait().expect("wait for watch process");
}

#[test]
fn lsp_initialize_and_diagnostics_work_over_stdio() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("main.tex");
    let uri = format!("file://{}", tex_file.to_str().expect("utf-8 path"));
    let mut child = ferritex_bin()
        .args(["lsp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ferritex lsp");
    let mut stdin = child.stdin.take().expect("lsp stdin");
    let stdout = child.stdout.take().expect("lsp stdout");
    let mut reader = BufReader::new(stdout);

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": format!("file://{}", dir.path().to_str().expect("utf-8 path")),
                "capabilities": {}
            }
        }),
    );
    let initialize = read_lsp_message(&mut reader);
    assert_eq!(initialize["id"], 1);
    assert_eq!(initialize["result"]["capabilities"]["textDocumentSync"], 1);
    assert!(initialize["result"]["capabilities"]["completionProvider"].is_object());
    assert_eq!(
        initialize["result"]["capabilities"]["codeActionProvider"],
        true
    );
    assert_eq!(
        initialize["result"]["capabilities"]["definitionProvider"],
        true
    );
    assert_eq!(initialize["result"]["capabilities"]["hoverProvider"], true);

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }),
    );
    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "latex",
                    "version": 1,
                    "text": "\\documentclass{article}\n\\begin{document}\n\\begin{equation}\na=b\n\\end{document}\n"
                }
            }
        }),
    );

    let diagnostics = read_lsp_message(&mut reader);
    assert_eq!(diagnostics["method"], "textDocument/publishDiagnostics");
    let messages = diagnostics["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics array")
        .iter()
        .filter_map(|diagnostic| diagnostic.get("message").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(messages
        .iter()
        .any(|message| message.contains("unclosed environment `equation`")));

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "shutdown",
            "params": null
        }),
    );
    let shutdown = read_lsp_message(&mut reader);
    assert_eq!(shutdown["id"], 2);
    assert_eq!(shutdown["result"], Value::Null);
    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }),
    );

    assert!(child.wait().expect("wait lsp").success());
}

#[test]
fn lsp_definition_resolves_labels_from_included_files() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let chapter_dir = dir.path().join("chapters");
    std::fs::create_dir_all(&chapter_dir).expect("create chapter dir");
    std::fs::write(
        chapter_dir.join("figures.tex"),
        "\\label{fig:external}\nFigure content\n",
    )
    .expect("write included file");

    let tex_file = dir.path().join("main.tex");
    let uri = format!("file://{}", tex_file.to_str().expect("utf-8 path"));
    let mut child = ferritex_bin()
        .args(["lsp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ferritex lsp");
    let mut stdin = child.stdin.take().expect("lsp stdin");
    let stdout = child.stdout.take().expect("lsp stdout");
    let mut reader = BufReader::new(stdout);

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": format!("file://{}", dir.path().to_str().expect("utf-8 path")),
                "capabilities": {}
            }
        }),
    );
    let _initialize = read_lsp_message(&mut reader);

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }),
    );
    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "latex",
                    "version": 1,
                    "text": "\\documentclass{article}\n\\begin{document}\n\\input{chapters/figures}\nSee \\ref{fig:external}.\n\\end{document}\n"
                }
            }
        }),
    );
    let _diagnostics = read_lsp_message(&mut reader);

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 3, "character": 13 }
            }
        }),
    );
    let definition = read_lsp_message(&mut reader);
    let expected_target = chapter_dir
        .join("figures.tex")
        .canonicalize()
        .expect("canonical included file");
    assert_eq!(definition["id"], 2);
    assert_eq!(
        definition["result"]["uri"],
        format!("file://{}", expected_target.to_str().expect("utf-8 path"))
    );
    assert_eq!(definition["result"]["range"]["start"]["line"], 0);

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "shutdown",
            "params": null
        }),
    );
    let shutdown = read_lsp_message(&mut reader);
    assert_eq!(shutdown["id"], 3);
    assert_eq!(shutdown["result"], Value::Null);
    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }),
    );

    assert!(child.wait().expect("wait lsp").success());
}

#[test]
fn lsp_diagnostics_include_compile_errors() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("main.tex");
    let uri = format!("file://{}", tex_file.to_str().expect("utf-8 path"));
    let mut child = ferritex_bin()
        .args(["lsp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ferritex lsp");
    let mut stdin = child.stdin.take().expect("lsp stdin");
    let stdout = child.stdout.take().expect("lsp stdout");
    let mut reader = BufReader::new(stdout);

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": format!("file://{}", dir.path().to_str().expect("utf-8 path")),
                "capabilities": {}
            }
        }),
    );
    let _initialize = read_lsp_message(&mut reader);

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }),
    );
    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "latex",
                    "version": 1,
                    "text": "\\documentclass{article}\n\\begin{document}\n\\begin{equation}\na=b\n"
                }
            }
        }),
    );

    let initial_diagnostics = read_lsp_message(&mut reader);
    let initial_messages = initial_diagnostics["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics array")
        .iter()
        .filter_map(|diagnostic| diagnostic.get("message").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(initial_messages
        .iter()
        .any(|message| message.contains("missing \\end{document}")));
    assert!(initial_messages
        .iter()
        .any(|message| message.contains("unclosed environment `equation`")));

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "version": 2
                },
                "contentChanges": [
                    {
                        "text": "\\documentclass{article}\n\\begin{document}\n\\begin{equation}\na=b\n\\end{document}\n"
                    }
                ]
            }
        }),
    );

    let updated_diagnostics = read_lsp_message(&mut reader);
    let updated_messages = updated_diagnostics["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics array")
        .iter()
        .filter_map(|diagnostic| diagnostic.get("message").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(!updated_messages
        .iter()
        .any(|message| message.contains("missing \\end{document}")));
    assert!(updated_messages
        .iter()
        .any(|message| message.contains("unclosed environment `equation`")));

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "shutdown",
            "params": null
        }),
    );
    let shutdown = read_lsp_message(&mut reader);
    assert_eq!(shutdown["id"], 2);
    assert_eq!(shutdown["result"], Value::Null);
    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }),
    );

    assert!(child.wait().expect("wait lsp").success());
}

fn wait_until(mut condition: impl FnMut() -> bool, timeout: Duration, message: &str) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("{message}");
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
