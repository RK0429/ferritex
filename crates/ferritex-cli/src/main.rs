use std::{
    path::PathBuf,
    process,
    sync::{Arc, Mutex},
    thread,
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use ferritex_application::compile_job_service::CompileJobService;
use ferritex_application::execution_policy_factory::ExecutionPolicyFactory;
use ferritex_application::ports::{PreviewTransportPort, TransportRevisionEvent};
use ferritex_application::preview_session_service::{
    PreviewSessionService, PreviewTarget, PreviewViewState, PublishDecision, SessionErrorResponse,
    SessionId,
};
use ferritex_application::runtime_options::{CompileArgs, CompileInteraction, RuntimeOptions};
use ferritex_core::diagnostics::{Diagnostic, Severity};
use ferritex_infra::asset_bundle::AssetBundleLoader;
use ferritex_infra::fs::FsFileAccessGate;
use ferritex_infra::preview::LoopbackPreviewTransport;
use ferritex_infra::shell::ShellCommandGateway;

mod lsp_server;
mod watch_runner;

#[derive(Debug, Parser)]
#[command(name = "ferritex", version, about = "A Rust-native LaTeX compiler")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Compile a LaTeX document to PDF
    Compile(CompileCommand),
    /// Compile and start a live preview server
    Preview(CompileCommand),
    /// Watch for changes and recompile automatically
    Watch(CompileCommand),
    /// Start the Language Server Protocol server
    #[command(
        long_about = "Start the Language Server Protocol server over stdio.

The server speaks JSON-RPC 2.0 using `Content-Length: <N>\\r\\n\\r\\n<N bytes of UTF-8 JSON>` framing.
Oversize or malformed frames are treated as fatal session errors and terminate the session.

Handshake: clients send `initialize` (request), wait for the InitializeResult response, and then send `initialized` (notification) before textDocument requests.

Diagnostics are delivered through `textDocument/publishDiagnostics` notifications.
See README.md for a minimal publishDiagnostics example."
    )]
    Lsp,
}

