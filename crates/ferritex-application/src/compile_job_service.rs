use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ferritex_core::compilation::{
    CompilationJob, CompilationSnapshot, DocumentState, SymbolLocation,
};
use ferritex_core::diagnostics::{Diagnostic, Severity};
use ferritex_core::font::TfmMetrics;
use ferritex_core::kernel::api::DimensionValue;
use ferritex_core::parser::{MinimalLatexParser, ParseError, ParseOutput};
use ferritex_core::pdf::PdfRenderer;
use ferritex_core::policy::{ExecutionPolicy, OutputArtifactRegistry, PreviewPublicationPolicy};
use ferritex_core::policy::{FileAccessError, FileAccessGate, PathAccessDecision};
use ferritex_core::typesetting::{MinimalTypesetter, TfmWidthProvider};

use crate::execution_policy_factory::ExecutionPolicyFactory;
use crate::ports::AssetBundleLoaderPort;
use crate::runtime_options::RuntimeOptions;
use crate::stable_compile_state::StableCompileState;

const DEFAULT_TFM_FALLBACK_WIDTH: DimensionValue = DimensionValue(65_536);
const CMR10_TFM_CANDIDATES: [&str; 4] = [
    "texmf/fonts/tfm/public/cm/cmr10.tfm",
    "fonts/tfm/public/cm/cmr10.tfm",
    "texmf/cmr10.tfm",
    "cmr10.tfm",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileResult {
    pub diagnostics: Vec<Diagnostic>,
    pub exit_code: i32,
    pub output_pdf: Option<PathBuf>,
    pub stable_compile_state: Option<StableCompileState>,
}

pub struct CompileJobService<'a> {
    file_access_gate: &'a dyn FileAccessGate,
    asset_bundle_loader: &'a dyn AssetBundleLoaderPort,
    parser: MinimalLatexParser,
    typesetter: MinimalTypesetter,
    pdf_renderer: PdfRenderer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LoadedSourceTree {
    source: String,
    document_state: DocumentState,
}

impl<'a> CompileJobService<'a> {
    pub fn new(
        file_access_gate: &'a dyn FileAccessGate,
        asset_bundle_loader: &'a dyn AssetBundleLoaderPort,
    ) -> Self {
        Self {
            file_access_gate,
            asset_bundle_loader,
            parser: MinimalLatexParser,
            typesetter: MinimalTypesetter,
            pdf_renderer: PdfRenderer::default(),
        }
    }

    pub fn compile(&self, options: &RuntimeOptions) -> CompileResult {
        let input_path = options.input_file.to_string_lossy().into_owned();
        let execution_policy = ExecutionPolicyFactory::create(options);
        let project_root = project_root_for_policy(&execution_policy, &options.input_file);

        if let Some(bundle_path) = &options.asset_bundle {
            let manifest_path = bundle_path.join("manifest.json");
            if self.file_access_gate.check_read(bundle_path) == PathAccessDecision::Denied
                || self.file_access_gate.check_read(&manifest_path) == PathAccessDecision::Denied
            {
                let diagnostics =
                    vec![
                        Diagnostic::new(Severity::Error, "asset bundle access denied")
                            .with_file(bundle_path.to_string_lossy().into_owned())
                            .with_suggestion("place the asset bundle under an allowed read root"),
                    ];

                return CompileResult {
                    exit_code: exit_code_for(&diagnostics),
                    diagnostics,
                    output_pdf: None,
                    stable_compile_state: None,
                };
            }

            if let Err(error) = self.asset_bundle_loader.validate(bundle_path) {
                let bundle_display = bundle_path.to_string_lossy().into_owned();
                let diagnostics = vec![Diagnostic::new(Severity::Error, error)
                    .with_file(bundle_display)
                    .with_suggestion("verify the asset bundle path and version")];

                return CompileResult {
                    exit_code: exit_code_for(&diagnostics),
                    diagnostics,
                    output_pdf: None,
                    stable_compile_state: None,
                };
            }
        }

        if self.file_access_gate.check_read(&options.input_file) == PathAccessDecision::Denied {
            let diagnostics = vec![Diagnostic::new(Severity::Error, "input file access denied")
                .with_file(input_path)
                .with_suggestion("check the workspace root and file access policy")];

            return CompileResult {
                exit_code: exit_code_for(&diagnostics),
                diagnostics,
                output_pdf: None,
                stable_compile_state: None,
            };
        }

        let source_tree = match self.load_source_tree(
            &options.input_file,
            &project_root,
            options.asset_bundle.as_deref(),
        ) {
            Ok(tree) => tree,
            Err(diagnostic) => {
                let diagnostics = vec![diagnostic];

                return CompileResult {
                    exit_code: exit_code_for(&diagnostics),
                    diagnostics,
                    output_pdf: None,
                    stable_compile_state: None,
                };
            }
        };
        let ParseOutput { document, errors } = self.parser.parse_recovering(&source_tree.source);
        let parse_diagnostics: Vec<Diagnostic> = errors
            .into_iter()
            .map(|error| diagnostic_for_parse_error(error, input_path.clone()))
            .collect();

        let parsed_document = match document {
            Some(document) => document,
            None => {
                let compilation_job = compilation_job(
                    options.input_file.clone(),
                    options.jobname.clone(),
                    execution_policy.clone(),
                );

                return CompileResult {
                    exit_code: exit_code_for(&parse_diagnostics),
                    diagnostics: parse_diagnostics.clone(),
                    output_pdf: None,
                    stable_compile_state: Some(stable_compile_state(
                        &compilation_job,
                        source_tree.document_state.clone(),
                        0,
                        false,
                        parse_diagnostics,
                    )),
                };
            }
        };

        if !is_valid_jobname(&options.jobname) {
            let diagnostics =
                vec![
                    Diagnostic::new(Severity::Error, "jobname contains invalid characters")
                        .with_file(options.input_file.to_string_lossy().into_owned())
                        .with_suggestion(
                            "use a jobname without control characters or path separators",
                        ),
                ];

            return CompileResult {
                exit_code: exit_code_for(&diagnostics),
                diagnostics,
                output_pdf: None,
                stable_compile_state: None,
            };
        }

        let output_pdf = options.output_dir.join(format!("{}.pdf", options.jobname));
        if self.file_access_gate.check_write(&output_pdf) == PathAccessDecision::Denied {
            let diagnostics = vec![
                Diagnostic::new(Severity::Error, "output file access denied")
                    .with_file(output_pdf.to_string_lossy().into_owned())
                    .with_suggestion("check the output directory and file access policy"),
            ];

            return CompileResult {
                exit_code: exit_code_for(&diagnostics),
                diagnostics,
                output_pdf: None,
                stable_compile_state: None,
            };
        }

        if let Err(error) = self.file_access_gate.ensure_directory(&options.output_dir) {
            let diagnostics = vec![Diagnostic::new(
                Severity::Error,
                format!("failed to prepare output directory: {error}"),
            )
            .with_file(options.output_dir.to_string_lossy().into_owned())];

            return CompileResult {
                exit_code: exit_code_for(&diagnostics),
                diagnostics,
                output_pdf: None,
                stable_compile_state: None,
            };
        }

        let typeset_document = if let Some(metrics) =
            load_cmr10_metrics(self.file_access_gate, options.asset_bundle.as_deref())
        {
            let provider = TfmWidthProvider {
                metrics: &metrics,
                fallback_width: DEFAULT_TFM_FALLBACK_WIDTH,
            };
            self.typesetter
                .typeset_with_provider(&parsed_document, &provider)
        } else {
            self.typesetter.typeset(&parsed_document)
        };
        let pdf_document = self.pdf_renderer.render(&typeset_document);
        let compilation_job = compilation_job(
            options.input_file.clone(),
            options.jobname.clone(),
            execution_policy,
        );

        if let Err(error) = self
            .file_access_gate
            .write_file(&output_pdf, &pdf_document.bytes)
        {
            let diagnostics = vec![diagnostic_for_output_error(error, &output_pdf)];

            return CompileResult {
                exit_code: exit_code_for(&diagnostics),
                diagnostics,
                output_pdf: None,
                stable_compile_state: None,
            };
        }

        let diagnostics = parse_diagnostics;
        let stable_compile_state = stable_compile_state(
            &compilation_job,
            source_tree.document_state,
            pdf_document.page_count,
            true,
            diagnostics.clone(),
        );

        tracing::info!(
            jobname = %options.jobname,
            input = %options.input_file.display(),
            output = %output_pdf.display(),
            document_class = %parsed_document.document_class,
            package_count = parsed_document.package_count,
            page_count = pdf_document.page_count,
            total_lines = pdf_document.total_lines,
            "compile succeeded"
        );

        CompileResult {
            exit_code: exit_code_for(&diagnostics),
            diagnostics,
            output_pdf: Some(output_pdf),
            stable_compile_state: Some(stable_compile_state),
        }
    }

    pub fn compile_from_source(&self, source: &str, uri: &str) -> StableCompileState {
        let primary_input = primary_input_from_uri(uri);
        let jobname = jobname_for_input(&primary_input);
        let execution_policy = in_memory_execution_policy(&primary_input, &jobname);
        let project_root = project_root_for_policy(&execution_policy, &primary_input);
        let compilation_job =
            compilation_job(primary_input.clone(), jobname.clone(), execution_policy);
        let source_tree = self
            .load_source_tree_with_root_source(&primary_input, Some(source), &project_root, None)
            .unwrap_or_else(|_| LoadedSourceTree {
                source: source.to_string(),
                document_state: DocumentState::default(),
            });

        let primary_input_path = primary_input.to_string_lossy().into_owned();
        let ParseOutput { document, errors } = self.parser.parse_recovering(&source_tree.source);
        let parse_diagnostics: Vec<Diagnostic> = errors
            .into_iter()
            .map(|error| diagnostic_for_parse_error(error, primary_input_path.clone()))
            .collect();

        match document {
            Some(parsed_document) => {
                let typeset_document = self.typesetter.typeset(&parsed_document);
                let pdf_document = self.pdf_renderer.render(&typeset_document);
                stable_compile_state(
                    &compilation_job,
                    source_tree.document_state,
                    pdf_document.page_count,
                    true,
                    parse_diagnostics,
                )
            }
            None => stable_compile_state(
                &compilation_job,
                source_tree.document_state,
                0,
                false,
                parse_diagnostics,
            ),
        }
    }

    fn load_source_tree(
        &self,
        input_file: &Path,
        project_root: &Path,
        asset_bundle_path: Option<&Path>,
    ) -> Result<LoadedSourceTree, Diagnostic> {
        self.load_source_tree_with_root_source(input_file, None, project_root, asset_bundle_path)
    }

    fn load_source_tree_with_root_source(
        &self,
        input_file: &Path,
        root_source: Option<&str>,
        project_root: &Path,
        asset_bundle_path: Option<&Path>,
    ) -> Result<LoadedSourceTree, Diagnostic> {
        let root_input = normalize_existing_path(input_file);
        let project_root = normalize_existing_path(project_root);
        let mut visited = BTreeSet::new();
        let mut include_guard = BTreeSet::new();
        let mut source_files = BTreeSet::new();
        let mut labels = BTreeMap::new();
        let mut citations = BTreeMap::new();
        let source = self.load_source_file(
            &root_input,
            &project_root,
            root_source,
            asset_bundle_path,
            &mut visited,
            &mut include_guard,
            &mut source_files,
            &mut labels,
            &mut citations,
        )?;

        Ok(LoadedSourceTree {
            source,
            document_state: DocumentState {
                revision: 0,
                bibliography_dirty: false,
                source_files: source_files
                    .into_iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
                labels,
                citations,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn load_source_file(
        &self,
        path: &Path,
        workspace_root: &Path,
        source_override: Option<&str>,
        asset_bundle_path: Option<&Path>,
        visited: &mut BTreeSet<PathBuf>,
        include_guard: &mut BTreeSet<PathBuf>,
        source_files: &mut BTreeSet<PathBuf>,
        labels: &mut BTreeMap<String, SymbolLocation>,
        citations: &mut BTreeMap<String, SymbolLocation>,
    ) -> Result<String, Diagnostic> {
        let normalized_path = normalize_existing_path(path);
        if !visited.insert(normalized_path.clone()) {
            return Err(Diagnostic::new(
                Severity::Error,
                "input cycle detected while expanding source files",
            )
            .with_file(normalized_path.to_string_lossy().into_owned())
            .with_suggestion("remove the recursive \\input/\\include chain"));
        }

        source_files.insert(normalized_path.clone());
        let source = match source_override {
            Some(source) => source.to_string(),
            None => read_utf8_file(self.file_access_gate, &normalized_path)?,
        };
        collect_symbol_locations(&source, &normalized_path, "label", labels);
        collect_symbol_locations(&source, &normalized_path, "bibitem", citations);

        let base_dir = normalized_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(workspace_root);
        let expanded = expand_inputs(
            self,
            &source,
            &normalized_path,
            base_dir,
            workspace_root,
            asset_bundle_path,
            visited,
            include_guard,
            source_files,
            labels,
            citations,
        )?;
        visited.remove(&normalized_path);
        Ok(expanded)
    }
}

fn stable_compile_state(
    compilation_job: &CompilationJob,
    document_state: DocumentState,
    page_count: usize,
    success: bool,
    diagnostics: Vec<Diagnostic>,
) -> StableCompileState {
    StableCompileState {
        snapshot: CompilationSnapshot::from_session(&compilation_job.begin_pass(1)),
        document_state,
        page_count,
        success,
        diagnostics,
    }
}

fn project_root_for_policy(policy: &ExecutionPolicy, input_file: &Path) -> PathBuf {
    policy
        .allowed_read_paths
        .first()
        .cloned()
        .unwrap_or_else(|| {
            input_file
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        })
}

fn compilation_job(
    primary_input: PathBuf,
    jobname: String,
    policy: ExecutionPolicy,
) -> CompilationJob {
    CompilationJob {
        primary_input,
        jobname,
        policy,
        document_state: DocumentState::default(),
        output_artifacts: OutputArtifactRegistry::new(),
    }
}

fn primary_input_from_uri(uri: &str) -> PathBuf {
    uri.strip_prefix("file://")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(uri))
}

fn jobname_for_input(primary_input: &Path) -> String {
    primary_input
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("main")
        .to_string()
}

fn in_memory_execution_policy(primary_input: &Path, jobname: &str) -> ExecutionPolicy {
    let workspace_root = primary_input
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    ExecutionPolicy {
        shell_escape_allowed: false,
        allowed_read_paths: vec![workspace_root.clone()],
        allowed_write_paths: vec![workspace_root.clone()],
        output_dir: workspace_root,
        jobname: jobname.to_string(),
        preview_publication: Some(PreviewPublicationPolicy {
            loopback_only: true,
            active_job_only: true,
        }),
    }
}

fn diagnostic_for_input_error(error: FileAccessError, input_path: String) -> Diagnostic {
    match error {
        FileAccessError::AccessDenied { .. } => {
            Diagnostic::new(Severity::Error, "input file access denied")
                .with_file(input_path)
                .with_suggestion("check the workspace root and file access policy")
        }
        FileAccessError::Io { source } if source.kind() == std::io::ErrorKind::NotFound => {
            Diagnostic::new(Severity::Error, "input file not found")
                .with_file(input_path)
                .with_suggestion("check the file path")
        }
        FileAccessError::Io { source } => Diagnostic::new(
            Severity::Error,
            format!("failed to read input file: {source}"),
        )
        .with_file(input_path),
    }
}

fn diagnostic_for_output_error(error: FileAccessError, output_pdf: &Path) -> Diagnostic {
    match error {
        FileAccessError::AccessDenied { .. } => {
            Diagnostic::new(Severity::Error, "output file access denied")
                .with_file(output_pdf.to_string_lossy().into_owned())
                .with_suggestion("check the output directory and file access policy")
        }
        FileAccessError::Io { source } => Diagnostic::new(
            Severity::Error,
            format!("failed to write output file: {source}"),
        )
        .with_file(output_pdf.to_string_lossy().into_owned()),
    }
}

fn diagnostic_for_parse_error(error: ParseError, input_path: String) -> Diagnostic {
    let diagnostic = Diagnostic::new(Severity::Error, error.to_string()).with_file(input_path);
    let diagnostic = if let Some(line) = error.line() {
        diagnostic.with_line(line)
    } else {
        diagnostic
    };

    match error {
        ParseError::EmptyInput => diagnostic
            .with_context("expected a LaTeX source file with a document preamble")
            .with_suggestion("add \\documentclass, \\begin{document}, and \\end{document}"),
        ParseError::MissingDocumentClass => diagnostic
            .with_context("the preamble must declare a document class")
            .with_suggestion("add \\documentclass{article} or another class at the top"),
        ParseError::InvalidDocumentClass { .. } => diagnostic
            .with_context("could not extract a class name from \\documentclass")
            .with_suggestion("use a form like \\documentclass{article}"),
        ParseError::MissingBeginDocument { .. } => diagnostic
            .with_context("the parser could not find the document body start")
            .with_suggestion("add \\begin{document} before the document body"),
        ParseError::MissingEndDocument { .. } => diagnostic
            .with_context("the parser reached EOF before the document body closed")
            .with_suggestion("add \\end{document} at the end of the file"),
        ParseError::UnexpectedEndDocument { .. } => diagnostic
            .with_context("the parser found a document terminator before the body started")
            .with_suggestion("remove the stray \\end{document} or move it to the end"),
        ParseError::TrailingContentAfterEndDocument { .. } => diagnostic
            .with_context("the parser found non-whitespace content after the document ended")
            .with_suggestion("remove content after \\end{document}"),
        ParseError::UnexpectedClosingBrace { .. } => diagnostic
            .with_context("a closing brace appeared without a matching opening brace")
            .with_suggestion("remove the extra } or add the missing opening brace"),
        ParseError::UnclosedBrace { .. } => diagnostic
            .with_context("the parser reached EOF while braces were still open")
            .with_suggestion("close the outstanding { ... } group"),
        ParseError::InvalidRegisterIndex { .. } => diagnostic
            .with_context("a count/dimen register index must be between 0 and 32767")
            .with_suggestion("use a register number in the supported range"),
        ParseError::UnclosedConditional { .. } => diagnostic
            .with_context("the parser reached EOF while a conditional branch was still open")
            .with_suggestion("add the missing \\fi for the open \\if... branch"),
        ParseError::UnexpectedElse { .. } => diagnostic
            .with_context("the parser found \\else without a matching open conditional")
            .with_suggestion("remove the stray \\else or add the matching \\if..."),
        ParseError::UnexpectedFi { .. } => diagnostic
            .with_context("the parser found \\fi without a matching open conditional")
            .with_suggestion("remove the stray \\fi or add the matching \\if..."),
        ParseError::DivisionByZero { .. } => diagnostic
            .with_context("register division requires a non-zero divisor")
            .with_suggestion("change the divisor to a non-zero integer"),
        ParseError::MacroExpansionLimit { .. } => diagnostic
            .with_context("macro expansion did not converge within the development safety limit")
            .with_suggestion("check for recursive macro definitions such as \\def\\foo{\\foo}"),
    }
}

fn is_valid_jobname(jobname: &str) -> bool {
    !jobname.is_empty()
        && !jobname
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '/' | '\\'))
}

fn exit_code_for(diagnostics: &[Diagnostic]) -> i32 {
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

#[allow(clippy::too_many_arguments)]
fn expand_inputs(
    service: &CompileJobService<'_>,
    source: &str,
    source_path: &Path,
    base_dir: &Path,
    workspace_root: &Path,
    asset_bundle_path: Option<&Path>,
    visited: &mut BTreeSet<PathBuf>,
    include_guard: &mut BTreeSet<PathBuf>,
    source_files: &mut BTreeSet<PathBuf>,
    labels: &mut BTreeMap<String, SymbolLocation>,
    citations: &mut BTreeMap<String, SymbolLocation>,
) -> Result<String, Diagnostic> {
    let mut expanded = String::with_capacity(source.len());

    for (line_index, line) in source.split_inclusive('\n').enumerate() {
        let visible = strip_line_comment(line);
        let matches = input_commands_in_line(&visible, line_index as u32 + 1);
        if matches.is_empty() {
            expanded.push_str(line);
            continue;
        }

        let mut cursor = 0usize;
        for command in matches {
            expanded.push_str(&line[cursor..command.start]);

            let resolved = resolve_input_path(
                base_dir,
                workspace_root,
                &command.value,
                service.asset_bundle_loader,
                asset_bundle_path,
            );

            match &command.kind {
                InlineCommandKind::Input => {
                    let nested = service
                        .load_source_file(
                            &resolved,
                            workspace_root,
                            None,
                            asset_bundle_path,
                            visited,
                            include_guard,
                            source_files,
                            labels,
                            citations,
                        )
                        .map_err(|diagnostic| {
                            diagnostic_for_nested_input_error(
                                diagnostic,
                                source_path,
                                command.line,
                                &command.value,
                            )
                        })?;
                    expanded.push_str(&nested);
                }
                InlineCommandKind::Include => {
                    if !include_guard.insert(resolved.clone()) {
                        cursor = command.end;
                        continue;
                    }

                    let nested = service
                        .load_source_file(
                            &resolved,
                            workspace_root,
                            None,
                            asset_bundle_path,
                            visited,
                            include_guard,
                            source_files,
                            labels,
                            citations,
                        )
                        .map_err(|diagnostic| {
                            diagnostic_for_nested_input_error(
                                diagnostic,
                                source_path,
                                command.line,
                                &command.value,
                            )
                        })?;
                    expanded.push_str(&nested);
                }
                InlineCommandKind::InputIfFileExists {
                    true_branch,
                    false_branch,
                } => {
                    if resolved.exists() {
                        let nested = service
                            .load_source_file(
                                &resolved,
                                workspace_root,
                                None,
                                asset_bundle_path,
                                visited,
                                include_guard,
                                source_files,
                                labels,
                                citations,
                            )
                            .map_err(|diagnostic| {
                                diagnostic_for_nested_input_error(
                                    diagnostic,
                                    source_path,
                                    command.line,
                                    &command.value,
                                )
                            })?;
                        expanded.push_str(&nested);
                        expanded.push_str(true_branch);
                    } else {
                        expanded.push_str(false_branch);
                    }
                }
            }
            cursor = command.end;
        }

        expanded.push_str(&line[cursor..]);
    }

    Ok(expanded)
}

fn read_utf8_file(
    file_access_gate: &dyn FileAccessGate,
    path: &Path,
) -> Result<String, Diagnostic> {
    let bytes = file_access_gate
        .read_file(path)
        .map_err(|error| diagnostic_for_input_error(error, path.to_string_lossy().into_owned()))?;
    String::from_utf8(bytes).map_err(|error| {
        Diagnostic::new(
            Severity::Error,
            format!("input file is not valid UTF-8: {error}"),
        )
        .with_file(path.to_string_lossy().into_owned())
        .with_suggestion("save the source as UTF-8 in this development build")
    })
}

fn normalize_existing_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn load_cmr10_metrics(
    file_access_gate: &dyn FileAccessGate,
    asset_bundle_path: Option<&Path>,
) -> Option<TfmMetrics> {
    let bundle_path = asset_bundle_path?;

    for relative_path in CMR10_TFM_CANDIDATES {
        let candidate = bundle_path.join(relative_path);
        if !candidate.is_file() {
            continue;
        }

        if file_access_gate.check_read(&candidate) == PathAccessDecision::Denied {
            tracing::warn!(
                path = %candidate.display(),
                "cmr10.tfm access denied; falling back to fixed-width typesetting"
            );
            continue;
        }

        let bytes = match file_access_gate.read_file(&candidate) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(
                    path = %candidate.display(),
                    %error,
                    "failed to read cmr10.tfm; falling back to fixed-width typesetting"
                );
                continue;
            }
        };

        match TfmMetrics::parse(&bytes) {
            Ok(metrics) => return Some(metrics),
            Err(error) => {
                tracing::warn!(
                    path = %candidate.display(),
                    %error,
                    "failed to parse cmr10.tfm; falling back to fixed-width typesetting"
                );
            }
        }
    }

    None
}

fn resolve_input_path(
    base_dir: &Path,
    workspace_root: &Path,
    value: &str,
    asset_bundle_loader: &dyn AssetBundleLoaderPort,
    asset_bundle_path: Option<&Path>,
) -> PathBuf {
    let candidate = tex_path_candidate(base_dir, value);
    if candidate.exists() {
        return candidate;
    }

    let workspace_candidate = tex_path_candidate(workspace_root, value);
    if workspace_candidate.exists() {
        return workspace_candidate;
    }

    if let Some(bundle_path) = asset_bundle_path {
        if let Some(relative_path) = bundle_relative_input_path(base_dir, bundle_path, value) {
            if let Some(path) = asset_bundle_loader
                .resolve_tex_input(bundle_path, relative_path.to_string_lossy().as_ref())
            {
                return path;
            }
        }
    }

    candidate
}

fn tex_path_candidate(base_dir: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    };

    if candidate.extension().is_some() {
        candidate
    } else {
        candidate.with_extension("tex")
    }
}

