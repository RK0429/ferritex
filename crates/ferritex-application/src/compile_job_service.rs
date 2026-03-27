use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use ferritex_core::assets::{AssetHandle, LogicalAssetId};
use ferritex_core::bibliography::api::{parse_bbl, BibliographyDiagnostic, BibliographyState};
use ferritex_core::compilation::{
    CompilationJob, CompilationSnapshot, DocumentState, IndexEntry, SymbolLocation,
};
use ferritex_core::diagnostics::{Diagnostic, Severity};
use ferritex_core::font::api::OpenTypeWidthProvider;
use ferritex_core::font::{
    resolve_named_font, OpenTypeFont, TfmMetrics, OPENTYPE_FONT_SEARCH_ROOTS,
};
use ferritex_core::graphics::api::{
    extract_png_image_data, parse_image_metadata, ExternalGraphic, GraphicAssetResolver,
    ImageMetadata,
};
use ferritex_core::incremental::DependencyGraph;
use ferritex_core::kernel::api::DimensionValue;
use ferritex_core::kernel::StableId;
use ferritex_core::parser::{MinimalLatexParser, ParseError, ParseOutput};
use ferritex_core::pdf::{FontResource, ImageFilter, PdfImageXObject, PdfRenderer, PlacedImage};
use ferritex_core::policy::{ExecutionPolicy, OutputArtifactRegistry, PreviewPublicationPolicy};
use ferritex_core::policy::{FileAccessError, FileAccessGate, PathAccessDecision};
use ferritex_core::synctex::{RenderedLineTrace, RenderedPageTrace, SourceLineTrace, SyncTexData};
use ferritex_core::typesetting::{
    resolve_page_labels, MinimalTypesetter, TextLine, TfmWidthProvider, TypesetDocument,
};
use serde_json::json;

