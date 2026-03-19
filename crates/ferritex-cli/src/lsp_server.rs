use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use ferritex_application::compile_job_service::CompileJobService;
use ferritex_application::live_analysis_snapshot::{
    CodeActionSuggestion, CompletionCandidate, CompletionKind, DefinitionLocation, HoverInfo,
    LiveAnalysisSnapshotFactory, TextRange,
};
use ferritex_application::lsp_capability_service::{
    LspCapabilityService, ServerCapabilities, TextDocumentSyncKind,
};
use ferritex_application::open_document_store::{OpenDocumentBuffer, OpenDocumentStore};
use ferritex_application::stable_compile_state::StableCompileState;
use ferritex_core::diagnostics::{Diagnostic, Severity};
use ferritex_core::policy::{ExecutionPolicy, PreviewPublicationPolicy};
use ferritex_infra::asset_bundle::AssetBundleLoader;
use ferritex_infra::fs::FsFileAccessGate;
use serde_json::{json, Value};

use crate::emit_diagnostic;

const MAX_LSP_MESSAGE_BYTES: usize = 10 * 1024 * 1024;

pub fn run_lsp() -> i32 {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());
    let capabilities = LspCapabilityService::default();
    let analysis_factory = LiveAnalysisSnapshotFactory::default();
    let mut documents = OpenDocumentStore::default();
    let mut latest_compile_state: Option<(String, StableCompileState)> = None;
    let mut shutdown_requested = false;

    loop {
        let message = match read_message(&mut reader) {
            Ok(Some(message)) => message,
            Ok(None) => return if shutdown_requested { 0 } else { 1 },
            Err(error) => {
                emit_diagnostic(&Diagnostic::new(
                    Severity::Error,
                    format!("failed to read LSP message: {error}"),
                ));
                return 2;
            }
        };

        let Some(method) = message.get("method").and_then(Value::as_str) else {
            continue;
        };
        let id = message.get("id").cloned();

        match method {
            "initialize" => {
                if let Some(id) = id {
                    let result = json!({
                        "capabilities": capabilities_to_json(&capabilities.capabilities()),
                        "serverInfo": {
                            "name": "ferritex",
                            "version": "0.1.0"
                        }
                    });
                    if let Err(error) = write_response(&mut writer, id, result) {
                        emit_diagnostic(&Diagnostic::new(
                            Severity::Error,
                            format!("failed to write initialize response: {error}"),
                        ));
                        return 2;
                    }
                }
            }
            "initialized" => {}
            "shutdown" => {
                shutdown_requested = true;
                if let Some(id) = id {
                    if let Err(error) = write_response(&mut writer, id, Value::Null) {
                        emit_diagnostic(&Diagnostic::new(
                            Severity::Error,
                            format!("failed to write shutdown response: {error}"),
                        ));
                        return 2;
                    }
                }
            }
            "exit" => return if shutdown_requested { 0 } else { 1 },
            "textDocument/didOpen" => {
                handle_did_open(
                    &mut documents,
                    &analysis_factory,
                    &mut latest_compile_state,
                    &mut writer,
                    &message,
                );
            }
            "textDocument/didChange" => {
                handle_did_change(
                    &mut documents,
                    &analysis_factory,
                    &mut latest_compile_state,
                    &mut writer,
                    &message,
                );
            }
            "textDocument/didClose" => {
                handle_did_close(
                    &mut documents,
                    &mut latest_compile_state,
                    &mut writer,
                    &message,
                );
            }
            "textDocument/completion" => {
                if let Some(id) = id {
                    let result = handle_completion(
                        &documents,
                        &analysis_factory,
                        request_uri(&message).and_then(|uri| {
                            compile_state_for_uri(latest_compile_state.as_ref(), uri)
                        }),
                        &message,
                    );
                    if let Err(error) = write_response(&mut writer, id, result) {
                        emit_diagnostic(&Diagnostic::new(
                            Severity::Error,
                            format!("failed to write completion response: {error}"),
                        ));
                        return 2;
                    }
                }
            }
            "textDocument/definition" => {
                if let Some(id) = id {
                    let result = handle_definition(
                        &documents,
                        &analysis_factory,
                        request_uri(&message).and_then(|uri| {
                            compile_state_for_uri(latest_compile_state.as_ref(), uri)
                        }),
                        &message,
                    );
                    if let Err(error) = write_response(&mut writer, id, result) {
                        emit_diagnostic(&Diagnostic::new(
                            Severity::Error,
                            format!("failed to write definition response: {error}"),
                        ));
                        return 2;
                    }
                }
            }
            "textDocument/hover" => {
                if let Some(id) = id {
                    let result = handle_hover(
                        &documents,
                        &analysis_factory,
                        request_uri(&message).and_then(|uri| {
                            compile_state_for_uri(latest_compile_state.as_ref(), uri)
                        }),
                        &message,
                    );
                    if let Err(error) = write_response(&mut writer, id, result) {
                        emit_diagnostic(&Diagnostic::new(
                            Severity::Error,
                            format!("failed to write hover response: {error}"),
                        ));
                        return 2;
                    }
                }
            }
            "textDocument/codeAction" => {
                if let Some(id) = id {
                    let result = handle_code_action(
                        &documents,
                        &analysis_factory,
                        request_uri(&message).and_then(|uri| {
                            compile_state_for_uri(latest_compile_state.as_ref(), uri)
                        }),
                        &message,
                    );
                    if let Err(error) = write_response(&mut writer, id, result) {
                        emit_diagnostic(&Diagnostic::new(
                            Severity::Error,
                            format!("failed to write code action response: {error}"),
                        ));
                        return 2;
                    }
                }
            }
            _ => {
                if let Some(id) = id {
                    let error = json!({
                        "code": -32601,
                        "message": format!("method `{method}` is not implemented"),
                    });
                    if let Err(error) = write_error(&mut writer, id, error) {
                        emit_diagnostic(&Diagnostic::new(
                            Severity::Error,
                            format!("failed to write error response: {error}"),
                        ));
                        return 2;
                    }
                }
            }
        }
    }
}