fn tex_relative_candidate(path: &Path) -> PathBuf {
    if path.extension().is_some() {
        path.to_path_buf()
    } else {
        path.with_extension("tex")
    }
}

fn bundle_relative_input_path(base_dir: &Path, bundle_path: &Path, value: &str) -> Option<PathBuf> {
    let candidate = tex_path_candidate(base_dir, value);
    if let Ok(relative_path) = candidate.strip_prefix(bundle_path) {
        return Some(tex_relative_candidate(relative_path));
    }

    let value_path = Path::new(value);
    if value_path.is_absolute() {
        return value_path
            .strip_prefix(bundle_path)
            .ok()
            .map(tex_relative_candidate);
    }

    Some(tex_relative_candidate(value_path))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InlineCommandKind {
    Input,
    Include,
    InputIfFileExists {
        true_branch: String,
        false_branch: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InlineCommand {
    kind: InlineCommandKind,
    value: String,
    start: usize,
    end: usize,
    line: u32,
}

fn input_commands_in_line(line: &str, line_number: u32) -> Vec<InlineCommand> {
    let mut matches = ["input", "include"]
        .into_iter()
        .flat_map(|command| find_braced_commands(line, command, line_number))
        .collect::<Vec<_>>();
    matches.extend(find_input_if_file_exists_commands(line, line_number));
    matches.sort_by_key(|command| command.start);
    matches
}

fn find_braced_commands(line: &str, command: &'static str, line_number: u32) -> Vec<InlineCommand> {
    let needle = format!("\\{command}");
    let mut matches = Vec::new();
    let mut search_offset = 0usize;

    while let Some(found) = line[search_offset..].find(&needle) {
        let start = search_offset + found;
        let mut cursor = start + needle.len();
        cursor = skip_command_whitespace(line, cursor);
        let Some((value, end)) = parse_braced_group(line, cursor) else {
            search_offset = cursor;
            continue;
        };
        matches.push(InlineCommand {
            kind: match command {
                "input" => InlineCommandKind::Input,
                "include" => InlineCommandKind::Include,
                _ => unreachable!("unsupported inline command"),
            },
            value,
            start,
            end,
            line: line_number,
        });
        search_offset = end;
    }

    matches
}

fn find_input_if_file_exists_commands(line: &str, line_number: u32) -> Vec<InlineCommand> {
    let needle = "\\InputIfFileExists";
    let mut matches = Vec::new();
    let mut search_offset = 0usize;

    while let Some(found) = line[search_offset..].find(needle) {
        let start = search_offset + found;
        let mut cursor = skip_command_whitespace(line, start + needle.len());
        let Some((value, next_cursor)) = parse_braced_group(line, cursor) else {
            break;
        };
        cursor = skip_command_whitespace(line, next_cursor);
        let Some((true_branch, next_cursor)) = parse_braced_group(line, cursor) else {
            break;
        };
        cursor = skip_command_whitespace(line, next_cursor);
        let Some((false_branch, end)) = parse_braced_group(line, cursor) else {
            break;
        };

        matches.push(InlineCommand {
            kind: InlineCommandKind::InputIfFileExists {
                true_branch,
                false_branch,
            },
            value,
            start,
            end,
            line: line_number,
        });
        search_offset = end;
    }

    matches
}

fn skip_command_whitespace(line: &str, cursor: usize) -> usize {
    cursor
        + line[cursor..]
            .chars()
            .take_while(|ch| ch.is_whitespace())
            .map(char::len_utf8)
            .sum::<usize>()
}

fn parse_braced_group(line: &str, cursor: usize) -> Option<(String, usize)> {
    let start = skip_command_whitespace(line, cursor);
    if !line[start..].starts_with('{') {
        return None;
    }

    let content_start = start + 1;
    let mut depth = 1u32;
    let mut escaped = false;

    for (offset, ch) in line[content_start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }

        match ch {
            '\\' => escaped = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let content_end = content_start + offset;
                    let end = content_end + 1;
                    return Some((line[content_start..content_end].to_string(), end));
                }
            }
            _ => {}
        }
    }

    None
}