#[derive(Debug, Clone, Args, PartialEq, Eq)]
struct CompileCommand {
    /// Input LaTeX file to compile
    file: PathBuf,
    /// Output directory for generated files
    #[arg(long, value_name = "DIR")]
    output_dir: Option<PathBuf>,
    /// Job name for the output file (defaults to input file stem)
    #[arg(long, value_name = "NAME")]
    jobname: Option<String>,
    /// Number of parallel compilation tasks (default: CPU cores). High values (>
    /// available cores) can significantly increase peak RSS on heavy fixtures.
    #[arg(long, value_name = "N")]
    jobs: Option<usize>,
    /// Additional directories to search for TeX files
    #[arg(long = "overlay", value_name = "DIR")]
    overlay_roots: Vec<PathBuf>,
    /// Disable compilation cache
    #[arg(long)]
    no_cache: bool,
    /// Path to a pre-built asset bundle
    #[arg(long, value_name = "PATH")]
    asset_bundle: Option<PathBuf>,
    /// Enable reproducible output (deterministic timestamps)
    #[arg(long)]
    reproducible: bool,
    /// TeX interaction mode
    #[arg(long, value_name = "MODE", value_enum)]
    interaction: Option<InteractionArg>,
    /// Generate SyncTeX data for editor synchronization
    #[arg(long)]
    synctex: bool,
    /// Emit font task tracing to stderr
    #[arg(long)]
    trace_font_tasks: bool,
    /// Enable shell escape for \write18 commands
    #[arg(long)]
    shell_escape: bool,
    /// Disable shell escape (overrides --shell-escape)
    #[arg(long)]
    no_shell_escape: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum InteractionArg {
    #[value(name = "nonstopmode")]
    Nonstopmode,
    #[value(name = "batchmode")]
    Batchmode,
    #[value(name = "scrollmode")]
    Scrollmode,
    #[value(name = "errorstopmode")]
    Errorstopmode,
}

impl CompileCommand {
    fn to_compile_args(&self) -> CompileArgs {
        CompileArgs {
            input_file: self.file.clone(),
            output_dir: self.output_dir.clone(),
            jobname: self.jobname.clone(),
            jobs: self.jobs,
            overlay_roots: self.overlay_roots.clone(),
            no_cache: self.no_cache,
            asset_bundle: self.asset_bundle.clone(),
            reproducible: self.reproducible,
            interaction: self.interaction.map(InteractionArg::to_compile_interaction),
            synctex: self.synctex,
            trace_font_tasks: self.trace_font_tasks,
            shell_escape: self.shell_escape,
            no_shell_escape: self.no_shell_escape,
        }
    }
}

impl InteractionArg {
    const fn to_compile_interaction(self) -> CompileInteraction {
        match self {
            Self::Nonstopmode => CompileInteraction::Nonstopmode,
            Self::Batchmode => CompileInteraction::Batchmode,
            Self::Scrollmode => CompileInteraction::Scrollmode,
            Self::Errorstopmode => CompileInteraction::Errorstopmode,
        }
    }
}

fn main() {
    process::exit(run(Cli::parse()));
}

fn run(cli: Cli) -> i32 {
    match cli.command {
        Commands::Compile(command) => handle_compile(&command),
        Commands::Preview(command) => handle_preview(&command),
        Commands::Watch(command) => handle_watch(&command),
        Commands::Lsp => handle_lsp(),
    }
}

fn handle_compile(command: &CompileCommand) -> i32 {
    let options = runtime_options_from_command(command);
    let policy = ExecutionPolicyFactory::create(&options);
    let shell_command_gateway = ShellCommandGateway::from_policy(&policy);
    let file_access_gate = FsFileAccessGate::from_policy(policy);
    let asset_bundle_loader = AssetBundleLoader;
    let service = CompileJobService::new(
        &file_access_gate,
        &asset_bundle_loader,
        &shell_command_gateway,
    );
    let result = service.compile(&options);
    emit_diagnostics(&result.diagnostics);
    if let Some(output_pdf) = &result.output_pdf {
        let page_count = result
            .stable_compile_state
            .as_ref()
            .map_or(0, |state| state.page_count);
        let warning_count = result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Warning)
            .count();
        if warning_count > 0 {
            println!(
                "{} -> {} ({} page{}, {} warning{})",
                command.file.display(),
                output_pdf.display(),
                page_count,
                if page_count == 1 { "" } else { "s" },
                warning_count,
                if warning_count == 1 { "" } else { "s" }
            );
        } else {
            println!(
                "{} -> {} ({} page{})",
                command.file.display(),
                output_pdf.display(),
                page_count,
                if page_count == 1 { "" } else { "s" }
            );
        }
    }
    result.exit_code
}

fn handle_watch(command: &CompileCommand) -> i32 {
    watch_runner::run_watch(command)
}

fn handle_preview(command: &CompileCommand) -> i32 {
    match execute_preview(command) {
        Ok(preview) => {
            println!("{}", preview.document_url);
            println!("{}", preview.events_url);
            eprintln!(
                "preview server listening on http://127.0.0.1:{}",
                preview.server_port
            );
            eprintln!("press Ctrl+C to stop");

            loop {
                thread::park();
            }
        }
        Err(diagnostics) => {
            emit_diagnostics(&diagnostics);
            diagnostics_exit_code(&diagnostics)
        }
    }
}

fn handle_lsp() -> i32 {
    lsp_server::run_lsp()
}

fn runtime_options_from_command(command: &CompileCommand) -> RuntimeOptions {
    let compile_args = command.to_compile_args();
    RuntimeOptions::from_compile_args(&compile_args)
}

fn emit_diagnostics(diagnostics: &[Diagnostic]) {
    for diagnostic in diagnostics {
        emit_diagnostic(diagnostic);
    }
}