fn handle_did_open(
    documents: &mut OpenDocumentStore,
    analysis_factory: &LiveAnalysisSnapshotFactory,
    latest_compile_state: &mut Option<(String, StableCompileState)>,
    writer: &mut impl Write,
    message: &Value,
) {
    let Some(document) = message
        .get("params")
        .and_then(|params| params.get("textDocument"))
    else {
        return;
    };
    let Some(uri) = document.get("uri").and_then(Value::as_str) else {
        return;
    };
    let Some(language_id) = document.get("languageId").and_then(Value::as_str) else {
        return;
    };
    let Some(version) = document.get("version").and_then(Value::as_i64) else {
        return;
    };
    let Some(text) = document.get("text").and_then(Value::as_str) else {
        return;
    };

    documents.open(OpenDocumentBuffer {
        uri: uri.to_string(),
        language_id: language_id.to_string(),
        version: version as i32,
        text: text.to_string(),
    });
    refresh_compile_state(latest_compile_state, uri, text);
    publish_diagnostics(
        documents,
        analysis_factory,
        compile_state_for_uri(latest_compile_state.as_ref(), uri),
        writer,
        uri,
    );
}

fn handle_did_change(
    documents: &mut OpenDocumentStore,
    analysis_factory: &LiveAnalysisSnapshotFactory,
    latest_compile_state: &mut Option<(String, StableCompileState)>,
    writer: &mut impl Write,
    message: &Value,
) {
    let Some(params) = message.get("params") else {
        return;
    };
    let Some(uri) = params
        .get("textDocument")
        .and_then(|document| document.get("uri"))
        .and_then(Value::as_str)
    else {
        return;
    };
    let Some(version) = params
        .get("textDocument")
        .and_then(|document| document.get("version"))
        .and_then(Value::as_i64)
    else {
        return;
    };
    let Some(text) = params
        .get("contentChanges")
        .and_then(Value::as_array)
        .and_then(|changes| changes.last())
        .and_then(|change| change.get("text"))
        .and_then(Value::as_str)
    else {
        return;
    };

    if documents
        .update(uri, version as i32, text.to_string())
        .is_some()
    {
        refresh_compile_state(latest_compile_state, uri, text);
        publish_diagnostics(
            documents,
            analysis_factory,
            compile_state_for_uri(latest_compile_state.as_ref(), uri),
            writer,
            uri,
        );
    }
}

fn handle_did_close(
    documents: &mut OpenDocumentStore,
    latest_compile_state: &mut Option<(String, StableCompileState)>,
    writer: &mut impl Write,
    message: &Value,
) {
    let Some(uri) = message
        .get("params")
        .and_then(|params| params.get("textDocument"))
        .and_then(|document| document.get("uri"))
        .and_then(Value::as_str)
    else {
        return;
    };

    if documents.close(uri).is_some() {
        if latest_compile_state
            .as_ref()
            .is_some_and(|(state_uri, _)| state_uri == uri)
        {
            *latest_compile_state = None;
        }
        let params = json!({
            "uri": uri,
            "diagnostics": [],
        });
        let _ = write_notification(writer, "textDocument/publishDiagnostics", params);
    }
}