use crate::compile_cache::{fingerprint_bytes, CachedSourceSubtree, CompileCache};
use crate::execution_policy_factory::ExecutionPolicyFactory;
use crate::ports::AssetBundleLoaderPort;
use crate::runtime_options::RuntimeOptions;
use crate::stable_compile_state::{
    CrossReferenceCaptionEntry, CrossReferenceSectionEntry, CrossReferenceSeed, StableCompileState,
};

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
    source_lines: Vec<SourceLineTrace>,
    document_state: DocumentState,
    dependency_graph: DependencyGraph,
    cached_source_subtrees: BTreeMap<PathBuf, CachedSourceSubtree>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LoadedSourceSubtree {
    expanded: ExpandedSource,
    source_files: BTreeSet<PathBuf>,
    labels: BTreeMap<String, SymbolLocation>,
    citations: BTreeMap<String, SymbolLocation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceTreeReusePlan {
    rebuild_paths: BTreeSet<PathBuf>,
    cached_dependency_graph: DependencyGraph,
    cached_source_subtrees: BTreeMap<PathBuf, CachedSourceSubtree>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ExpandedSource {
    text: String,
    source_lines: Vec<SourceLineTrace>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ExpandedSourceBuilder {
    text: String,
    source_lines: Vec<SourceLineTrace>,
    current_line_text: String,
    current_line_origin: Option<(String, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LoadedOpenTypeFont {
    base_font: String,
    font: OpenTypeFont,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FontFamilySelection {
    main: Option<LoadedOpenTypeFont>,
    sans: Option<LoadedOpenTypeFont>,
    mono: Option<LoadedOpenTypeFont>,
}

impl FontFamilySelection {
    fn font_for_role(&self, role: u8) -> Option<&LoadedOpenTypeFont> {
        match role {
            0 => self.main.as_ref(),
            1 => self.sans.as_ref(),
            2 => self.mono.as_ref(),
            _ => None,
        }
    }
}

enum CompileFontSelection {
    OpenType(LoadedOpenTypeFont),
    Tfm(TfmMetrics),
    Basic,
}

struct FontLoadTaskResult {
    loaded_font: Option<LoadedOpenTypeFont>,
    diagnostic: Option<Diagnostic>,
}

enum FontLoadResult {
    Main(FontLoadTaskResult),
    Sans(FontLoadTaskResult),
    Mono(FontLoadTaskResult),
}

struct CompileGraphicAssetResolver<'a> {
    file_access_gate: &'a dyn FileAccessGate,
    input_dir: &'a Path,
    project_root: &'a Path,
    overlay_roots: &'a [PathBuf],
    asset_bundle_path: Option<&'a Path>,
}

impl GraphicAssetResolver for CompileGraphicAssetResolver<'_> {
    fn resolve(&self, path: &str) -> Option<ExternalGraphic> {
        let resolved_path = resolve_graphic_path(
            self.input_dir,
            self.project_root,
            self.overlay_roots,
            path,
            self.asset_bundle_path,
        );
        if self.file_access_gate.check_read(&resolved_path) == PathAccessDecision::Denied {
            return None;
        }

        let bytes = self.file_access_gate.read_file(&resolved_path).ok()?;
        let metadata = parse_image_metadata(&bytes)?;

        Some(ExternalGraphic {
            path: resolved_path.to_string_lossy().into_owned(),
            asset_handle: AssetHandle {
                id: LogicalAssetId(stable_id_for_path(&resolved_path)),
            },
            metadata,
        })
    }
}

impl ExpandedSourceBuilder {
    fn append_with_origin(&mut self, text: &str, file: &str, line: u32) {
        if text.is_empty() {
            return;
        }

        let mut remaining = text;
        let mut current_line = line;
        while !remaining.is_empty() {
            if self.current_line_origin.is_none() {
                self.current_line_origin = Some((file.to_string(), current_line));
            }

            if let Some(newline_index) = remaining.find('\n') {
                let prefix = &remaining[..newline_index];
                self.text.push_str(prefix);
                self.text.push('\n');
                self.current_line_text.push_str(prefix);
                let (origin_file, origin_line) = self
                    .current_line_origin
                    .take()
                    .expect("line origin should exist before newline flush");
                self.source_lines.push(SourceLineTrace {
                    file: origin_file,
                    line: origin_line,
                    text: self.current_line_text.clone(),
                });
                self.current_line_text.clear();
                remaining = &remaining[newline_index + 1..];
                current_line += 1;
            } else {
                self.text.push_str(remaining);
                self.current_line_text.push_str(remaining);
                break;
            }
        }
    }

    fn append_expanded(&mut self, expanded: &ExpandedSource) {
        for (segment, origin) in expanded
            .text
            .split_inclusive('\n')
            .zip(expanded.source_lines.iter())
        {
            self.append_with_origin(segment, &origin.file, origin.line);
        }
    }

    fn finish(mut self) -> ExpandedSource {
        if let Some((file, line)) = self.current_line_origin.take() {
            self.source_lines.push(SourceLineTrace {
                file,
                line,
                text: std::mem::take(&mut self.current_line_text),
            });
        }

        ExpandedSource {
            text: self.text,
            source_lines: self.source_lines,
        }
    }
}

impl LoadedSourceSubtree {
    fn from_cached_subtree(cached: &CachedSourceSubtree) -> Self {
        Self {
            expanded: ExpandedSource {
                text: cached.text.clone(),
                source_lines: cached.source_lines.clone(),
            },
            source_files: cached.source_files.iter().cloned().collect(),
            labels: cached.labels.clone(),
            citations: cached.citations.clone(),
        }
    }

    fn to_cached_subtree(&self) -> CachedSourceSubtree {
        CachedSourceSubtree {
            text: self.expanded.text.clone(),
            source_lines: self.expanded.source_lines.clone(),
            source_files: self.source_files.iter().cloned().collect(),
            labels: self.labels.clone(),
            citations: self.citations.clone(),
        }
    }
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

        let compile_cache = CompileCache::new(
            self.file_access_gate,
            &options.output_dir,
            &options.input_file,
            &options.jobname,
        );
        let mut cache_diagnostics = Vec::new();
        let mut cached_cross_reference_seed = None;
        let mut source_tree_reuse_plan = None;
        if !options.no_cache {
            let lookup = compile_cache.lookup();
            cached_cross_reference_seed = lookup
                .baseline_state
                .as_ref()
                .map(|state| state.cross_reference_seed.clone());
            if let Some(cached_artifact) = lookup.artifact {
                tracing::info!(
                    jobname = %options.jobname,
                    input = %options.input_file.display(),
                    output = %cached_artifact.output_pdf.display(),
                    "compile cache hit"
                );
                let diagnostics = cached_artifact.stable_compile_state.diagnostics.clone();
                return CompileResult {
                    exit_code: exit_code_for(&diagnostics),
                    diagnostics,
                    output_pdf: Some(cached_artifact.output_pdf),
                    stable_compile_state: Some(cached_artifact.stable_compile_state),
                };
            }

            if let Some(scope) = lookup.scope {
                tracing::info!(
                    jobname = %options.jobname,
                    input = %options.input_file.display(),
                    changed_paths = ?lookup.changed_paths,
                    rebuild_paths = ?lookup.rebuild_paths,
                    ?scope,
                    "compile cache miss due to changed dependencies"
                );
            }
            if let Some(cached_dependency_graph) = lookup.cached_dependency_graph {
                source_tree_reuse_plan = Some(SourceTreeReusePlan {
                    rebuild_paths: lookup.rebuild_paths,
                    cached_dependency_graph,
                    cached_source_subtrees: lookup.cached_source_subtrees,
                });
            }
            cache_diagnostics.extend(lookup.diagnostics);
        }

        let mut source_tree = match self.load_source_tree(
            &options.input_file,
            &project_root,
            &options.overlay_roots,
            options.asset_bundle.as_deref(),
            source_tree_reuse_plan.as_ref(),
        ) {
            Ok(tree) => tree,
            Err(diagnostic) => {
                let mut diagnostics = cache_diagnostics;
                diagnostics.push(diagnostic);

                return CompileResult {
                    exit_code: exit_code_for(&diagnostics),
                    diagnostics,
                    output_pdf: None,
                    stable_compile_state: None,
                };
            }
        };
        let loaded_bibliography_state = load_bibliography_state(
            self.file_access_gate,
            &project_root,
            &options.overlay_roots,
            &options.output_dir,
            &options.jobname,
        );
        let initial_bibliography_state = loaded_bibliography_state
            .as_ref()
            .map(|loaded| loaded.state.clone());
        if let Some(bibliography_state) = &initial_bibliography_state {
            source_tree.document_state.bibliography_state = bibliography_state.clone();
        }

        let input_dir = options
            .input_file
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .map(normalize_existing_path)
            .unwrap_or_else(|| project_root.clone());
        let graphics_resolver = CompileGraphicAssetResolver {
            file_access_gate: self.file_access_gate,
            input_dir: &input_dir,
            project_root: &project_root,
            overlay_roots: &options.overlay_roots,
            asset_bundle_path: options.asset_bundle.as_deref(),
        };
        let mut compile_font_selection = None;
        let mut font_family_selection = None;
        let mut font_diagnostics = Vec::new();
        let parse_pass_result = self.parse_document_with_cross_references(
            &source_tree.source,
            &project_root,
            &options.overlay_roots,
            options.asset_bundle.as_deref(),
            initial_bibliography_state.clone(),
            source_tree.document_state.index_state.entries.clone(),
            cached_cross_reference_seed.as_ref(),
            |document| {
                if compile_font_selection.is_none() {
                    let (selection, families, diagnostics) = self.select_compile_fonts(
                        &input_path,
                        document.main_font_name.as_deref(),
                        document.sans_font_name.as_deref(),
                        document.mono_font_name.as_deref(),
                        &input_dir,
                        &project_root,
                        &options.overlay_roots,
                        options.asset_bundle.as_deref(),
                        if options.host_font_fallback {
                            &options.host_font_roots
                        } else {
                            &[]
                        },
                        options.parallelism,
                        options.trace_font_tasks,
                    );
                    font_diagnostics.extend(diagnostics);
                    compile_font_selection = Some(selection);
                    font_family_selection = Some(families);
                }

                let selection = compile_font_selection
                    .as_ref()
                    .expect("font selection initialized");

                match selection {
                    CompileFontSelection::OpenType(loaded_font) => {
                        let provider = OpenTypeWidthProvider {
                            font: &loaded_font.font,
                            fallback_width: DEFAULT_TFM_FALLBACK_WIDTH,
                        };
                        self.typesetter.typeset_with_provider_and_graphics_resolver(
                            document,
                            &provider,
                            Some(&graphics_resolver),
                        )
                    }
                    CompileFontSelection::Tfm(metrics) => {
                        let provider = TfmWidthProvider {
                            metrics,
                            fallback_width: DEFAULT_TFM_FALLBACK_WIDTH,
                        };
                        self.typesetter.typeset_with_provider_and_graphics_resolver(
                            document,
                            &provider,
                            Some(&graphics_resolver),
                        )
                    }
                    CompileFontSelection::Basic => self
                        .typesetter
                        .typeset_with_graphics_resolver(document, &graphics_resolver),
                }
            },
        );
        let pdf_renderer = match (
            font_family_selection.as_ref(),
            parse_pass_result.typeset_document.as_ref(),
        ) {
            (Some(families), Some(typeset_document)) => {
                let font_resources = build_multi_font_pdf_resources(
                    families,
                    typeset_document,
                    options.parallelism,
                    options.trace_font_tasks,
                );
                if font_resources.is_empty() {
                    self.pdf_renderer.clone()
                } else {
                    PdfRenderer::with_fonts(font_resources)
                }
            }
            _ => self.pdf_renderer.clone(),
        };
        let ParsePassResult {
            output: ParseOutput { document, errors },
            typeset_document,
            pass_count,
        } = parse_pass_result;
        let mut parse_diagnostics: Vec<Diagnostic> = errors
            .into_iter()
            .map(|error| diagnostic_for_parse_error(error, input_path.clone()))
            .collect();
        parse_diagnostics.extend(font_diagnostics);
        if let Some(loaded_bibliography_state) = &loaded_bibliography_state {
            let bibliography_names = extract_bibliography_declarations(&source_tree.source);
            if let Some(diagnostic) = check_bbl_freshness(
                &loaded_bibliography_state.path,
                &bibliography_names,
                &project_root,
                &options.overlay_roots,
            ) {
                parse_diagnostics.push(diagnostic_for_bibliography(diagnostic, Vec::new()));
            }
        }
        if initial_bibliography_state.is_none()
            && source_uses_citations(&source_tree.source)
            && document
                .as_ref()
                .map(|parsed| {
                    parsed
                        .bibliography_state
                        .bbl
                        .as_ref()
                        .map(|snapshot| snapshot.entries.is_empty())
                        .unwrap_or(true)
                })
                .unwrap_or(true)
        {
            parse_diagnostics.push(diagnostic_for_bibliography(
                BibliographyDiagnostic::MissingBbl,
                bibliography_candidate_paths(
                    &project_root,
                    &options.overlay_roots,
                    &options.output_dir,
                    &options.jobname,
                ),
            ));
        }

        let parsed_document = match document {
            Some(document) => document,
            None => {
                let compilation_job = compilation_job(
                    options.input_file.clone(),
                    options.jobname.clone(),
                    execution_policy.clone(),
                );
                let mut diagnostics = cache_diagnostics.clone();
                diagnostics.extend(parse_diagnostics.clone());

                return CompileResult {
                    exit_code: exit_code_for(&diagnostics),
                    diagnostics: diagnostics.clone(),
                    output_pdf: None,
                    stable_compile_state: Some(stable_compile_state(
                        &compilation_job,
                        source_tree.document_state.clone(),
                        CrossReferenceSeed::default(),
                        pass_count,
                        0,
                        false,
                        diagnostics,
                    )),
                };
            }
        };
        let typeset_document = typeset_document.expect("parsed documents should always typeset");
        let cross_reference_seed =
            cross_reference_seed_from_document(&parsed_document, &typeset_document);
        persist_compiled_document_state(
            &mut source_tree.document_state,
            &parsed_document,
            &typeset_document,
        );
        let pdf_renderer = match build_pdf_renderer_with_images(
            self.file_access_gate,
            pdf_renderer,
            &typeset_document,
        ) {
            Ok(renderer) => renderer,
            Err(diagnostic) => {
                let mut diagnostics = cache_diagnostics.clone();
                diagnostics.extend(parse_diagnostics.clone());
                diagnostics.push(diagnostic);

                return CompileResult {
                    exit_code: exit_code_for(&diagnostics),
                    diagnostics,
                    output_pdf: None,
                    stable_compile_state: None,
                };
            }
        };
        let pdf_document = pdf_renderer.render(&typeset_document);
        let compilation_job = compilation_job(
            options.input_file.clone(),
            options.jobname.clone(),
            execution_policy,
        );
        let cacheable_diagnostics = parse_diagnostics;
        let mut diagnostics = cache_diagnostics;
        diagnostics.extend(cacheable_diagnostics.clone());

        if let Err(error) = self
            .file_access_gate
            .write_file(&output_pdf, &pdf_document.bytes)
        {
            diagnostics.push(diagnostic_for_output_error(error, &output_pdf));

            return CompileResult {
                exit_code: exit_code_for(&diagnostics),
                diagnostics,
                output_pdf: None,
                stable_compile_state: None,
            };
        }

        if options.synctex {
            let synctex_path = options
                .output_dir
                .join(format!("{}.synctex", options.jobname));
            let synctex = SyncTexData::build_line_based(
                &synctex_pages_for(&typeset_document),
                &source_tree.source_lines,
            );
            match serde_json::to_vec_pretty(&synctex) {
                Ok(bytes) => {
                    if let Err(error) = self.file_access_gate.write_file(&synctex_path, &bytes) {
                        diagnostics.push(diagnostic_for_synctex_error(error, &synctex_path));
                    }
                }
                Err(error) => diagnostics.push(
                    Diagnostic::new(
                        Severity::Error,
                        format!("failed to serialize SyncTeX data: {error}"),
                    )
                    .with_file(synctex_path.to_string_lossy().into_owned()),
                ),
            }
        }

        let output_pdf_hash = fingerprint_bytes(&pdf_document.bytes);
        let provisional_stable_state = stable_compile_state(
            &compilation_job,
            source_tree.document_state.clone(),
            cross_reference_seed.clone(),
            pass_count,
            pdf_document.page_count,
            true,
            cacheable_diagnostics.clone(),
        );
        if !options.no_cache {
            if let Some(diagnostic) = compile_cache.store(
                &source_tree.dependency_graph,
                &provisional_stable_state,
                output_pdf_hash,
                &source_tree.cached_source_subtrees,
            ) {
                diagnostics.push(diagnostic);
            }
        }

        let stable_compile_state = stable_compile_state(
            &compilation_job,
            source_tree.document_state,
            cross_reference_seed,
            pass_count,
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
        let mut source_tree = self
            .load_source_tree_with_root_source(
                &primary_input,
                Some(source),
                &project_root,
                &[],
                None,
                None,
            )
            .unwrap_or_else(|_| LoadedSourceTree {
                source: source.to_string(),
                source_lines: vec![SourceLineTrace {
                    file: primary_input.to_string_lossy().into_owned(),
                    line: 1,
                    text: source.to_string(),
                }],
                document_state: DocumentState::default(),
                dependency_graph: DependencyGraph::default(),
                cached_source_subtrees: BTreeMap::new(),
            });

        let primary_input_path = primary_input.to_string_lossy().into_owned();
        let ParsePassResult {
            output: ParseOutput { document, errors },
            typeset_document,
            pass_count,
        } = self.parse_document_with_cross_references(
            &source_tree.source,
            &project_root,
            &[],
            None,
            source_tree.document_state.bibliography_state.clone().into(),
            source_tree.document_state.index_state.entries.clone(),
            None,
            |document| self.typesetter.typeset(document),
        );
        let parse_diagnostics: Vec<Diagnostic> = errors
            .into_iter()
            .map(|error| diagnostic_for_parse_error(error, primary_input_path.clone()))
            .collect();

        match document {
            Some(parsed_document) => {
                let typeset_document =
                    typeset_document.expect("parsed documents should always typeset");
                persist_compiled_document_state(
                    &mut source_tree.document_state,
                    &parsed_document,
                    &typeset_document,
                );
                let pdf_document = self.pdf_renderer.render(&typeset_document);
                let cross_reference_seed =
                    cross_reference_seed_from_document(&parsed_document, &typeset_document);
                stable_compile_state(
                    &compilation_job,
                    source_tree.document_state,
                    cross_reference_seed,
                    pass_count,
                    pdf_document.page_count,
                    true,
                    parse_diagnostics,
                )
            }
            None => stable_compile_state(
                &compilation_job,
                source_tree.document_state,
                CrossReferenceSeed::default(),
                pass_count,
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
        overlay_roots: &[PathBuf],
        asset_bundle_path: Option<&Path>,
        reuse_plan: Option<&SourceTreeReusePlan>,
    ) -> Result<LoadedSourceTree, Diagnostic> {
        self.load_source_tree_with_root_source(
            input_file,
            None,
            project_root,
            overlay_roots,
            asset_bundle_path,
            reuse_plan,
        )
    }

    fn select_compile_fonts(
        &self,
        input_path: &str,
        requested_main_font: Option<&str>,
        requested_sans_font: Option<&str>,
        requested_mono_font: Option<&str>,
        input_dir: &Path,
        project_root: &Path,
        overlay_roots: &[PathBuf],
        asset_bundle_path: Option<&Path>,
        host_font_roots: &[PathBuf],
        parallelism: usize,
        trace_font_tasks: bool,
    ) -> (CompileFontSelection, FontFamilySelection, Vec<Diagnostic>) {
        let mut diagnostics = Vec::new();
        let resolution_surface = if host_font_roots.is_empty() {
            "project directory, overlay roots, or asset bundle"
        } else {
            "project directory, overlay roots, asset bundle, or host font catalog"
        };
        let resolution_suggestion = if host_font_roots.is_empty() {
            "place the font in the document directory, project fonts/, configured overlay roots, or asset bundle"
        } else {
            "place the font in the document directory, project fonts/, configured overlay roots, asset bundle, or host font catalog"
        };
        let mut tasks: Vec<Box<dyn FnOnce() -> FontLoadResult + Send + '_>> =
            vec![Box::new(move || {
                FontLoadResult::Main(load_main_font_task(
                    input_path,
                    requested_main_font,
                    input_dir,
                    project_root,
                    overlay_roots,
                    asset_bundle_path,
                    host_font_roots,
                    self.file_access_gate,
                    resolution_surface,
                    resolution_suggestion,
                    trace_font_tasks,
                    0,
                ))
            })];
        if let Some(font_name) = requested_sans_font {
            tasks.push(Box::new(move || {
                FontLoadResult::Sans(load_optional_font_task(
                    input_path,
                    font_name,
                    input_dir,
                    project_root,
                    overlay_roots,
                    asset_bundle_path,
                    host_font_roots,
                    self.file_access_gate,
                    resolution_surface,
                    resolution_suggestion,
                    "font-load-sans",
                    "PDF output will fall back to a built-in sans font until then",
                    trace_font_tasks,
                    1,
                ))
            }));
        }
        if let Some(font_name) = requested_mono_font {
            tasks.push(Box::new(move || {
                FontLoadResult::Mono(load_optional_font_task(
                    input_path,
                    font_name,
                    input_dir,
                    project_root,
                    overlay_roots,
                    asset_bundle_path,
                    host_font_roots,
                    self.file_access_gate,
                    resolution_surface,
                    resolution_suggestion,
                    "font-load-mono",
                    "PDF output will fall back to a built-in mono font until then",
                    trace_font_tasks,
                    2,
                ))
            }));
        }

        let mut main = None;
        let mut sans = None;
        let mut mono = None;
        for result in run_font_tasks(parallelism, tasks) {
            match result {
                FontLoadResult::Main(result) => {
                    main = result.loaded_font;
                    if let Some(diagnostic) = result.diagnostic {
                        diagnostics.push(diagnostic);
                    }
                }
                FontLoadResult::Sans(result) => {
                    sans = result.loaded_font;
                    if let Some(diagnostic) = result.diagnostic {
                        diagnostics.push(diagnostic);
                    }
                }
                FontLoadResult::Mono(result) => {
                    mono = result.loaded_font;
                    if let Some(diagnostic) = result.diagnostic {
                        diagnostics.push(diagnostic);
                    }
                }
            }
        }

        let families = FontFamilySelection { main, sans, mono };

        if let Some(loaded_font) = families.main.clone() {
            return (
                CompileFontSelection::OpenType(loaded_font),
                families,
                diagnostics,
            );
        }

        if let Some(metrics) = trace_font_task(
            trace_font_tasks,
            "font-load-cmr10-fallback",
            "cmr10.tfm",
            0,
            || load_cmr10_metrics(self.file_access_gate, asset_bundle_path),
        ) {
            return (CompileFontSelection::Tfm(metrics), families, diagnostics);
        }

        if trace_font_tasks {
            let timestamp = trace_timestamp_micros();
            emit_font_task_trace(
                "font-load-basic-fallback",
                "builtin:basic",
                timestamp,
                timestamp,
                0,
            );
        }
        (CompileFontSelection::Basic, families, diagnostics)
    }

    fn load_source_tree_with_root_source(
        &self,
        input_file: &Path,
        root_source: Option<&str>,
        project_root: &Path,
        overlay_roots: &[PathBuf],
        asset_bundle_path: Option<&Path>,
        reuse_plan: Option<&SourceTreeReusePlan>,
    ) -> Result<LoadedSourceTree, Diagnostic> {
        let root_input = normalize_existing_path(input_file);
        let project_root = normalize_existing_path(project_root);
        let mut visited = BTreeSet::new();
        let mut include_guard = BTreeSet::new();
        let mut dependency_graph = DependencyGraph::default();
        let mut cached_source_subtrees = BTreeMap::new();
        let source = self.load_source_file(
            &root_input,
            &project_root,
            root_source,
            overlay_roots,
            asset_bundle_path,
            &mut visited,
            &mut include_guard,
            reuse_plan,
            &mut dependency_graph,
            &mut cached_source_subtrees,
        )?;

        Ok(LoadedSourceTree {
            source: source.expanded.text,
            source_lines: source.expanded.source_lines,
            document_state: DocumentState {
                revision: 0,
                bibliography_dirty: false,
                source_files: source
                    .source_files
                    .into_iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
                labels: source.labels,
                citations: source.citations,
                bibliography_state: BibliographyState::default(),
                navigation: Default::default(),
                index_state: Default::default(),
            },
            dependency_graph,
            cached_source_subtrees,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn load_source_file(
        &self,
        path: &Path,
        workspace_root: &Path,
        source_override: Option<&str>,
        overlay_roots: &[PathBuf],
        asset_bundle_path: Option<&Path>,
        visited: &mut BTreeSet<PathBuf>,
        include_guard: &mut BTreeSet<PathBuf>,
        reuse_plan: Option<&SourceTreeReusePlan>,
        dependency_graph: &mut DependencyGraph,
        cached_source_subtrees: &mut BTreeMap<PathBuf, CachedSourceSubtree>,
    ) -> Result<LoadedSourceSubtree, Diagnostic> {
        let normalized_path = normalize_existing_path(path);
        if !visited.insert(normalized_path.clone()) {
            return Err(Diagnostic::new(
                Severity::Error,
                "input cycle detected while expanding source files",
            )
            .with_file(normalized_path.to_string_lossy().into_owned())
            .with_suggestion("remove the recursive \\input/\\include chain"));
        }

        if source_override.is_none() {
            if let Some(reuse_plan) = reuse_plan {
                if !reuse_plan.rebuild_paths.contains(&normalized_path) {
                    if let Some(cached_subtree) =
                        reuse_plan.cached_source_subtrees.get(&normalized_path)
                    {
                        restore_cached_subtree_graph(
                            dependency_graph,
                            &reuse_plan.cached_dependency_graph,
                            cached_subtree,
                        );
                        cached_source_subtrees
                            .insert(normalized_path.clone(), cached_subtree.clone());
                        visited.remove(&normalized_path);
                        return Ok(LoadedSourceSubtree::from_cached_subtree(cached_subtree));
                    }
                }
            }
        }

        let source = match source_override {
            Some(source) => source.to_string(),
            None => read_utf8_file(self.file_access_gate, &normalized_path)?,
        };
        dependency_graph.record_node(
            normalized_path.clone(),
            fingerprint_bytes(source.as_bytes()),
        );
        let mut source_files = BTreeSet::from([normalized_path.clone()]);
        let mut labels = BTreeMap::new();
        let mut citations = BTreeMap::new();
        collect_symbol_locations(&source, &normalized_path, "label", &mut labels);
        collect_symbol_locations(&source, &normalized_path, "bibitem", &mut citations);

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
            overlay_roots,
            asset_bundle_path,
            visited,
            include_guard,
            reuse_plan,
            &mut source_files,
            &mut labels,
            &mut citations,
            dependency_graph,
            cached_source_subtrees,
        )?;
        visited.remove(&normalized_path);
        let subtree = LoadedSourceSubtree {
            expanded,
            source_files,
            labels,
            citations,
        };
        cached_source_subtrees.insert(normalized_path, subtree.to_cached_subtree());
        Ok(subtree)
    }

    fn parse_document_with_cross_references<F>(
        &self,
        source: &str,
        project_root: &Path,
        overlay_roots: &[PathBuf],
        asset_bundle_path: Option<&Path>,
        initial_bibliography_state: Option<BibliographyState>,
        initial_index_entries: Vec<IndexEntry>,
        cached_cross_reference_seed: Option<&CrossReferenceSeed>,
        mut typeset_document_for: F,
    ) -> ParsePassResult
    where
        F: FnMut(&ferritex_core::parser::ParsedDocument) -> TypesetDocument,
    {
        let sty_resolver = |package_name: &str| {
            load_package_source(
                self.file_access_gate,
                self.asset_bundle_loader,
                project_root,
                overlay_roots,
                asset_bundle_path,
                package_name,
            )
        };
        let initial_labels = cached_cross_reference_seed
            .map(|seed| seed.labels.clone())
            .unwrap_or_default();
        let initial_section_entries = cached_cross_reference_seed
            .map(seed_section_entries_to_parser)
            .unwrap_or_default();
        let initial_figure_entries = cached_cross_reference_seed
            .map(|seed| seed_caption_entries_to_parser(&seed.figure_entries))
            .unwrap_or_default();
        let initial_table_entries = cached_cross_reference_seed
            .map(|seed| seed_caption_entries_to_parser(&seed.table_entries))
            .unwrap_or_default();
        let initial_bibliography = cached_cross_reference_seed
            .map(|seed| seed.bibliography.clone())
            .unwrap_or_default();
        let initial_page_labels = cached_cross_reference_seed
            .map(|seed| seed.page_labels.clone())
            .unwrap_or_default();
        let initial_index_entries = if initial_index_entries.is_empty() {
            cached_cross_reference_seed
                .map(|seed| seed.index_entries.clone())
                .unwrap_or_default()
        } else {
            initial_index_entries
        };
        let mut output = self
            .parser
            .parse_recovering_with_context_and_package_resolver(
                source,
                initial_labels,
                initial_section_entries,
                initial_figure_entries,
                initial_table_entries,
                initial_bibliography,
                initial_bibliography_state.clone(),
                initial_page_labels,
                initial_index_entries.clone(),
                Some(&sty_resolver),
            );
        let Some(mut document) = output.document.clone() else {
            return ParsePassResult {
                output,
                typeset_document: None,
                pass_count: 1,
            };
        };
        let mut pass_count = 1;

        if document.has_unresolved_refs
            || document.has_unresolved_toc
            || document.has_unresolved_lof
            || document.has_unresolved_lot
        {
            let second = self
                .parser
                .parse_recovering_with_context_and_package_resolver(
                    source,
                    document.labels.clone().into_inner(),
                    document.section_entries.clone(),
                    document.figure_entries.clone(),
                    document.table_entries.clone(),
                    document.bibliography.clone(),
                    Some(document.bibliography_state.clone()),
                    BTreeMap::new(),
                    initial_index_entries,
                    Some(&sty_resolver),
                );
            if let Some(next_document) = second.document.clone() {
                output = second;
                document = next_document;
                pass_count = 2;
            }
        }

        let mut typeset_document = typeset_document_for(&document);

        while pass_count < 3 {
            let page_labels = if document.has_pageref_markers() {
                resolve_page_labels(&document, &typeset_document.pages)
            } else {
                BTreeMap::new()
            };
            let index_entries = typeset_document.index_entries.clone();
            let needs_pageref_pass = document.has_pageref_markers() && !page_labels.is_empty();
            let needs_index_pass = document.has_unresolved_index && !index_entries.is_empty();

            if !needs_pageref_pass && !needs_index_pass {
                break;
            }

            let next = self
                .parser
                .parse_recovering_with_context_and_package_resolver(
                    source,
                    document.labels.clone().into_inner(),
                    document.section_entries.clone(),
                    document.figure_entries.clone(),
                    document.table_entries.clone(),
                    document.bibliography.clone(),
                    Some(document.bibliography_state.clone()),
                    page_labels,
                    index_entries,
                    Some(&sty_resolver),
                );
            let Some(next_document) = next.document.clone() else {
                break;
            };

            output = next;
            document = next_document;
            pass_count += 1;
            typeset_document = typeset_document_for(&document);
        }

        ParsePassResult {
            output,
            typeset_document: Some(typeset_document),
            pass_count,
        }
    }
}

fn stable_compile_state(
    compilation_job: &CompilationJob,
    document_state: DocumentState,
    cross_reference_seed: CrossReferenceSeed,
    pass_count: u32,
    page_count: usize,
    success: bool,
    diagnostics: Vec<Diagnostic>,
) -> StableCompileState {
    StableCompileState {
        snapshot: CompilationSnapshot::from_session(&compilation_job.begin_pass(pass_count)),
        document_state,
        cross_reference_seed,
        page_count,
        success,
        diagnostics,
    }
}

fn cross_reference_seed_from_document(
    parsed_document: &ferritex_core::parser::ParsedDocument,
    typeset_document: &TypesetDocument,
) -> CrossReferenceSeed {
    let page_labels = if parsed_document.has_pageref_markers() {
        resolve_page_labels(parsed_document, &typeset_document.pages)
    } else {
        BTreeMap::new()
    };

    CrossReferenceSeed {
        labels: parsed_document.labels.clone().into_inner(),
        section_entries: parsed_document
            .section_entries
            .iter()
            .map(|entry| CrossReferenceSectionEntry {
                level: entry.level,
                number: entry.number.clone(),
                title: entry.title.clone(),
            })
            .collect(),
        figure_entries: parsed_document
            .figure_entries
            .iter()
            .map(|entry| CrossReferenceCaptionEntry {
                kind: caption_kind_name(entry.kind),
                number: entry.number.clone(),
                caption: entry.caption.clone(),
            })
            .collect(),
        table_entries: parsed_document
            .table_entries
            .iter()
            .map(|entry| CrossReferenceCaptionEntry {
                kind: caption_kind_name(entry.kind),
                number: entry.number.clone(),
                caption: entry.caption.clone(),
            })
            .collect(),
        bibliography: parsed_document.bibliography.clone(),
        page_labels,
        index_entries: typeset_document.index_entries.clone(),
    }
}

fn seed_section_entries_to_parser(
    seed: &CrossReferenceSeed,
) -> Vec<ferritex_core::parser::SectionEntry> {
    seed.section_entries
        .iter()
        .map(|entry| ferritex_core::parser::SectionEntry {
            level: entry.level,
            number: entry.number.clone(),
            title: entry.title.clone(),
        })
        .collect()
}

fn seed_caption_entries_to_parser(
    entries: &[CrossReferenceCaptionEntry],
) -> Vec<ferritex_core::parser::api::CaptionEntry> {
    entries
        .iter()
        .map(|entry| ferritex_core::parser::api::CaptionEntry {
            kind: caption_kind_from_name(&entry.kind),
            number: entry.number.clone(),
            caption: entry.caption.clone(),
        })
        .collect()
}

fn caption_kind_name(kind: ferritex_core::parser::api::FloatType) -> String {
    match kind {
        ferritex_core::parser::api::FloatType::Figure => "figure".to_string(),
        ferritex_core::parser::api::FloatType::Table => "table".to_string(),
    }
}

fn caption_kind_from_name(kind: &str) -> ferritex_core::parser::api::FloatType {
    match kind {
        "table" => ferritex_core::parser::api::FloatType::Table,
        _ => ferritex_core::parser::api::FloatType::Figure,
    }
}

fn persist_compiled_document_state(
    document_state: &mut DocumentState,
    parsed_document: &ferritex_core::parser::ParsedDocument,
    typeset_document: &TypesetDocument,
) {
    document_state.bibliography_state = parsed_document.bibliography_state.clone();
    document_state.navigation = typeset_document.navigation.clone();
    document_state.index_state.enabled = parsed_document.index_enabled;
    document_state.index_state.entries = typeset_document.index_entries.clone();
}

struct ParsePassResult {
    output: ParseOutput,
    typeset_document: Option<TypesetDocument>,
    pass_count: u32,
}

struct LoadedBibliographyState {
    state: BibliographyState,
    path: PathBuf,
}

fn load_bibliography_state(
    file_access_gate: &dyn FileAccessGate,
    project_root: &Path,
    overlay_roots: &[PathBuf],
    artifact_root: &Path,
    jobname: &str,
) -> Option<LoadedBibliographyState> {
    for candidate in
        bibliography_candidate_paths(project_root, overlay_roots, artifact_root, jobname)
    {
        if !candidate.exists()
            || file_access_gate.check_read(&candidate) == PathAccessDecision::Denied
        {
            continue;
        }

        let Ok(bytes) = file_access_gate.read_file(&candidate) else {
            continue;
        };
        let input = String::from_utf8_lossy(&bytes);
        return Some(LoadedBibliographyState {
            state: BibliographyState::from_snapshot(parse_bbl(&input)),
            path: candidate,
        });
    }

    None
}

fn bibliography_candidate_paths(
    project_root: &Path,
    overlay_roots: &[PathBuf],
    artifact_root: &Path,
    jobname: &str,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();

    for root in std::iter::once(project_root)
        .chain(overlay_roots.iter().map(PathBuf::as_path))
        .chain(std::iter::once(artifact_root))
    {
        let candidate = root.join(format!("{jobname}.bbl"));
        if seen.insert(candidate.clone()) {
            candidates.push(candidate);
        }
    }

    candidates
}

fn source_uses_citations(source: &str) -> bool {
    source.contains(r"\cite")
}

fn extract_bibliography_declarations(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = 0usize;

    while let Some(relative_start) = source[cursor..].find(r"\bibliography") {
        let command_start = cursor + relative_start;
        let mut index = command_start + r"\bibliography".len();
        let trimmed = source[index..].trim_start();
        index += source[index..].len() - trimmed.len();

        if !source[index..].starts_with('{') {
            cursor = (index + 1).min(source.len());
            continue;
        }

        let Some(closing_offset) = source[index..].find('}') else {
            break;
        };
        let declaration = &source[index + 1..index + closing_offset];
        names.extend(
            declaration
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned),
        );
        cursor = index + closing_offset + 1;
    }

    names
}

fn check_bbl_freshness(
    bbl_path: &Path,
    bib_names: &[String],
    project_root: &Path,
    overlay_roots: &[PathBuf],
) -> Option<BibliographyDiagnostic> {
    let bbl_modified = std::fs::metadata(bbl_path).ok()?.modified().ok()?;

    for bib_name in bib_names {
        let Some(candidate) = bibliography_input_path(project_root, overlay_roots, bib_name) else {
            continue;
        };
        let Ok(metadata) = std::fs::metadata(&candidate) else {
            continue;
        };
        let Ok(bib_modified) = metadata.modified() else {
            continue;
        };

        if bib_modified > bbl_modified {
            return Some(BibliographyDiagnostic::StaleBbl {
                reason: format!(
                    "bibliography source `{}` is newer than `{}`",
                    candidate.display(),
                    bbl_path.display()
                ),
            });
        }
    }

    None
}

fn bibliography_input_path(
    project_root: &Path,
    overlay_roots: &[PathBuf],
    bib_name: &str,
) -> Option<PathBuf> {
    std::iter::once(project_root)
        .chain(overlay_roots.iter().map(PathBuf::as_path))
        .map(|root| root.join(format!("{bib_name}.bib")))
        .find(|candidate| candidate.exists())
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

fn synctex_pages_for(document: &TypesetDocument) -> Vec<RenderedPageTrace> {
    document
        .pages
        .iter()
        .map(|page| RenderedPageTrace {
            lines: page
                .lines
                .iter()
                .map(|line| RenderedLineTrace {
                    text: line.text.clone(),
                    y: line.y,
                })
                .collect(),
        })
        .collect()
}

fn diagnostic_for_synctex_error(error: FileAccessError, synctex_path: &Path) -> Diagnostic {
    match error {
        FileAccessError::AccessDenied { .. } => {
            Diagnostic::new(Severity::Error, "SyncTeX output access denied")
                .with_file(synctex_path.to_string_lossy().into_owned())
                .with_suggestion("check the output directory and file access policy")
        }
        FileAccessError::Io { source } => Diagnostic::new(
            Severity::Error,
            format!("failed to write SyncTeX output: {source}"),
        )
        .with_file(synctex_path.to_string_lossy().into_owned()),
    }
}

fn diagnostic_for_bibliography(
    diagnostic: BibliographyDiagnostic,
    searched_paths: Vec<PathBuf>,
) -> Diagnostic {
    match diagnostic {
        BibliographyDiagnostic::MissingBbl => {
            let looked_in = searched_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Diagnostic::new(Severity::Warning, "bibliography .bbl file not found")
                .with_context(format!("looked for {}", looked_in))
                .with_suggestion("run bibtex or biber to generate the .bbl file")
        }
        BibliographyDiagnostic::StaleBbl { reason } => {
            Diagnostic::new(Severity::Warning, "bibliography .bbl file appears stale")
                .with_context(reason)
                .with_suggestion("rebuild the bibliography with bibtex or biber")
        }
        BibliographyDiagnostic::UnresolvedCitation { key } => {
            Diagnostic::new(Severity::Warning, format!("unresolved citation `{key}`"))
                .with_suggestion("ensure the bibliography contains the cited key")
        }
    }
}

fn diagnostic_for_parse_error(error: ParseError, input_path: String) -> Diagnostic {
    let severity = match error {
        ParseError::FontspecNotLoaded { .. } | ParseError::SetmainfontInBody { .. } => {
            Severity::Warning
        }
        _ => Severity::Error,
    };
    let diagnostic = Diagnostic::new(severity, error.to_string()).with_file(input_path);
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
        ParseError::UnclosedEnvironment { name, .. } => diagnostic
            .with_context(format!(
                "the parser reached EOF while `{name}` was still open"
            ))
            .with_suggestion(format!("add the matching \\end{{{name}}}")),
        ParseError::UnexpectedElse { .. } => diagnostic
            .with_context("the parser found \\else without a matching open conditional")
            .with_suggestion("remove the stray \\else or add the matching \\if..."),
        ParseError::UnexpectedFi { .. } => diagnostic
            .with_context("the parser found \\fi without a matching open conditional")
            .with_suggestion("remove the stray \\fi or add the matching \\if..."),
        ParseError::FontspecNotLoaded { .. } => diagnostic
            .with_context(
                "font selection commands activate only after loading the fontspec package",
            )
            .with_suggestion("add \\usepackage{fontspec} in the preamble before \\setmainfont"),
        ParseError::SetmainfontInBody { .. } => diagnostic
            .with_context(
                "the minimal fontspec implementation only supports document-global font selection",
            )
            .with_suggestion("move \\setmainfont{...} before \\begin{document}"),
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
    overlay_roots: &[PathBuf],
    asset_bundle_path: Option<&Path>,
    visited: &mut BTreeSet<PathBuf>,
    include_guard: &mut BTreeSet<PathBuf>,
    reuse_plan: Option<&SourceTreeReusePlan>,
    source_files: &mut BTreeSet<PathBuf>,
    labels: &mut BTreeMap<String, SymbolLocation>,
    citations: &mut BTreeMap<String, SymbolLocation>,
    dependency_graph: &mut DependencyGraph,
    cached_source_subtrees: &mut BTreeMap<PathBuf, CachedSourceSubtree>,
) -> Result<ExpandedSource, Diagnostic> {
    let mut expanded = ExpandedSourceBuilder::default();
    let source_file = source_path.to_string_lossy().into_owned();

    for (line_index, line) in source.split_inclusive('\n').enumerate() {
        let source_line = line_index as u32 + 1;
        let visible = strip_line_comment(line);
        let matches = input_commands_in_line(&visible, source_line);
        if matches.is_empty() {
            expanded.append_with_origin(line, &source_file, source_line);
            continue;
        }

        let mut cursor = 0usize;
        for command in matches {
            expanded.append_with_origin(&line[cursor..command.start], &source_file, source_line);

            let resolved = resolve_input_path(
                base_dir,
                workspace_root,
                overlay_roots,
                &command.value,
                service.asset_bundle_loader,
                asset_bundle_path,
            );

            match &command.kind {
                InlineCommandKind::Input => {
                    dependency_graph.record_edge(source_path.to_path_buf(), resolved.clone());
                    let nested = service
                        .load_source_file(
                            &resolved,
                            workspace_root,
                            None,
                            overlay_roots,
                            asset_bundle_path,
                            visited,
                            include_guard,
                            reuse_plan,
                            dependency_graph,
                            cached_source_subtrees,
                        )
                        .map_err(|diagnostic| {
                            diagnostic_for_nested_input_error(
                                diagnostic,
                                source_path,
                                command.line,
                                &command.value,
                            )
                        })?;
                    merge_loaded_subtree(source_files, labels, citations, &nested);
                    expanded.append_expanded(&nested.expanded);
                }
                InlineCommandKind::Include => {
                    dependency_graph.record_edge(source_path.to_path_buf(), resolved.clone());
                    if !include_guard.insert(resolved.clone()) {
                        cursor = command.end;
                        continue;
                    }

                    let nested = service
                        .load_source_file(
                            &resolved,
                            workspace_root,
                            None,
                            overlay_roots,
                            asset_bundle_path,
                            visited,
                            include_guard,
                            reuse_plan,
                            dependency_graph,
                            cached_source_subtrees,
                        )
                        .map_err(|diagnostic| {
                            diagnostic_for_nested_input_error(
                                diagnostic,
                                source_path,
                                command.line,
                                &command.value,
                            )
                        })?;
                    merge_loaded_subtree(source_files, labels, citations, &nested);
                    expanded.append_expanded(&nested.expanded);
                }
                InlineCommandKind::InputIfFileExists {
                    true_branch,
                    false_branch,
                } => {
                    if resolved.exists() {
                        dependency_graph.record_edge(source_path.to_path_buf(), resolved.clone());
                        let nested = service
                            .load_source_file(
                                &resolved,
                                workspace_root,
                                None,
                                overlay_roots,
                                asset_bundle_path,
                                visited,
                                include_guard,
                                reuse_plan,
                                dependency_graph,
                                cached_source_subtrees,
                            )
                            .map_err(|diagnostic| {
                                diagnostic_for_nested_input_error(
                                    diagnostic,
                                    source_path,
                                    command.line,
                                    &command.value,
                                )
                            })?;
                        merge_loaded_subtree(source_files, labels, citations, &nested);
                        expanded.append_expanded(&nested.expanded);
                        expanded.append_with_origin(true_branch, &source_file, source_line);
                    } else {
                        expanded.append_with_origin(false_branch, &source_file, source_line);
                    }
                }
            }
            cursor = command.end;
        }

        expanded.append_with_origin(&line[cursor..], &source_file, source_line);
    }

    Ok(expanded.finish())
}

fn merge_loaded_subtree(
    source_files: &mut BTreeSet<PathBuf>,
    labels: &mut BTreeMap<String, SymbolLocation>,
    citations: &mut BTreeMap<String, SymbolLocation>,
    nested: &LoadedSourceSubtree,
) {
    source_files.extend(nested.source_files.iter().cloned());
    extend_symbol_locations(labels, &nested.labels);
    extend_symbol_locations(citations, &nested.citations);
}

fn extend_symbol_locations(
    target: &mut BTreeMap<String, SymbolLocation>,
    additional: &BTreeMap<String, SymbolLocation>,
) {
    for (key, value) in additional {
        target.entry(key.clone()).or_insert_with(|| value.clone());
    }
}

fn restore_cached_subtree_graph(
    dependency_graph: &mut DependencyGraph,
    cached_dependency_graph: &DependencyGraph,
    cached_subtree: &CachedSourceSubtree,
) {
    let subtree_paths = cached_subtree
        .source_files
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();

    for path in &subtree_paths {
        if let Some(node) = cached_dependency_graph.nodes.get(path) {
            dependency_graph.nodes.insert(path.clone(), node.clone());
        }
        if let Some(edges) = cached_dependency_graph.edges.get(path) {
            dependency_graph
                .edges
                .entry(path.clone())
                .or_default()
                .extend(
                    edges
                        .iter()
                        .filter(|target| subtree_paths.contains(*target))
                        .cloned(),
                );
        }
    }
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

fn build_pdf_renderer_with_images(
    file_access_gate: &dyn FileAccessGate,
    renderer: PdfRenderer,
    document: &TypesetDocument,
) -> Result<PdfRenderer, Diagnostic> {
    if document.pages.iter().all(|page| page.images.is_empty()) {
        return Ok(renderer);
    }

    let mut images = Vec::new();
    let mut image_indices = std::collections::HashMap::new();
    let mut page_images = Vec::with_capacity(document.pages.len());

    for page in &document.pages {
        let mut placements = Vec::with_capacity(page.images.len());
        for image in &page.images {
            let xobject_index = if let Some(index) =
                image_indices.get(&image.graphic.asset_handle.id)
            {
                *index
            } else {
                let path = Path::new(&image.graphic.path);
                let bytes = file_access_gate.read_file(path).map_err(|error| {
                    diagnostic_for_input_error(error, image.graphic.path.clone())
                })?;
                let xobject =
                    build_pdf_image_xobject(&image.graphic.path, &image.graphic.metadata, &bytes)?;
                let index = images.len();
                images.push(xobject);
                image_indices.insert(image.graphic.asset_handle.id.clone(), index);
                index
            };

            placements.push(PlacedImage {
                xobject_index,
                x: image.x,
                y: image.y,
                display_width: image.display_width,
                display_height: image.display_height,
            });
        }
        page_images.push(placements);
    }

    Ok(renderer.with_images(images, page_images))
}

fn build_pdf_image_xobject(
    path: &str,
    metadata: &ImageMetadata,
    bytes: &[u8],
) -> Result<PdfImageXObject, Diagnostic> {
    if let Some(image_data) = extract_png_image_data(bytes) {
        return Ok(PdfImageXObject {
            object_id: 0,
            width: metadata.width,
            height: metadata.height,
            color_space: metadata.color_space,
            bits_per_component: metadata.bits_per_component,
            data: image_data,
            filter: ImageFilter::FlateDecode,
        });
    }

    if bytes.starts_with(&[0xFF, 0xD8]) {
        return Ok(PdfImageXObject {
            object_id: 0,
            width: metadata.width,
            height: metadata.height,
            color_space: metadata.color_space,
            bits_per_component: metadata.bits_per_component,
            data: bytes.to_vec(),
            filter: ImageFilter::DCTDecode,
        });
    }

    Err(Diagnostic::new(
        Severity::Error,
        "unsupported image format for \\includegraphics",
    )
    .with_file(path.to_string())
    .with_suggestion("use a non-interlaced PNG or a baseline/progressive JPEG"))
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

fn load_opentype_font(
    file_access_gate: &dyn FileAccessGate,
    asset_bundle_path: Option<&Path>,
) -> Option<LoadedOpenTypeFont> {
    let bundle_path = asset_bundle_path?;

    for candidate in collect_ttf_candidates(bundle_path) {
        if file_access_gate.check_read(&candidate) == PathAccessDecision::Denied {
            tracing::warn!(
                path = %candidate.display(),
                "ttf access denied; falling back to other font paths"
            );
            continue;
        }

        let bytes = match file_access_gate.read_file(&candidate) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(
                    path = %candidate.display(),
                    %error,
                    "failed to read TTF font; falling back to other font paths"
                );
                continue;
            }
        };

        match OpenTypeFont::parse(&bytes) {
            Ok(font) => {
                let stem = candidate
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("FerritexOpenType");
                return Some(LoadedOpenTypeFont {
                    base_font: sanitize_pdf_font_name(stem),
                    font,
                });
            }
            Err(error) => {
                tracing::warn!(
                    path = %candidate.display(),
                    %error,
                    "failed to parse TTF font; falling back to other font paths"
                );
            }
        }
    }

    None
}

fn collect_ttf_candidates(bundle_path: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut visited = BTreeSet::new();

    for root in OPENTYPE_FONT_SEARCH_ROOTS {
        collect_ttf_candidates_in_dir(&bundle_path.join(root), &mut visited, &mut candidates);
    }
    collect_ttf_candidates_in_dir(bundle_path, &mut visited, &mut candidates);

    candidates
}

fn collect_ttf_candidates_in_dir(
    path: &Path,
    visited: &mut BTreeSet<PathBuf>,
    candidates: &mut Vec<PathBuf>,
) {
    let normalized = normalize_existing_path(path);
    if !visited.insert(normalized.clone()) {
        return;
    }

    if normalized.is_file() {
        if is_ttf_path(&normalized) {
            candidates.push(normalized);
        }
        return;
    }
    if !normalized.is_dir() {
        return;
    }

    let Ok(read_dir) = std::fs::read_dir(&normalized) else {
        return;
    };
    let mut entries = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();

    for entry in entries {
        if entry.is_dir() {
            collect_ttf_candidates_in_dir(&entry, visited, candidates);
        } else if is_ttf_path(&entry) {
            candidates.push(entry);
        }
    }
}

fn is_ttf_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("ttf"))
        .unwrap_or(false)
}

fn build_multi_font_pdf_resources(
    selection: &FontFamilySelection,
    document: &TypesetDocument,
    parallelism: usize,
    trace_font_tasks: bool,
) -> Vec<FontResource> {
    let mut used_characters = vec![BTreeMap::new(), BTreeMap::new(), BTreeMap::new()];
    let mut used_roles = [false; 3];

    for page in &document.pages {
        collect_used_characters_for_lines(&page.lines, &mut used_characters, &mut used_roles);
        for placement in &page.float_placements {
            collect_used_characters_for_lines(
                &placement.content.lines,
                &mut used_characters,
                &mut used_roles,
            );
        }
    }

    let Some(highest_used_role) = used_roles.iter().rposition(|used| *used) else {
        return Vec::new();
    };

    let mut resources = (0..=highest_used_role)
        .map(|role| {
            selection
                .font_for_role(role as u8)
                .is_none()
                .then(|| builtin_font_resource_for_role(role as u8))
        })
        .collect::<Vec<_>>();
    let mut tasks: Vec<Box<dyn FnOnce() -> (usize, Option<FontResource>) + Send>> = Vec::new();

    for role in 0..=highest_used_role {
        let Some(font) = selection.font_for_role(role as u8) else {
            continue;
        };
        let font = font.clone();
        let role_characters = used_characters[role].clone();
        tasks.push(Box::new(move || {
            (
                role,
                trace_font_task(
                    trace_font_tasks,
                    subset_task_id_for_role(role as u8),
                    &font.base_font,
                    role,
                    || build_opentype_font_resource(&font, &role_characters),
                ),
            )
        }));
    }

    for (role, resource) in run_font_tasks(parallelism, tasks) {
        resources[role] =
            Some(resource.unwrap_or_else(|| builtin_font_resource_for_role(role as u8)));
    }

    resources.into_iter().flatten().collect()
}

fn run_font_tasks<'a, T>(
    parallelism: usize,
    mut tasks: Vec<Box<dyn FnOnce() -> T + Send + 'a>>,
) -> Vec<T>
where
    T: Send + 'a,
{
    if tasks.len() <= 1 || parallelism <= 1 {
        return tasks.into_iter().map(|task| task()).collect();
    }

    let mut results = Vec::with_capacity(tasks.len());
    let concurrency = parallelism.max(1);

    while !tasks.is_empty() {
        let batch = tasks
            .drain(..concurrency.min(tasks.len()))
            .collect::<Vec<_>>();
        let batch_results = thread::scope(|scope| {
            let handles = batch
                .into_iter()
                .map(|task| scope.spawn(move || task()))
                .collect::<Vec<_>>();

            handles
                .into_iter()
                .map(|handle| handle.join().expect("font task thread panicked"))
                .collect::<Vec<_>>()
        });
        results.extend(batch_results);
    }

    results
}

fn subset_task_id_for_role(role: u8) -> &'static str {
    match role {
        1 => "font-subset-sans",
        2 => "font-subset-mono",
        _ => "font-subset-main",
    }
}

fn collect_used_characters_for_lines(
    lines: &[TextLine],
    used_characters: &mut [BTreeMap<u8, char>],
    used_roles: &mut [bool; 3],
) {
    for line in lines {
        let role = usize::from(line.font_index.min(2));
        used_roles[role] = true;
        for codepoint in line.text.chars() {
            let Ok(code) = u8::try_from(u32::from(codepoint)) else {
                continue;
            };
            used_characters[role].insert(code, codepoint);
        }
    }
}

fn build_opentype_font_resource(
    loaded_font: &LoadedOpenTypeFont,
    role_characters: &BTreeMap<u8, char>,
) -> Option<FontResource> {
    let mut used_characters = BTreeMap::new();
    let mut used_glyph_ids = BTreeSet::new();

    for (&code, &codepoint) in role_characters {
        let Some(glyph_id) = loaded_font.font.glyph_id(u32::from(codepoint)) else {
            continue;
        };
        used_characters.insert(code, codepoint);
        used_glyph_ids.insert(glyph_id);
    }

    let (&first_char, &last_char) = match (
        used_characters.keys().next(),
        used_characters.keys().next_back(),
    ) {
        (Some(first_char), Some(last_char)) => (first_char, last_char),
        _ => return None,
    };

    let widths = (first_char..=last_char)
        .map(|code| {
            let codepoint = char::from(code);
            loaded_font
                .font
                .glyph_id(u32::from(codepoint))
                .and_then(|glyph_id| loaded_font.font.advance_width(glyph_id))
                .map(|advance_width| {
                    u16::try_from(
                        i64::from(advance_width) * 1000
                            / i64::from(loaded_font.font.units_per_em()),
                    )
                    .expect("PDF width must fit in u16")
                })
                .unwrap_or(0)
        })
        .collect();
    let to_unicode_map = used_characters
        .into_iter()
        .map(|(code, codepoint)| (u16::from(code), codepoint))
        .collect();

    Some(FontResource::EmbeddedTrueType {
        base_font: format!("FerritexSubset+{}", loaded_font.base_font),
        font_data: loaded_font.font.subset(&used_glyph_ids),
        first_char,
        last_char,
        widths,
        bbox: loaded_font.font.bounding_box(),
        ascent: loaded_font.font.ascender(),
        descent: loaded_font.font.descender(),
        italic_angle: 0,
        stem_v: 80,
        cap_height: loaded_font.font.ascender(),
        units_per_em: loaded_font.font.units_per_em(),
        to_unicode_map: Some(to_unicode_map),
    })
}

fn builtin_font_resource_for_role(role: u8) -> FontResource {
    let base_font = match role {
        2 => "Courier",
        _ => "Helvetica",
    };
    FontResource::BuiltinType1 {
        base_font: base_font.to_string(),
    }
}

fn sanitize_pdf_font_name(value: &str) -> String {
    let sanitized = value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>();

    if sanitized.is_empty() {
        "FerritexOpenType".to_string()
    } else {
        sanitized
    }
}

fn trace_font_task<T, F>(
    enabled: bool,
    font_task_id: impl AsRef<str>,
    font_asset: impl AsRef<str>,
    worker_id: usize,
    operation: F,
) -> T
where
    F: FnOnce() -> T,
{
    if !enabled {
        return operation();
    }

    let started_at = trace_timestamp_micros();
    let result = operation();
    let finished_at = trace_timestamp_micros();
    emit_font_task_trace(
        font_task_id.as_ref(),
        font_asset.as_ref(),
        started_at,
        finished_at,
        worker_id,
    );
    result
}

fn emit_font_task_trace(
    font_task_id: &str,
    font_asset: &str,
    started_at: u64,
    finished_at: u64,
    worker_id: usize,
) {
    eprintln!(
        "{}",
        json!({
            "fontTaskId": font_task_id,
            "fontAsset": font_asset,
            "startedAt": started_at,
            "finishedAt": finished_at,
            "workerId": worker_id,
        })
    );
}

fn trace_timestamp_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .min(u128::from(u64::MAX)) as u64
}

#[allow(clippy::too_many_arguments)]
fn load_main_font_task(
    input_path: &str,
    requested_main_font: Option<&str>,
    input_dir: &Path,
    project_root: &Path,
    overlay_roots: &[PathBuf],
    asset_bundle_path: Option<&Path>,
    host_font_roots: &[PathBuf],
    file_access_gate: &dyn FileAccessGate,
    resolution_surface: &str,
    resolution_suggestion: &str,
    trace_font_tasks: bool,
    worker_id: usize,
) -> FontLoadTaskResult {
    if let Some(font_name) = requested_main_font {
        match trace_font_task(trace_font_tasks, "font-load-main", font_name, worker_id, || {
            resolve_named_font(
                font_name,
                input_dir,
                project_root,
                overlay_roots,
                asset_bundle_path,
                host_font_roots,
                file_access_gate,
            )
        }) {
            Some(resolved_font) => FontLoadTaskResult {
                loaded_font: Some(LoadedOpenTypeFont {
                    base_font: sanitize_pdf_font_name(&resolved_font.base_font_name),
                    font: resolved_font.font,
                }),
                diagnostic: None,
            },
            None => FontLoadTaskResult {
                loaded_font: trace_font_task(
                    trace_font_tasks,
                    "font-load-main-default",
                    default_font_asset_label(asset_bundle_path),
                    worker_id,
                    || load_opentype_font(file_access_gate, asset_bundle_path),
                ),
                diagnostic: Some(
                    Diagnostic::new(
                        Severity::Error,
                        format!("Font \"{font_name}\" not found in {resolution_surface}"),
                    )
                    .with_file(input_path.to_string())
                    .with_suggestion(format!(
                        "{resolution_suggestion}; compile will fall back to another available main font until then"
                    )),
                ),
            },
        }
    } else {
        FontLoadTaskResult {
            loaded_font: trace_font_task(
                trace_font_tasks,
                "font-load-main-default",
                default_font_asset_label(asset_bundle_path),
                worker_id,
                || load_opentype_font(file_access_gate, asset_bundle_path),
            ),
            diagnostic: None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn load_optional_font_task(
    input_path: &str,
    font_name: &str,
    input_dir: &Path,
    project_root: &Path,
    overlay_roots: &[PathBuf],
    asset_bundle_path: Option<&Path>,
    host_font_roots: &[PathBuf],
    file_access_gate: &dyn FileAccessGate,
    resolution_surface: &str,
    resolution_suggestion: &str,
    task_id: &'static str,
    fallback_message: &'static str,
    trace_font_tasks: bool,
    worker_id: usize,
) -> FontLoadTaskResult {
    let loaded_font = trace_font_task(trace_font_tasks, task_id, font_name, worker_id, || {
        resolve_named_font(
            font_name,
            input_dir,
            project_root,
            overlay_roots,
            asset_bundle_path,
            host_font_roots,
            file_access_gate,
        )
    })
    .map(|resolved_font| LoadedOpenTypeFont {
        base_font: sanitize_pdf_font_name(&resolved_font.base_font_name),
        font: resolved_font.font,
    });
    let diagnostic = loaded_font.is_none().then(|| {
        Diagnostic::new(
            Severity::Error,
            format!("Font \"{font_name}\" not found in {resolution_surface}"),
        )
        .with_file(input_path.to_string())
        .with_suggestion(format!("{resolution_suggestion}; {fallback_message}"))
    });

    FontLoadTaskResult {
        loaded_font,
        diagnostic,
    }
}

fn default_font_asset_label(asset_bundle_path: Option<&Path>) -> String {
    asset_bundle_path.map_or_else(
        || "asset-bundle:none".to_string(),
        |path| format!("asset-bundle:{}", path.display()),
    )
}

fn resolve_input_path(
    base_dir: &Path,
    workspace_root: &Path,
    overlay_roots: &[PathBuf],
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

    for overlay_root in overlay_roots {
        let overlay_candidate = tex_path_candidate(overlay_root, value);
        if overlay_candidate.exists() {
            return overlay_candidate;
        }
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

fn load_package_source(
    file_access_gate: &dyn FileAccessGate,
    asset_bundle_loader: &dyn AssetBundleLoaderPort,
    project_root: &Path,
    overlay_roots: &[PathBuf],
    asset_bundle_path: Option<&Path>,
    package_name: &str,
) -> Option<String> {
    let resolved_path = resolve_package_path(
        asset_bundle_loader,
        project_root,
        overlay_roots,
        asset_bundle_path,
        package_name,
    )?;
    if file_access_gate.check_read(&resolved_path) == PathAccessDecision::Denied {
        return None;
    }

    let bytes = file_access_gate.read_file(&resolved_path).ok()?;
    String::from_utf8(bytes).ok()
}

fn resolve_package_path(
    asset_bundle_loader: &dyn AssetBundleLoaderPort,
    project_root: &Path,
    overlay_roots: &[PathBuf],
    asset_bundle_path: Option<&Path>,
    package_name: &str,
) -> Option<PathBuf> {
    if let Some(candidate) = resolve_guarded_path(
        project_root,
        &project_root.join(format!("{package_name}.sty")),
    ) {
        return Some(candidate);
    }

    for overlay_root in overlay_roots {
        if let Some(candidate) = resolve_guarded_path(
            overlay_root,
            &overlay_root.join(format!("{package_name}.sty")),
        ) {
            return Some(candidate);
        }
    }

    if let Some(bundle_path) = asset_bundle_path {
        return asset_bundle_loader.resolve_package(bundle_path, package_name, None);
    }

    None
}

fn resolve_graphic_path(
    base_dir: &Path,
    workspace_root: &Path,
    overlay_roots: &[PathBuf],
    value: &str,
    asset_bundle_path: Option<&Path>,
) -> PathBuf {
    let candidate = graphic_path_candidate(base_dir, value);
    if candidate.exists() {
        return normalize_existing_path(&candidate);
    }

    let workspace_candidate = graphic_path_candidate(workspace_root, value);
    if workspace_candidate.exists() {
        return normalize_existing_path(&workspace_candidate);
    }

    for overlay_root in overlay_roots {
        let overlay_candidate = graphic_path_candidate(overlay_root, value);
        if overlay_candidate.exists() {
            return normalize_existing_path(&overlay_candidate);
        }
    }

    if let Some(bundle_path) = asset_bundle_path {
        let bundle_candidate = graphic_path_candidate(bundle_path, value);
        if bundle_candidate.exists() {
            return normalize_existing_path(&bundle_candidate);
        }
    }

    candidate
}

fn resolve_guarded_path(root: &Path, candidate: &Path) -> Option<PathBuf> {
    let resolved = candidate.canonicalize().ok()?;
    let root = root.canonicalize().ok()?;
    resolved.starts_with(&root).then_some(resolved)
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

fn graphic_path_candidate(base_dir: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn stable_id_for_path(path: &Path) -> StableId {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    StableId(hasher.finish())
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
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use super::{run_font_tasks, CompileJobService};
    use crate::ports::AssetBundleLoaderPort;
    use crate::runtime_options::{InteractionMode, RuntimeOptions, ShellEscapeMode};
    use ferritex_core::diagnostics::Severity;
    use ferritex_core::font::OpenTypeFont;
    use ferritex_core::policy::{FileAccessError, FileAccessGate, PathAccessDecision};
    use ferritex_core::synctex::SyncTexData;
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

    #[derive(Clone)]
    struct CountingFsTestFileAccessGate {
        read_counts: Arc<Mutex<BTreeMap<PathBuf, usize>>>,
    }

    impl CountingFsTestFileAccessGate {
        fn new() -> Self {
            Self {
                read_counts: Arc::new(Mutex::new(BTreeMap::new())),
            }
        }

        fn reset(&self) {
            self.read_counts.lock().expect("lock read counts").clear();
        }

        fn read_count(&self, path: &Path) -> usize {
            *self
                .read_counts
                .lock()
                .expect("lock read counts")
                .get(path)
                .unwrap_or(&0)
        }
    }

    impl FileAccessGate for CountingFsTestFileAccessGate {
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
            let mut counts = self.read_counts.lock().expect("lock read counts");
            *counts.entry(path.to_path_buf()).or_default() += 1;
            drop(counts);
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

    struct DelayedFontReadGate {
        delay: Duration,
        active_font_reads: AtomicUsize,
        max_concurrent_font_reads: AtomicUsize,
    }

    impl DelayedFontReadGate {
        fn new(delay: Duration) -> Self {
            Self {
                delay,
                active_font_reads: AtomicUsize::new(0),
                max_concurrent_font_reads: AtomicUsize::new(0),
            }
        }

        fn max_concurrent_font_reads(&self) -> usize {
            self.max_concurrent_font_reads.load(Ordering::SeqCst)
        }
    }

    impl FileAccessGate for DelayedFontReadGate {
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
            if matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("ttf" | "otf")
            ) {
                let active = self.active_font_reads.fetch_add(1, Ordering::SeqCst) + 1;
                record_peak(&self.max_concurrent_font_reads, active);
                thread::sleep(self.delay);
                let result = fs::read(path).map_err(FileAccessError::from);
                self.active_font_reads.fetch_sub(1, Ordering::SeqCst);
                return result;
            }

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
        package_paths: BTreeMap<String, PathBuf>,
    }

    impl MockAssetBundleLoader {
        fn valid() -> Self {
            Self {
                result: Ok(()),
                tex_inputs: BTreeMap::new(),
                package_paths: BTreeMap::new(),
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

        fn resolve_package(
            &self,
            _bundle_path: &Path,
            package_name: &str,
            _project_root: Option<&Path>,
        ) -> Option<PathBuf> {
            self.package_paths.get(package_name).cloned()
        }
    }

    fn runtime_options(input_file: PathBuf, output_dir: PathBuf) -> RuntimeOptions {
        RuntimeOptions {
            input_file,
            output_dir,
            jobname: "main".to_string(),
            parallelism: 1,
            overlay_roots: Vec::new(),
            no_cache: true,
            asset_bundle: None,
            host_font_fallback: false,
            host_font_roots: Vec::new(),
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
        String::from_utf8_lossy(&fs::read(path).expect("read output pdf")).into_owned()
    }

    fn read_synctex(path: &Path) -> SyncTexData {
        serde_json::from_slice(&fs::read(path).expect("read output synctex"))
            .expect("parse output synctex")
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

    fn build_test_ttf() -> Vec<u8> {
        let head = build_head_table(1000, 0, 1);
        let hhea = build_hhea_table(800, -200, 200, 4);
        let maxp = build_maxp_table(4);
        let hmtx = build_hmtx_table(&[(500, 0), (550, 0), (600, 0), (650, 0)], &[]);
        let cmap = build_cmap_table(
            3,
            1,
            &[
                TestCmapSegment {
                    start_code: 32,
                    end_code: 32,
                    id_delta: 0,
                    glyph_ids: &[1],
                },
                TestCmapSegment {
                    start_code: 65,
                    end_code: 66,
                    id_delta: 0,
                    glyph_ids: &[1, 2],
                },
            ],
        );
        let glyphs = build_default_glyphs(4);
        let (loca, glyf) = build_glyph_tables(&glyphs, 1);

        build_sfnt(
            0x0001_0000,
            &[
                (*b"head", head),
                (*b"hhea", hhea),
                (*b"maxp", maxp),
                (*b"hmtx", hmtx),
                (*b"cmap", cmap),
                (*b"loca", loca),
                (*b"glyf", glyf),
            ],
        )
    }

    fn build_head_table(units_per_em: u16, flags: u16, index_to_loc_format: i16) -> Vec<u8> {
        let mut data = vec![0; 54];
        write_u32(&mut data, 0, 0x0001_0000);
        write_u32(&mut data, 12, 0x5f0f_3cf5);
        write_u16(&mut data, 16, flags);
        write_u16(&mut data, 18, units_per_em);
        write_i16(&mut data, 36, -50);
        write_i16(&mut data, 38, -200);
        write_i16(&mut data, 40, 1000);
        write_i16(&mut data, 42, 800);
        write_i16(&mut data, 50, index_to_loc_format);
        data
    }

    fn build_hhea_table(
        ascender: i16,
        descender: i16,
        line_gap: i16,
        number_of_h_metrics: u16,
    ) -> Vec<u8> {
        let mut data = vec![0; 36];
        write_u32(&mut data, 0, 0x0001_0000);
        write_i16(&mut data, 4, ascender);
        write_i16(&mut data, 6, descender);
        write_i16(&mut data, 8, line_gap);
        write_u16(&mut data, 34, number_of_h_metrics);
        data
    }

    fn build_maxp_table(num_glyphs: u16) -> Vec<u8> {
        let mut data = vec![0; 6];
        write_u32(&mut data, 0, 0x0001_0000);
        write_u16(&mut data, 4, num_glyphs);
        data
    }

    fn build_hmtx_table(h_metrics: &[(u16, i16)], extra_lsbs: &[i16]) -> Vec<u8> {
        let mut data = Vec::with_capacity(h_metrics.len() * 4 + extra_lsbs.len() * 2);
        for (advance_width, lsb) in h_metrics {
            data.extend_from_slice(&advance_width.to_be_bytes());
            data.extend_from_slice(&lsb.to_be_bytes());
        }
        for lsb in extra_lsbs {
            data.extend_from_slice(&lsb.to_be_bytes());
        }
        data
    }

    fn build_default_glyphs(count: usize) -> Vec<Vec<u8>> {
        (0..count)
            .map(|index| {
                let mut glyph = vec![0; 10];
                write_i16(&mut glyph, 0, if index == 0 { 0 } else { 1 });
                write_i16(&mut glyph, 2, 0);
                write_i16(&mut glyph, 4, 0);
                write_i16(&mut glyph, 6, 50 + index as i16);
                write_i16(&mut glyph, 8, 100 + index as i16);
                glyph
            })
            .collect()
    }

    fn record_peak(peak: &AtomicUsize, value: usize) {
        let _ = peak.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            (value > current).then_some(value)
        });
    }

    fn build_glyph_tables(glyphs: &[Vec<u8>], index_to_loc_format: i16) -> (Vec<u8>, Vec<u8>) {
        let mut glyf = Vec::new();
        let mut offsets = Vec::with_capacity(glyphs.len() + 1);

        for glyph in glyphs {
            offsets.push(u32::try_from(glyf.len()).expect("glyf offset"));
            glyf.extend_from_slice(glyph);
            if glyf.len() % 2 != 0 {
                glyf.push(0);
            }
        }
        offsets.push(u32::try_from(glyf.len()).expect("glyf offset"));

        (build_loca_table(&offsets, index_to_loc_format), glyf)
    }

    fn build_loca_table(offsets: &[u32], index_to_loc_format: i16) -> Vec<u8> {
        match index_to_loc_format {
            0 => {
                let mut data = Vec::with_capacity(offsets.len() * 2);
                for offset in offsets {
                    data.extend_from_slice(
                        &u16::try_from(offset / 2)
                            .expect("short loca offset")
                            .to_be_bytes(),
                    );
                }
                data
            }
            1 => {
                let mut data = Vec::with_capacity(offsets.len() * 4);
                for offset in offsets {
                    data.extend_from_slice(&offset.to_be_bytes());
                }
                data
            }
            _ => panic!("unsupported indexToLocFormat"),
        }
    }

    fn build_cmap_table(
        platform_id: u16,
        encoding_id: u16,
        segments: &[TestCmapSegment<'_>],
    ) -> Vec<u8> {
        let format4 = build_cmap_format4(segments);

        let mut data = Vec::with_capacity(12 + format4.len());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&1u16.to_be_bytes());
        data.extend_from_slice(&platform_id.to_be_bytes());
        data.extend_from_slice(&encoding_id.to_be_bytes());
        data.extend_from_slice(&12u32.to_be_bytes());
        data.extend_from_slice(&format4);
        data
    }

    fn build_cmap_format4(segments: &[TestCmapSegment<'_>]) -> Vec<u8> {
        let mut all_segments = segments
            .iter()
            .map(|segment| TestCmapSegment {
                start_code: segment.start_code,
                end_code: segment.end_code,
                id_delta: segment.id_delta,
                glyph_ids: segment.glyph_ids,
            })
            .collect::<Vec<_>>();
        all_segments.push(TestCmapSegment {
            start_code: 0xffff,
            end_code: 0xffff,
            id_delta: 1,
            glyph_ids: &[],
        });

        let seg_count = all_segments.len();
        let mut end_codes = Vec::with_capacity(seg_count);
        let mut start_codes = Vec::with_capacity(seg_count);
        let mut id_deltas = Vec::with_capacity(seg_count);
        let mut id_range_offsets = Vec::with_capacity(seg_count);
        let mut glyph_id_array = Vec::new();

        for (index, segment) in all_segments.iter().enumerate() {
            if segment.glyph_ids.is_empty() {
                id_range_offsets.push(0u16);
            } else {
                let offset_words = seg_count - index + glyph_id_array.len();
                id_range_offsets.push(u16::try_from(offset_words * 2).expect("idRangeOffset"));
                glyph_id_array.extend_from_slice(segment.glyph_ids);
            }
            end_codes.push(segment.end_code);
            start_codes.push(segment.start_code);
            id_deltas.push(segment.id_delta);
        }

        let seg_count_x2 = u16::try_from(seg_count * 2).expect("segCountX2");
        let length = 16 + seg_count * 8 + glyph_id_array.len() * 2;
        let mut data = Vec::with_capacity(length);
        data.extend_from_slice(&4u16.to_be_bytes());
        data.extend_from_slice(&u16::try_from(length).expect("format4 length").to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&seg_count_x2.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());

        for value in end_codes {
            data.extend_from_slice(&value.to_be_bytes());
        }
        data.extend_from_slice(&0u16.to_be_bytes());
        for value in start_codes {
            data.extend_from_slice(&value.to_be_bytes());
        }
        for value in id_deltas {
            data.extend_from_slice(&value.to_be_bytes());
        }
        for value in id_range_offsets {
            data.extend_from_slice(&value.to_be_bytes());
        }
        for value in glyph_id_array {
            data.extend_from_slice(&value.to_be_bytes());
        }

        data
    }

    fn build_sfnt(sf_version: u32, tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
        let directory_len = 12 + tables.len() * 16;
        let mut offsets = Vec::with_capacity(tables.len());
        let mut next_offset = directory_len;
        for (_, table_data) in tables {
            next_offset = align_to_four(next_offset);
            offsets.push(next_offset);
            next_offset += align_to_four(table_data.len());
        }

        let mut data = Vec::with_capacity(next_offset);
        data.extend_from_slice(&sf_version.to_be_bytes());
        data.extend_from_slice(&(u16::try_from(tables.len()).expect("table count")).to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());

        for ((tag, table_data), offset) in tables.iter().zip(offsets.iter()) {
            data.extend_from_slice(tag);
            data.extend_from_slice(&0u32.to_be_bytes());
            data.extend_from_slice(&u32::try_from(*offset).expect("table offset").to_be_bytes());
            data.extend_from_slice(
                &u32::try_from(table_data.len())
                    .expect("table length")
                    .to_be_bytes(),
            );
        }

        let mut current_offset = directory_len;
        for ((_, table_data), offset) in tables.iter().zip(offsets.iter()) {
            while current_offset < *offset {
                data.push(0);
                current_offset += 1;
            }
            data.extend_from_slice(table_data);
            current_offset += table_data.len();
            while current_offset % 4 != 0 {
                data.push(0);
                current_offset += 1;
            }
        }

        data
    }

    fn align_to_four(value: usize) -> usize {
        (value + 3) & !3
    }

    fn write_u16(data: &mut [u8], offset: usize, value: u16) {
        data[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
    }

    fn write_i16(data: &mut [u8], offset: usize, value: i16) {
        data[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
    }

    fn write_u32(data: &mut [u8], offset: usize, value: u32) {
        data[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    #[derive(Clone, Copy)]
    struct TestCmapSegment<'a> {
        start_code: u16,
        end_code: u16,
        id_delta: i16,
        glyph_ids: &'a [u16],
    }

    #[test]
    fn returns_missing_input_diagnostic_for_nonexistent_file() {
        let dir = tempdir().expect("create tempdir");
        let options = runtime_options(dir.path().join("missing.tex"), dir.path().join("out"));
        let gate = MockFileAccessGate {
            read_decision: PathAccessDecision::Allowed,
            write_decision: PathAccessDecision::Allowed,
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
    fn compile_with_synctex_writes_trace_sidecar() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\begin{document}\nHello world\n\\end{document}\n",
        )
        .expect("write input");

        let mut options = runtime_options(input_file.clone(), dir.path().join("out"));
        options.synctex = true;
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let synctex_path = options.output_dir.join("main.synctex");
        assert!(synctex_path.exists());
        let synctex = read_synctex(&synctex_path);
        assert_eq!(
            synctex.files,
            vec![input_file
                .canonicalize()
                .expect("canonical input path")
                .to_string_lossy()
                .into_owned()]
        );
        assert!(!synctex.fragments.is_empty());
        let positions = synctex.forward_search(ferritex_core::kernel::api::SourceLocation {
            file_id: 0,
            line: 3,
            column: 1,
        });
        assert!(!positions.is_empty());
        assert_eq!(positions[0].page, 1);
    }

    #[test]
    fn reuses_cached_pdf_when_inputs_are_unchanged() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        fs::write(&input_file, document("Hello cache reuse")).expect("write input");

        let mut options = runtime_options(input_file, dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let first = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(first.exit_code, 0);
        let pdf_path = options.output_dir.join("main.pdf");
        let first_modified = fs::metadata(&pdf_path)
            .expect("pdf metadata")
            .modified()
            .expect("pdf modified time");
        assert!(options.output_dir.join(".ferritex-cache").exists());

        std::thread::sleep(Duration::from_millis(1100));

        let second = service(&FsTestFileAccessGate, &loader).compile(&options);
        let second_modified = fs::metadata(&pdf_path)
            .expect("pdf metadata")
            .modified()
            .expect("pdf modified time");

        assert_eq!(second.exit_code, 0);
        assert!(second.diagnostics.is_empty());
        assert_eq!(first_modified, second_modified);
    }

    #[test]
    fn invalid_compile_cache_falls_back_to_full_compile() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        fs::write(&input_file, document("Hello cache fallback")).expect("write input");

        let mut options = runtime_options(input_file, dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let first = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(first.exit_code, 0);
        let pdf_path = options.output_dir.join("main.pdf");
        let first_modified = fs::metadata(&pdf_path)
            .expect("pdf metadata")
            .modified()
            .expect("pdf modified time");

        let cache_file = fs::read_dir(options.output_dir.join(".ferritex-cache"))
            .expect("read cache dir")
            .map(|entry| entry.expect("cache entry").path())
            .next()
            .expect("cache metadata file");
        fs::write(&cache_file, b"{not json").expect("corrupt cache metadata");

        std::thread::sleep(Duration::from_millis(1100));

        let second = service(&FsTestFileAccessGate, &loader).compile(&options);
        let second_modified = fs::metadata(&pdf_path)
            .expect("pdf metadata")
            .modified()
            .expect("pdf modified time");

        let cache_diagnostic = second
            .diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic
                    .message
                    .contains("compile cache metadata is invalid")
            })
            .expect("cache fallback diagnostic");

        assert_eq!(second.exit_code, 0);
        assert_eq!(cache_diagnostic.severity, Severity::Info);
        assert!(second_modified > first_modified);
    }

    #[test]
    fn changed_dependency_reuses_cached_cross_reference_seed_and_matches_full_compile() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let chapter_file = dir.path().join("chapter.tex");
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\begin{document}\n\\tableofcontents\n\\input{chapter}\nSee Section \\ref{sec:intro}.\n\\end{document}\n",
        )
        .expect("write main input");
        fs::write(
            &chapter_file,
            "\\section{Intro}\\label{sec:intro}\nInitial paragraph.\n",
        )
        .expect("write chapter input");

        let mut options = runtime_options(input_file.clone(), dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let first = service(&FsTestFileAccessGate, &loader).compile(&options);
        let first_state = first
            .stable_compile_state
            .clone()
            .expect("first stable state");
        assert_eq!(first.exit_code, 0);
        assert!(
            first_state.snapshot.pass_number >= 2,
            "first compile should need at least one fixpoint pass",
        );

        fs::write(
            &chapter_file,
            "\\section{Intro}\\label{sec:intro}\nEdited paragraph after cache warmup.\n",
        )
        .expect("update chapter input");

        let second = service(&FsTestFileAccessGate, &loader).compile(&options);
        let second_state = second
            .stable_compile_state
            .clone()
            .expect("second stable state");
        assert_eq!(second.exit_code, 0);
        assert_eq!(second_state.snapshot.pass_number, 1);

        let incremental_pdf =
            fs::read(options.output_dir.join("main.pdf")).expect("read incremental pdf");
        let incremental_pdf_text = String::from_utf8_lossy(&incremental_pdf);
        assert!(incremental_pdf_text.contains("Edited paragraph after cache warmup."));
        assert!(incremental_pdf_text.contains("See Section 1."));

        let mut full_options = runtime_options(input_file, dir.path().join("out-full"));
        full_options.no_cache = true;
        let full = service(&FsTestFileAccessGate, &loader).compile(&full_options);
        assert_eq!(full.exit_code, 0);

        let full_pdf = fs::read(full_options.output_dir.join("main.pdf")).expect("read full pdf");
        assert_eq!(incremental_pdf, full_pdf);
    }

    #[test]
    fn changed_dependency_reuses_unaffected_cached_subtree() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let chapter_one = dir.path().join("chapter-one.tex");
        let chapter_two = dir.path().join("chapter-two.tex");
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\begin{document}\n\\input{chapter-one}\n\\input{chapter-two}\n\\end{document}\n",
        )
        .expect("write main input");
        fs::write(&chapter_one, "Chapter one before.\n").expect("write chapter one");
        fs::write(&chapter_two, "Chapter two stable.\n").expect("write chapter two");

        let mut options = runtime_options(input_file, dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();
        let gate = CountingFsTestFileAccessGate::new();
        let normalized_chapter_one = super::normalize_existing_path(&chapter_one);
        let normalized_chapter_two = super::normalize_existing_path(&chapter_two);

        let first = service(&gate, &loader).compile(&options);
        assert_eq!(first.exit_code, 0);

        gate.reset();
        fs::write(&chapter_one, "Chapter one after edit.\n").expect("update chapter one");

        let second = service(&gate, &loader).compile(&options);
        assert_eq!(second.exit_code, 0);
        assert!(gate.read_count(&normalized_chapter_one) >= 1);
        assert_eq!(
            gate.read_count(&normalized_chapter_two),
            1,
            "unchanged sibling subtree should only be touched during hash detection",
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
    fn stable_compile_state_persists_navigation_metadata_from_hypersetup() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\title{Visible Title}\n\\author{Visible Author}\n\\hypersetup{pdftitle={Persisted Title},pdfauthor={Persisted Author}}\n\\begin{document}\nHello\n\\end{document}\n",
        )
        .expect("write input");

        let options = runtime_options(input_file, dir.path().join("out"));
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let navigation = &result
            .stable_compile_state
            .as_ref()
            .expect("stable compile state")
            .document_state
            .navigation;
        assert_eq!(
            navigation.metadata.title.as_deref(),
            Some("Persisted Title")
        );
        assert_eq!(
            navigation.metadata.author.as_deref(),
            Some("Persisted Author")
        );
    }

    #[test]
    fn loads_bbl_from_project_root_and_renders_bibliography() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        fs::write(
            &input_file,
            document("See \\cite{key}.\n\\bibliography{refs}"),
        )
        .expect("write input");
        fs::write(
            dir.path().join("main.bbl"),
            "\\begin{thebibliography}{99}\n\\bibitem{key} Reference text\n\\end{thebibliography}\n",
        )
        .expect("write bbl");

        let options = runtime_options(input_file, dir.path().join("out"));
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        assert!(result.diagnostics.is_empty());
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("See [1]."));
        assert!(pdf.contains("[1] Reference text"));
        assert_eq!(
            result
                .stable_compile_state
                .as_ref()
                .and_then(|state| state
                    .document_state
                    .bibliography_state
                    .resolve_citation("key"))
                .map(|citation| citation.formatted_text.as_str()),
            Some("1")
        );
    }

    #[test]
    fn loads_bbl_from_overlay_root_and_renders_bibliography() {
        let dir = tempdir().expect("create tempdir");
        let overlay_root = dir.path().join("overlay");
        let input_file = dir.path().join("main.tex");
        fs::create_dir_all(&overlay_root).expect("create overlay root");
        fs::write(
            &input_file,
            document("See \\cite{key}.\n\\bibliography{refs}"),
        )
        .expect("write input");
        fs::write(
            overlay_root.join("main.bbl"),
            "\\begin{thebibliography}{99}\n\\bibitem{key} Overlay reference\n\\end{thebibliography}\n",
        )
        .expect("write overlay bbl");

        let mut options = runtime_options(input_file, dir.path().join("out"));
        options.overlay_roots = vec![overlay_root];
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        assert!(result.diagnostics.is_empty());
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("See [1]."));
        assert!(pdf.contains("[1] Overlay reference"));
    }

    #[test]
    fn stale_bbl_emits_warning_when_bib_is_newer() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let bbl_path = dir.path().join("main.bbl");
        let bib_path = dir.path().join("refs.bib");
        fs::write(
            &input_file,
            document("See \\cite{key}.\n\\bibliography{refs}"),
        )
        .expect("write input");
        fs::write(
            &bbl_path,
            "\\begin{thebibliography}{99}\n\\bibitem{key} Reference text\n\\end{thebibliography}\n",
        )
        .expect("write bbl");
        std::thread::sleep(Duration::from_millis(1100));
        fs::write(&bib_path, "@book{key,\n  title = {Reference text}\n}\n").expect("write bib");

        let options = runtime_options(input_file, dir.path().join("out"));
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 1);
        let stale = result
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message == "bibliography .bbl file appears stale")
            .expect("stale bibliography diagnostic");
        assert_eq!(stale.severity, Severity::Warning);
        assert!(stale
            .context
            .as_deref()
            .unwrap_or_default()
            .contains("refs.bib"));
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("See [1]."));
        assert!(pdf.contains("[1] Reference text"));
    }

    #[test]
    fn missing_bbl_emits_warning_when_citations_are_present() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        fs::write(&input_file, document("See \\cite{missing}.")).expect("write input");

        let options = runtime_options(input_file, dir.path().join("out"));
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 1);
        assert!(result.output_pdf.is_some());
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].severity, Severity::Warning);
        assert_eq!(
            result.diagnostics[0].message,
            "bibliography .bbl file not found"
        );
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("See [?]."));
    }

    #[test]
    fn compile_uses_third_pass_to_resolve_pageref() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        fs::write(
            &input_file,
            document(&format!(
                "See page \\pageref{{sec:later}}.\n\\newpage\n\\section{{Later}}\\label{{sec:later}}\nDone."
            )),
        )
        .expect("write input");

        let options = runtime_options(input_file, dir.path().join("out"));
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("See page 2."));
        assert!(pdf.contains("1 Later"));
        assert!(!pdf.contains("??"));
        assert_eq!(
            result
                .stable_compile_state
                .as_ref()
                .map(|state| state.snapshot.pass_number),
            Some(3)
        );
    }

    #[test]
    fn compile_resolves_index_entries_on_second_pass() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\makeindex\n\\begin{document}\nAlpha\\index{Alpha}\n\\newpage\nBeta\\index{beta@Beta}\n\\printindex\n\\end{document}\n",
        )
        .expect("write input");

        let options = runtime_options(input_file, dir.path().join("out"));
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("Alpha . . . . 1"));
        assert!(pdf.contains("Beta . . . . 2"));
        let stable_state = result
            .stable_compile_state
            .as_ref()
            .expect("stable compile state");
        assert_eq!(stable_state.snapshot.pass_number, 2);
        assert!(stable_state.document_state.index_state.enabled);
        assert_eq!(stable_state.document_state.index_state.entries.len(), 2);
        assert_eq!(
            stable_state.document_state.index_state.entries[0].page,
            Some(1)
        );
        assert_eq!(
            stable_state.document_state.index_state.entries[1].page,
            Some(2)
        );
    }

    #[test]
    fn embeds_truetype_font_with_tounicode_when_asset_bundle_contains_ttf() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        let bundle_path = dir.path().join("bundle");
        let font_dir = bundle_path.join("texmf/fonts/truetype/public/test");
        fs::create_dir_all(&font_dir).expect("create font dir");
        fs::write(bundle_path.join("manifest.json"), "{}").expect("write manifest");
        fs::write(&input_file, document("AB")).expect("write input");
        let font_bytes = build_test_ttf();
        fs::write(font_dir.join("TestSans.ttf"), &font_bytes).expect("write font");

        let mut options = runtime_options(input_file.clone(), output_dir.clone());
        options.asset_bundle = Some(bundle_path);
        let loader = MockAssetBundleLoader::valid();
        let mut used_glyphs = BTreeSet::new();
        used_glyphs.insert(1);
        used_glyphs.insert(2);
        let subset_len = OpenTypeFont::parse(&font_bytes)
            .expect("parse font")
            .subset(&used_glyphs)
            .len();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        assert!(result.diagnostics.is_empty());
        let pdf = read_pdf(&output_dir.join("main.pdf"));
        assert!(pdf.contains("/Subtype /TrueType"));
        assert!(pdf.contains("/ToUnicode"));
        assert!(pdf.contains("/FontFile2"));
        assert!(pdf.contains("/CMapName /Adobe-Identity-UCS"));
        assert!(!pdf.contains("/BaseFont /Helvetica"));
        assert!(subset_len < font_bytes.len());
        assert!(pdf.contains(&format!("/Length1 {subset_len}")));
    }

    #[test]
    fn compile_with_setmainfont_uses_named_font() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        let bundle_path = dir.path().join("bundle");
        let font_dir = bundle_path.join("texmf/fonts/truetype/public/test");
        fs::create_dir_all(&font_dir).expect("create font dir");
        fs::write(bundle_path.join("manifest.json"), "{}").expect("write manifest");
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmainfont{ChosenSans}\n\\begin{document}\nAB\n\\end{document}\n",
        )
        .expect("write input");
        fs::write(font_dir.join("AFirst.ttf"), build_test_ttf()).expect("write first font");
        fs::write(font_dir.join("ChosenSans.ttf"), build_test_ttf()).expect("write chosen font");

        let mut options = runtime_options(input_file, output_dir.clone());
        options.asset_bundle = Some(bundle_path);
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        assert!(result.diagnostics.is_empty());
        let pdf = read_pdf(&output_dir.join("main.pdf"));
        assert!(pdf.contains("FerritexSubset+ChosenSans"));
        assert!(!pdf.contains("FerritexSubset+AFirst"));
    }

    #[test]
    fn compile_with_setmainfont_uses_overlay_root_font() {
        let dir = tempdir().expect("create tempdir");
        let overlay_root = dir.path().join("overlay");
        let input_file = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(overlay_root.join("fonts")).expect("create overlay font dir");
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmainfont{OverlaySans}\n\\begin{document}\nAB\n\\end{document}\n",
        )
        .expect("write input");
        fs::write(overlay_root.join("fonts/OverlaySans.ttf"), build_test_ttf())
            .expect("write overlay font");

        let mut options = runtime_options(input_file, output_dir.clone());
        options.overlay_roots = vec![overlay_root];
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        assert!(result.diagnostics.is_empty());
        let pdf = read_pdf(&output_dir.join("main.pdf"));
        assert!(pdf.contains("FerritexSubset+OverlaySans"));
    }

    #[test]
    fn compile_with_font_family_roles_embeds_multiple_font_resources() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        let bundle_path = dir.path().join("bundle");
        let font_dir = bundle_path.join("texmf/fonts/truetype/public/test");
        fs::create_dir_all(&font_dir).expect("create font dir");
        fs::write(bundle_path.join("manifest.json"), "{}").expect("write manifest");
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmainfont{MainFace}\n\\setsansfont{SansFace}\n\\setmonofont{MonoFace}\n\\begin{document}\nAB\\par\n\\textsf{AB}\\par\n\\texttt{AB}\n\\end{document}\n",
        )
        .expect("write input");
        fs::write(font_dir.join("MainFace.ttf"), build_test_ttf()).expect("write main font");
        fs::write(font_dir.join("SansFace.ttf"), build_test_ttf()).expect("write sans font");
        fs::write(font_dir.join("MonoFace.ttf"), build_test_ttf()).expect("write mono font");

        let mut options = runtime_options(input_file, output_dir.clone());
        options.asset_bundle = Some(bundle_path);
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        assert!(result.diagnostics.is_empty());
        let pdf = read_pdf(&output_dir.join("main.pdf"));
        assert!(pdf.contains("FerritexSubset+MainFace"));
        assert!(pdf.contains("FerritexSubset+SansFace"));
        assert!(pdf.contains("FerritexSubset+MonoFace"));
        assert!(pdf.contains("/F1 "));
        assert!(pdf.contains("/F2 "));
        assert!(pdf.contains("/F3 "));
        assert!(pdf.contains("/F2 12 Tf"));
        assert!(pdf.contains("/F3 12 Tf"));
    }

    #[test]
    fn run_font_tasks_parallelizes_up_to_requested_parallelism() {
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let tasks: Vec<Box<dyn FnOnce() -> usize + Send>> = (0..3)
            .map(|index| {
                let active_ref = &active;
                let peak_ref = &peak;
                Box::new(move || {
                    let in_flight = active_ref.fetch_add(1, Ordering::SeqCst) + 1;
                    record_peak(peak_ref, in_flight);
                    thread::sleep(Duration::from_millis(30));
                    active_ref.fetch_sub(1, Ordering::SeqCst);
                    index as usize
                }) as Box<dyn FnOnce() -> usize + Send>
            })
            .collect();

        let results = run_font_tasks(2, tasks);

        assert_eq!(results, vec![0, 1, 2]);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn compile_parallelizes_independent_font_loads_when_jobs_exceeds_one() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        let bundle_path = dir.path().join("bundle");
        let font_dir = bundle_path.join("texmf/fonts/truetype/public/test");
        fs::create_dir_all(&font_dir).expect("create font dir");
        fs::write(bundle_path.join("manifest.json"), "{}").expect("write manifest");
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmainfont{MainFace}\n\\setsansfont{SansFace}\n\\setmonofont{MonoFace}\n\\begin{document}\nAB\\par\n\\textsf{AB}\\par\n\\texttt{AB}\n\\end{document}\n",
        )
        .expect("write input");
        fs::write(font_dir.join("MainFace.ttf"), build_test_ttf()).expect("write main font");
        fs::write(font_dir.join("SansFace.ttf"), build_test_ttf()).expect("write sans font");
        fs::write(font_dir.join("MonoFace.ttf"), build_test_ttf()).expect("write mono font");

        let mut options = runtime_options(input_file, output_dir.clone());
        options.asset_bundle = Some(bundle_path);
        options.parallelism = 4;
        let loader = MockAssetBundleLoader::valid();
        let gate = DelayedFontReadGate::new(Duration::from_millis(40));

        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        assert!(result.diagnostics.is_empty());
        assert!(gate.max_concurrent_font_reads() >= 2);
        let pdf = read_pdf(&output_dir.join("main.pdf"));
        assert!(pdf.contains("FerritexSubset+MainFace"));
        assert!(pdf.contains("FerritexSubset+SansFace"));
        assert!(pdf.contains("FerritexSubset+MonoFace"));
    }

    #[test]
    fn basic_mode_provides_builtin_font_slots_for_sans_and_mono_lines() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\begin{document}\nMain\\par\n\\textsf{Sans}\\par\n\\texttt{Mono}\n\\end{document}\n",
        )
        .expect("write input");
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader)
            .compile(&runtime_options(input_file, output_dir.clone()));

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&output_dir.join("main.pdf"));
        assert!(pdf.contains("/BaseFont /Helvetica"));
        assert!(pdf.contains("/BaseFont /Courier"));
        assert!(pdf.contains("/F2 12 Tf"));
        assert!(pdf.contains("/F3 12 Tf"));
    }

    #[test]
    fn tfm_mode_provides_builtin_font_slots_for_sans_and_mono_lines() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        let bundle_path = dir.path().join("bundle");
        let tfm_path = bundle_path.join("texmf/fonts/tfm/public/cm/cmr10.tfm");
        fs::create_dir_all(
            tfm_path
                .parent()
                .expect("cmr10.tfm path should have a parent directory"),
        )
        .expect("create tfm dir");
        fs::write(bundle_path.join("manifest.json"), "{}").expect("write manifest");
        fs::write(&tfm_path, build_test_tfm()).expect("write tfm");
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\begin{document}\nMain\\par\n\\textsf{Sans}\\par\n\\texttt{Mono}\n\\end{document}\n",
        )
        .expect("write input");
        let mut options = runtime_options(input_file, output_dir.clone());
        options.asset_bundle = Some(bundle_path);
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&output_dir.join("main.pdf"));
        assert!(pdf.contains("/BaseFont /Helvetica"));
        assert!(pdf.contains("/BaseFont /Courier"));
        assert!(pdf.contains("/F2 12 Tf"));
        assert!(pdf.contains("/F3 12 Tf"));
    }

    #[test]
    fn compile_with_setmainfont_not_found_emits_diagnostic() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        let bundle_path = dir.path().join("bundle");
        let font_dir = bundle_path.join("texmf/fonts/truetype/public/test");
        fs::create_dir_all(&font_dir).expect("create font dir");
        fs::write(bundle_path.join("manifest.json"), "{}").expect("write manifest");
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmainfont{MissingFont}\n\\begin{document}\nAB\n\\end{document}\n",
        )
        .expect("write input");
        fs::write(font_dir.join("FallbackSans.ttf"), build_test_ttf())
            .expect("write fallback font");

        let mut options = runtime_options(input_file, output_dir.clone());
        options.asset_bundle = Some(bundle_path);
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert!(result.output_pdf.is_some());
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.message
                    == "Font \"MissingFont\" not found in project directory, overlay roots, or asset bundle"
        }));
        let pdf = read_pdf(&output_dir.join("main.pdf"));
        assert!(pdf.contains("FerritexSubset+FallbackSans"));
    }

    #[test]
    fn compile_with_setmainfont_uses_host_font_catalog_fallback() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        let host_font_root = dir.path().join("host-fonts");
        fs::create_dir_all(&host_font_root).expect("create host font root");
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmainfont{Noto Serif}\n\\begin{document}\nAB\n\\end{document}\n",
        )
        .expect("write input");
        fs::write(host_font_root.join("Noto Serif.ttf"), build_test_ttf())
            .expect("write host font");

        let mut options = runtime_options(input_file, output_dir.clone());
        options.host_font_fallback = true;
        options.host_font_roots = vec![host_font_root];
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        assert!(result.diagnostics.is_empty());
        let pdf = read_pdf(&output_dir.join("main.pdf"));
        assert!(pdf.contains("FerritexSubset+NotoSerif"));
    }

    #[test]
    fn reproducible_mode_disables_host_font_catalog_fallback() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        let host_font_root = dir.path().join("host-fonts");
        fs::create_dir_all(&host_font_root).expect("create host font root");
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmainfont{Noto Serif}\n\\begin{document}\nAB\n\\end{document}\n",
        )
        .expect("write input");
        fs::write(host_font_root.join("Noto Serif.ttf"), build_test_ttf())
            .expect("write host font");

        let mut options = runtime_options(input_file, output_dir.clone());
        options.host_font_fallback = false;
        options.host_font_roots = vec![host_font_root];
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert!(result.output_pdf.is_some());
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.message
                    == "Font \"Noto Serif\" not found in project directory, overlay roots, or asset bundle"
        }));
        let pdf = read_pdf(&output_dir.join("main.pdf"));
        assert!(pdf.contains("/Subtype /Type1 /BaseFont /Helvetica"));
        assert!(!pdf.contains("FerritexSubset+NotoSerif"));
    }

    #[test]
    fn compile_without_fontspec_uses_first_ttf_behavior() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        let bundle_path = dir.path().join("bundle");
        let font_dir = bundle_path.join("texmf/fonts/truetype/public/test");
        fs::create_dir_all(&font_dir).expect("create font dir");
        fs::write(bundle_path.join("manifest.json"), "{}").expect("write manifest");
        fs::write(&input_file, document("AB")).expect("write input");
        fs::write(font_dir.join("AFirst.ttf"), build_test_ttf()).expect("write first font");
        fs::write(font_dir.join("ChosenSans.ttf"), build_test_ttf()).expect("write second font");

        let mut options = runtime_options(input_file, output_dir.clone());
        options.asset_bundle = Some(bundle_path);
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        assert!(result.diagnostics.is_empty());
        let pdf = read_pdf(&output_dir.join("main.pdf"));
        assert!(pdf.contains("FerritexSubset+AFirst"));
        assert!(!pdf.contains("FerritexSubset+ChosenSans"));
    }

    #[test]
    fn falls_back_to_helvetica_when_asset_bundle_has_no_ttf() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        let bundle_path = dir.path().join("bundle");
        fs::create_dir_all(&bundle_path).expect("create bundle dir");
        fs::write(bundle_path.join("manifest.json"), "{}").expect("write manifest");
        fs::write(&input_file, document("Hello")).expect("write input");

        let mut options = runtime_options(input_file, output_dir.clone());
        options.asset_bundle = Some(bundle_path);
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&output_dir.join("main.pdf"));
        assert!(pdf.contains("/Subtype /Type1 /BaseFont /Helvetica"));
        assert!(!pdf.contains("/Subtype /TrueType"));
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
    fn compile_from_source_persists_navigation_metadata_from_hypersetup() {
        let gate = MockFileAccessGate {
            read_decision: PathAccessDecision::Allowed,
            write_decision: PathAccessDecision::Allowed,
            read_result: MockReadResult::Success(Vec::new()),
            created_dirs: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
        };
        let loader = MockAssetBundleLoader::valid();

        let state = service(&gate, &loader).compile_from_source(
            "\\documentclass{article}\n\\title{Visible Title}\n\\author{Visible Author}\n\\hypersetup{pdftitle={Source Title},pdfauthor={Source Author}}\n\\begin{document}\nHello\n\\end{document}\n",
            "file:///tmp/main.tex",
        );

        assert!(state.success);
        assert!(state.diagnostics.is_empty());
        assert_eq!(
            state.document_state.navigation.metadata.title.as_deref(),
            Some("Source Title")
        );
        assert_eq!(
            state.document_state.navigation.metadata.author.as_deref(),
            Some("Source Author")
        );
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
    fn compile_from_source_treats_unclosed_equation_as_recoverable_when_document_end_exists() {
        let gate = MockFileAccessGate {
            read_decision: PathAccessDecision::Allowed,
            write_decision: PathAccessDecision::Allowed,
            read_result: MockReadResult::Success(Vec::new()),
            created_dirs: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
        };
        let loader = MockAssetBundleLoader::valid();

        let state = service(&gate, &loader).compile_from_source(
            "\\documentclass{article}\n\\begin{document}\n\\begin{equation}\na=b\n\\end{document}\n",
            "file:///tmp/main.tex",
        );

        assert!(state.success);
        let messages = state
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();
        assert!(messages
            .iter()
            .any(|message| message.contains("unclosed environment `equation`")));
        assert!(!messages
            .iter()
            .any(|message| message.contains("missing \\end{document}")));
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
            package_paths: BTreeMap::new(),
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
    fn overlay_root_fallback_resolves_when_project_root_misses_input() {
        let dir = tempdir().expect("create tempdir");
        let project_root = dir.path().join("project");
        let overlay_root = dir.path().join("overlay");
        let src = project_root.join("src");
        let subdir = src.join("subdir");
        fs::create_dir_all(project_root.join(".git")).expect("create git marker");
        fs::create_dir_all(overlay_root.join("shared")).expect("create overlay shared dir");
        fs::create_dir_all(&subdir).expect("create subdir");
        fs::write(
            overlay_root.join("shared/macros.tex"),
            "OVERLAY ROOT MACROS\n",
        )
        .expect("write overlay macros");
        fs::write(src.join("main.tex"), document("\\input{subdir/section}")).expect("write main");
        fs::write(subdir.join("section.tex"), "\\input{shared/macros}\n").expect("write section");

        let mut options = runtime_options(src.join("main.tex"), project_root.join("out"));
        options.overlay_roots = vec![overlay_root];
        let gate = FsTestFileAccessGate;
        let loader = MockAssetBundleLoader::valid();

        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("OVERLAY ROOT MACROS"));
    }

    #[test]
    fn usepackage_loads_project_local_sty_and_recurses_requirepackage() {
        let dir = tempdir().expect("create tempdir");
        let project_root = dir.path().join("project");
        fs::create_dir_all(&project_root).expect("create project root");
        fs::write(
            project_root.join("mypkg.sty"),
            "\\NeedsTeXFormat{LaTeX2e}\n\
             \\ProvidesPackage{mypkg}[2024/01/01 Test package]\n\
             \\RequirePackage{amsmath}\n\
             \\DeclareOption{bold}{\\def\\mypkgstyle{bold}}\n\
             \\DeclareOption*{}\n\
             \\ProcessOptions*\n\
             \\newcommand{\\mypkgcmd}[1]{[#1]}\n\
             \\newenvironment{mypkgenv}{\\begin{center}}{\\end{center}}\n",
        )
        .expect("write package");

        let source = "\\documentclass{article}\n\
                      \\usepackage[bold]{mypkg}\n\
                      \\usepackage{mypkg}\n\
                      \\begin{document}\n\
                      \\mypkgcmd{ok}\n\
                      \\end{document}\n";
        let gate = FsTestFileAccessGate;
        let loader = MockAssetBundleLoader::valid();
        let compile_service = service(&gate, &loader);
        let parse_result = compile_service.parse_document_with_cross_references(
            source,
            &project_root,
            &[],
            None,
            None,
            Vec::new(),
            None,
            |document| compile_service.typesetter.typeset(document),
        );
        let document = parse_result
            .output
            .document
            .expect("document should parse with project-local package");

        assert!(
            parse_result.output.errors.is_empty(),
            "{:?}",
            parse_result.output.errors
        );
        assert_eq!(parse_result.pass_count, 1);
        assert!(document.body.contains("[ok]"));
        assert!(document
            .loaded_packages
            .iter()
            .any(|package| package.name == "mypkg"));
        assert!(document
            .loaded_packages
            .iter()
            .any(|package| package.name == "amsmath"));
        assert_eq!(
            document
                .loaded_packages
                .iter()
                .filter(|package| package.name == "mypkg")
                .count(),
            1
        );
    }

    #[test]
    fn usepackage_loads_overlay_root_sty_and_recurses_requirepackage() {
        let dir = tempdir().expect("create tempdir");
        let project_root = dir.path().join("project");
        let overlay_root = dir.path().join("overlay");
        fs::create_dir_all(&project_root).expect("create project root");
        fs::create_dir_all(&overlay_root).expect("create overlay root");
        fs::write(
            overlay_root.join("overlaypkg.sty"),
            "\\NeedsTeXFormat{LaTeX2e}\n\
             \\ProvidesPackage{overlaypkg}[2024/01/01 Overlay package]\n\
             \\RequirePackage{amsmath}\n\
             \\newcommand{\\overlaycmd}[1]{<#1>}\n",
        )
        .expect("write overlay package");

        let source = "\\documentclass{article}\n\
                      \\usepackage{overlaypkg}\n\
                      \\begin{document}\n\
                      \\overlaycmd{ok}\n\
                      \\end{document}\n";
        let gate = FsTestFileAccessGate;
        let loader = MockAssetBundleLoader::valid();
        let compile_service = service(&gate, &loader);
        let parse_result = compile_service.parse_document_with_cross_references(
            source,
            &project_root,
            &[overlay_root],
            None,
            None,
            Vec::new(),
            None,
            |document| compile_service.typesetter.typeset(document),
        );
        let document = parse_result
            .output
            .document
            .expect("document should parse with overlay package");

        assert!(
            parse_result.output.errors.is_empty(),
            "{:?}",
            parse_result.output.errors
        );
        assert!(document.body.contains("<ok>"));
        assert!(document
            .loaded_packages
            .iter()
            .any(|package| package.name == "overlaypkg"));
        assert!(document
            .loaded_packages
            .iter()
            .any(|package| package.name == "amsmath"));
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
            package_paths: BTreeMap::new(),
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
            package_paths: BTreeMap::new(),
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