fn emit_diagnostic(diagnostic: &Diagnostic) {
    eprintln!("{diagnostic}");
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreviewExecution {
    document_url: String,
    events_url: String,
    server_port: u16,
}

fn execute_preview(command: &CompileCommand) -> Result<PreviewExecution, Vec<Diagnostic>> {
    let options = runtime_options_from_command(command);
    let policy = ExecutionPolicyFactory::create(&options);
    let Some(_preview_policy) = &policy.preview_publication else {
        return Err(vec![Diagnostic::new(
            Severity::Error,
            "preview is disabled by the execution policy",
        )
        .with_file(options.input_file.to_string_lossy().into_owned())
        .with_suggestion("rerun with preview publication enabled")]);
    };

    let transport = Arc::new(LoopbackPreviewTransport::bind().map_err(|error| {
        vec![Diagnostic::new(
            Severity::Error,
            format!("failed to start loopback preview server: {error}"),
        )
        .with_file(options.input_file.to_string_lossy().into_owned())
        .with_suggestion("retry after ensuring loopback TCP ports are available")]
    })?);
    transport.start_background();

    let target = PreviewTarget {
        input_file: options.input_file.clone(),
        jobname: options.jobname.clone(),
    };
    let shell_command_gateway = ShellCommandGateway::from_policy(&policy);
    let file_access_gate = FsFileAccessGate::from_policy(policy.clone());
    let asset_bundle_loader = AssetBundleLoader;
    let service = CompileJobService::new(
        &file_access_gate,
        &asset_bundle_loader,
        &shell_command_gateway,
    );
    let result = service.compile(&options);
    if result.exit_code != 0 {
        return Err(result.diagnostics);
    }

    let output_pdf = result.output_pdf.ok_or_else(|| {
        vec![Diagnostic::new(
            Severity::Error,
            "compile succeeded without producing a PDF artifact",
        )
        .with_file(options.input_file.to_string_lossy().into_owned())
        .with_suggestion("inspect the compile pipeline and retry the preview command")]
    })?;

    let preview_transport: Arc<dyn PreviewTransportPort> = transport.clone();
    let session_service = Arc::new(Mutex::new(PreviewSessionService::new(Arc::clone(
        &preview_transport,
    ))));

    let bootstrap = session_service
        .lock()
        .expect("preview session service poisoned")
        .create_session(&target, &policy)
        .map_err(|error| vec![diagnostic_for_session_error(&error)])?;

    let publish_decision = session_service
        .lock()
        .expect("preview session service poisoned")
        .check_publish(&bootstrap.session_id, &target, &policy);

    match publish_decision {
        PublishDecision::Allowed => {
            let pdf_bytes = std::fs::read(&output_pdf).map_err(|error| {
                vec![Diagnostic::new(
                    Severity::Error,
                    format!("failed to read compiled PDF for preview publish: {error}"),
                )
                .with_file(output_pdf.to_string_lossy().into_owned())
                .with_suggestion("rerun the preview command after verifying the output directory")]
            })?;

            let page_count = estimate_pdf_page_count(&pdf_bytes);
            session_service
                .lock()
                .expect("preview session service poisoned")
                .apply_page_fallback(&bootstrap.session_id, page_count);

            preview_transport
                .publish_pdf(bootstrap.session_id.as_str(), &pdf_bytes)
                .map_err(|error| {
                    vec![Diagnostic::new(
                        Severity::Error,
                        format!("failed to publish preview PDF: {error}"),
                    )
                    .with_file(output_pdf.to_string_lossy().into_owned())
                    .with_suggestion("retry after resetting the preview session")]
                })?;

            let revision = session_service
                .lock()
                .expect("preview session service poisoned")
                .advance_revision(&bootstrap.session_id, page_count)
                .ok_or_else(|| {
                    vec![Diagnostic::new(
                        Severity::Error,
                        "failed to advance preview revision for an existing session",
                    )
                    .with_context(format!("session id: {}", bootstrap.session_id))
                    .with_suggestion(
                        "bootstrap a new preview session and retry the preview command",
                    )]
                })?;
            preview_transport
                .publish_revision_event(&TransportRevisionEvent {
                    session_id: bootstrap.session_id.to_string(),
                    target_input: revision.target.input_file.to_string_lossy().into_owned(),
                    target_jobname: revision.target.jobname,
                    revision: revision.revision,
                    page_count: revision.page_count,
                })
                .map_err(|error| {
                    vec![Diagnostic::new(
                        Severity::Error,
                        format!("failed to publish preview revision event: {error}"),
                    )
                    .with_context(format!("session id: {}", bootstrap.session_id))
                    .with_suggestion("retry after resetting the preview session")]
                })?;
            {
                let svc = Arc::clone(&session_service);
                transport.set_view_state_handler(Arc::new(move |session_id, update| {
                    let view_state = PreviewViewState {
                        page_number: update.page_number,
                        zoom: update.zoom,
                        viewport_offset_y: update.viewport_offset_y,
                    };
                    svc.lock()
                        .expect("preview session service poisoned")
                        .update_view_state(&SessionId::new(session_id), view_state);
                }));
            }

            tracing::info!(
                session_id = %bootstrap.session_id,
                input = %target.input_file.display(),
                jobname = %target.jobname,
                revision = revision.revision,
                page_count = revision.page_count,
                document_url = %bootstrap.document_url,
                events_url = %bootstrap.events_url,
                "preview command published compiled PDF and revision event"
            );

            Ok(PreviewExecution {
                document_url: bootstrap.document_url,
                events_url: bootstrap.events_url,
                server_port: transport.port(),
            })
        }
        PublishDecision::Denied(error) => Err(vec![diagnostic_for_session_error(&error)]),
    }
}

fn estimate_pdf_page_count(pdf_bytes: &[u8]) -> usize {
    let content = String::from_utf8_lossy(pdf_bytes);
    let count = content.matches("/Type /Page").count();
    // /Type /Pages also matches, subtract those
    let pages_obj = content.matches("/Type /Pages").count();
    let page_count = count.saturating_sub(pages_obj);
    if page_count == 0 {
        1
    } else {
        page_count
    }
}

fn diagnostic_for_session_error(error: &SessionErrorResponse) -> Diagnostic {
    Diagnostic::new(
        Severity::Error,
        format!("preview session error: {}", error.error_kind),
    )
    .with_context(format!("session id: {}", error.session_id))
    .with_suggestion(&error.recovery_instruction)
}

fn diagnostics_exit_code(diagnostics: &[Diagnostic]) -> i32 {
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        2
    } else if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Warning)
    {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        execute_preview, runtime_options_from_command, Cli, Commands, CompileCommand,
        InteractionArg,
    };
    use clap::Parser;
    use ferritex_application::runtime_options::{InteractionMode, ShellEscapeMode};
    use tempfile::tempdir;

    fn compile_command() -> CompileCommand {
        CompileCommand {
            file: PathBuf::from("chapters/main.tex"),
            output_dir: Some(PathBuf::from("build")),
            jobname: None,
            jobs: Some(4),
            overlay_roots: vec![PathBuf::from("shared"), PathBuf::from("vendor/texmf")],
            no_cache: true,
            asset_bundle: Some(PathBuf::from("bundle")),
            reproducible: false,
            interaction: Some(InteractionArg::Batchmode),
            synctex: true,
            trace_font_tasks: true,
            shell_escape: true,
            no_shell_escape: false,
        }
    }

    #[test]
    fn explicit_jobname_overrides_runtime_options_default() {
        let mut command = compile_command();
        command.jobname = Some("custom-job".to_string());

        let options = runtime_options_from_command(&command);

        assert_eq!(options.jobname, "custom-job");
        assert_eq!(options.output_dir, PathBuf::from("build"));
        assert_eq!(options.parallelism, 4);
        assert_eq!(
            options.overlay_roots,
            vec![PathBuf::from("shared"), PathBuf::from("vendor/texmf")]
        );
        assert!(options.no_cache);
        assert_eq!(options.asset_bundle, Some(PathBuf::from("bundle")));
        assert!(options.host_font_fallback);
        assert!(!options.host_font_roots.is_empty() || cfg!(target_os = "unknown"));
        assert_eq!(options.interaction_mode, InteractionMode::Batchmode);
        assert!(options.synctex);
        assert!(options.trace_font_tasks);
        assert_eq!(options.shell_escape, ShellEscapeMode::Enabled);
    }

    #[test]
    fn clap_parses_compile_subcommand_flags() {
        let cli = Cli::try_parse_from([
            "ferritex",
            "compile",
            "book.tex",
            "--jobname",
            "book",
            "--overlay",
            "shared",
            "--overlay",
            "vendor/texmf",
            "--reproducible",
            "--interaction",
            "scrollmode",
            "--shell-escape",
            "--no-shell-escape",
        ])
        .expect("parse CLI");

        let Commands::Compile(command) = cli.command else {
            panic!("expected compile subcommand");
        };

        assert_eq!(command.file, PathBuf::from("book.tex"));
        assert_eq!(command.jobname.as_deref(), Some("book"));
        assert_eq!(
            command.overlay_roots,
            vec![PathBuf::from("shared"), PathBuf::from("vendor/texmf")]
        );
        assert!(command.reproducible);
        assert_eq!(command.interaction, Some(InteractionArg::Scrollmode));
        assert!(command.shell_escape);
        assert!(command.no_shell_escape);
    }

    #[test]
    fn watch_reuses_compile_option_shape() {
        let cli = Cli::try_parse_from([
            "ferritex",
            "watch",
            "notes.tex",
            "--output-dir",
            "out",
            "--jobs",
            "2",
        ])
        .expect("parse CLI");

        let Commands::Watch(command) = cli.command else {
            panic!("expected watch subcommand");
        };

        assert_eq!(command.file, PathBuf::from("notes.tex"));
        assert_eq!(command.output_dir, Some(PathBuf::from("out")));
        assert_eq!(command.jobs, Some(2));
    }

    #[test]
    fn preview_reuses_compile_option_shape() {
        let cli = Cli::try_parse_from([
            "ferritex",
            "preview",
            "notes.tex",
            "--output-dir",
            "out",
            "--jobs",
            "2",
        ])
        .expect("parse CLI");

        let Commands::Preview(command) = cli.command else {
            panic!("expected preview subcommand");
        };

        assert_eq!(command.file, PathBuf::from("notes.tex"));
        assert_eq!(command.output_dir, Some(PathBuf::from("out")));
        assert_eq!(command.jobs, Some(2));
    }

    #[test]
    fn preview_subcommand_compiles_and_creates_session() {
        let dir = tempdir().expect("create tempdir");
        let tex_file = dir.path().join("hello.tex");
        std::fs::write(
            &tex_file,
            "\\documentclass{article}\n\\begin{document}\nHello preview\n\\end{document}\n",
        )
        .expect("write input file");

        let mut command = compile_command();
        command.file = tex_file.clone();
        command.output_dir = Some(dir.path().to_path_buf());
        command.jobname = Some("hello".to_string());
        command.jobs = Some(1);
        command.no_cache = false;
        command.asset_bundle = None;
        command.reproducible = false;
        command.interaction = None;
        command.synctex = false;
        command.trace_font_tasks = false;
        command.shell_escape = false;
        command.no_shell_escape = true;

        let preview = execute_preview(&command).expect("run preview");

        assert!(preview.document_url.contains("http://127.0.0.1:"));
        assert!(preview
            .document_url
            .contains("/preview/preview-session-1/document"));
        assert!(preview.events_url.contains("ws://127.0.0.1:"));
        assert!(preview
            .events_url
            .contains("/preview/preview-session-1/events"));
        assert!(preview.server_port > 0);
        assert!(dir.path().join("hello.pdf").exists());
    }
}