fn strip_line_comment(line: &str) -> String {
    let mut visible = String::with_capacity(line.len());
    let mut escaped = false;

    for ch in line.chars() {
        if escaped {
            visible.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' => {
                visible.push(ch);
                escaped = true;
            }
            '%' => break,
            _ => visible.push(ch),
        }
    }

    visible
}

fn collect_symbol_locations(
    source: &str,
    path: &Path,
    command: &'static str,
    target: &mut BTreeMap<String, SymbolLocation>,
) {
    for (line_index, line) in source.lines().enumerate() {
        let visible = strip_line_comment(line);
        let needle = format!("\\{command}{{");
        let mut search_offset = 0usize;

        while let Some(found) = visible[search_offset..].find(&needle) {
            let start = search_offset + found;
            let value_start = start + needle.len();
            let Some(value_end_relative) = visible[value_start..].find('}') else {
                break;
            };
            let value_end = value_start + value_end_relative;
            let value = visible[value_start..value_end].trim();
            if !value.is_empty() {
                target
                    .entry(value.to_string())
                    .or_insert_with(|| SymbolLocation {
                        file: path.to_string_lossy().into_owned(),
                        line: line_index as u32 + 1,
                        column: visible[..start].chars().count() as u32,
                    });
            }
            search_offset = value_end + 1;
        }
    }
}