fn handle_completion(
    documents: &OpenDocumentStore,
    analysis_factory: &LiveAnalysisSnapshotFactory,
    compile_state: Option<&StableCompileState>,
    message: &Value,
) -> Value {
    let Some((uri, line, character)) = request_position(message) else {
        return json!([]);
    };
    let Some(document) = documents.get(&uri) else {
        return json!([]);
    };

    let snapshot = analysis_factory.build(document, compile_state);
    let items = snapshot
        .completions(ferritex_application::live_analysis_snapshot::TextPosition { line, character })
        .into_iter()
        .map(completion_to_json)
        .collect::<Vec<_>>();

    json!(items)
}

fn handle_definition(
    documents: &OpenDocumentStore,
    analysis_factory: &LiveAnalysisSnapshotFactory,
    compile_state: Option<&StableCompileState>,
    message: &Value,
) -> Value {
    let Some((uri, line, character)) = request_position(message) else {
        return Value::Null;
    };
    let Some(document) = documents.get(&uri) else {
        return Value::Null;
    };

    let snapshot = analysis_factory.build(document, compile_state);
    snapshot
        .definition(ferritex_application::live_analysis_snapshot::TextPosition { line, character })
        .map(definition_to_json)
        .unwrap_or(Value::Null)
}

fn handle_hover(
    documents: &OpenDocumentStore,
    analysis_factory: &LiveAnalysisSnapshotFactory,
    compile_state: Option<&StableCompileState>,
    message: &Value,
) -> Value {
    let Some((uri, line, character)) = request_position(message) else {
        return Value::Null;
    };
    let Some(document) = documents.get(&uri) else {
        return Value::Null;
    };

    let snapshot = analysis_factory.build(document, compile_state);
    snapshot
        .hover(ferritex_application::live_analysis_snapshot::TextPosition { line, character })
        .map(hover_to_json)
        .unwrap_or(Value::Null)
}

fn handle_code_action(
    documents: &OpenDocumentStore,
    analysis_factory: &LiveAnalysisSnapshotFactory,
    compile_state: Option<&StableCompileState>,
    message: &Value,
) -> Value {
    let Some(uri) = message
        .get("params")
        .and_then(|params| params.get("textDocument"))
        .and_then(|document| document.get("uri"))
        .and_then(Value::as_str)
    else {
        return json!([]);
    };
    let Some(document) = documents.get(uri) else {
        return json!([]);
    };

    let snapshot = analysis_factory.build(document, compile_state);
    let actions = snapshot
        .code_actions()
        .iter()
        .map(|action| code_action_to_json(uri, action))
        .collect::<Vec<_>>();
    json!(actions)
}

fn publish_diagnostics(
    documents: &OpenDocumentStore,
    analysis_factory: &LiveAnalysisSnapshotFactory,
    compile_state: Option<&StableCompileState>,
    writer: &mut impl Write,
    uri: &str,
) {
    let Some(document) = documents.get(uri) else {
        return;
    };
    let snapshot = analysis_factory.build(document, compile_state);
    let diagnostics = snapshot
        .diagnostics()
        .iter()
        .map(diagnostic_to_json)
        .collect::<Vec<_>>();

    let params = json!({
        "uri": uri,
        "diagnostics": diagnostics,
    });
    let _ = write_notification(writer, "textDocument/publishDiagnostics", params);
}

fn request_position(message: &Value) -> Option<(String, u32, u32)> {
    let params = message.get("params")?;
    let uri = params
        .get("textDocument")?
        .get("uri")?
        .as_str()?
        .to_string();
    let line = params.get("position")?.get("line")?.as_u64()? as u32;
    let character = params.get("position")?.get("character")?.as_u64()? as u32;
    Some((uri, line, character))
}

fn request_uri(message: &Value) -> Option<&str> {
    message
        .get("params")
        .and_then(|params| params.get("textDocument"))
        .and_then(|document| document.get("uri"))
        .and_then(Value::as_str)
}