fn diagnostic_for_nested_input_error(
    diagnostic: Diagnostic,
    source_path: &Path,
    line: u32,
    input_value: &str,
) -> Diagnostic {
    Diagnostic::new(
        Severity::Error,
        format!("failed to resolve \\input/\\include target `{input_value}`"),
    )
    .with_file(source_path.to_string_lossy().into_owned())
    .with_line(line)
    .with_context(diagnostic.message)
    .with_suggestion("verify the referenced file path and access policy")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use super::CompileJobService;
    use crate::ports::AssetBundleLoaderPort;
    use crate::runtime_options::{InteractionMode, RuntimeOptions, ShellEscapeMode};
    use ferritex_core::policy::{FileAccessError, FileAccessGate, PathAccessDecision};
    use tempfile::tempdir;

    enum MockReadResult {
        Success(Vec<u8>),
        NotFound,
        AccessDenied(PathBuf),
    }

    struct MockFileAccessGate {
        read_decision: PathAccessDecision,
        write_decision: PathAccessDecision,
        read_result: MockReadResult,
        created_dirs: Mutex<Vec<PathBuf>>,
        writes: Mutex<Vec<(PathBuf, Vec<u8>)>>,
    }

    impl FileAccessGate for MockFileAccessGate {
        fn ensure_directory(&self, path: &Path) -> Result<(), FileAccessError> {
            if self.write_decision == PathAccessDecision::Denied {
                return Err(FileAccessError::AccessDenied {
                    path: path.to_path_buf(),
                });
            }

            self.created_dirs
                .lock()
                .expect("lock created dirs")
                .push(path.to_path_buf());
            Ok(())
        }

        fn check_read(&self, _path: &Path) -> PathAccessDecision {
            self.read_decision
        }

        fn check_write(&self, _path: &Path) -> PathAccessDecision {
            self.write_decision
        }

        fn check_readback(
            &self,
            _path: &Path,
            _primary_input: &Path,
            _jobname: &str,
        ) -> PathAccessDecision {
            PathAccessDecision::Denied
        }

        fn read_file(&self, _path: &Path) -> Result<Vec<u8>, FileAccessError> {
            match &self.read_result {
                MockReadResult::Success(bytes) => Ok(bytes.clone()),
                MockReadResult::NotFound => Err(FileAccessError::Io {
                    source: std::io::Error::from(std::io::ErrorKind::NotFound),
                }),
                MockReadResult::AccessDenied(path) => {
                    Err(FileAccessError::AccessDenied { path: path.clone() })
                }
            }
        }

        fn write_file(&self, path: &Path, content: &[u8]) -> Result<(), FileAccessError> {
            if self.write_decision == PathAccessDecision::Denied {
                return Err(FileAccessError::AccessDenied {
                    path: path.to_path_buf(),
                });
            }

            self.writes
                .lock()
                .expect("lock writes")
                .push((path.to_path_buf(), content.to_vec()));
            Ok(())
        }

        fn read_readback(
            &self,
            _path: &Path,
            _primary_input: &Path,
            _jobname: &str,
        ) -> Result<Vec<u8>, FileAccessError> {
            Err(FileAccessError::AccessDenied {
                path: PathBuf::from("denied"),
            })
        }
    }

    struct FsTestFileAccessGate;

    impl FileAccessGate for FsTestFileAccessGate {
        fn ensure_directory(&self, path: &Path) -> Result<(), FileAccessError> {
            fs::create_dir_all(path).map_err(FileAccessError::from)
        }

        fn check_read(&self, _path: &Path) -> PathAccessDecision {
            PathAccessDecision::Allowed
        }

        fn check_write(&self, _path: &Path) -> PathAccessDecision {
            PathAccessDecision::Allowed
        }

        fn check_readback(
            &self,
            _path: &Path,
            _primary_input: &Path,
            _jobname: &str,
        ) -> PathAccessDecision {
            PathAccessDecision::Allowed
        }

        fn read_file(&self, path: &Path) -> Result<Vec<u8>, FileAccessError> {
            fs::read(path).map_err(FileAccessError::from)
        }

        fn write_file(&self, path: &Path, content: &[u8]) -> Result<(), FileAccessError> {
            fs::write(path, content).map_err(FileAccessError::from)
        }

        fn read_readback(
            &self,
            path: &Path,
            _primary_input: &Path,
            _jobname: &str,
        ) -> Result<Vec<u8>, FileAccessError> {
            fs::read(path).map_err(FileAccessError::from)
        }
    }

    struct MockAssetBundleLoader {
        result: Result<(), String>,
        tex_inputs: BTreeMap<String, PathBuf>,
    }

    impl MockAssetBundleLoader {
        fn valid() -> Self {
            Self {
                result: Ok(()),
                tex_inputs: BTreeMap::new(),
            }
        }
    }

    impl AssetBundleLoaderPort for MockAssetBundleLoader {
        fn validate(&self, _bundle_path: &Path) -> Result<(), String> {
            self.result.clone()
        }

        fn resolve_tex_input(&self, _bundle_path: &Path, relative_path: &str) -> Option<PathBuf> {
            let lookup_key = if Path::new(relative_path).extension().is_some() {
                relative_path.to_string()
            } else {
                Path::new(relative_path)
                    .with_extension("tex")
                    .to_string_lossy()
                    .into_owned()
            };

            self.tex_inputs.get(&lookup_key).cloned()
        }
    }

    fn runtime_options(input_file: PathBuf, output_dir: PathBuf) -> RuntimeOptions {
        RuntimeOptions {
            input_file,
            output_dir,
            jobname: "main".to_string(),
            parallelism: 1,
            no_cache: false,
            asset_bundle: None,
            interaction_mode: InteractionMode::Nonstopmode,
            synctex: false,
            trace_font_tasks: false,
            shell_escape: ShellEscapeMode::Disabled,
        }
    }

    fn service<'a>(
        file_access_gate: &'a dyn FileAccessGate,
        asset_bundle_loader: &'a dyn AssetBundleLoaderPort,
    ) -> CompileJobService<'a> {
        CompileJobService::new(file_access_gate, asset_bundle_loader)
    }

    fn document(body: &str) -> String {
        format!("\\documentclass{{article}}\n\\begin{{document}}\n{body}\n\\end{{document}}\n")
    }

    fn read_pdf(path: &Path) -> String {
        fs::read_to_string(path).expect("read output pdf")
    }

    fn build_test_tfm() -> Vec<u8> {
        let bc = 65u16;
        let ec = 122u16;
        let char_count = usize::from(ec - bc + 1);
        let lh = 2u16;
        let nw = 3u16;
        let nh = 1u16;
        let nd = 1u16;
        let ni = 1u16;
        let lf = 6u16 + lh + char_count as u16 + nw + nh + nd + ni;
        let mut data = Vec::with_capacity(usize::from(lf) * 4);

        for value in [lf, lh, bc, ec, nw, nh, nd, ni, 0, 0, 0, 0] {
            data.extend_from_slice(&value.to_be_bytes());
        }
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&10_485_760i32.to_be_bytes());

        for code in bc..=ec {
            let width_index = if code == u16::from(b'A') { 2u8 } else { 1u8 };
            data.extend_from_slice(&[width_index, 0, 0, 0]);
        }

        data.extend_from_slice(&0i32.to_be_bytes());
        data.extend_from_slice(&104_858i32.to_be_bytes());
        data.extend_from_slice(&20_971_520i32.to_be_bytes());
        data.extend_from_slice(&0i32.to_be_bytes());
        data.extend_from_slice(&0i32.to_be_bytes());
        data.extend_from_slice(&0i32.to_be_bytes());

        data
    }

    #[test]
    fn returns_missing_input_diagnostic_for_nonexistent_file() {
        let dir = tempdir().expect("create tempdir");
        let options = runtime_options(dir.path().join("missing.tex"), dir.path().join("out"));
        let gate = MockFileAccessGate {
            read_decision: PathAccessDecision::Allowed,
            write_decision: PathAccessDecision::Denied,
            read_result: MockReadResult::NotFound,
            created_dirs: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
        };
        let loader = MockAssetBundleLoader::valid();
        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 2);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message, "input file not found");
        assert_eq!(
            result.diagnostics[0].suggestion.as_deref(),
            Some("check the file path")
        );
        assert_eq!(result.output_pdf, None);
        assert_eq!(result.stable_compile_state, None);
    }

    #[test]
    fn returns_encoding_diagnostic_for_non_utf8_input() {
        let dir = tempdir().expect("create tempdir");
        let options = runtime_options(dir.path().join("main.tex"), dir.path().join("out"));
        let gate = MockFileAccessGate {
            read_decision: PathAccessDecision::Allowed,
            write_decision: PathAccessDecision::Allowed,
            read_result: MockReadResult::Success(vec![0xff, 0xfe, b'{']),
            created_dirs: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
        };
        let loader = MockAssetBundleLoader::valid();
        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 2);
        assert!(result.diagnostics[0]
            .message
            .contains("input file is not valid UTF-8"));
        assert_eq!(result.output_pdf, None);
        assert_eq!(result.stable_compile_state, None);
    }

    #[test]
    fn writes_pdf_with_document_content_and_stable_compile_state() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let options = runtime_options(input_file, dir.path().join("out"));
        let gate = MockFileAccessGate {
            read_decision: PathAccessDecision::Allowed,
            write_decision: PathAccessDecision::Allowed,
            read_result: MockReadResult::Success(
                b"\\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\n".to_vec(),
            ),
            created_dirs: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
        };
        let loader = MockAssetBundleLoader::valid();
        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        assert!(result.diagnostics.is_empty());
        assert_eq!(result.output_pdf, Some(options.output_dir.join("main.pdf")));
        assert_eq!(
            result
                .stable_compile_state
                .as_ref()
                .map(|state| state.page_count),
            Some(1)
        );
        assert_eq!(
            result
                .stable_compile_state
                .as_ref()
                .map(|state| state.snapshot.jobname.as_str()),
            Some("main")
        );

        let writes = gate.writes.lock().expect("lock writes");
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, options.output_dir.join("main.pdf"));
        let pdf = String::from_utf8_lossy(&writes[0].1);
        assert!(pdf.contains("%PDF-1.4"));
        assert!(pdf.contains("Hello"));
        assert!(!pdf.contains("Ferritex placeholder PDF"));
        drop(writes);

        let created_dirs = gate.created_dirs.lock().expect("lock created dirs");
        assert_eq!(
            created_dirs.as_slice(),
            std::slice::from_ref(&options.output_dir)
        );
    }

    #[test]
    fn writes_pdf_and_reports_recoverable_parse_diagnostics() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let options = runtime_options(input_file, dir.path().join("out"));
        let gate = MockFileAccessGate {
            read_decision: PathAccessDecision::Allowed,
            write_decision: PathAccessDecision::Allowed,
            read_result: MockReadResult::Success(
                b"\\documentclass{article}\n\\begin{document}\nA}B\n\\end{document}\n".to_vec(),
            ),
            created_dirs: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
        };
        let loader = MockAssetBundleLoader::valid();
        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 2);
        assert_eq!(result.output_pdf, Some(options.output_dir.join("main.pdf")));
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message, "unexpected closing brace");
        assert_eq!(result.diagnostics[0].line, Some(3));
        let stable_state = result
            .stable_compile_state
            .as_ref()
            .expect("stable compile state");
        assert!(stable_state.success);
        assert_eq!(stable_state.page_count, 1);
        assert_eq!(stable_state.diagnostics, result.diagnostics);

        let writes = gate.writes.lock().expect("lock writes");
        assert_eq!(writes.len(), 1);
        let pdf = String::from_utf8_lossy(&writes[0].1);
        assert!(pdf.contains("AB"));
    }

    #[test]
    fn rejects_jobname_with_control_characters() {
        let dir = tempdir().expect("create tempdir");
        let mut options = runtime_options(dir.path().join("main.tex"), dir.path().join("out"));
        options.jobname = "bad\nname".to_string();
        let gate = MockFileAccessGate {
            read_decision: PathAccessDecision::Allowed,
            write_decision: PathAccessDecision::Allowed,
            read_result: MockReadResult::Success(
                b"\\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\n".to_vec(),
            ),
            created_dirs: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
        };
        let loader = MockAssetBundleLoader::valid();
        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 2);
        assert_eq!(
            result.diagnostics[0].message,
            "jobname contains invalid characters"
        );
        assert_eq!(result.output_pdf, None);
        assert_eq!(result.stable_compile_state, None);
    }

    #[test]
    fn returns_parse_diagnostic_for_unclosed_brace() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let options = runtime_options(input_file, dir.path().join("out"));
        let gate = MockFileAccessGate {
            read_decision: PathAccessDecision::Allowed,
            write_decision: PathAccessDecision::Allowed,
            read_result: MockReadResult::Success(
                b"\\documentclass{article}\n\\begin{document}\n{text\n\\end{document}\n".to_vec(),
            ),
            created_dirs: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
        };
        let loader = MockAssetBundleLoader::valid();
        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 2);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message, "unclosed brace");
        assert_eq!(result.diagnostics[0].line, Some(3));
        assert_eq!(result.output_pdf, Some(options.output_dir.join("main.pdf")));
        let stable_state = result
            .stable_compile_state
            .as_ref()
            .expect("stable compile state");
        assert!(stable_state.success);
        assert_eq!(stable_state.page_count, 1);
        assert_eq!(stable_state.diagnostics, result.diagnostics);
    }

    #[test]
    fn returns_multiple_parse_diagnostics_for_structural_errors() {
        let dir = tempdir().expect("create tempdir");
        let options = runtime_options(dir.path().join("main.tex"), dir.path().join("out"));
        let gate = MockFileAccessGate {
            read_decision: PathAccessDecision::Allowed,
            write_decision: PathAccessDecision::Allowed,
            read_result: MockReadResult::Success(b"plain text".to_vec()),
            created_dirs: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
        };
        let loader = MockAssetBundleLoader::valid();
        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 2);
        assert_eq!(result.output_pdf, None);
        assert_eq!(result.diagnostics.len(), 3);
        assert_eq!(
            result
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            vec![
                "missing \\begin{document}",
                "missing \\end{document}",
                "missing \\documentclass declaration",
            ]
        );
    }

    #[test]
    fn compile_from_source_returns_stable_state() {
        let gate = MockFileAccessGate {
            read_decision: PathAccessDecision::Allowed,
            write_decision: PathAccessDecision::Allowed,
            read_result: MockReadResult::Success(Vec::new()),
            created_dirs: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
        };
        let loader = MockAssetBundleLoader::valid();

        let state = service(&gate, &loader).compile_from_source(
            "\\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\n",
            "file:///tmp/main.tex",
        );

        assert!(state.success);
        assert!(state.diagnostics.is_empty());
        assert_eq!(state.page_count, 1);
        assert_eq!(state.snapshot.primary_input, PathBuf::from("/tmp/main.tex"));
        assert_eq!(state.snapshot.jobname, "main");
    }

    #[test]
    fn compile_from_source_preserves_recoverable_parse_diagnostics() {
        let gate = MockFileAccessGate {
            read_decision: PathAccessDecision::Allowed,
            write_decision: PathAccessDecision::Allowed,
            read_result: MockReadResult::Success(Vec::new()),
            created_dirs: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
        };
        let loader = MockAssetBundleLoader::valid();

        let state = service(&gate, &loader).compile_from_source(
            "\\documentclass{article}\n\\begin{document}\nA}B\n\\end{document}\n",
            "file:///tmp/main.tex",
        );

        assert!(state.success);
        assert_eq!(state.page_count, 1);
        assert_eq!(state.diagnostics.len(), 1);
        assert_eq!(state.diagnostics[0].message, "unexpected closing brace");
        assert_eq!(state.diagnostics[0].line, Some(3));
    }

    #[test]
    fn returns_access_denied_when_file_access_gate_rejects_input() {
        let dir = tempdir().expect("create tempdir");
        let options = runtime_options(dir.path().join("main.tex"), dir.path().join("out"));
        let gate = MockFileAccessGate {
            read_decision: PathAccessDecision::Denied,
            write_decision: PathAccessDecision::Denied,
            read_result: MockReadResult::AccessDenied(options.input_file.clone()),
            created_dirs: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
        };
        let loader = MockAssetBundleLoader::valid();
        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 2);
        assert_eq!(result.diagnostics[0].message, "input file access denied");
        assert_eq!(result.output_pdf, None);
        assert_eq!(result.stable_compile_state, None);
    }

    #[test]
    fn validates_asset_bundle_before_reading_input() {
        let dir = tempdir().expect("create tempdir");
        let mut options = runtime_options(dir.path().join("main.tex"), dir.path().join("out"));
        options.asset_bundle = Some(dir.path().join("bundle"));

        let gate = MockFileAccessGate {
            read_decision: PathAccessDecision::Allowed,
            write_decision: PathAccessDecision::Allowed,
            read_result: MockReadResult::Success(Vec::new()),
            created_dirs: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
        };
        let loader = MockAssetBundleLoader {
            result: Err("bundle not found at /tmp/bundle".to_string()),
            tex_inputs: BTreeMap::new(),
        };
        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 2);
        assert!(result.diagnostics[0].message.contains("bundle not found"));
        assert_eq!(
            result.diagnostics[0].suggestion.as_deref(),
            Some("verify the asset bundle path and version")
        );
        assert_eq!(result.output_pdf, None);
        assert_eq!(result.stable_compile_state, None);
    }

    #[test]
    fn rejects_asset_bundle_outside_allowed_read_root() {
        let dir = tempdir().expect("create tempdir");
        let mut options = runtime_options(dir.path().join("main.tex"), dir.path().join("out"));
        options.asset_bundle = Some(dir.path().join("../bundle"));

        let gate = MockFileAccessGate {
            read_decision: PathAccessDecision::Denied,
            write_decision: PathAccessDecision::Allowed,
            read_result: MockReadResult::Success(Vec::new()),
            created_dirs: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
        };
        let loader = MockAssetBundleLoader::valid();
        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 2);
        assert_eq!(result.diagnostics[0].message, "asset bundle access denied");
        assert_eq!(result.output_pdf, None);
        assert_eq!(result.stable_compile_state, None);
    }

    #[test]
    fn current_file_relative_takes_precedence_over_project_root() {
        let dir = tempdir().expect("create tempdir");
        let subdir = dir.path().join("subdir");
        fs::create_dir_all(&subdir).expect("create subdir");
        fs::write(dir.path().join("helper.tex"), "PROJECT ROOT HELPER\n")
            .expect("write root helper");
        fs::write(subdir.join("helper.tex"), "CURRENT FILE HELPER\n").expect("write local helper");
        fs::write(
            dir.path().join("main.tex"),
            document("\\input{subdir/section}"),
        )
        .expect("write main");
        fs::write(subdir.join("section.tex"), "\\input{helper}\n").expect("write section");

        let options = runtime_options(dir.path().join("main.tex"), dir.path().join("out"));
        let gate = FsTestFileAccessGate;
        let loader = MockAssetBundleLoader::valid();

        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("CURRENT FILE HELPER"));
        assert!(!pdf.contains("PROJECT ROOT HELPER"));
    }

    #[test]
    fn project_root_fallback_resolves_when_not_in_current_dir() {
        let dir = tempdir().expect("create tempdir");
        let project_root = dir.path().join("project");
        let src = project_root.join("src");
        let subdir = src.join("subdir");
        let shared = project_root.join("shared");
        fs::create_dir_all(project_root.join(".git")).expect("create git marker");
        fs::create_dir_all(&subdir).expect("create subdir");
        fs::create_dir_all(&shared).expect("create shared");
        fs::write(shared.join("macros.tex"), "PROJECT ROOT MACROS\n").expect("write macros");
        fs::write(src.join("main.tex"), document("\\input{subdir/section}")).expect("write main");
        fs::write(subdir.join("section.tex"), "\\input{shared/macros}\n").expect("write section");

        let options = runtime_options(src.join("main.tex"), project_root.join("out"));
        let gate = FsTestFileAccessGate;
        let loader = MockAssetBundleLoader::valid();

        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("PROJECT ROOT MACROS"));
    }

    #[test]
    fn input_if_file_exists_uses_false_branch_when_missing() {
        let dir = tempdir().expect("create tempdir");
        fs::write(
            dir.path().join("main.tex"),
            document("\\InputIfFileExists{missing}{TRUE}{FALSE BRANCH}"),
        )
        .expect("write main");

        let options = runtime_options(dir.path().join("main.tex"), dir.path().join("out"));
        let gate = FsTestFileAccessGate;
        let loader = MockAssetBundleLoader::valid();

        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("FALSE BRANCH"));
        assert!(!pdf.contains("TRUE"));
    }

    #[test]
    fn input_if_file_exists_uses_true_branch_and_file_when_found() {
        let dir = tempdir().expect("create tempdir");
        fs::write(
            dir.path().join("main.tex"),
            document("\\InputIfFileExists{helper}{AFTER INPUT}{MISSING}"),
        )
        .expect("write main");
        fs::write(dir.path().join("helper.tex"), "HELPER CONTENT\n").expect("write helper");

        let options = runtime_options(dir.path().join("main.tex"), dir.path().join("out"));
        let gate = FsTestFileAccessGate;
        let loader = MockAssetBundleLoader::valid();

        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("HELPER CONTENT"));
        assert!(pdf.contains("AFTER INPUT"));
        assert!(!pdf.contains("MISSING"));
    }

    #[test]
    fn input_if_file_exists_resolves_from_bundle() {
        let dir = tempdir().expect("create tempdir");
        let bundle_root = dir.path().join("bundle");
        let bundled_file = bundle_root.join("texmf/bundled.tex");
        fs::create_dir_all(bundled_file.parent().expect("bundle texmf parent"))
            .expect("create bundle texmf");
        fs::write(&bundled_file, "BUNDLED FILE CONTENT\n").expect("write bundled file");
        fs::write(
            dir.path().join("main.tex"),
            document("\\InputIfFileExists{bundled}{AFTER BUNDLE INPUT}{FALLBACK}"),
        )
        .expect("write main");

        let mut options = runtime_options(dir.path().join("main.tex"), dir.path().join("out"));
        options.asset_bundle = Some(bundle_root);

        let gate = FsTestFileAccessGate;
        let loader = MockAssetBundleLoader {
            result: Ok(()),
            tex_inputs: BTreeMap::from([("bundled.tex".to_string(), bundled_file)]),
        };

        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("BUNDLED FILE CONTENT"));
        assert!(pdf.contains("AFTER BUNDLE INPUT"));
        assert!(!pdf.contains("FALLBACK"));
    }

    #[test]
    fn bundle_backed_resolution_provides_tex_input() {
        let dir = tempdir().expect("create tempdir");
        let bundle_root = dir.path().join("bundle");
        let bundled_file = bundle_root.join("texmf/bundled.tex");
        fs::create_dir_all(bundled_file.parent().expect("bundle texmf parent"))
            .expect("create bundle texmf");
        fs::write(&bundled_file, "BUNDLED CONTENT\n").expect("write bundled file");
        fs::write(dir.path().join("main.tex"), document("\\input{bundled}")).expect("write main");

        let mut options = runtime_options(dir.path().join("main.tex"), dir.path().join("out"));
        options.asset_bundle = Some(bundle_root);

        let gate = FsTestFileAccessGate;
        let loader = MockAssetBundleLoader {
            result: Ok(()),
            tex_inputs: BTreeMap::from([("bundled.tex".to_string(), bundled_file)]),
        };

        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("BUNDLED CONTENT"));
    }

    #[test]
    fn falls_back_to_fixed_width_typesetting_when_bundle_has_no_cmr10_tfm() {
        let dir = tempdir().expect("create tempdir");
        let bundle_root = dir.path().join("bundle");
        fs::create_dir_all(&bundle_root).expect("create bundle");
        fs::write(dir.path().join("main.tex"), document("AA")).expect("write main");

        let mut options = runtime_options(dir.path().join("main.tex"), dir.path().join("out"));
        options.asset_bundle = Some(bundle_root);

        let gate = FsTestFileAccessGate;
        let loader = MockAssetBundleLoader::valid();

        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("(AA) Tj"));
        assert!(!pdf.contains("(A) Tj\n0 -18 Td\n(A) Tj"));
    }

    #[test]
    fn uses_bundle_cmr10_tfm_metrics_when_available() {
        let dir = tempdir().expect("create tempdir");
        let bundle_root = dir.path().join("bundle");
        let tfm_path = bundle_root.join("texmf/fonts/tfm/public/cm/cmr10.tfm");
        fs::create_dir_all(tfm_path.parent().expect("cmr10 parent")).expect("create tfm dir");
        fs::write(&tfm_path, build_test_tfm()).expect("write cmr10.tfm");
        fs::write(dir.path().join("main.tex"), document("AA")).expect("write main");

        let mut options = runtime_options(dir.path().join("main.tex"), dir.path().join("out"));
        options.asset_bundle = Some(bundle_root);

        let gate = FsTestFileAccessGate;
        let loader = MockAssetBundleLoader::valid();

        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(!pdf.contains("(AA) Tj"));
        assert!(pdf.contains("(A) Tj\n0 -18 Td\n(A) Tj"));
    }
}