fn refresh_compile_state(
    latest_compile_state: &mut Option<(String, StableCompileState)>,
    uri: &str,
    text: &str,
) {
    if uri.starts_with("file://") {
        let primary_input = PathBuf::from(uri.trim_start_matches("file://"));
        let workspace_root = primary_input
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let file_access_gate = FsFileAccessGate::from_policy(ExecutionPolicy {
            shell_escape_allowed: false,
            allowed_read_paths: vec![workspace_root.clone()],
            allowed_write_paths: vec![workspace_root.clone()],
            output_dir: workspace_root,
            jobname: "lsp".to_string(),
            preview_publication: Some(PreviewPublicationPolicy {
                loopback_only: true,
                active_job_only: true,
            }),
        });
        let asset_bundle_loader = AssetBundleLoader;
        let compile_service = CompileJobService::new(&file_access_gate, &asset_bundle_loader);
        *latest_compile_state = Some((
            uri.to_string(),
            compile_service.compile_from_source(text, uri),
        ));
    } else {
        *latest_compile_state = None;
    }
}

fn compile_state_for_uri<'a>(
    latest_compile_state: Option<&'a (String, StableCompileState)>,
    uri: &str,
) -> Option<&'a StableCompileState> {
    latest_compile_state.and_then(|(state_uri, state)| (state_uri == uri).then_some(state))
}

fn capabilities_to_json(capabilities: &ServerCapabilities) -> Value {
    json!({
        "textDocumentSync": match capabilities.text_document_sync {
            TextDocumentSyncKind::None => 0,
            TextDocumentSyncKind::Full => 1,
            TextDocumentSyncKind::Incremental => 2,
        },
        "completionProvider": {
            "triggerCharacters": capabilities.completion_provider.trigger_characters,
        },
        "codeActionProvider": capabilities.code_action_provider,
        "definitionProvider": capabilities.definition_provider,
        "hoverProvider": capabilities.hover_provider,
    })
}

fn diagnostic_to_json(
    diagnostic: &ferritex_application::live_analysis_snapshot::AnalysisDiagnostic,
) -> Value {
    json!({
        "range": range_to_json(diagnostic.range),
        "severity": severity_to_lsp(diagnostic.severity),
        "message": diagnostic.message,
        "data": diagnostic.suggestion.as_ref().map(|suggestion| json!({ "suggestion": suggestion })),
    })
}

fn completion_to_json(completion: CompletionCandidate) -> Value {
    json!({
        "label": completion.label,
        "kind": match completion.kind {
            CompletionKind::Command => 3,
            CompletionKind::Environment => 14,
            CompletionKind::Label => 18,
            CompletionKind::Citation => 18,
        },
        "detail": completion.detail,
    })
}

fn definition_to_json(definition: DefinitionLocation) -> Value {
    json!({
        "uri": definition.uri,
        "range": range_to_json(definition.range),
    })
}

fn hover_to_json(hover: HoverInfo) -> Value {
    json!({
        "contents": {
            "kind": "markdown",
            "value": hover.markdown,
        },
        "range": range_to_json(hover.range),
    })
}

fn code_action_to_json(uri: &str, action: &CodeActionSuggestion) -> Value {
    json!({
        "title": action.title,
        "kind": "quickfix",
        "edit": {
            "changes": {
                uri: [
                    {
                        "range": range_to_json(action.edit.range),
                        "newText": action.edit.new_text,
                    }
                ]
            }
        }
    })
}

fn range_to_json(range: TextRange) -> Value {
    json!({
        "start": {
            "line": range.start.line,
            "character": range.start.character,
        },
        "end": {
            "line": range.end.line,
            "character": range.end.character,
        }
    })
}

fn severity_to_lsp(severity: Severity) -> u8 {
    match severity {
        Severity::Error => 1,
        Severity::Warning => 2,
        Severity::Info => 3,
    }
}

fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut content_length = None;

    loop {
        let mut header = String::new();
        let bytes = reader.read_line(&mut header)?;
        if bytes == 0 {
            return Ok(None);
        }

        if header == "\r\n" {
            break;
        }

        if let Some(value) = header.strip_prefix("Content-Length:") {
            let parsed = value.trim().parse::<usize>().map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid content length: {error}"),
                )
            })?;
            if parsed > MAX_LSP_MESSAGE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("content length {parsed} exceeds limit {MAX_LSP_MESSAGE_BYTES}"),
                ));
            }
            content_length = Some(parsed);
        }
    }

    let Some(content_length) = content_length else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing Content-Length header",
        ));
    };

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn write_response(writer: &mut impl Write, id: Value, result: Value) -> io::Result<()> {
    write_message(
        writer,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }),
    )
}

fn write_error(writer: &mut impl Write, id: Value, error: Value) -> io::Result<()> {
    write_message(
        writer,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": error,
        }),
    )
}

fn write_notification(writer: &mut impl Write, method: &str, params: Value) -> io::Result<()> {
    write_message(
        writer,
        &json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }),
    )
}

fn write_message(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}
