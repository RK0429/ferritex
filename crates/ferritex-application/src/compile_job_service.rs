#![allow(
    clippy::large_enum_variant,
    clippy::needless_range_loop,
    clippy::redundant_closure,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use ferritex_core::assets::{AssetHandle, LogicalAssetId};
use ferritex_core::bibliography::api::{
    parse_bbl, BibliographyDiagnostic, BibliographyInputFingerprint, BibliographyState,
    BibliographyToolchain,
};
use ferritex_core::compilation::{
    CompilationJob, CompilationSnapshot, DestinationAnchor, DocumentPartitionPlan, DocumentState,
    DocumentWorkUnit, IndexEntry, LinkStyle, NavigationState, OutlineDraftEntry, PartitionKind,
    PdfMetadataDraft, SectionOutlineEntry, SymbolLocation,
};
use ferritex_core::diagnostics::{Diagnostic, Severity};
use ferritex_core::font::api::OpenTypeWidthProvider;
use ferritex_core::font::{
    resolve_named_font, OpenTypeFont, ResolvedFont, TfmMetrics, Type1Font,
    OPENTYPE_FONT_SEARCH_ROOTS,
};
use ferritex_core::graphics::api::{
    extract_png_image_data, is_pdf_signature, parse_image_metadata, parse_pdf_metadata,
    ExternalGraphic, GraphicAssetResolver, GraphicNode, ImageMetadata, PdfGraphic,
    PdfGraphicMetadata, ResolvedGraphic,
};
use ferritex_core::incremental::{DependencyGraph, DocumentPartitionPlanner, RecompilationScope};
use ferritex_core::kernel::api::{DimensionValue, SourceLocation, SourceSpan};
use ferritex_core::kernel::StableId;
use ferritex_core::parser::api::{DocumentNode, FloatType};
use ferritex_core::parser::{
    MinimalLatexParser, ParseError, ParseOutput, ParsedDocument, RegisterStore,
};
use ferritex_core::pdf::{
    page_partition_ids_for_plan, FontResource, ImageFilter, PageRenderPayload, PdfFormXObject,
    PdfImageXObject, PdfRenderer, PlacedFormXObject, PlacedImage,
};
use ferritex_core::policy::{ExecutionPolicy, OutputArtifactRegistry, PreviewPublicationPolicy};
use ferritex_core::policy::{
    FileAccessError, FileAccessGate, FileOperationHandler, FileOperationResult, PathAccessDecision,
    ShellEscapeHandler, ShellEscapeResult,
};
use ferritex_core::synctex::{
    PlacedTextNode, RenderedLineTrace, RenderedPageTrace, SourceLineTrace, SyncTexData,
};
use ferritex_core::typesetting::{
    append_footnotes_to_pages, append_footnotes_to_pages_starting_at,
    finalize_partitioned_typeset_document, paginate_vlist_continuing_detailed,
    paginate_vlist_continuing_with_state, resolve_page_labels, snapshot_paginated_vlist_state,
    DocumentLayoutFragment, FixedWidthProvider, FloatCounters, FloatItem, FloatQueue,
    MinimalTypesetter, PaginationMergeCoordinator, PartitionVListResult, RawBlockCheckpoint,
    TextLine, TfmWidthProvider, TypesetDocument, TypesetPage, TypesetterReusePlan,
};
use serde_json::json;

use crate::compile_cache::{
    fingerprint_bytes, BackgroundCacheWriter, BlockCheckpoint, BlockCheckpointData,
    BlockLayoutState, CachedPagePayload, CachedSourceSubtree, CachedTypesetFragment, CompileCache,
    PendingFloat, WarmPartitionCache,
};
use crate::execution_policy_factory::ExecutionPolicyFactory;
use crate::ports::{AssetBundleLoaderPort, ShellCommandGatewayPort};
use crate::runtime_options::{default_lsp_asset_bundle, RuntimeOptions};
use crate::stable_compile_state::{
    CrossReferenceCaptionEntry, CrossReferenceSectionEntry, CrossReferenceSeed, StableCompileState,
};

const DEFAULT_TFM_FALLBACK_WIDTH: DimensionValue = DimensionValue(65_536);
const SCALED_POINTS_PER_POINT: i64 = 65_536;
const FONT_CACHE_HIT_TASK_ID: &str = "font-load-cache-hit";
const FONT_CACHE_HIT_ASSET_SENTINEL: &str = "builtin:font-cache-hit";
const CMR10_TFM_CANDIDATES: [&str; 4] = [
    "texmf/fonts/tfm/public/cm/cmr10.tfm",
    "fonts/tfm/public/cm/cmr10.tfm",
    "texmf/cmr10.tfm",
    "cmr10.tfm",
];
const CMBX12_TFM_CANDIDATES: [&str; 4] = [
    "texmf/fonts/tfm/public/cm/cmbx12.tfm",
    "fonts/tfm/public/cm/cmbx12.tfm",
    "texmf/cmbx12.tfm",
    "cmbx12.tfm",
];
const CM_PFB_CANDIDATE_TEMPLATES: [&str; 4] = [
    "texmf/fonts/type1/public/amsfonts/cm/{name}.pfb",
    "fonts/type1/public/amsfonts/cm/{name}.pfb",
    "texmf/fonts/type1/public/cm/{name}.pfb",
    "texmf/{name}.pfb",
];
const PDF_TYPE1_DEFAULT_FLAGS: u32 = 6;

#[cfg(test)]
static FORCE_PARALLEL_FULL_TYPESET_COLLISION: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileResult {
    pub diagnostics: Vec<Diagnostic>,
    pub exit_code: i32,
    pub output_pdf: Option<PathBuf>,
    pub stable_compile_state: Option<StableCompileState>,
    pub stage_timing: StageTiming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PartitionTypesetReuseType {
    BlockReuse,
    SuffixRebuild,
    FullRebuild,
    Cached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionTypesetFallbackReason {
    Pageref,
    TypesetCallbackCount,
    ReusePlanRequiresFull,
    NoPartitionsToRebuild,
    CheckpointMissing,
    BlockStructureChanged,
    AffectedBlockIndexZero,
    SuffixValidationFailed(SuffixRebuildFailure),
    ParallelPartialTypeset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionTypesetDetail {
    pub partition_id: String,
    pub reuse_type: PartitionTypesetReuseType,
    pub suffix_block_count: usize,
    pub total_block_count: usize,
    pub elapsed: Option<std::time::Duration>,
    pub fallback_reason: Option<PartitionTypesetFallbackReason>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StageTiming {
    pub cache_load: Option<std::time::Duration>,
    pub source_tree_load: Option<std::time::Duration>,
    pub parse: Option<std::time::Duration>,
    pub typeset: Option<std::time::Duration>,
    pub pdf_render: Option<std::time::Duration>,
    pub cache_store: Option<std::time::Duration>,
    pub typeset_partition_details: Option<Vec<PartitionTypesetDetail>>,
    pub pass_count: Option<u32>,
}

impl StageTiming {
    pub fn total(&self) -> std::time::Duration {
        [
            self.cache_load,
            self.source_tree_load,
            self.parse,
            self.typeset,
            self.pdf_render,
            self.cache_store,
        ]
        .iter()
        .filter_map(|duration| *duration)
        .sum()
    }
}

pub struct CompileJobService<'a> {
    file_access_gate: &'a dyn FileAccessGate,
    asset_bundle_loader: &'a dyn AssetBundleLoaderPort,
    shell_command_gateway: &'a dyn ShellCommandGatewayPort,
    parser: MinimalLatexParser,
    typesetter: MinimalTypesetter,
    pdf_renderer: PdfRenderer,
    warm_partition_cache: Mutex<Option<WarmPartitionCache>>,
    background_cache_writer: BackgroundCacheWriter,
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
struct SourceLineSpan {
    normalized_text: String,
    span: SourceSpan,
    source_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceSpanAnnotator {
    files: Vec<String>,
    line_spans: Vec<SourceLineSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderedLineKey {
    page_index: usize,
    line_index: usize,
    placement_index: Option<usize>,
    normalized_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisibleSourceChar {
    ch: char,
    column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LoadedOpenTypeFont {
    base_font: String,
    font: OpenTypeFont,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TfmFontBundle {
    font_name: String,
    metrics: TfmMetrics,
    type1: Option<Type1Font>,
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
    Tfm {
        main: TfmFontBundle,
        bold: Option<TfmFontBundle>,
    },
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
    diagnostics: RefCell<Vec<Diagnostic>>,
}

struct ShellEscapeAdapter<'a> {
    gateway: &'a dyn ShellCommandGatewayPort,
    working_dir: PathBuf,
}

impl ShellEscapeHandler for ShellEscapeAdapter<'_> {
    fn execute_write18(&self, command: &str, _line: u32) -> ShellEscapeResult {
        let mut parts = command.split_whitespace();
        let Some(program) = parts.next() else {
            return ShellEscapeResult::Error("empty \\write18 command".to_string());
        };
        let args = parts.collect::<Vec<_>>();
        match self.gateway.execute(program, &args, &self.working_dir) {
            Ok(output) => ShellEscapeResult::Success {
                exit_code: output.exit_code,
            },
            Err(error) if error == "shell escape is not allowed" => ShellEscapeResult::Denied,
            Err(error) => ShellEscapeResult::Error(error),
        }
    }
}

struct FileOperationAdapter<'a> {
    gate: &'a dyn FileAccessGate,
    base_dir: PathBuf,
}

impl FileOperationAdapter<'_> {
    fn resolve(&self, path: &str) -> PathBuf {
        let candidate = Path::new(path);
        if candidate.is_absolute() {
            normalize_existing_path(candidate)
        } else {
            normalize_existing_path(&self.base_dir.join(candidate))
        }
    }

    fn check(&self, path: &str, write: bool) -> FileOperationResult {
        let resolved = self.resolve(path);
        let decision = if write {
            self.gate.check_write(&resolved)
        } else {
            self.gate.check_read(&resolved)
        };
        if decision == PathAccessDecision::Allowed {
            FileOperationResult::Allowed
        } else {
            FileOperationResult::Denied {
                path: resolved.to_string_lossy().into_owned(),
                reason: "outside allowed read/write roots".to_string(),
            }
        }
    }
}

impl FileOperationHandler for FileOperationAdapter<'_> {
    fn check_open_read(&self, path: &str, _line: u32) -> FileOperationResult {
        self.check(path, false)
    }

    fn check_open_write(&self, path: &str, _line: u32) -> FileOperationResult {
        self.check(path, true)
    }
}

impl CompileGraphicAssetResolver<'_> {
    fn take_diagnostics(&self) -> Vec<Diagnostic> {
        std::mem::take(&mut *self.diagnostics.borrow_mut())
    }

    fn push_diagnostic(&self, diagnostic: Diagnostic) {
        self.diagnostics.borrow_mut().push(diagnostic);
    }
}

impl GraphicAssetResolver for CompileGraphicAssetResolver<'_> {
    fn resolve(&self, path: &str) -> Option<ResolvedGraphic> {
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
        if let Some(metadata) = parse_image_metadata(&bytes) {
            return Some(ResolvedGraphic::Raster(ExternalGraphic {
                path: resolved_path.to_string_lossy().into_owned(),
                asset_handle: AssetHandle {
                    id: LogicalAssetId(stable_id_for_path(&resolved_path)),
                },
                metadata,
            }));
        }

        let looks_like_pdf = resolved_path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
            || is_pdf_signature(&bytes);
        if !looks_like_pdf {
            return None;
        }

        let Some(metadata) = parse_pdf_metadata(&bytes) else {
            self.push_diagnostic(
                Diagnostic::new(Severity::Error, "invalid PDF input for \\includegraphics")
                    .with_file(resolved_path.to_string_lossy().into_owned())
                    .with_suggestion(
                        "use an unencrypted single-page PDF whose first page defines /MediaBox",
                    ),
            );
            return None;
        };

        Some(ResolvedGraphic::Pdf(PdfGraphic {
            path: resolved_path.to_string_lossy().into_owned(),
            asset_handle: AssetHandle {
                id: LogicalAssetId(stable_id_for_path(&resolved_path)),
            },
            metadata,
        }))
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

    fn append_str_with_source_lines(&mut self, text: &str, source_lines: &[SourceLineTrace]) {
        if text.is_empty() {
            return;
        }

        debug_assert_eq!(text.split_inclusive('\n').count(), source_lines.len());
        // The open line retains its original origin (file, line); only the text content from the cached subtree's first line is merged.
        for (segment, origin) in text.split_inclusive('\n').zip(source_lines.iter()) {
            self.append_with_origin(segment, &origin.file, origin.line);
        }
    }

    fn append_expanded(&mut self, expanded: &ExpandedSource) {
        self.append_str_with_source_lines(&expanded.text, &expanded.source_lines);
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

impl SourceSpanAnnotator {
    fn new(source_lines: &[SourceLineTrace]) -> Self {
        let mut files = Vec::new();
        let line_spans = source_lines
            .iter()
            .enumerate()
            .filter_map(|(source_index, line)| {
                let visible_chars = visible_source_chars(&line.text);
                let start = visible_chars.first()?;
                let end = visible_chars.last()?;
                let file_id = file_id_for_source(&mut files, &line.file);
                Some(SourceLineSpan {
                    normalized_text: visible_chars.iter().map(|entry| entry.ch).collect(),
                    span: SourceSpan {
                        start: SourceLocation {
                            file_id,
                            line: line.line,
                            column: start.column,
                        },
                        end: SourceLocation {
                            file_id,
                            line: line.line,
                            column: end.column + 1,
                        },
                    },
                    source_index,
                })
            })
            .collect();

        Self { files, line_spans }
    }

    fn annotate_pages(&self, document: &mut TypesetDocument) -> BTreeSet<usize> {
        let rendered_lines = collect_rendered_line_keys(document);
        let mut assignments = vec![None; rendered_lines.len()];
        let mut used_source_lines = BTreeSet::new();
        let mut rendered_index = 0;
        let mut source_index = 0;

        while rendered_index < rendered_lines.len() {
            let Some((candidate_index, end_rendered_index)) =
                self.find_match(&rendered_lines, rendered_index, source_index)
            else {
                rendered_index += 1;
                continue;
            };

            let candidate = &self.line_spans[candidate_index];
            for assignment in assignments
                .iter_mut()
                .take(end_rendered_index + 1)
                .skip(rendered_index)
            {
                *assignment = Some(candidate.span);
            }
            used_source_lines.insert(candidate.source_index);
            rendered_index = end_rendered_index + 1;
            source_index = candidate_index + 1;
        }

        for (assignment, rendered_line) in assignments.into_iter().zip(rendered_lines.iter()) {
            if let Some(span) = assignment {
                match rendered_line.placement_index {
                    Some(placement_index) => {
                        document.pages[rendered_line.page_index].float_placements
                            [placement_index]
                            .content
                            .lines[rendered_line.line_index]
                            .source_span = Some(span);
                    }
                    None => {
                        document.pages[rendered_line.page_index].lines[rendered_line.line_index]
                            .source_span = Some(span);
                    }
                }
            }
        }

        used_source_lines
    }

    fn source_lines_without(
        &self,
        source_lines: &[SourceLineTrace],
        used_source_lines: &BTreeSet<usize>,
    ) -> Vec<SourceLineTrace> {
        source_lines
            .iter()
            .enumerate()
            .filter(|(source_index, _)| !used_source_lines.contains(source_index))
            .map(|(_, source_line)| source_line.clone())
            .collect()
    }

    fn used_source_lines_for_document(&self, document: &TypesetDocument) -> BTreeSet<usize> {
        let mut used_source_lines = BTreeSet::new();

        for line in document.pages.iter().flat_map(|page| page.lines.iter()) {
            let Some(span) = line.source_span else {
                continue;
            };
            for source_index in self
                .line_spans
                .iter()
                .filter(|candidate| source_span_contains_span(span, candidate.span))
                .map(|candidate| candidate.source_index)
            {
                used_source_lines.insert(source_index);
            }
        }

        for line in document
            .pages
            .iter()
            .flat_map(|page| page.float_placements.iter())
            .flat_map(|placement| placement.content.lines.iter())
        {
            let Some(span) = line.source_span else {
                continue;
            };
            for source_index in self
                .line_spans
                .iter()
                .filter(|candidate| source_span_contains_span(span, candidate.span))
                .map(|candidate| candidate.source_index)
            {
                used_source_lines.insert(source_index);
            }
        }

        used_source_lines
    }

    fn find_match(
        &self,
        rendered_lines: &[RenderedLineKey],
        rendered_index: usize,
        source_start: usize,
    ) -> Option<(usize, usize)> {
        let rendered_text = rendered_lines.get(rendered_index)?.normalized_text.as_str();
        if rendered_text.is_empty() {
            return None;
        }

        for (candidate_index, candidate) in self.line_spans.iter().enumerate().skip(source_start) {
            if candidate.normalized_text != rendered_text
                && !candidate.normalized_text.starts_with(rendered_text)
            {
                continue;
            }

            let mut combined = String::new();
            for (end_rendered_index, rendered_line) in
                rendered_lines.iter().enumerate().skip(rendered_index)
            {
                combined.push_str(&rendered_line.normalized_text);
                if combined == candidate.normalized_text {
                    return Some((candidate_index, end_rendered_index));
                }
                if !candidate.normalized_text.starts_with(&combined) {
                    break;
                }
            }
        }

        None
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

fn annotated_body_nodes(
    document: &ParsedDocument,
    source_lines: Option<&[SourceLineTrace]>,
) -> Option<(ParsedDocument, Vec<DocumentNode>)> {
    let source_lines = source_lines?;
    let mut annotated = document.clone();
    annotated.annotate_structure_source_spans(source_lines);
    let body_nodes = annotated.body_nodes_with_source_spans(source_lines);
    Some((annotated, body_nodes))
}

impl<'a> CompileJobService<'a> {
    pub fn new(
        file_access_gate: &'a dyn FileAccessGate,
        asset_bundle_loader: &'a dyn AssetBundleLoaderPort,
        shell_command_gateway: &'a dyn ShellCommandGatewayPort,
    ) -> Self {
        Self {
            file_access_gate,
            asset_bundle_loader,
            shell_command_gateway,
            parser: MinimalLatexParser,
            typesetter: MinimalTypesetter,
            pdf_renderer: PdfRenderer::default(),
            warm_partition_cache: Mutex::new(None),
            background_cache_writer: BackgroundCacheWriter::new(),
        }
    }

    pub fn flush_cache(&self) {
        self.background_cache_writer.flush();
    }

    fn try_generate_bibliography(
        &self,
        bibliography_context: &BibliographyContext,
        output_dir: &Path,
        jobname: &str,
    ) -> Option<Diagnostic> {
        match bibliography_context.toolchain() {
            Some(BibliographyToolchain::Bibtex) => {
                let aux_contents = bibliography_context.bibtex_aux_contents()?;
                let aux_path = output_dir.join(format!("{jobname}.aux"));
                if let Err(error) = self
                    .file_access_gate
                    .write_file(&aux_path, aux_contents.as_bytes())
                {
                    return Some(
                        Diagnostic::new(
                            Severity::Warning,
                            format!("failed to prepare bibliography aux file: {error}"),
                        )
                        .with_file(aux_path.to_string_lossy().into_owned())
                        .with_suggestion("run bibtex manually or verify the output directory"),
                    );
                }

                let command = self
                    .shell_command_gateway
                    .execute("bibtex", &[jobname], output_dir);
                match command {
                    Ok(result) if result.exit_code == 0 => None,
                    Ok(result) => {
                        let mut detail = format!("bibtex exited with code {}", result.exit_code);
                        let stdout = String::from_utf8_lossy(&result.stdout).trim().to_string();
                        let stderr = String::from_utf8_lossy(&result.stderr).trim().to_string();
                        if !stdout.is_empty() {
                            detail.push_str(&format!(", stdout: {stdout}"));
                        }
                        if !stderr.is_empty() {
                            detail.push_str(&format!(", stderr: {stderr}"));
                        }

                        Some(
                            Diagnostic::new(
                                Severity::Warning,
                                "automatic bibliography generation failed",
                            )
                            .with_file(aux_path.to_string_lossy().into_owned())
                            .with_context(detail)
                            .with_suggestion(
                                "inspect the bibliography tool output or run bibtex manually",
                            ),
                        )
                    }
                    Err(error) => Some(
                        Diagnostic::new(
                            Severity::Warning,
                            "automatic bibliography generation failed",
                        )
                        .with_file(aux_path.to_string_lossy().into_owned())
                        .with_context(error)
                        .with_suggestion(
                            "inspect the bibliography tool output or run bibtex manually",
                        ),
                    ),
                }
            }
            Some(BibliographyToolchain::Biber) => Some(
                Diagnostic::new(
                    Severity::Warning,
                    "automatic bibliography generation for biber is not implemented",
                )
                .with_file(
                    output_dir
                        .join(format!("{jobname}.bbl"))
                        .to_string_lossy()
                        .into_owned(),
                )
                .with_suggestion("run biber manually or provide a pre-generated .bbl file"),
            ),
            None => None,
        }
    }

    fn write_bibliography_sidecar(
        &self,
        bbl_path: &Path,
        input_fingerprint: &BibliographyInputFingerprint,
        toolchain: BibliographyToolchain,
    ) -> Option<Diagnostic> {
        let sidecar_path = bibliography_sidecar_path(bbl_path);
        let toolchain = match toolchain {
            BibliographyToolchain::Bibtex => "bibtex",
            BibliographyToolchain::Biber => "biber",
        };
        let payload = json!({
            "inputFingerprint": { "hash": input_fingerprint.hash },
            "toolchain": toolchain,
        });
        let bytes = match serde_json::to_vec_pretty(&payload) {
            Ok(bytes) => bytes,
            Err(error) => {
                return Some(
                    Diagnostic::new(
                        Severity::Warning,
                        format!("failed to serialize bibliography sidecar metadata: {error}"),
                    )
                    .with_file(sidecar_path.to_string_lossy().into_owned()),
                );
            }
        };

        self.file_access_gate
            .write_file(&sidecar_path, &bytes)
            .err()
            .map(|error| {
                Diagnostic::new(
                    Severity::Warning,
                    format!("failed to persist bibliography sidecar metadata: {error}"),
                )
                .with_file(sidecar_path.to_string_lossy().into_owned())
            })
    }

    pub fn compile(&self, options: &RuntimeOptions) -> CompileResult {
        self.compile_with_changed_paths(options, &[])
    }

    /// Compile with an optional watcher-provided change hint.
    ///
    /// `changed_paths_hint` should contain canonical paths that match the
    /// dependency graph node keys used by `CompileCache`. The current watcher
    /// pipeline guarantees this for existing callers.
    pub fn compile_with_changed_paths(
        &self,
        options: &RuntimeOptions,
        changed_paths_hint: &[PathBuf],
    ) -> CompileResult {
        let input_path = options.input_file.to_string_lossy().into_owned();
        let execution_policy = ExecutionPolicyFactory::create(options);
        let project_root = project_root_for_policy(&execution_policy, &options.input_file);
        let input_dir = input_dir_for_input(&options.input_file, &project_root);
        let mut stage_timing = StageTiming::default();

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
                    stage_timing,
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
                    stage_timing,
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
                stage_timing,
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
                stage_timing,
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
                stage_timing,
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
                stage_timing,
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
        let mut changed_paths = BTreeSet::new();
        let mut cached_recompilation_scope = None;
        let mut cached_typeset_fragments = BTreeMap::new();
        let mut cached_page_payloads = BTreeMap::new();
        let mut source_tree_reuse_plan = None;
        if !options.no_cache {
            let cache_load_start = std::time::Instant::now();
            let warm_cache = self
                .warm_partition_cache
                .lock()
                .expect("lock warm partition cache")
                .take();
            let mut lookup = compile_cache.lookup_with_warm_cache(changed_paths_hint, warm_cache);
            cached_cross_reference_seed = lookup
                .baseline_state
                .as_ref()
                .map(|state| state.cross_reference_seed.clone());
            changed_paths = lookup.changed_paths.iter().cloned().collect();
            cached_recompilation_scope = lookup.scope.clone();
            if let Some(cached_artifact) = lookup.artifact.take() {
                tracing::info!(
                    jobname = %options.jobname,
                    input = %options.input_file.display(),
                    output = %cached_artifact.output_pdf.display(),
                    "compile cache hit"
                );
                if options.trace_font_tasks {
                    let timestamp = trace_timestamp_micros();
                    emit_font_task_trace(
                        FONT_CACHE_HIT_TASK_ID,
                        FONT_CACHE_HIT_ASSET_SENTINEL,
                        timestamp,
                        timestamp,
                        0,
                    );
                }
                let diagnostics = cached_artifact.stable_compile_state.diagnostics.clone();
                let warm_cache = lookup.into_warm_cache();
                *self
                    .warm_partition_cache
                    .lock()
                    .expect("lock warm partition cache") = Some(warm_cache);
                stage_timing.cache_load = Some(cache_load_start.elapsed());
                return CompileResult {
                    exit_code: exit_code_for(&diagnostics),
                    diagnostics,
                    output_pdf: Some(cached_artifact.output_pdf),
                    stable_compile_state: Some(cached_artifact.stable_compile_state),
                    stage_timing,
                };
            }

            let lookup_diagnostics = lookup.diagnostics;
            let lookup_changed_paths_vec = lookup.changed_paths;
            let lookup_rebuild_paths = lookup.rebuild_paths;
            let lookup_cached_dependency_graph = lookup.cached_dependency_graph;
            cached_typeset_fragments = lookup.cached_typeset_fragments;
            cached_page_payloads = lookup.cached_page_payloads;
            if let Some(scope) = cached_recompilation_scope.as_ref() {
                tracing::info!(
                    jobname = %options.jobname,
                    input = %options.input_file.display(),
                    changed_paths = ?lookup_changed_paths_vec,
                    rebuild_paths = ?lookup_rebuild_paths,
                    ?scope,
                    "compile cache miss due to changed dependencies"
                );
            }
            if let Some(cached_dependency_graph) = lookup_cached_dependency_graph {
                source_tree_reuse_plan = Some(SourceTreeReusePlan {
                    rebuild_paths: lookup_rebuild_paths,
                    cached_dependency_graph,
                    cached_source_subtrees: lookup.cached_source_subtrees,
                });
            }
            cache_diagnostics.extend(lookup_diagnostics);
            stage_timing.cache_load = Some(cache_load_start.elapsed());
        }

        let source_tree_load_start = std::time::Instant::now();
        let mut source_tree = match self.load_source_tree(
            &options.input_file,
            &project_root,
            &options.overlay_roots,
            options.asset_bundle.as_deref(),
            source_tree_reuse_plan.as_ref(),
        ) {
            Ok(tree) => {
                stage_timing.source_tree_load = Some(source_tree_load_start.elapsed());
                tree
            }
            Err(diagnostic) => {
                stage_timing.source_tree_load = Some(source_tree_load_start.elapsed());
                let mut diagnostics = cache_diagnostics;
                diagnostics.push(diagnostic);

                return CompileResult {
                    exit_code: exit_code_for(&diagnostics),
                    diagnostics,
                    output_pdf: None,
                    stable_compile_state: None,
                    stage_timing,
                };
            }
        };
        let bibliography_context = BibliographyContext::from_source(&source_tree.source);
        let mut bibliography_diagnostics = Vec::new();
        let mut loaded_bibliography_state = load_bibliography_state(
            self.file_access_gate,
            &project_root,
            &input_dir,
            &options.overlay_roots,
            &options.output_dir,
            &options.jobname,
        );
        let bibliography_issue = loaded_bibliography_state
            .as_ref()
            .and_then(|loaded| {
                check_bbl_freshness(
                    loaded,
                    &bibliography_context,
                    &project_root,
                    &options.overlay_roots,
                )
            })
            .or_else(|| {
                (loaded_bibliography_state.is_none() && bibliography_context.has_citations())
                    .then_some(BibliographyDiagnostic::MissingBbl)
            });
        if bibliography_issue.is_some() && execution_policy.shell_escape_allowed {
            if let Some(diagnostic) = self.try_generate_bibliography(
                &bibliography_context,
                &options.output_dir,
                &options.jobname,
            ) {
                bibliography_diagnostics.push(diagnostic);
            }

            let bbl_path = options.output_dir.join(format!("{}.bbl", options.jobname));
            if bbl_path.exists() {
                if let (Some(input_fingerprint), Some(toolchain)) = (
                    bibliography_context.current_fingerprint(&project_root, &options.overlay_roots),
                    bibliography_context.toolchain(),
                ) {
                    if let Some(diagnostic) =
                        self.write_bibliography_sidecar(&bbl_path, &input_fingerprint, toolchain)
                    {
                        bibliography_diagnostics.push(diagnostic);
                    }
                }
            }

            loaded_bibliography_state = load_bibliography_state(
                self.file_access_gate,
                &project_root,
                &input_dir,
                &options.overlay_roots,
                &options.output_dir,
                &options.jobname,
            );
        }
        let initial_bibliography_state = loaded_bibliography_state
            .as_ref()
            .map(|loaded| loaded.state.clone());
        if let Some(bibliography_state) = &initial_bibliography_state {
            source_tree.document_state.bibliography_state = bibliography_state.clone();
        }

        let graphics_resolver = CompileGraphicAssetResolver {
            file_access_gate: self.file_access_gate,
            input_dir: &input_dir,
            project_root: &project_root,
            overlay_roots: &options.overlay_roots,
            asset_bundle_path: options.asset_bundle.as_deref(),
            diagnostics: RefCell::new(Vec::new()),
        };
        let normalized_input_file = normalize_existing_path(&options.input_file);
        let mut compile_font_selection = None;
        let mut font_family_selection = None;
        let mut font_diagnostics = Vec::new();
        let mut font_resolution_fatal = false;
        let mut typeset_callback_count = 0usize;
        let mut typeset_accumulated = std::time::Duration::ZERO;
        let mut typeset_partition_details = None;
        let parse_and_typeset_start = std::time::Instant::now();
        let parse_pass_result = self.parse_document_with_cross_references(
            &source_tree.source,
            &options.input_file,
            &project_root,
            &options.overlay_roots,
            options.asset_bundle.as_deref(),
            execution_policy.shell_escape_allowed,
            initial_bibliography_state.clone(),
            source_tree.document_state.index_state.entries.clone(),
            cached_cross_reference_seed.as_ref(),
            |document| {
                let typeset_iter_start = std::time::Instant::now();
                let result = {
                    typeset_callback_count += 1;
                    typeset_partition_details = None;
                    if compile_font_selection.is_none() {
                        let (selection, families, diagnostics, fatal) = self.select_compile_fonts(
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
                        font_resolution_fatal = fatal;
                        compile_font_selection = Some(selection);
                        font_family_selection = Some(families);
                    }

                    let selection = compile_font_selection
                        .as_ref()
                        .expect("font selection initialized");

                    let full_typeset = || {
                        let sequential_typeset = || {
                            self.typeset_document_with_selection(
                                document,
                                Some(&source_tree.source_lines),
                                selection,
                                &graphics_resolver,
                            )
                        };

                        if options.parallelism <= 1 {
                            return sequential_typeset();
                        }

                        let partition_plan =
                            partition_plan_for_document(&options.input_file, document, &source_tree);
                        if partition_plan.work_units.len() < 2 {
                            return sequential_typeset();
                        }

                        match try_parallel_full_typeset(
                            self,
                            document,
                            &source_tree.source_lines,
                            selection,
                            &graphics_resolver,
                            options.parallelism,
                            &partition_plan,
                            graphics_resolver.file_access_gate,
                            graphics_resolver.input_dir,
                            graphics_resolver.project_root,
                            graphics_resolver.overlay_roots,
                            graphics_resolver.asset_bundle_path,
                            typeset_callback_count as u32,
                        ) {
                            Ok(document) => document,
                            Err(reason) => {
                                tracing::info!(
                                    jobname = %options.jobname,
                                    input = %options.input_file.display(),
                                    "{}",
                                    format!("full typeset fallback to sequential ({reason})")
                                );
                                sequential_typeset()
                            }
                        }
                    };

                    let partial_typeset_available = matches!(
                        cached_recompilation_scope,
                        Some(RecompilationScope::LocalRegion)
                            | Some(RecompilationScope::BlockLevel { .. })
                    ) && !cached_typeset_fragments.is_empty()
                        && !document.has_pageref_markers();
                    if partial_typeset_available && typeset_callback_count > 1 {
                        let partition_plan =
                            partition_plan_for_document(&options.input_file, document, &source_tree);
                        typeset_partition_details = Some(synthetic_full_rebuild_partition_details(
                            &partition_plan,
                            &cached_typeset_fragments,
                            PartitionTypesetFallbackReason::TypesetCallbackCount,
                        ));
                        tracing::info!(
                            jobname = %options.jobname,
                            input = %options.input_file.display(),
                            "partial typeset fallback to full typeset"
                        );
                        full_typeset()
                    } else if !partial_typeset_available || typeset_callback_count != 1 {
                        if document.has_pageref_markers()
                            && matches!(
                                cached_recompilation_scope,
                                Some(RecompilationScope::LocalRegion)
                                    | Some(RecompilationScope::BlockLevel { .. })
                            )
                            && !cached_typeset_fragments.is_empty()
                        {
                            let partition_plan = partition_plan_for_document(
                                &options.input_file,
                                document,
                                &source_tree,
                            );
                            typeset_partition_details =
                                Some(synthetic_full_rebuild_partition_details(
                                    &partition_plan,
                                    &cached_typeset_fragments,
                                    PartitionTypesetFallbackReason::Pageref,
                                ));
                        }
                        full_typeset()
                    } else if let Some(reuse_plan) = source_tree_reuse_plan.as_ref() {
                        let partition_plan =
                            partition_plan_for_document(&options.input_file, document, &source_tree);
                        let usable_cached_fragments = cached_document_layout_fragments_for(
                            &partition_plan,
                            &source_tree,
                            &cached_typeset_fragments,
                        );
                        let is_block_level = matches!(
                            cached_recompilation_scope,
                            Some(RecompilationScope::BlockLevel { .. })
                        );
                        let typesetter_reuse_plan = TypesetterReusePlan::create(
                            &partition_plan,
                            &reuse_plan.rebuild_paths,
                            &usable_cached_fragments,
                            if is_block_level {
                                false
                            } else {
                                changed_paths.contains(&normalized_input_file)
                            },
                            is_block_level,
                        );

                        if typesetter_reuse_plan.requires_full_typeset {
                            typeset_partition_details =
                                Some(synthetic_full_rebuild_partition_details(
                                    &partition_plan,
                                    &cached_typeset_fragments,
                                    PartitionTypesetFallbackReason::ReusePlanRequiresFull,
                                ));
                            tracing::info!(
                                jobname = %options.jobname,
                                input = %options.input_file.display(),
                                "partial typeset fallback to full typeset (reuse plan requires full)"
                            );
                            full_typeset()
                        } else if typesetter_reuse_plan.rebuild_partition_ids.is_empty() {
                            typeset_partition_details =
                                Some(synthetic_full_rebuild_partition_details(
                                    &partition_plan,
                                    &cached_typeset_fragments,
                                    PartitionTypesetFallbackReason::NoPartitionsToRebuild,
                                ));
                            tracing::info!(
                                jobname = %options.jobname,
                                input = %options.input_file.display(),
                                "partial typeset fallback to full typeset (no partitions to rebuild)"
                            );
                            full_typeset()
                        } else {
                            match try_partial_typeset_document(
                                self,
                                document,
                                &source_tree.source_lines,
                                selection,
                                &graphics_resolver,
                                options.parallelism,
                                graphics_resolver.file_access_gate,
                                graphics_resolver.input_dir,
                                graphics_resolver.project_root,
                                graphics_resolver.overlay_roots,
                                graphics_resolver.asset_bundle_path,
                                &partition_plan,
                                &typesetter_reuse_plan,
                                &cached_typeset_fragments,
                                &source_tree.cached_source_subtrees,
                                &reuse_plan.cached_source_subtrees,
                            ) {
                                Ok(execution) => {
                                    typeset_partition_details =
                                        Some(execution.partition_details.clone());
                                    tracing::info!(
                                        jobname = %options.jobname,
                                        input = %options.input_file.display(),
                                        rebuilt_partitions = ?typesetter_reuse_plan.rebuild_partition_ids,
                                        "partial typeset reuse applied"
                                    );
                                    execution.document
                                }
                                Err(reason) => {
                                    tracing::info!(
                                        jobname = %options.jobname,
                                        input = %options.input_file.display(),
                                        "{}",
                                        format!("partial typeset fallback to full typeset ({reason})")
                                    );
                                    full_typeset()
                                }
                            }
                        }
                    } else {
                        full_typeset()
                    }
                };
                typeset_accumulated += typeset_iter_start.elapsed();
                result
            },
        );
        let total_parse_and_typeset = parse_and_typeset_start.elapsed();
        stage_timing.typeset = Some(typeset_accumulated);
        stage_timing.parse = Some(total_parse_and_typeset - typeset_accumulated);
        stage_timing.typeset_partition_details = typeset_partition_details;
        let pdf_renderer = match (
            compile_font_selection.as_ref(),
            font_family_selection.as_ref(),
            parse_pass_result.typeset_document.as_ref(),
        ) {
            (
                Some(CompileFontSelection::Tfm { main, bold }),
                Some(families),
                Some(typeset_document),
            ) if main.type1.is_some() => {
                let font_resources = build_type1_font_resources(
                    families,
                    main,
                    bold.as_ref(),
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
            (_, Some(families), Some(typeset_document)) => {
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
        stage_timing.pass_count = Some(pass_count);
        let mut parse_diagnostics: Vec<Diagnostic> = errors
            .into_iter()
            .map(|error| diagnostic_for_parse_error(error, input_path.clone()))
            .collect();
        parse_diagnostics.extend(font_diagnostics);
        parse_diagnostics.extend(bibliography_diagnostics);
        if font_resolution_fatal {
            let mut diagnostics = cache_diagnostics.clone();
            diagnostics.extend(parse_diagnostics.clone());
            return CompileResult {
                exit_code: exit_code_for(&diagnostics),
                diagnostics,
                output_pdf: None,
                stable_compile_state: None,
                stage_timing,
            };
        }
        if let Some(loaded_bibliography_state) = &loaded_bibliography_state {
            if let Some(diagnostic) = check_bbl_freshness(
                loaded_bibliography_state,
                &bibliography_context,
                &project_root,
                &options.overlay_roots,
            ) {
                parse_diagnostics.push(diagnostic_for_bibliography(diagnostic, Vec::new()));
            }
        }
        if initial_bibliography_state.is_none()
            && bibliography_context.has_citations()
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
                    &input_dir,
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
                    stage_timing,
                };
            }
        };
        for citation_key in &bibliography_context.citation_keys {
            if parsed_document
                .bibliography_state
                .resolve_citation(citation_key)
                .is_none()
            {
                parse_diagnostics.push(diagnostic_for_bibliography(
                    BibliographyDiagnostic::UnresolvedCitation {
                        key: citation_key.clone(),
                    },
                    Vec::new(),
                ));
            }
        }
        let typeset_document = typeset_document.expect("parsed documents should always typeset");
        let graphics_diagnostics = graphics_resolver.take_diagnostics();
        if graphics_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            let mut diagnostics = cache_diagnostics.clone();
            diagnostics.extend(parse_diagnostics.clone());
            diagnostics.extend(graphics_diagnostics);
            return CompileResult {
                exit_code: exit_code_for(&diagnostics),
                diagnostics,
                output_pdf: None,
                stable_compile_state: None,
                stage_timing,
            };
        }
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
                    stage_timing,
                };
            }
        };
        let partition_plan =
            partition_plan_for_document(&options.input_file, &parsed_document, &source_tree);
        let reusable_page_payloads = reusable_page_payloads_for_render(
            &typeset_document,
            &partition_plan,
            stage_timing.typeset_partition_details.as_deref(),
            &cached_page_payloads,
        );
        let reused_page_count = reusable_page_payloads.len();
        if reused_page_count > 0 {
            tracing::info!(
                jobname = %options.jobname,
                input = %options.input_file.display(),
                "pdf page payload reuse applied (reused_pages={}, rendered_pages={})",
                reused_page_count,
                typeset_document.pages.len().saturating_sub(reused_page_count),
            );
        } else if stage_timing.typeset_partition_details.is_some() {
            tracing::info!(
                jobname = %options.jobname,
                input = %options.input_file.display(),
                "pdf page payload reuse skipped (reused_pages=0)"
            );
        }
        let pdf_render_start = std::time::Instant::now();
        let pdf_document = pdf_renderer.render_with_partition_plan(
            &typeset_document,
            options.parallelism,
            pass_count,
            &partition_plan,
            (!reusable_page_payloads.is_empty()).then_some(&reusable_page_payloads),
        );
        stage_timing.pdf_render = Some(pdf_render_start.elapsed());
        let compilation_job = compilation_job(
            options.input_file.clone(),
            options.jobname.clone(),
            execution_policy,
        );
        let cacheable_diagnostics = parse_diagnostics;
        let mut diagnostics = cache_diagnostics;
        diagnostics.extend(cacheable_diagnostics.clone());
        for warning in &pdf_document.document.warnings {
            diagnostics.push(Diagnostic::new(Severity::Warning, warning.clone()));
        }
        let cached_typeset_fragments = cached_typeset_fragments_for(
            self,
            &parsed_document,
            &typeset_document,
            &partition_plan,
            &source_tree,
            compile_font_selection
                .as_ref()
                .expect("font selection initialized after typesetting"),
            &graphics_resolver,
        );

        if let Err(error) = self
            .file_access_gate
            .write_file(&output_pdf, &pdf_document.document.bytes)
        {
            diagnostics.push(diagnostic_for_output_error(error, &output_pdf));

            return CompileResult {
                exit_code: exit_code_for(&diagnostics),
                diagnostics,
                output_pdf: None,
                stable_compile_state: None,
                stage_timing,
            };
        }

        if options.synctex {
            let synctex_path = options
                .output_dir
                .join(format!("{}.synctex", options.jobname));
            let synctex = synctex_data_for(&typeset_document, &source_tree.source_lines);
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

        let output_pdf_hash = fingerprint_bytes(&pdf_document.document.bytes);
        let provisional_stable_state = stable_compile_state(
            &compilation_job,
            source_tree.document_state.clone(),
            cross_reference_seed.clone(),
            pass_count,
            pdf_document.document.page_count,
            true,
            cacheable_diagnostics.clone(),
        );
        if !options.no_cache {
            let cache_store_start = std::time::Instant::now();
            let cached_page_payloads = cached_page_payloads_for(
                &typeset_document,
                &partition_plan,
                &pdf_document.page_payloads,
            );
            let dirty_partition_ids: BTreeSet<String> = stage_timing
                .typeset_partition_details
                .as_ref()
                .map(|details| {
                    details
                        .iter()
                        .filter(|detail| detail.reuse_type != PartitionTypesetReuseType::Cached)
                        .map(|detail| detail.partition_id.clone())
                        .collect()
                })
                .unwrap_or_default();
            let dirty_source_paths: &BTreeSet<PathBuf> = &changed_paths;
            let (opt_dirty_partitions, opt_dirty_sources) =
                if stage_timing.typeset_partition_details.is_some() {
                    (Some(&dirty_partition_ids), Some(dirty_source_paths))
                } else {
                    (None, None)
                };
            let (stored_partition_hashes, stored_cached_index) = match compile_cache
                .store_background(
                    &source_tree.dependency_graph,
                    &provisional_stable_state,
                    output_pdf_hash,
                    &source_tree.cached_source_subtrees,
                    &cached_typeset_fragments,
                    &cached_page_payloads,
                    opt_dirty_partitions,
                    opt_dirty_sources,
                    &self.background_cache_writer,
                ) {
                Ok(outcome) => {
                    if let Some(diagnostic) = outcome.diagnostic {
                        diagnostics.push(diagnostic);
                    }
                    (Some(outcome.partition_hashes), outcome.cached_index)
                }
                Err(diagnostic) => {
                    diagnostics.push(diagnostic);
                    (None, None)
                }
            };
            if let Some(partition_hashes) = stored_partition_hashes {
                let warm_cache = WarmPartitionCache {
                    partition_hashes,
                    cached_source_subtrees: source_tree.cached_source_subtrees,
                    cached_typeset_fragments,
                    cached_page_payloads,
                    cached_index: stored_cached_index,
                };
                *self
                    .warm_partition_cache
                    .lock()
                    .expect("lock warm partition cache") = Some(warm_cache);
            }
            // cache_store now measures only the synchronous enqueue; background I/O
            // runs off the critical path.
            stage_timing.cache_store = Some(cache_store_start.elapsed());
        }

        let stable_compile_state = stable_compile_state(
            &compilation_job,
            source_tree.document_state,
            cross_reference_seed,
            pass_count,
            pdf_document.document.page_count,
            true,
            diagnostics.clone(),
        );

        tracing::info!(
            cache_load_us = stage_timing.cache_load.map(|d| d.as_micros() as u64),
            source_tree_load_us = stage_timing.source_tree_load.map(|d| d.as_micros() as u64),
            parse_us = stage_timing.parse.map(|d| d.as_micros() as u64),
            typeset_us = stage_timing.typeset.map(|d| d.as_micros() as u64),
            pdf_render_us = stage_timing.pdf_render.map(|d| d.as_micros() as u64),
            cache_store_us = stage_timing.cache_store.map(|d| d.as_micros() as u64),
            total_us = stage_timing.total().as_micros() as u64,
            "compile stage timing"
        );

        tracing::info!(
            jobname = %options.jobname,
            input = %options.input_file.display(),
            output = %output_pdf.display(),
            document_class = %parsed_document.document_class,
            package_count = parsed_document.package_count,
            page_count = pdf_document.document.page_count,
            total_lines = pdf_document.document.total_lines,
            "compile succeeded"
        );

        CompileResult {
            exit_code: exit_code_for(&diagnostics),
            diagnostics,
            output_pdf: Some(output_pdf),
            stable_compile_state: Some(stable_compile_state),
            stage_timing,
        }
    }

    pub fn compile_from_source(&self, source: &str, uri: &str) -> StableCompileState {
        let primary_input = primary_input_from_uri(uri);
        let jobname = jobname_for_input(&primary_input);
        let execution_policy = in_memory_execution_policy(&primary_input, &jobname);
        let project_root = project_root_for_policy(&execution_policy, &primary_input);
        let asset_bundle = default_lsp_asset_bundle();
        let compilation_job =
            compilation_job(primary_input.clone(), jobname.clone(), execution_policy);
        let mut source_tree = self
            .load_source_tree_with_root_source(
                &primary_input,
                Some(source),
                &project_root,
                &[],
                asset_bundle.as_deref(),
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
            &primary_input,
            &project_root,
            &[],
            asset_bundle.as_deref(),
            false,
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
    ) -> (
        CompileFontSelection,
        FontFamilySelection,
        Vec<Diagnostic>,
        bool,
    ) {
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
                    self.asset_bundle_loader,
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
                    self.asset_bundle_loader,
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
                    self.asset_bundle_loader,
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
                false,
            );
        }

        if let Some(metrics) = trace_font_task(
            trace_font_tasks,
            "font-load-cmr10-fallback",
            "cmr10.tfm",
            0,
            || {
                load_cmr10_metrics(
                    self.file_access_gate,
                    self.asset_bundle_loader,
                    asset_bundle_path,
                )
            },
        ) {
            let main = TfmFontBundle {
                font_name: "cmr10".to_string(),
                type1: resolve_type1_font(
                    self.file_access_gate,
                    self.asset_bundle_loader,
                    asset_bundle_path,
                    "cmr10",
                ),
                metrics,
            };
            let bold = load_cmbx12_metrics(
                self.file_access_gate,
                self.asset_bundle_loader,
                asset_bundle_path,
            )
            .map(|metrics| TfmFontBundle {
                font_name: "cmbx12".to_string(),
                type1: resolve_type1_font(
                    self.file_access_gate,
                    self.asset_bundle_loader,
                    asset_bundle_path,
                    "cmbx12",
                ),
                metrics,
            });
            return (
                CompileFontSelection::Tfm { main, bold },
                families,
                diagnostics,
                false,
            );
        }

        if asset_bundle_path.is_some() {
            diagnostics.push(
                Diagnostic::new(
                    Severity::Error,
                    "required asset bundle font metrics \"cmr10\" could not be resolved",
                )
                .with_file(input_path.to_string())
                .with_suggestion(
                    "restore the cmr10.tfm asset (and matching asset-index entry) or add a default OpenType font to the asset bundle",
                ),
            );
            return (CompileFontSelection::Basic, families, diagnostics, true);
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
        (CompileFontSelection::Basic, families, diagnostics, false)
    }

    fn typeset_document_with_selection(
        &self,
        document: &ParsedDocument,
        source_lines: Option<&[SourceLineTrace]>,
        selection: &CompileFontSelection,
        graphics_resolver: &CompileGraphicAssetResolver<'_>,
    ) -> TypesetDocument {
        let mut annotated_document = document.clone();
        let body_nodes = source_lines.map(|source_lines| {
            annotated_document.annotate_structure_source_spans(source_lines);
            annotated_document.body_nodes_with_source_spans(source_lines)
        });
        let document = if source_lines.is_some() {
            &annotated_document
        } else {
            document
        };

        match (selection, body_nodes) {
            (CompileFontSelection::OpenType(loaded_font), Some(body_nodes)) => {
                let provider = OpenTypeWidthProvider {
                    font: &loaded_font.font,
                    fallback_width: DEFAULT_TFM_FALLBACK_WIDTH,
                };
                self.typesetter.typeset_with_body_nodes(
                    document,
                    body_nodes,
                    &provider,
                    Some(graphics_resolver),
                )
            }
            (CompileFontSelection::OpenType(loaded_font), None) => {
                let provider = OpenTypeWidthProvider {
                    font: &loaded_font.font,
                    fallback_width: DEFAULT_TFM_FALLBACK_WIDTH,
                };
                self.typesetter.typeset_with_provider_and_graphics_resolver(
                    document,
                    &provider,
                    Some(graphics_resolver),
                )
            }
            (CompileFontSelection::Tfm { main, .. }, Some(body_nodes)) => {
                let provider = TfmWidthProvider {
                    metrics: &main.metrics,
                    fallback_width: DEFAULT_TFM_FALLBACK_WIDTH,
                };
                self.typesetter.typeset_with_body_nodes(
                    document,
                    body_nodes,
                    &provider,
                    Some(graphics_resolver),
                )
            }
            (CompileFontSelection::Tfm { main, .. }, None) => {
                let provider = TfmWidthProvider {
                    metrics: &main.metrics,
                    fallback_width: DEFAULT_TFM_FALLBACK_WIDTH,
                };
                self.typesetter.typeset_with_provider_and_graphics_resolver(
                    document,
                    &provider,
                    Some(graphics_resolver),
                )
            }
            (CompileFontSelection::Basic, Some(body_nodes)) => {
                let provider = FixedWidthProvider {
                    char_width: DimensionValue(65_536),
                    space_width: DimensionValue(65_536),
                };
                self.typesetter.typeset_with_body_nodes(
                    document,
                    body_nodes,
                    &provider,
                    Some(graphics_resolver),
                )
            }
            (CompileFontSelection::Basic, None) => self
                .typesetter
                .typeset_with_graphics_resolver(document, graphics_resolver),
        }
    }

    fn build_partition_vlist_with_selection(
        &self,
        context_document: &ParsedDocument,
        body_document: &ParsedDocument,
        source_lines: Option<&[SourceLineTrace]>,
        selection: &CompileFontSelection,
        graphics_resolver: &CompileGraphicAssetResolver<'_>,
        continues_from_previous_block: bool,
        initial_float_counters: FloatCounters,
    ) -> PartitionVListResult {
        let (_annotated_document, body_nodes) = annotated_body_nodes(body_document, source_lines)
            .unwrap_or_else(|| (body_document.clone(), body_document.body_nodes()));
        self.build_vlist_from_body_nodes_with_selection(
            context_document,
            body_nodes,
            selection,
            graphics_resolver,
            continues_from_previous_block,
            initial_float_counters,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_vlist_from_body_nodes_with_selection(
        &self,
        context_document: &ParsedDocument,
        body_nodes: Vec<DocumentNode>,
        selection: &CompileFontSelection,
        graphics_resolver: &CompileGraphicAssetResolver<'_>,
        continues_from_previous_block: bool,
        initial_float_counters: FloatCounters,
        checkpoint_collector: Option<&mut Vec<RawBlockCheckpoint>>,
    ) -> PartitionVListResult {
        match selection {
            CompileFontSelection::OpenType(loaded_font) => {
                let provider = OpenTypeWidthProvider {
                    font: &loaded_font.font,
                    fallback_width: DEFAULT_TFM_FALLBACK_WIDTH,
                };
                self.typesetter
                    .build_vlist_for_partition_continuing_with_state(
                        context_document,
                        body_nodes,
                        &provider,
                        Some(graphics_resolver),
                        continues_from_previous_block,
                        initial_float_counters,
                        checkpoint_collector,
                    )
            }
            CompileFontSelection::Tfm { main, .. } => {
                let provider = TfmWidthProvider {
                    metrics: &main.metrics,
                    fallback_width: DEFAULT_TFM_FALLBACK_WIDTH,
                };
                self.typesetter
                    .build_vlist_for_partition_continuing_with_state(
                        context_document,
                        body_nodes,
                        &provider,
                        Some(graphics_resolver),
                        continues_from_previous_block,
                        initial_float_counters,
                        checkpoint_collector,
                    )
            }
            CompileFontSelection::Basic => {
                let provider = FixedWidthProvider {
                    char_width: DimensionValue(65_536),
                    space_width: DimensionValue(65_536),
                };
                self.typesetter
                    .build_vlist_for_partition_continuing_with_state(
                        context_document,
                        body_nodes,
                        &provider,
                        Some(graphics_resolver),
                        continues_from_previous_block,
                        initial_float_counters,
                        checkpoint_collector,
                    )
            }
        }
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
        primary_input: &Path,
        project_root: &Path,
        overlay_roots: &[PathBuf],
        asset_bundle_path: Option<&Path>,
        shell_escape_allowed: bool,
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
        let base_dir = primary_input
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .map(normalize_existing_path)
            .unwrap_or_else(|| project_root.to_path_buf());
        let shell_escape_adapter = shell_escape_allowed.then(|| ShellEscapeAdapter {
            gateway: self.shell_command_gateway,
            working_dir: base_dir.clone(),
        });
        let file_operation_adapter = FileOperationAdapter {
            gate: self.file_access_gate,
            base_dir,
        };
        let mut output = self.parser.parse_recovering_with_context_and_handlers(
            source,
            initial_labels,
            initial_section_entries,
            initial_figure_entries,
            initial_table_entries,
            initial_bibliography,
            initial_bibliography_state.clone(),
            initial_page_labels.clone(),
            initial_index_entries.clone(),
            Some(&sty_resolver),
            shell_escape_adapter
                .as_ref()
                .map(|adapter| adapter as &dyn ShellEscapeHandler),
            Some(&file_operation_adapter),
        );
        let Some(mut document) = output.document.clone() else {
            return ParsePassResult {
                output,
                typeset_document: None,
                pass_count: 1,
            };
        };
        let mut pass_count = 1;
        let mut current_page_labels = initial_page_labels;
        let mut current_index_entries = initial_index_entries.clone();

        if document.has_unresolved_refs
            || document.has_unresolved_toc
            || document.has_unresolved_lof
            || document.has_unresolved_lot
        {
            let second = self.parser.parse_recovering_with_context_and_handlers(
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
                shell_escape_adapter
                    .as_ref()
                    .map(|adapter| adapter as &dyn ShellEscapeHandler),
                Some(&file_operation_adapter),
            );
            if let Some(next_document) = second.document.clone() {
                output = second;
                document = next_document;
                pass_count = 2;
            }
        }

        let mut typeset_document = typeset_document_for(&document);

        while pass_count < 3 {
            let page_labels = if document.has_pageref_markers() || !current_page_labels.is_empty() {
                resolve_page_labels(&document, &typeset_document.pages)
            } else {
                BTreeMap::new()
            };
            let index_entries = typeset_document.index_entries.clone();
            let needs_pageref_pass = !page_labels.is_empty() && page_labels != current_page_labels;
            let needs_index_pass = document.has_unresolved_index
                && !index_entries.is_empty()
                && index_entries != current_index_entries;

            if !needs_pageref_pass && !needs_index_pass {
                break;
            }

            let next = self.parser.parse_recovering_with_context_and_handlers(
                source,
                document.labels.clone().into_inner(),
                document.section_entries.clone(),
                document.figure_entries.clone(),
                document.table_entries.clone(),
                document.bibliography.clone(),
                Some(document.bibliography_state.clone()),
                page_labels.clone(),
                index_entries.clone(),
                Some(&sty_resolver),
                shell_escape_adapter
                    .as_ref()
                    .map(|adapter| adapter as &dyn ShellEscapeHandler),
                Some(&file_operation_adapter),
            );
            let Some(next_document) = next.document.clone() else {
                break;
            };

            output = next;
            document = next_document;
            current_page_labels = page_labels;
            current_index_entries = index_entries;
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

fn partition_plan_for_document(
    primary_input: &Path,
    document: &ParsedDocument,
    source_tree: &LoadedSourceTree,
) -> DocumentPartitionPlan {
    let section_outline = document
        .section_entries
        .iter()
        .map(SectionOutlineEntry::from)
        .collect::<Vec<_>>();
    let mut partition_plan =
        DocumentPartitionPlanner::plan(primary_input, &document.document_class, &section_outline);
    assign_partition_entry_files(&mut partition_plan, &source_tree.source_lines);
    partition_plan
}

fn assign_partition_entry_files(
    partition_plan: &mut DocumentPartitionPlan,
    source_lines: &[SourceLineTrace],
) {
    let Some(first_work_unit) = partition_plan.work_units.first() else {
        return;
    };
    let Some(command_name) = command_name_for_partition_level(first_work_unit.locator.level) else {
        return;
    };

    let entry_files = source_lines
        .iter()
        .filter_map(|line| {
            let content = line.text.split('%').next().unwrap_or_default().trim_start();
            matches_partition_command(content, command_name)
                .then(|| normalize_existing_path(Path::new(&line.file)))
        })
        .collect::<Vec<_>>();
    if entry_files.len() != partition_plan.work_units.len() {
        return;
    }

    for (work_unit, entry_file) in partition_plan.work_units.iter_mut().zip(entry_files) {
        work_unit.locator.entry_file = entry_file;
    }
}

fn command_name_for_partition_level(level: u8) -> Option<&'static str> {
    match level {
        0 => Some("\\chapter"),
        1 => Some("\\section"),
        2 => Some("\\subsection"),
        3 => Some("\\subsubsection"),
        _ => None,
    }
}

fn matches_partition_command(content: &str, command_name: &str) -> bool {
    content
        .strip_prefix(command_name)
        .map(|rest| {
            rest.starts_with('{')
                || rest.starts_with('[')
                || rest.starts_with('*')
                || rest.starts_with(char::is_whitespace)
        })
        .unwrap_or(false)
}

fn cached_document_layout_fragments_for(
    partition_plan: &DocumentPartitionPlan,
    source_tree: &LoadedSourceTree,
    cached_typeset_fragments: &BTreeMap<String, CachedTypesetFragment>,
) -> BTreeMap<String, ferritex_core::typesetting::DocumentLayoutFragment> {
    partition_plan
        .work_units
        .iter()
        .filter_map(|work_unit| {
            let cached_fragment = cached_typeset_fragments.get(&work_unit.partition_id)?;
            let current_hash = source_tree
                .dependency_graph
                .nodes
                .get(&work_unit.locator.entry_file)
                .map(|node| node.content_hash)?;
            (current_hash == cached_fragment.source_hash).then(|| {
                (
                    work_unit.partition_id.clone(),
                    cached_fragment.fragment.clone(),
                )
            })
        })
        .collect()
}

fn slice_line_columns(text: &str, start_column: u32, end_column: u32) -> Option<String> {
    if start_column == 0 || end_column < start_column {
        return None;
    }
    let start = usize::try_from(start_column - 1).ok()?;
    let len = usize::try_from(end_column - start_column).ok()?;
    Some(text.chars().skip(start).take(len).collect())
}

fn source_files_for_lines(source_lines: &[SourceLineTrace]) -> Vec<&str> {
    let mut files = Vec::new();
    for line in source_lines {
        if !files.iter().any(|candidate| *candidate == line.file) {
            files.push(line.file.as_str());
        }
    }
    files
}

fn extract_text_for_span(source_lines: &[SourceLineTrace], span: SourceSpan) -> Option<String> {
    if span.start.file_id != span.end.file_id {
        return None;
    }
    let files = source_files_for_lines(source_lines);
    let file = files.get(usize::try_from(span.start.file_id).ok()?)?;
    let file_lines = source_lines
        .iter()
        .filter(|line| line.file == *file)
        .collect::<Vec<_>>();
    let mut parts = Vec::new();

    for line in file_lines
        .into_iter()
        .filter(|line| (span.start.line..=span.end.line).contains(&line.line))
    {
        let text = if span.start.line == span.end.line {
            slice_line_columns(&line.text, span.start.column, span.end.column)?
        } else if line.line == span.start.line {
            slice_line_columns(
                &line.text,
                span.start.column,
                u32::try_from(line.text.chars().count() + 1).ok()?,
            )?
        } else if line.line == span.end.line {
            slice_line_columns(&line.text, 1, span.end.column)?
        } else {
            line.text.clone()
        };
        parts.push(text);
    }

    Some(parts.join("\n"))
}

fn normalize_block_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn block_signature(nodes: &[DocumentNode], source_lines: &[SourceLineTrace]) -> String {
    fn collect_parts(
        nodes: &[DocumentNode],
        source_lines: &[SourceLineTrace],
        parts: &mut Vec<String>,
    ) {
        for node in nodes {
            match node {
                DocumentNode::Text(text, _) => parts.push(normalize_block_text(text)),
                DocumentNode::FontFamily { children, .. }
                | DocumentNode::Bold { children }
                | DocumentNode::Italic { children }
                | DocumentNode::SmallCaps { children }
                | DocumentNode::Underline { children }
                | DocumentNode::HBox(children)
                | DocumentNode::VBox(children) => collect_parts(children, source_lines, parts),
                DocumentNode::Link { children, .. } => collect_parts(children, source_lines, parts),
                DocumentNode::DisplayMath(_, span) => {
                    let text = span
                        .and_then(|span| extract_text_for_span(source_lines, span))
                        .unwrap_or_else(|| "<display-math>".to_string());
                    parts.push(normalize_block_text(&text));
                }
                DocumentNode::EquationEnv { source_span, .. } => {
                    let text = source_span
                        .and_then(|span| extract_text_for_span(source_lines, span))
                        .unwrap_or_else(|| "<equation-env>".to_string());
                    parts.push(normalize_block_text(&text));
                }
                DocumentNode::IncludeGraphics { path, .. } => {
                    parts.push(format!("<includegraphics:{path}>"));
                }
                DocumentNode::TikzPicture { .. } => parts.push("<tikzpicture>".to_string()),
                DocumentNode::Float {
                    content,
                    caption,
                    label,
                    ..
                } => {
                    parts.push("<float>".to_string());
                    collect_parts(content, source_lines, parts);
                    if let Some(caption) = caption {
                        parts.push(normalize_block_text(caption));
                    }
                    if let Some(label) = label {
                        parts.push(format!("<label:{label}>"));
                    }
                }
                DocumentNode::ParBreak => parts.push("<parbreak>".to_string()),
                DocumentNode::LineBreak => parts.push("<linebreak>".to_string()),
                DocumentNode::PageBreak => parts.push("<pagebreak>".to_string()),
                DocumentNode::ClearPage => parts.push("<clearpage>".to_string()),
                DocumentNode::ClearDoublePage => parts.push("<cleardoublepage>".to_string()),
                DocumentNode::IndexMarker(entry) => parts.push(format!("<index:{entry:?}>")),
                DocumentNode::InlineMath(nodes) => parts.push(format!("<inline-math:{nodes:?}>")),
                DocumentNode::Multicols {
                    children,
                    column_count,
                    ..
                } => {
                    parts.push(format!("<multicols:{column_count}>"));
                    collect_parts(children, source_lines, parts);
                }
                DocumentNode::ColumnBreak => parts.push("<columnbreak>".to_string()),
            }
        }
    }

    let mut parts = Vec::new();
    collect_parts(nodes, source_lines, &mut parts);
    parts.join("|")
}

fn checkpoint_suffix_range<T>(
    checkpoints: &[T],
    index: usize,
    nodes_len: usize,
    node_index: impl Fn(&T) -> usize,
) -> Option<(usize, usize)> {
    let start = node_index(checkpoints.get(index)?);
    if start > nodes_len {
        return None;
    }
    let end = checkpoints
        .get(index + 1)
        .map(&node_index)
        .unwrap_or(nodes_len);
    (end <= nodes_len && start <= end).then_some((start, end))
}

#[cfg(test)]
fn find_affected_block_index(
    cached_checkpoints: &[BlockCheckpoint],
    cached_nodes: &[DocumentNode],
    cached_source_lines: &[SourceLineTrace],
    current_checkpoints: &[RawBlockCheckpoint],
    current_nodes: &[DocumentNode],
    current_source_lines: &[SourceLineTrace],
    cached_source: &str,
    current_source: &str,
) -> Option<usize> {
    if cached_source == current_source {
        return None;
    }
    if cached_checkpoints.is_empty() || current_checkpoints.is_empty() {
        return Some(0);
    }
    if cached_checkpoints.len() != current_checkpoints.len() {
        return Some(0);
    }

    for index in 0..cached_checkpoints.len() {
        let cached_checkpoint = &cached_checkpoints[index];
        let current_checkpoint = &current_checkpoints[index];
        match (
            cached_checkpoint.source_span,
            current_checkpoint.source_span,
        ) {
            (Some(cached_span), Some(current_span)) => {
                let cached_text = extract_text_for_span(cached_source_lines, cached_span);
                let current_text = extract_text_for_span(current_source_lines, current_span);
                if cached_text != current_text {
                    return Some(index);
                }
            }
            (None, None) => {
                let Some(cached_range) = checkpoint_suffix_range(
                    cached_checkpoints,
                    index,
                    cached_nodes.len(),
                    |item| item.node_index,
                ) else {
                    return Some(0);
                };
                let Some(current_range) = checkpoint_suffix_range(
                    current_checkpoints,
                    index,
                    current_nodes.len(),
                    |item| item.node_index,
                ) else {
                    return Some(0);
                };
                let cached_signature = block_signature(
                    &cached_nodes[cached_range.0..cached_range.1],
                    cached_source_lines,
                );
                let current_signature = block_signature(
                    &current_nodes[current_range.0..current_range.1],
                    current_source_lines,
                );
                if cached_signature != current_signature {
                    return Some(index);
                }
            }
            _ => return Some(0),
        }
    }

    Some(0)
}

fn wrapped_partition_source(document_class: &str, body_source: &str) -> String {
    format!(
        "\\documentclass{{{document_class}}}\n\\begin{{document}}\n{body_source}\n\\end{{document}}\n"
    )
}

fn parse_partition_body_document(
    service: &CompileJobService<'_>,
    document_class: &str,
    body_source: &str,
) -> Option<ParsedDocument> {
    service
        .parser
        .parse_recovering_with_context_and_handlers(
            &wrapped_partition_source(document_class, body_source),
            BTreeMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            BTreeMap::new(),
            None,
            BTreeMap::new(),
            Vec::new(),
            None,
            None,
            None,
        )
        .document
}

fn count_footnotes_in_text(text: &str) -> usize {
    text.match_indices(r"\footnote").count()
}

fn count_footnotes_in_nodes(nodes: &[DocumentNode]) -> usize {
    fn count(children: &[DocumentNode]) -> usize {
        children
            .iter()
            .map(|node| match node {
                DocumentNode::Text(text, _) => count_footnotes_in_text(text),
                DocumentNode::FontFamily { children, .. }
                | DocumentNode::HBox(children)
                | DocumentNode::VBox(children) => count(children),
                DocumentNode::Link { children, .. } => count(children),
                DocumentNode::Float { content, .. } => count(content),
                _ => 0,
            })
            .sum()
    }

    count(nodes)
}

fn count_floats_in_nodes(nodes: &[DocumentNode]) -> (u32, u32) {
    fn count(children: &[DocumentNode]) -> (u32, u32) {
        let mut figures = 0u32;
        let mut tables = 0u32;
        for node in children {
            match node {
                DocumentNode::Float {
                    float_type,
                    content,
                    ..
                } => {
                    match float_type {
                        FloatType::Figure => figures += 1,
                        FloatType::Table => tables += 1,
                    }
                    let (child_figures, child_tables) = count(content);
                    figures += child_figures;
                    tables += child_tables;
                }
                DocumentNode::FontFamily { children, .. }
                | DocumentNode::HBox(children)
                | DocumentNode::VBox(children) => {
                    let (child_figures, child_tables) = count(children);
                    figures += child_figures;
                    tables += child_tables;
                }
                DocumentNode::Link { children, .. } => {
                    let (child_figures, child_tables) = count(children);
                    figures += child_figures;
                    tables += child_tables;
                }
                _ => {}
            }
        }
        (figures, tables)
    }

    count(nodes)
}

fn count_rendered_float_placements(pages: &[TypesetPage]) -> usize {
    pages
        .iter()
        .map(|page| page.float_placements.len())
        .sum::<usize>()
}

fn count_rendered_footnote_markers(pages: &[TypesetPage]) -> usize {
    let footnote_font_size = DimensionValue(8 * SCALED_POINTS_PER_POINT);
    let footnote_text_gap = DimensionValue(3 * SCALED_POINTS_PER_POINT);

    pages
        .iter()
        .map(|page| {
            page.lines
                .iter()
                .filter(|line| {
                    line.source_span.is_none()
                        && line.font_size == footnote_font_size
                        && !line.text.is_empty()
                        && line.text.chars().all(|ch| ch.is_ascii_digit())
                        && page.lines.iter().any(|candidate| {
                            candidate.font_size == footnote_font_size
                                && candidate.y == line.y - footnote_text_gap
                                && (candidate.source_span.is_some()
                                    || !candidate.text.chars().all(|ch| ch.is_ascii_digit()))
                        })
                })
                .count()
        })
        .sum()
}

fn append_cached_footnote_lines(
    target: &mut TypesetPage,
    cached_page: &TypesetPage,
    prefix_footnote_count: usize,
) {
    if prefix_footnote_count == 0 {
        return;
    }

    let footnote_font_size = DimensionValue(8 * SCALED_POINTS_PER_POINT);
    let mut cached_footnote_lines = cached_page
        .lines
        .iter()
        .filter(|line| line.font_size == footnote_font_size)
        .cloned()
        .collect::<Vec<_>>();
    cached_footnote_lines.sort_by(|left, right| right.y.cmp(&left.y));
    target.lines.extend(
        cached_footnote_lines
            .into_iter()
            .take(prefix_footnote_count.saturating_mul(2)),
    );
    target.lines.sort_by(|left, right| right.y.cmp(&left.y));
}

fn raw_checkpoints_to_block_checkpoint_data(
    raw_checkpoints: &[RawBlockCheckpoint],
    body_nodes: &[DocumentNode],
    vlist_result: &PartitionVListResult,
    source_hash: u64,
    partition_body: String,
) -> BlockCheckpointData {
    let checkpoints = raw_checkpoints
        .iter()
        .filter_map(|raw_checkpoint| {
            let prefix_nodes = body_nodes.get(..raw_checkpoint.node_index)?;
            let snapshot = snapshot_paginated_vlist_state(
                &vlist_result.vlist[..raw_checkpoint.vlist_position],
                &vlist_result.page_box,
                vlist_result.layout,
                DimensionValue::zero(),
                FloatQueue::new(),
            );
            let (figure_count, table_count) = count_floats_in_nodes(prefix_nodes);
            Some(BlockCheckpoint {
                node_index: raw_checkpoint.node_index,
                source_span: raw_checkpoint.source_span,
                layout_state: BlockLayoutState {
                    content_used: snapshot.content_used,
                    completed_page_count: snapshot.completed_page_count,
                    pending_floats: snapshot
                        .pending_floats
                        .into_iter()
                        .map(|item| PendingFloat {
                            spec: item.spec,
                            content: item.content,
                            defer_count: item.defer_count,
                        })
                        .collect(),
                    footnote_count: count_footnotes_in_nodes(prefix_nodes),
                    figure_count,
                    table_count,
                },
            })
        })
        .collect();

    BlockCheckpointData {
        checkpoints,
        source_hash,
        partition_body,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockReuseDecision {
    BlockReuse {
        total_block_count: usize,
    },
    SuffixRebuild {
        affected_checkpoint_index: usize,
        suffix_block_count: usize,
        total_block_count: usize,
    },
    FullRebuild {
        reason: PartitionTypesetFallbackReason,
        total_block_count: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuffixRebuildFailure {
    FootnoteCountMismatch,
    FloatCountMismatch,
    #[allow(dead_code)]
    PageCountChanged {
        expected: usize,
        got: usize,
    },
    FloatPlacementCountMismatch,
    FootnoteMarkerCountMismatch,
    SlicingFailed,
}

#[derive(Debug)]
struct PartialTypesetExecution {
    document: TypesetDocument,
    partition_details: Vec<PartitionTypesetDetail>,
}

fn block_count_from_checkpoint_count(checkpoint_count: usize, has_content: bool) -> usize {
    if has_content {
        checkpoint_count.saturating_add(1)
    } else {
        0
    }
}

fn partition_block_count_from_cached_fragment(fragment: Option<&CachedTypesetFragment>) -> usize {
    fragment
        .and_then(|fragment| fragment.block_checkpoints.as_ref())
        .map(|data| {
            block_count_from_checkpoint_count(
                data.checkpoints.len(),
                !data.partition_body.trim().is_empty(),
            )
        })
        .unwrap_or(0)
}

fn cached_partition_detail(
    partition_id: &str,
    total_block_count: usize,
    reuse_type: PartitionTypesetReuseType,
) -> PartitionTypesetDetail {
    PartitionTypesetDetail {
        partition_id: partition_id.to_string(),
        reuse_type,
        suffix_block_count: 0,
        total_block_count,
        elapsed: Some(std::time::Duration::ZERO),
        fallback_reason: None,
    }
}

fn full_rebuild_partition_detail(
    partition_id: &str,
    total_block_count: usize,
    elapsed: Option<std::time::Duration>,
    reason: PartitionTypesetFallbackReason,
) -> PartitionTypesetDetail {
    PartitionTypesetDetail {
        partition_id: partition_id.to_string(),
        reuse_type: PartitionTypesetReuseType::FullRebuild,
        suffix_block_count: total_block_count,
        total_block_count,
        elapsed,
        fallback_reason: Some(reason),
    }
}

fn synthetic_full_rebuild_partition_details(
    partition_plan: &DocumentPartitionPlan,
    cached_typeset_fragments: &BTreeMap<String, CachedTypesetFragment>,
    reason: PartitionTypesetFallbackReason,
) -> Vec<PartitionTypesetDetail> {
    partition_plan
        .work_units
        .iter()
        .map(|work_unit| {
            full_rebuild_partition_detail(
                &work_unit.partition_id,
                partition_block_count_from_cached_fragment(
                    cached_typeset_fragments.get(&work_unit.partition_id),
                ),
                None,
                reason,
            )
        })
        .collect()
}

fn classify_block_reuse_decision(
    cached_checkpoints: &[BlockCheckpoint],
    cached_nodes: &[DocumentNode],
    cached_source_lines: &[SourceLineTrace],
    current_checkpoints: &[RawBlockCheckpoint],
    current_nodes: &[DocumentNode],
    current_source_lines: &[SourceLineTrace],
    cached_source: &str,
    current_source: &str,
) -> BlockReuseDecision {
    let total_block_count =
        block_count_from_checkpoint_count(current_checkpoints.len(), !current_nodes.is_empty());
    if cached_source == current_source {
        return BlockReuseDecision::BlockReuse { total_block_count };
    }
    if cached_checkpoints.is_empty() || current_checkpoints.is_empty() {
        return BlockReuseDecision::FullRebuild {
            reason: PartitionTypesetFallbackReason::CheckpointMissing,
            total_block_count,
        };
    }
    if cached_checkpoints.len() != current_checkpoints.len() {
        return BlockReuseDecision::FullRebuild {
            reason: PartitionTypesetFallbackReason::BlockStructureChanged,
            total_block_count,
        };
    }

    for index in 0..cached_checkpoints.len() {
        let cached_checkpoint = &cached_checkpoints[index];
        let current_checkpoint = &current_checkpoints[index];
        let mismatched = match (
            cached_checkpoint.source_span,
            current_checkpoint.source_span,
        ) {
            (Some(cached_span), Some(current_span)) => {
                extract_text_for_span(cached_source_lines, cached_span)
                    != extract_text_for_span(current_source_lines, current_span)
            }
            (None, None) => {
                let Some(cached_range) = checkpoint_suffix_range(
                    cached_checkpoints,
                    index,
                    cached_nodes.len(),
                    |item| item.node_index,
                ) else {
                    return BlockReuseDecision::FullRebuild {
                        reason: PartitionTypesetFallbackReason::BlockStructureChanged,
                        total_block_count,
                    };
                };
                let Some(current_range) = checkpoint_suffix_range(
                    current_checkpoints,
                    index,
                    current_nodes.len(),
                    |item| item.node_index,
                ) else {
                    return BlockReuseDecision::FullRebuild {
                        reason: PartitionTypesetFallbackReason::BlockStructureChanged,
                        total_block_count,
                    };
                };
                let cached_signature = block_signature(
                    &cached_nodes[cached_range.0..cached_range.1],
                    cached_source_lines,
                );
                let current_signature = block_signature(
                    &current_nodes[current_range.0..current_range.1],
                    current_source_lines,
                );
                cached_signature != current_signature
            }
            _ => {
                return BlockReuseDecision::FullRebuild {
                    reason: PartitionTypesetFallbackReason::BlockStructureChanged,
                    total_block_count,
                };
            }
        };

        if !mismatched {
            continue;
        }

        if index == 0 {
            return BlockReuseDecision::SuffixRebuild {
                affected_checkpoint_index: 0,
                suffix_block_count: total_block_count,
                total_block_count,
            };
        }

        return BlockReuseDecision::SuffixRebuild {
            affected_checkpoint_index: index,
            suffix_block_count: total_block_count.saturating_sub(index + 1),
            total_block_count,
        };
    }

    BlockReuseDecision::FullRebuild {
        reason: PartitionTypesetFallbackReason::AffectedBlockIndexZero,
        total_block_count,
    }
}

#[allow(clippy::too_many_arguments)]
fn suffix_rebuild(
    service: &CompileJobService<'_>,
    context_document: &ParsedDocument,
    current_nodes: &[DocumentNode],
    selection: &CompileFontSelection,
    graphics_resolver: &CompileGraphicAssetResolver<'_>,
    cached_fragment: &CachedTypesetFragment,
    checkpoint_data: &BlockCheckpointData,
    affected_block_index: usize,
) -> Result<DocumentLayoutFragment, SuffixRebuildFailure> {
    let checkpoint = checkpoint_data
        .checkpoints
        .get(affected_block_index)
        .ok_or(SuffixRebuildFailure::SlicingFailed)?;
    let prefix_nodes = current_nodes
        .get(..checkpoint.node_index)
        .ok_or(SuffixRebuildFailure::SlicingFailed)?
        .to_vec();
    let prefix_vlist_result = service.build_vlist_from_body_nodes_with_selection(
        context_document,
        prefix_nodes,
        selection,
        graphics_resolver,
        false,
        FloatCounters::default(),
        None,
    );
    let prefix_snapshot = snapshot_paginated_vlist_state(
        &prefix_vlist_result.vlist,
        &prefix_vlist_result.page_box,
        prefix_vlist_result.layout,
        DimensionValue::zero(),
        FloatQueue::new(),
    );
    let mut result_pages = cached_fragment
        .fragment
        .pages
        .get(..checkpoint.layout_state.completed_page_count)
        .ok_or(SuffixRebuildFailure::SlicingFailed)?
        .to_vec();
    let mut prefix_active_page = prefix_snapshot.current_page;
    if let (Some(page), Some(cached_page)) = (
        prefix_active_page.as_mut(),
        cached_fragment
            .fragment
            .pages
            .get(checkpoint.layout_state.completed_page_count),
    ) {
        append_cached_footnote_lines(page, cached_page, checkpoint.layout_state.footnote_count);
    }
    let suffix_nodes = current_nodes
        .get(checkpoint.node_index..)
        .ok_or(SuffixRebuildFailure::SlicingFailed)?
        .to_vec();
    let total_footnotes = count_footnotes_in_nodes(current_nodes);
    let (total_figures, total_tables) = count_floats_in_nodes(current_nodes);
    let (suffix_figures, suffix_tables) = count_floats_in_nodes(&suffix_nodes);
    let vlist_result = service.build_vlist_from_body_nodes_with_selection(
        context_document,
        suffix_nodes,
        selection,
        graphics_resolver,
        true,
        FloatCounters {
            figure: checkpoint.layout_state.figure_count,
            table: checkpoint.layout_state.table_count,
        },
        None,
    );
    let initial_float_queue = FloatQueue::from_items(
        checkpoint
            .layout_state
            .pending_floats
            .iter()
            .cloned()
            .map(|item| FloatItem {
                spec: item.spec,
                content: item.content,
                defer_count: item.defer_count,
            })
            .collect(),
    );
    if checkpoint.layout_state.footnote_count + vlist_result.footnotes.len() != total_footnotes {
        return Err(SuffixRebuildFailure::FootnoteCountMismatch);
    }

    if checkpoint.layout_state.figure_count + suffix_figures != total_figures
        || checkpoint.layout_state.table_count + suffix_tables != total_tables
    {
        return Err(SuffixRebuildFailure::FloatCountMismatch);
    }

    let pagination = paginate_vlist_continuing_with_state(
        &vlist_result.vlist,
        &vlist_result.page_box,
        vlist_result.layout,
        checkpoint.layout_state.content_used,
        initial_float_queue,
    );
    let mut rebuilt_pages = pagination.pages;

    if pagination.continued_on_initial_page {
        let first_page = rebuilt_pages
            .first_mut()
            .ok_or(SuffixRebuildFailure::SlicingFailed)?;
        append_footnotes_to_pages_starting_at(
            std::slice::from_mut(first_page),
            &vlist_result.footnotes,
            vlist_result.layout,
            checkpoint.layout_state.footnote_count,
        );
        let continued_page = rebuilt_pages.remove(0);
        if let Some(mut merged_page) = prefix_active_page {
            merge_continued_page(&mut merged_page, continued_page);
            result_pages.push(merged_page);
        } else {
            result_pages.push(continued_page);
        }
        result_pages.extend(rebuilt_pages);
    } else {
        if let Some(prefix_page) = prefix_active_page {
            result_pages.push(prefix_page);
        }
        append_footnotes_to_pages_starting_at(
            &mut rebuilt_pages,
            &vlist_result.footnotes,
            vlist_result.layout,
            checkpoint.layout_state.footnote_count,
        );
        result_pages.extend(rebuilt_pages);
    }

    if count_rendered_float_placements(&result_pages) != (total_figures + total_tables) as usize {
        return Err(SuffixRebuildFailure::FloatPlacementCountMismatch);
    }

    if count_rendered_footnote_markers(&result_pages) != total_footnotes {
        return Err(SuffixRebuildFailure::FootnoteMarkerCountMismatch);
    }

    let finalized = finalize_partitioned_typeset_document(context_document, result_pages);
    Ok(DocumentLayoutFragment {
        partition_id: cached_fragment.fragment.partition_id.clone(),
        pages: finalized.pages,
        local_label_pages: finalized
            .named_destinations
            .iter()
            .map(|destination| (destination.name.clone(), destination.page_index))
            .collect(),
        outlines: finalized.outlines,
        named_destinations: finalized.named_destinations,
    })
}

fn try_partial_typeset_document(
    service: &CompileJobService<'_>,
    document: &ParsedDocument,
    source_lines: &[SourceLineTrace],
    selection: &CompileFontSelection,
    graphics_resolver: &CompileGraphicAssetResolver<'_>,
    parallelism: usize,
    file_access_gate: &dyn FileAccessGate,
    input_dir: &Path,
    project_root: &Path,
    overlay_roots: &[PathBuf],
    asset_bundle_path: Option<&Path>,
    partition_plan: &DocumentPartitionPlan,
    reuse_plan: &TypesetterReusePlan,
    cached_typeset_fragments: &BTreeMap<String, CachedTypesetFragment>,
    current_source_subtrees: &BTreeMap<PathBuf, CachedSourceSubtree>,
    cached_source_subtrees: &BTreeMap<PathBuf, CachedSourceSubtree>,
) -> Result<PartialTypesetExecution, &'static str> {
    let body_ranges =
        partition_body_ranges(document, partition_plan).ok_or("partition body slicing failed")?;
    let section_ranges = partition_section_ranges(document, partition_plan)
        .ok_or("partition section slicing failed")?;
    let mut fragments = reuse_plan.reuse_fragments.clone();
    let mut partition_details = partition_plan
        .work_units
        .iter()
        .filter(|work_unit| {
            reuse_plan
                .reuse_fragments
                .contains_key(&work_unit.partition_id)
        })
        .map(|work_unit| {
            (
                work_unit.partition_id.clone(),
                cached_partition_detail(
                    &work_unit.partition_id,
                    partition_block_count_from_cached_fragment(
                        cached_typeset_fragments.get(&work_unit.partition_id),
                    ),
                    PartitionTypesetReuseType::Cached,
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let rebuild_work_units = partition_plan
        .work_units
        .iter()
        .filter(|work_unit| {
            reuse_plan
                .rebuild_partition_ids
                .contains(&work_unit.partition_id)
        })
        .collect::<Vec<_>>();

    if parallelism > 1 && rebuild_work_units.len() >= 2 {
        let work_items =
            collect_partial_typeset_work_items(&rebuild_work_units, &body_ranges, &section_ranges)?;
        tracing::info!(
            rebuilt_partitions = work_items.len(),
            parallelism,
            "partial typeset rebuild executing in parallel"
        );
        let rebuilt_fragments = run_parallel_partial_typeset(
            service,
            document,
            source_lines,
            selection,
            parallelism,
            work_items,
            file_access_gate,
            input_dir,
            project_root,
            overlay_roots,
            asset_bundle_path,
        )?;
        if has_cross_partition_layout_collision(
            rebuilt_fragments.iter().map(|result| &result.fragment),
        ) {
            return Err("authority key collision in parallel typeset, falling back to sequential");
        }
        for rebuilt in rebuilt_fragments {
            for diagnostic in rebuilt.diagnostics {
                graphics_resolver.push_diagnostic(diagnostic);
            }
            partition_details.insert(
                rebuilt.partition_id.clone(),
                full_rebuild_partition_detail(
                    &rebuilt.partition_id,
                    partition_block_count_from_cached_fragment(
                        cached_typeset_fragments.get(&rebuilt.partition_id),
                    ),
                    Some(rebuilt.elapsed),
                    PartitionTypesetFallbackReason::ParallelPartialTypeset,
                ),
            );
            fragments.insert(rebuilt.partition_id, rebuilt.fragment);
        }
    } else {
        for work_unit in rebuild_work_units {
            let partition_document = partition_document_for_work_unit(
                document,
                work_unit,
                body_ranges
                    .get(&work_unit.partition_id)
                    .copied()
                    .ok_or("missing body range for rebuilt partition")?,
                section_ranges
                    .get(&work_unit.partition_id)
                    .copied()
                    .ok_or("missing section range for rebuilt partition")?,
            )
            .ok_or("failed to build partition-scoped parsed document")?;
            let cached_fragment = cached_typeset_fragments
                .get(&work_unit.partition_id)
                .ok_or("missing cached fragment for rebuilt partition")?;
            let current_subtree = current_source_subtrees.get(&work_unit.locator.entry_file);
            let cached_subtree = cached_source_subtrees.get(&work_unit.locator.entry_file);

            let (fragment, detail) = if let (
                Some(checkpoint_data),
                Some(current_subtree),
                Some(cached_subtree),
            ) = (
                cached_fragment.block_checkpoints.as_ref(),
                current_subtree,
                cached_subtree,
            ) {
                let (_annotated_current_document, current_nodes) =
                    annotated_body_nodes(&partition_document, Some(&current_subtree.source_lines))
                        .ok_or("failed to annotate current partition body nodes")?;
                let mut current_raw_checkpoints = Vec::new();
                let _ = service.build_vlist_from_body_nodes_with_selection(
                    &partition_document,
                    current_nodes.clone(),
                    selection,
                    graphics_resolver,
                    false,
                    FloatCounters::default(),
                    Some(&mut current_raw_checkpoints),
                );

                let cached_partition_document = if checkpoint_data.partition_body.is_empty() {
                    parse_partition_body_document(
                        service,
                        &document.document_class,
                        &cached_subtree.text,
                    )
                    .ok_or("failed to parse cached partition body")?
                } else {
                    ParsedDocument {
                        document_class: document.document_class.clone(),
                        class_options: document.class_options.clone(),
                        loaded_packages: document.loaded_packages.clone(),
                        package_count: document.package_count,
                        main_font_name: document.main_font_name.clone(),
                        sans_font_name: document.sans_font_name.clone(),
                        mono_font_name: document.mono_font_name.clone(),
                        body: checkpoint_data.partition_body.clone(),
                        labels: document.labels.clone(),
                        bibliography_state: document.bibliography_state.clone(),
                        has_maketitle: document.has_maketitle,
                        has_unresolved_refs: document.has_unresolved_refs,
                    }
                };
                let (_annotated_cached_document, cached_nodes) = annotated_body_nodes(
                    &cached_partition_document,
                    Some(&cached_subtree.source_lines),
                )
                .ok_or("failed to annotate cached partition body nodes")?;

                match classify_block_reuse_decision(
                    &checkpoint_data.checkpoints,
                    &cached_nodes,
                    &cached_subtree.source_lines,
                    &current_raw_checkpoints,
                    &current_nodes,
                    &current_subtree.source_lines,
                    &cached_subtree.text,
                    &current_subtree.text,
                ) {
                    BlockReuseDecision::BlockReuse { total_block_count } => (
                        cached_fragment.fragment.clone(),
                        cached_partition_detail(
                            &work_unit.partition_id,
                            total_block_count,
                            PartitionTypesetReuseType::BlockReuse,
                        ),
                    ),
                    BlockReuseDecision::FullRebuild {
                        reason,
                        total_block_count,
                    } => {
                        let partition_typeset_start = std::time::Instant::now();
                        let partition_typeset = service.typeset_document_with_selection(
                            &partition_document,
                            Some(source_lines),
                            selection,
                            graphics_resolver,
                        );
                        (
                            extract_rebuilt_fragment(partition_typeset, work_unit)?,
                            full_rebuild_partition_detail(
                                &work_unit.partition_id,
                                total_block_count,
                                Some(partition_typeset_start.elapsed()),
                                reason,
                            ),
                        )
                    }
                    BlockReuseDecision::SuffixRebuild {
                        affected_checkpoint_index,
                        suffix_block_count,
                        total_block_count,
                    } => {
                        let partition_typeset_start = std::time::Instant::now();
                        let rebuild_result = suffix_rebuild(
                            service,
                            &partition_document,
                            &current_nodes,
                            selection,
                            graphics_resolver,
                            cached_fragment,
                            checkpoint_data,
                            affected_checkpoint_index,
                        );
                        match rebuild_result {
                            Ok(fragment) => (
                                fragment,
                                PartitionTypesetDetail {
                                    partition_id: work_unit.partition_id.clone(),
                                    reuse_type: PartitionTypesetReuseType::SuffixRebuild,
                                    suffix_block_count,
                                    total_block_count,
                                    elapsed: Some(partition_typeset_start.elapsed()),
                                    fallback_reason: None,
                                },
                            ),
                            Err(failure) => {
                                let partition_typeset = service.typeset_document_with_selection(
                                    &partition_document,
                                    Some(source_lines),
                                    selection,
                                    graphics_resolver,
                                );
                                (
                                    extract_rebuilt_fragment(partition_typeset, work_unit)?,
                                    full_rebuild_partition_detail(
                                        &work_unit.partition_id,
                                        total_block_count,
                                        Some(partition_typeset_start.elapsed()),
                                        PartitionTypesetFallbackReason::SuffixValidationFailed(
                                            failure,
                                        ),
                                    ),
                                )
                            }
                        }
                    }
                }
            } else {
                let partition_typeset_start = std::time::Instant::now();
                let partition_typeset = service.typeset_document_with_selection(
                    &partition_document,
                    Some(source_lines),
                    selection,
                    graphics_resolver,
                );
                (
                    extract_rebuilt_fragment(partition_typeset, work_unit)?,
                    full_rebuild_partition_detail(
                        &work_unit.partition_id,
                        partition_block_count_from_cached_fragment(Some(cached_fragment)),
                        Some(partition_typeset_start.elapsed()),
                        PartitionTypesetFallbackReason::CheckpointMissing,
                    ),
                )
            };
            partition_details.insert(work_unit.partition_id.clone(), detail);
            fragments.insert(work_unit.partition_id.clone(), fragment);
        }
    }

    let merged = PaginationMergeCoordinator.merge_owned(
        partition_plan,
        fragments,
        &navigation_state_for_document(document),
    );
    if !merged_matches_reuse_expectations(&merged, partition_plan, &reuse_plan.reuse_fragments) {
        return Err("merged reuse fragments did not preserve cached label/page counts");
    }

    Ok(PartialTypesetExecution {
        document: merged,
        partition_details: partition_plan
            .work_units
            .iter()
            .filter_map(|work_unit| partition_details.get(&work_unit.partition_id).cloned())
            .collect(),
    })
}

fn try_parallel_full_typeset(
    service: &CompileJobService<'_>,
    document: &ParsedDocument,
    source_lines: &[SourceLineTrace],
    selection: &CompileFontSelection,
    graphics_resolver: &CompileGraphicAssetResolver<'_>,
    parallelism: usize,
    partition_plan: &DocumentPartitionPlan,
    file_access_gate: &dyn FileAccessGate,
    input_dir: &Path,
    project_root: &Path,
    overlay_roots: &[PathBuf],
    asset_bundle_path: Option<&Path>,
    pass_number: u32,
) -> Result<TypesetDocument, &'static str> {
    if parallelism <= 1 || partition_plan.work_units.len() < 2 {
        return Err("parallel full typeset requires at least two partitions");
    }

    let body_ranges =
        partition_body_ranges(document, partition_plan).ok_or("partition body slicing failed")?;
    let section_ranges = partition_section_ranges(document, partition_plan)
        .ok_or("partition section slicing failed")?;
    let (coalesced_plan, coalesced_body_ranges, coalesced_section_ranges) =
        coalesce_full_typeset_partitions(
            partition_plan,
            &body_ranges,
            &section_ranges,
            parallelism,
        )?;
    let work_items = collect_full_typeset_work_items(
        &coalesced_plan,
        &coalesced_body_ranges,
        &coalesced_section_ranges,
    )?;

    tracing::info!(
        original_partitions = partition_plan.work_units.len(),
        coalesced_groups = work_items.len(),
        parallelism,
        pass_number,
        "full typeset executing in parallel with coalesced partitions"
    );

    let is_chapter_level = partition_plan
        .work_units
        .iter()
        .all(|unit| unit.kind == PartitionKind::Chapter);

    if !is_chapter_level {
        return run_parallel_pipelined_full_typeset(
            service,
            document,
            source_lines,
            selection,
            graphics_resolver,
            parallelism,
            &coalesced_plan,
            work_items,
            file_access_gate,
            input_dir,
            project_root,
            overlay_roots,
            asset_bundle_path,
        );
    }

    let parallel_results = run_parallel_full_typeset(
        service,
        document,
        source_lines,
        selection,
        parallelism,
        work_items,
        file_access_gate,
        input_dir,
        project_root,
        overlay_roots,
        asset_bundle_path,
    )?;
    if should_force_parallel_full_typeset_collision() {
        tracing::warn!(
            pass_number,
            "parallel full typeset authority key collision; falling back to sequential"
        );
        return Err("authority key collision in parallel full typeset");
    }
    if has_cross_partition_layout_collision(
        parallel_results.iter().map(|(_, fragment, _)| fragment),
    ) {
        tracing::warn!(
            pass_number,
            "parallel full typeset authority key collision; falling back to sequential"
        );
        return Err("authority key collision in parallel full typeset");
    }

    let mut fragments = BTreeMap::new();
    for (partition_id, fragment, diagnostics) in parallel_results {
        for diagnostic in diagnostics {
            graphics_resolver.push_diagnostic(diagnostic);
        }
        fragments.insert(partition_id, fragment);
    }
    if fragments.len() < coalesced_plan.work_units.len() {
        return Err("parallel full typeset produced incomplete layout fragments");
    }

    Ok(PaginationMergeCoordinator.merge_owned(
        &coalesced_plan,
        fragments,
        &navigation_state_for_document(document),
    ))
}

fn run_parallel_pipelined_full_typeset(
    service: &CompileJobService<'_>,
    document: &ParsedDocument,
    source_lines: &[SourceLineTrace],
    selection: &CompileFontSelection,
    graphics_resolver: &CompileGraphicAssetResolver<'_>,
    parallelism: usize,
    coalesced_plan: &DocumentPartitionPlan,
    work_items: Vec<FullTypesetWorkItem>,
    file_access_gate: &dyn FileAccessGate,
    input_dir: &Path,
    project_root: &Path,
    overlay_roots: &[PathBuf],
    asset_bundle_path: Option<&Path>,
) -> Result<TypesetDocument, &'static str> {
    let built_vlists = run_parallel_vlist_build(
        service,
        document,
        source_lines,
        selection,
        parallelism,
        work_items,
        file_access_gate,
        input_dir,
        project_root,
        overlay_roots,
        asset_bundle_path,
    )?;

    let mut vlist_results = BTreeMap::new();
    for built in built_vlists {
        for diagnostic in built.diagnostics {
            graphics_resolver.push_diagnostic(diagnostic);
        }
        vlist_results.insert(built.partition_id, built.vlist_result);
    }
    if vlist_results.len() < coalesced_plan.work_units.len() {
        return Err("parallel full typeset produced incomplete vlist results");
    }

    let mut all_pages = Vec::new();
    let mut content_offset = DimensionValue::zero();

    for work_unit in &coalesced_plan.work_units {
        let vlist_result = vlist_results
            .remove(&work_unit.partition_id)
            .ok_or("missing pipelined vlist result for coalesced partition")?;
        let pagination = paginate_vlist_continuing_detailed(
            &vlist_result.vlist,
            &vlist_result.page_box,
            vlist_result.layout,
            content_offset,
        );
        let mut pages = pagination.pages;

        if pagination.continued_on_initial_page {
            let first_page = pages
                .first_mut()
                .ok_or("continued partition must produce at least one page")?;
            append_footnotes_to_pages(
                std::slice::from_mut(first_page),
                &vlist_result.footnotes,
                vlist_result.layout,
            );
            let continued_page = pages.remove(0);
            let previous_page = all_pages
                .last_mut()
                .ok_or("continued partition requires an existing page")?;
            merge_continued_page(previous_page, continued_page);
        } else {
            append_footnotes_to_pages(&mut pages, &vlist_result.footnotes, vlist_result.layout);
        }

        all_pages.extend(pages);
        content_offset = pagination.next_partition_content_used;
    }

    Ok(finalize_partitioned_typeset_document(document, all_pages))
}

fn merge_continued_page(target: &mut TypesetPage, mut continued: TypesetPage) {
    target.lines.append(&mut continued.lines);
    target.lines.sort_by(|left, right| right.y.cmp(&left.y));
    target.images.append(&mut continued.images);
    target
        .float_placements
        .append(&mut continued.float_placements);
    target.index_entries.append(&mut continued.index_entries);
}

#[derive(Debug)]
struct PartialTypesetWorkItem {
    work_unit: DocumentWorkUnit,
    body_range: (usize, usize),
    section_range: (usize, usize),
}

#[derive(Debug)]
struct FullTypesetWorkItem {
    work_unit: DocumentWorkUnit,
    body_range: (usize, usize),
    section_range: (usize, usize),
}

#[derive(Debug)]
struct BuiltPartitionVList {
    partition_id: String,
    vlist_result: PartitionVListResult,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Debug)]
struct TypesetPartitionResult {
    partition_id: String,
    fragment: DocumentLayoutFragment,
    diagnostics: Vec<Diagnostic>,
    elapsed: std::time::Duration,
}

fn distribute_round_robin<T>(items: Vec<T>, concurrency: usize) -> Vec<Vec<T>> {
    if items.is_empty() {
        return Vec::new();
    }

    let worker_count = concurrency.max(1).min(items.len());
    let mut groups = (0..worker_count).map(|_| Vec::new()).collect::<Vec<_>>();
    for (index, item) in items.into_iter().enumerate() {
        groups[index % worker_count].push(item);
    }
    groups
}

fn collect_partial_typeset_work_items(
    rebuild_work_units: &[&DocumentWorkUnit],
    body_ranges: &BTreeMap<String, (usize, usize)>,
    section_ranges: &BTreeMap<String, (usize, usize)>,
) -> Result<Vec<PartialTypesetWorkItem>, &'static str> {
    rebuild_work_units
        .iter()
        .map(|work_unit| {
            Ok(PartialTypesetWorkItem {
                work_unit: (*work_unit).clone(),
                body_range: body_ranges
                    .get(&work_unit.partition_id)
                    .copied()
                    .ok_or("missing body range for rebuilt partition")?,
                section_range: section_ranges
                    .get(&work_unit.partition_id)
                    .copied()
                    .ok_or("missing section range for rebuilt partition")?,
            })
        })
        .collect()
}

fn run_parallel_partial_typeset(
    service: &CompileJobService<'_>,
    document: &ParsedDocument,
    source_lines: &[SourceLineTrace],
    selection: &CompileFontSelection,
    parallelism: usize,
    work_items: Vec<PartialTypesetWorkItem>,
    file_access_gate: &dyn FileAccessGate,
    input_dir: &Path,
    project_root: &Path,
    overlay_roots: &[PathBuf],
    asset_bundle_path: Option<&Path>,
) -> Result<Vec<TypesetPartitionResult>, &'static str> {
    let concurrency = parallelism.max(1);
    let work_item_count = work_items.len();
    let mut groups = distribute_round_robin(work_items, concurrency);
    let inline_group = groups.pop().unwrap_or_default();

    let results = thread::scope(|scope| -> Result<Vec<_>, &'static str> {
        let handles = groups
            .into_iter()
            .map(|group| {
                scope.spawn(move || {
                    execute_partial_typeset_group(
                        service,
                        document,
                        source_lines,
                        selection,
                        group,
                        file_access_gate,
                        input_dir,
                        project_root,
                        overlay_roots,
                        asset_bundle_path,
                    )
                })
            })
            .collect::<Vec<_>>();

        let mut results = Vec::with_capacity(work_item_count);
        results.extend(execute_partial_typeset_group(
            service,
            document,
            source_lines,
            selection,
            inline_group,
            file_access_gate,
            input_dir,
            project_root,
            overlay_roots,
            asset_bundle_path,
        )?);
        for handle in handles {
            let worker_results = handle.join().expect("partial typeset thread panicked")?;
            results.extend(worker_results);
        }
        Ok(results)
    })?;

    Ok(results)
}

fn collect_full_typeset_work_items(
    partition_plan: &DocumentPartitionPlan,
    body_ranges: &BTreeMap<String, (usize, usize)>,
    section_ranges: &BTreeMap<String, (usize, usize)>,
) -> Result<Vec<FullTypesetWorkItem>, &'static str> {
    partition_plan
        .work_units
        .iter()
        .map(|work_unit| {
            Ok(FullTypesetWorkItem {
                work_unit: work_unit.clone(),
                body_range: body_ranges
                    .get(&work_unit.partition_id)
                    .copied()
                    .ok_or("missing body range for partition")?,
                section_range: section_ranges
                    .get(&work_unit.partition_id)
                    .copied()
                    .ok_or("missing section range for partition")?,
            })
        })
        .collect()
}

fn coalesce_full_typeset_partitions(
    partition_plan: &DocumentPartitionPlan,
    body_ranges: &BTreeMap<String, (usize, usize)>,
    section_ranges: &BTreeMap<String, (usize, usize)>,
    parallelism: usize,
) -> Result<
    (
        DocumentPartitionPlan,
        BTreeMap<String, (usize, usize)>,
        BTreeMap<String, (usize, usize)>,
    ),
    &'static str,
> {
    let work_unit_count = partition_plan.work_units.len();
    let group_count = parallelism.max(1).min(work_unit_count);
    let base_chunk_size = work_unit_count / group_count;
    let remainder = work_unit_count % group_count;
    let mut coalesced_work_units = Vec::with_capacity(group_count);
    let mut coalesced_body_ranges = BTreeMap::new();
    let mut coalesced_section_ranges = BTreeMap::new();
    let mut chunk_start = 0usize;

    for chunk_index in 0..group_count {
        let chunk_len = base_chunk_size + usize::from(chunk_index < remainder);
        let chunk_end = chunk_start + chunk_len;
        let chunk = partition_plan
            .work_units
            .get(chunk_start..chunk_end)
            .ok_or("parallel full typeset requires at least one partition chunk")?;
        let first = chunk
            .first()
            .ok_or("parallel full typeset requires at least one partition chunk")?;
        let last = chunk
            .last()
            .ok_or("parallel full typeset requires at least one partition chunk")?;
        let first_body = body_ranges
            .get(&first.partition_id)
            .copied()
            .ok_or("missing body range for coalesced partition")?;
        let last_body = body_ranges
            .get(&last.partition_id)
            .copied()
            .ok_or("missing body range for coalesced partition")?;
        let first_section = section_ranges
            .get(&first.partition_id)
            .copied()
            .ok_or("missing section range for coalesced partition")?;
        let last_section = section_ranges
            .get(&last.partition_id)
            .copied()
            .ok_or("missing section range for coalesced partition")?;
        let group_id = first.partition_id.clone();

        coalesced_body_ranges.insert(group_id.clone(), (first_body.0, last_body.1));
        coalesced_section_ranges.insert(group_id.clone(), (first_section.0, last_section.1));
        coalesced_work_units.push(DocumentWorkUnit {
            partition_id: group_id,
            kind: first.kind,
            locator: first.locator.clone(),
            title: first.title.clone(),
        });
        chunk_start = chunk_end;
    }

    Ok((
        DocumentPartitionPlan {
            fallback_partition_id: partition_plan.fallback_partition_id.clone(),
            work_units: coalesced_work_units,
        },
        coalesced_body_ranges,
        coalesced_section_ranges,
    ))
}

fn run_parallel_full_typeset(
    service: &CompileJobService<'_>,
    document: &ParsedDocument,
    source_lines: &[SourceLineTrace],
    selection: &CompileFontSelection,
    parallelism: usize,
    work_items: Vec<FullTypesetWorkItem>,
    file_access_gate: &dyn FileAccessGate,
    input_dir: &Path,
    project_root: &Path,
    overlay_roots: &[PathBuf],
    asset_bundle_path: Option<&Path>,
) -> Result<Vec<(String, DocumentLayoutFragment, Vec<Diagnostic>)>, &'static str> {
    let concurrency = parallelism.max(1);
    let work_item_count = work_items.len();
    let mut groups = distribute_round_robin(work_items, concurrency);
    let inline_group = groups.pop().unwrap_or_default();

    let results = thread::scope(|scope| -> Result<Vec<_>, &'static str> {
        let handles = groups
            .into_iter()
            .map(|group| {
                scope.spawn(move || {
                    execute_full_typeset_group(
                        service,
                        document,
                        source_lines,
                        selection,
                        group,
                        file_access_gate,
                        input_dir,
                        project_root,
                        overlay_roots,
                        asset_bundle_path,
                    )
                })
            })
            .collect::<Vec<_>>();

        let mut results = Vec::with_capacity(work_item_count);
        results.extend(execute_full_typeset_group(
            service,
            document,
            source_lines,
            selection,
            inline_group,
            file_access_gate,
            input_dir,
            project_root,
            overlay_roots,
            asset_bundle_path,
        )?);
        for handle in handles {
            let worker_results = handle.join().expect("full typeset thread panicked")?;
            results.extend(worker_results);
        }
        Ok(results)
    })?;

    Ok(results)
}

fn run_parallel_vlist_build(
    service: &CompileJobService<'_>,
    document: &ParsedDocument,
    source_lines: &[SourceLineTrace],
    selection: &CompileFontSelection,
    parallelism: usize,
    work_items: Vec<FullTypesetWorkItem>,
    file_access_gate: &dyn FileAccessGate,
    input_dir: &Path,
    project_root: &Path,
    overlay_roots: &[PathBuf],
    asset_bundle_path: Option<&Path>,
) -> Result<Vec<BuiltPartitionVList>, &'static str> {
    let concurrency = parallelism.max(1);
    let work_item_count = work_items.len();
    let mut groups = distribute_round_robin(work_items, concurrency);
    let inline_group = groups.pop().unwrap_or_default();

    let results = thread::scope(|scope| -> Result<Vec<_>, &'static str> {
        let handles = groups
            .into_iter()
            .map(|group| {
                scope.spawn(move || {
                    execute_vlist_build_group(
                        service,
                        document,
                        source_lines,
                        selection,
                        group,
                        file_access_gate,
                        input_dir,
                        project_root,
                        overlay_roots,
                        asset_bundle_path,
                    )
                })
            })
            .collect::<Vec<_>>();

        let mut results = Vec::with_capacity(work_item_count);
        results.extend(execute_vlist_build_group(
            service,
            document,
            source_lines,
            selection,
            inline_group,
            file_access_gate,
            input_dir,
            project_root,
            overlay_roots,
            asset_bundle_path,
        )?);
        for handle in handles {
            let worker_results = handle.join().expect("vlist build thread panicked")?;
            results.extend(worker_results);
        }
        Ok(results)
    })?;

    Ok(results)
}

fn execute_partial_typeset_group(
    service: &CompileJobService<'_>,
    document: &ParsedDocument,
    source_lines: &[SourceLineTrace],
    selection: &CompileFontSelection,
    group: Vec<PartialTypesetWorkItem>,
    file_access_gate: &dyn FileAccessGate,
    input_dir: &Path,
    project_root: &Path,
    overlay_roots: &[PathBuf],
    asset_bundle_path: Option<&Path>,
) -> Result<Vec<TypesetPartitionResult>, &'static str> {
    let thread_resolver = CompileGraphicAssetResolver {
        file_access_gate,
        input_dir,
        project_root,
        overlay_roots,
        asset_bundle_path,
        diagnostics: RefCell::new(Vec::new()),
    };
    let mut worker_results = Vec::with_capacity(group.len());
    for work_item in group {
        let partition_document = partition_document_for_work_unit(
            document,
            &work_item.work_unit,
            work_item.body_range,
            work_item.section_range,
        )
        .ok_or("failed to build partition-scoped parsed document")?;
        let partition_typeset_start = std::time::Instant::now();
        let partition_typeset = service.typeset_document_with_selection(
            &partition_document,
            Some(source_lines),
            selection,
            &thread_resolver,
        );
        let fragment = extract_rebuilt_fragment(partition_typeset, &work_item.work_unit)?;
        worker_results.push(TypesetPartitionResult {
            partition_id: work_item.work_unit.partition_id,
            fragment,
            diagnostics: thread_resolver.take_diagnostics(),
            elapsed: partition_typeset_start.elapsed(),
        });
    }
    Ok(worker_results)
}

fn execute_full_typeset_group(
    service: &CompileJobService<'_>,
    document: &ParsedDocument,
    source_lines: &[SourceLineTrace],
    selection: &CompileFontSelection,
    group: Vec<FullTypesetWorkItem>,
    file_access_gate: &dyn FileAccessGate,
    input_dir: &Path,
    project_root: &Path,
    overlay_roots: &[PathBuf],
    asset_bundle_path: Option<&Path>,
) -> Result<Vec<(String, DocumentLayoutFragment, Vec<Diagnostic>)>, &'static str> {
    let thread_resolver = CompileGraphicAssetResolver {
        file_access_gate,
        input_dir,
        project_root,
        overlay_roots,
        asset_bundle_path,
        diagnostics: RefCell::new(Vec::new()),
    };
    let mut worker_results = Vec::with_capacity(group.len());
    for work_item in group {
        let partition_document = partition_document_for_work_unit(
            document,
            &work_item.work_unit,
            work_item.body_range,
            work_item.section_range,
        )
        .ok_or("failed to build partition-scoped parsed document")?;
        let partition_typeset = service.typeset_document_with_selection(
            &partition_document,
            Some(source_lines),
            selection,
            &thread_resolver,
        );
        let fragment = extract_rebuilt_fragment(partition_typeset, &work_item.work_unit)?;
        worker_results.push((
            work_item.work_unit.partition_id.clone(),
            fragment,
            thread_resolver.take_diagnostics(),
        ));
    }
    Ok(worker_results)
}

fn execute_vlist_build_group(
    service: &CompileJobService<'_>,
    document: &ParsedDocument,
    source_lines: &[SourceLineTrace],
    selection: &CompileFontSelection,
    group: Vec<FullTypesetWorkItem>,
    file_access_gate: &dyn FileAccessGate,
    input_dir: &Path,
    project_root: &Path,
    overlay_roots: &[PathBuf],
    asset_bundle_path: Option<&Path>,
) -> Result<Vec<BuiltPartitionVList>, &'static str> {
    let thread_resolver = CompileGraphicAssetResolver {
        file_access_gate,
        input_dir,
        project_root,
        overlay_roots,
        asset_bundle_path,
        diagnostics: RefCell::new(Vec::new()),
    };
    let mut worker_results = Vec::with_capacity(group.len());
    for work_item in group {
        let partition_document = partition_document_for_work_unit(
            document,
            &work_item.work_unit,
            work_item.body_range,
            work_item.section_range,
        )
        .ok_or("failed to build partition-scoped parsed document")?;
        let continues_from_previous_block =
            section_partition_continues_from_previous_block(document, &work_item);
        let initial_float_counters =
            float_counters_before_body_range(document, work_item.body_range.0);
        let vlist_result = service.build_partition_vlist_with_selection(
            document,
            &partition_document,
            Some(source_lines),
            selection,
            &thread_resolver,
            continues_from_previous_block,
            initial_float_counters,
        );
        worker_results.push(BuiltPartitionVList {
            partition_id: work_item.work_unit.partition_id,
            vlist_result,
            diagnostics: thread_resolver.take_diagnostics(),
        });
    }
    Ok(worker_results)
}

fn section_partition_continues_from_previous_block(
    document: &ParsedDocument,
    work_item: &FullTypesetWorkItem,
) -> bool {
    const BODY_PAGE_BREAK_MARKER: char = '\u{E000}';
    const BODY_CLEAR_PAGE_MARKER: char = '\u{E01B}';
    const BODY_CLEAR_DOUBLE_PAGE_MARKER: char = '\u{E02A}';

    if !matches!(work_item.work_unit.kind, PartitionKind::Section)
        || work_item.work_unit.locator.ordinal == 0
    {
        return false;
    }

    let Some(prefix) = document.body.get(..work_item.body_range.0) else {
        return true;
    };
    let last_marker = prefix.chars().rev().find(|ch| !ch.is_whitespace());

    !matches!(
        last_marker,
        Some(BODY_PAGE_BREAK_MARKER | BODY_CLEAR_PAGE_MARKER | BODY_CLEAR_DOUBLE_PAGE_MARKER)
    )
}

fn float_counters_before_body_range(document: &ParsedDocument, body_start: usize) -> FloatCounters {
    let mut prefix_document = document.clone();
    prefix_document.body = document
        .body
        .get(..body_start)
        .unwrap_or_default()
        .to_string();
    let (figure, table) = count_floats_in_nodes(&prefix_document.body_nodes());
    FloatCounters { figure, table }
}

fn should_force_parallel_full_typeset_collision() -> bool {
    #[cfg(test)]
    {
        return FORCE_PARALLEL_FULL_TYPESET_COLLISION.load(std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(not(test))]
    {
        false
    }
}

fn extract_rebuilt_fragment(
    partition_typeset: TypesetDocument,
    work_unit: &DocumentWorkUnit,
) -> Result<DocumentLayoutFragment, &'static str> {
    let TypesetDocument {
        pages,
        outlines,
        named_destinations,
        ..
    } = partition_typeset;
    let local_label_pages = named_destinations
        .iter()
        .map(|destination| (destination.name.clone(), destination.page_index))
        .collect();
    Ok(DocumentLayoutFragment {
        partition_id: work_unit.partition_id.clone(),
        pages,
        local_label_pages,
        outlines,
        named_destinations,
    })
}

fn has_cross_partition_layout_collision<'a>(
    fragments: impl IntoIterator<Item = &'a DocumentLayoutFragment>,
) -> bool {
    let mut label_owners = BTreeMap::new();
    let mut destination_owners = BTreeMap::new();

    for fragment in fragments {
        for label in fragment.local_label_pages.keys() {
            if label_owners
                .insert(label.clone(), fragment.partition_id.clone())
                .is_some_and(|owner| owner != fragment.partition_id)
            {
                return true;
            }
        }
        for destination in &fragment.named_destinations {
            if destination_owners
                .insert(destination.name.clone(), fragment.partition_id.clone())
                .is_some_and(|owner| owner != fragment.partition_id)
            {
                return true;
            }
        }
    }

    false
}

fn partition_document_for_work_unit(
    document: &ParsedDocument,
    work_unit: &DocumentWorkUnit,
    body_range: (usize, usize),
    section_range: (usize, usize),
) -> Option<ParsedDocument> {
    let (body_start, body_end) = body_range;
    let (section_start, section_end) = section_range;
    let mut labels = document
        .labels
        .clone_with_section_entries(document.section_entries[section_start..section_end].to_vec());
    if labels.section_entries.is_empty() {
        labels.section_entries = vec![document
            .section_entries
            .get(work_unit.locator.ordinal)?
            .clone()];
    }
    Some(ParsedDocument {
        document_class: document.document_class.clone(),
        class_options: document.class_options.clone(),
        loaded_packages: document.loaded_packages.clone(),
        package_count: document.package_count,
        main_font_name: document.main_font_name.clone(),
        sans_font_name: document.sans_font_name.clone(),
        mono_font_name: document.mono_font_name.clone(),
        body: document.body.get(body_start..body_end)?.to_string(),
        labels,
        bibliography_state: document.bibliography_state.clone(),
        has_maketitle: document.has_maketitle,
        has_unresolved_refs: document.has_unresolved_refs,
    })
}

fn partition_body_ranges(
    document: &ParsedDocument,
    partition_plan: &DocumentPartitionPlan,
) -> Option<BTreeMap<String, (usize, usize)>> {
    if partition_plan.work_units.len() <= 1 {
        let partition_id = partition_plan
            .work_units
            .first()
            .map(|work_unit| work_unit.partition_id.clone())
            .unwrap_or_else(|| partition_plan.fallback_partition_id.clone());
        return Some(BTreeMap::from([(partition_id, (0, document.body.len()))]));
    }

    let mut starts = vec![0usize; partition_plan.work_units.len()];
    let mut upper_bound = document.body.len();
    for index in (1..partition_plan.work_units.len()).rev() {
        let title = &partition_plan.work_units[index].title;
        let start = partition_heading_offset(&document.body[..upper_bound], title)?;
        starts[index] = start;
        upper_bound = start;
    }

    Some(
        partition_plan
            .work_units
            .iter()
            .enumerate()
            .map(|(index, work_unit)| {
                let end = starts
                    .get(index + 1)
                    .copied()
                    .unwrap_or(document.body.len());
                (work_unit.partition_id.clone(), (starts[index], end))
            })
            .collect(),
    )
}

fn partition_heading_offset(body: &str, title: &str) -> Option<usize> {
    let heading_with_prefix = format!("\n{title}\n\n");
    if let Some(offset) = body.rfind(&heading_with_prefix) {
        return Some(offset + 1);
    }

    body.rfind(&format!("{title}\n\n"))
}

fn partition_section_ranges(
    document: &ParsedDocument,
    partition_plan: &DocumentPartitionPlan,
) -> Option<BTreeMap<String, (usize, usize)>> {
    let level = partition_plan.work_units.first()?.locator.level;
    let top_level_indices = document
        .section_entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| (entry.level == level).then_some(index))
        .collect::<Vec<_>>();
    if top_level_indices.len() != partition_plan.work_units.len() {
        return None;
    }

    Some(
        partition_plan
            .work_units
            .iter()
            .enumerate()
            .map(|(index, work_unit)| {
                let start = top_level_indices[index];
                let end = top_level_indices
                    .get(index + 1)
                    .copied()
                    .unwrap_or(document.section_entries.len());
                (work_unit.partition_id.clone(), (start, end))
            })
            .collect(),
    )
}

fn navigation_state_for_document(document: &ParsedDocument) -> NavigationState {
    let mut named_destinations = document
        .labels
        .keys()
        .map(|name| (name.clone(), DestinationAnchor { name: name.clone() }))
        .collect::<BTreeMap<_, _>>();
    for entry in &document.section_entries {
        let title = entry.display_title();
        if title.is_empty() {
            continue;
        }
        let name = format!("section:{title}");
        named_destinations
            .entry(name.clone())
            .or_insert_with(|| DestinationAnchor { name });
    }
    if let Some(snapshot) = document.bibliography_state.bbl.as_ref() {
        for entry in &snapshot.entries {
            let name = format!("bib:{}", entry.key);
            named_destinations
                .entry(name.clone())
                .or_insert_with(|| DestinationAnchor { name });
        }
    }

    NavigationState {
        metadata: PdfMetadataDraft {
            title: document
                .labels
                .pdf_title
                .clone()
                .or_else(|| document.title.clone()),
            author: document
                .labels
                .pdf_author
                .clone()
                .or_else(|| document.author.clone()),
        },
        outline_entries: document
            .section_entries
            .iter()
            .filter_map(|entry| {
                let title = entry.display_title();
                (!title.is_empty()).then_some(OutlineDraftEntry {
                    level: entry.level,
                    title,
                })
            })
            .collect(),
        named_destinations,
        default_link_style: LinkStyle {
            color_links: document.labels.color_links.unwrap_or(false),
            link_color: document.labels.link_color.clone(),
        },
    }
}

fn merged_matches_reuse_expectations(
    merged: &TypesetDocument,
    partition_plan: &DocumentPartitionPlan,
    reuse_fragments: &BTreeMap<String, ferritex_core::typesetting::DocumentLayoutFragment>,
) -> bool {
    let merged_fragments = merged.extract_fragments(partition_plan);
    reuse_fragments
        .iter()
        .all(|(partition_id, _)| merged_fragments.contains_key(partition_id))
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
    let session = compilation_job.begin_pass(pass_count);
    StableCompileState {
        snapshot: CompilationSnapshot::derive_snapshot(
            &session,
            &RegisterStore::default(),
            &document_state,
        ),
        document_state,
        cross_reference_seed,
        page_count,
        success,
        diagnostics,
    }
}

fn cached_typeset_fragments_for(
    service: &CompileJobService<'_>,
    parsed_document: &ParsedDocument,
    document: &TypesetDocument,
    partition_plan: &ferritex_core::compilation::DocumentPartitionPlan,
    source_tree: &LoadedSourceTree,
    selection: &CompileFontSelection,
    graphics_resolver: &CompileGraphicAssetResolver<'_>,
) -> BTreeMap<String, CachedTypesetFragment> {
    let body_ranges = partition_body_ranges(parsed_document, partition_plan);
    let section_ranges = partition_section_ranges(parsed_document, partition_plan);

    document
        .extract_fragments(partition_plan)
        .into_iter()
        .map(|(partition_id, fragment)| {
            let (source_hash, block_checkpoints) = partition_plan
                .work_units
                .iter()
                .find(|work_unit| work_unit.partition_id == partition_id)
                .map(|work_unit| {
                    let source_hash = source_tree
                        .dependency_graph
                        .nodes
                        .get(&work_unit.locator.entry_file)
                        .map(|node| node.content_hash)
                        .unwrap_or_default();
                    let block_checkpoints = source_tree
                        .cached_source_subtrees
                        .get(&work_unit.locator.entry_file)
                        .and_then(|subtree| {
                            let body_range = body_ranges.as_ref()?.get(&partition_id).copied()?;
                            let section_range =
                                section_ranges.as_ref()?.get(&partition_id).copied()?;
                            let partition_document = partition_document_for_work_unit(
                                parsed_document,
                                work_unit,
                                body_range,
                                section_range,
                            )?;
                            let (_annotated_document, body_nodes) = annotated_body_nodes(
                                &partition_document,
                                Some(&subtree.source_lines),
                            )?;
                            let mut raw_checkpoints = Vec::new();
                            let vlist_result = service.build_vlist_from_body_nodes_with_selection(
                                &partition_document,
                                body_nodes.clone(),
                                selection,
                                graphics_resolver,
                                false,
                                FloatCounters::default(),
                                Some(&mut raw_checkpoints),
                            );
                            Some(raw_checkpoints_to_block_checkpoint_data(
                                &raw_checkpoints,
                                &body_nodes,
                                &vlist_result,
                                source_hash,
                                partition_document.body.clone(),
                            ))
                        });
                    (source_hash, block_checkpoints)
                })
                .unwrap_or((0, None));

            (
                partition_id,
                CachedTypesetFragment {
                    fragment,
                    source_hash,
                    block_checkpoints,
                },
            )
        })
        .collect()
}

fn cached_page_payloads_for(
    document: &TypesetDocument,
    partition_plan: &DocumentPartitionPlan,
    page_payloads: &[PageRenderPayload],
) -> BTreeMap<String, Vec<CachedPagePayload>> {
    ordered_partition_page_ranges(document, partition_plan)
        .into_iter()
        .filter_map(|(partition_id, range)| {
            page_payloads.get(range.clone()).map(|payloads| {
                (
                    partition_id,
                    payloads
                        .iter()
                        .cloned()
                        .map(CachedPagePayload::from)
                        .collect::<Vec<_>>(),
                )
            })
        })
        .collect()
}

fn reusable_page_payloads_for_render(
    document: &TypesetDocument,
    partition_plan: &DocumentPartitionPlan,
    partition_details: Option<&[PartitionTypesetDetail]>,
    cached_page_payloads: &BTreeMap<String, Vec<CachedPagePayload>>,
) -> BTreeMap<usize, PageRenderPayload> {
    let Some(partition_details) = partition_details else {
        return BTreeMap::new();
    };

    let reusable_partition_ids = partition_details
        .iter()
        .filter(|detail| {
            matches!(
                detail.reuse_type,
                PartitionTypesetReuseType::Cached | PartitionTypesetReuseType::BlockReuse
            )
        })
        .map(|detail| detail.partition_id.clone())
        .collect::<BTreeSet<_>>();

    let ordered_ranges = ordered_partition_page_ranges(document, partition_plan);
    if ordered_ranges
        .iter()
        .any(|(partition_id, _)| partition_id == &partition_plan.fallback_partition_id)
        || ordered_ranges
            .first()
            .is_some_and(|(_, range)| range.len() > 1)
    {
        return BTreeMap::new();
    }

    let mut reusable_page_payloads = BTreeMap::new();
    for (partition_id, range) in ordered_ranges {
        if !reusable_partition_ids.contains(&partition_id) {
            continue;
        }
        if range.clone().any(|page_index| {
            document
                .pages
                .get(page_index)
                .is_some_and(page_uses_reindexed_xobjects)
        }) {
            continue;
        }

        let Some(payloads) = cached_page_payloads.get(&partition_id) else {
            continue;
        };
        if payloads.len() != range.len() {
            continue;
        }

        let restored_payloads = payloads
            .iter()
            .enumerate()
            .map(|(offset, payload)| payload.to_page_render_payload(range.start + offset))
            .collect::<Option<Vec<_>>>();
        let Some(restored_payloads) = restored_payloads else {
            continue;
        };

        for payload in restored_payloads {
            reusable_page_payloads.insert(payload.page_index, payload);
        }
    }

    reusable_page_payloads
}

fn page_uses_reindexed_xobjects(page: &TypesetPage) -> bool {
    page.images.iter().any(|image| {
        image.scene.nodes.len() == 1
            && matches!(
                image.scene.nodes.first(),
                Some(GraphicNode::External(_) | GraphicNode::Pdf(_))
            )
    })
}

fn ordered_partition_page_ranges(
    document: &TypesetDocument,
    partition_plan: &DocumentPartitionPlan,
) -> Vec<(String, std::ops::Range<usize>)> {
    let page_partition_ids = page_partition_ids_for_plan(document, partition_plan);
    if page_partition_ids.is_empty() {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let mut start = 0usize;
    while start < page_partition_ids.len() {
        let partition_id = page_partition_ids[start].clone();
        let mut end = start + 1;
        while end < page_partition_ids.len() && page_partition_ids[end] == partition_id {
            end += 1;
        }
        ranges.push((partition_id, start..end));
        start = end;
    }
    ranges
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
            span: None,
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
            span: None,
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
    sidecar: Option<BibliographySidecarMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BibliographyContext {
    declarations: Vec<String>,
    addbibresources: Vec<String>,
    citation_keys: Vec<String>,
    style: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BibliographySidecarMetadata {
    input_fingerprint: BibliographyInputFingerprint,
    toolchain: BibliographyToolchain,
}

fn load_bibliography_state(
    file_access_gate: &dyn FileAccessGate,
    project_root: &Path,
    input_dir: &Path,
    overlay_roots: &[PathBuf],
    artifact_root: &Path,
    jobname: &str,
) -> Option<LoadedBibliographyState> {
    for candidate in bibliography_candidate_paths(
        project_root,
        input_dir,
        overlay_roots,
        artifact_root,
        jobname,
    ) {
        if !candidate.exists()
            || file_access_gate.check_read(&candidate) == PathAccessDecision::Denied
        {
            continue;
        }

        let Ok(bytes) = file_access_gate.read_file(&candidate) else {
            continue;
        };
        let input = String::from_utf8_lossy(&bytes);
        let sidecar = load_bibliography_sidecar(file_access_gate, &candidate);
        let mut state = BibliographyState::from_snapshot(parse_bbl(&input));
        if let Some(snapshot) = state.bbl.as_mut() {
            if let Some(sidecar) = &sidecar {
                snapshot.input_fingerprint = Some(sidecar.input_fingerprint.clone());
                snapshot.toolchain = Some(sidecar.toolchain);
            }
        }
        return Some(LoadedBibliographyState {
            state,
            path: candidate,
            sidecar,
        });
    }

    None
}

fn bibliography_candidate_paths(
    project_root: &Path,
    input_dir: &Path,
    overlay_roots: &[PathBuf],
    artifact_root: &Path,
    jobname: &str,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();

    for root in std::iter::once(artifact_root)
        .chain(std::iter::once(input_dir))
        .chain(std::iter::once(project_root))
        .chain(overlay_roots.iter().map(PathBuf::as_path))
    {
        let candidate = root.join(format!("{jobname}.bbl"));
        if seen.insert(candidate.clone()) {
            candidates.push(candidate);
        }
    }

    candidates
}

impl BibliographyContext {
    fn from_source(source: &str) -> Self {
        Self {
            declarations: extract_bibliography_declarations(source),
            addbibresources: extract_addbibresource_declarations(source),
            citation_keys: extract_citation_keys(source),
            style: extract_bibliography_style(source),
        }
    }

    fn has_citations(&self) -> bool {
        !self.citation_keys.is_empty()
    }

    fn toolchain(&self) -> Option<BibliographyToolchain> {
        if !self.addbibresources.is_empty() {
            Some(BibliographyToolchain::Biber)
        } else if !self.declarations.is_empty() {
            Some(BibliographyToolchain::Bibtex)
        } else {
            None
        }
    }

    fn current_fingerprint(
        &self,
        project_root: &Path,
        overlay_roots: &[PathBuf],
    ) -> Option<BibliographyInputFingerprint> {
        let toolchain = self.toolchain()?;
        let inputs = bibliography_input_paths(project_root, overlay_roots, self)
            .into_iter()
            .map(|path| {
                let hash = std::fs::read(&path)
                    .map(|bytes| format!("{:016x}", fingerprint_bytes(&bytes)))
                    .unwrap_or_else(|_| "unreadable".to_string());
                (path.to_string_lossy().into_owned(), hash)
            })
            .collect::<Vec<_>>();
        let canonical = json!({
            "toolchain": match toolchain {
                BibliographyToolchain::Bibtex => "bibtex",
                BibliographyToolchain::Biber => "biber",
            },
            "bibliography": self.declarations,
            "resources": self.addbibresources,
            "style": self.style,
            "inputs": inputs,
        });
        let bytes = serde_json::to_vec(&canonical).ok()?;
        Some(BibliographyInputFingerprint {
            hash: format!("{:016x}", fingerprint_bytes(&bytes)),
        })
    }

    fn bibtex_aux_contents(&self) -> Option<String> {
        if self.toolchain() != Some(BibliographyToolchain::Bibtex) || self.declarations.is_empty() {
            return None;
        }

        let mut lines = Vec::new();
        lines.push("\\relax".to_string());
        for key in &self.citation_keys {
            lines.push(format!("\\citation{{{key}}}"));
        }
        lines.push(format!(
            "\\bibstyle{{{}}}",
            self.style.as_deref().unwrap_or("plain")
        ));
        lines.push(format!("\\bibdata{{{}}}", self.declarations.join(",")));
        Some(format!("{}\n", lines.join("\n")))
    }
}

fn bibliography_sidecar_path(bbl_path: &Path) -> PathBuf {
    bbl_path.with_extension("bbl.ferritex.json")
}

fn load_bibliography_sidecar(
    file_access_gate: &dyn FileAccessGate,
    bbl_path: &Path,
) -> Option<BibliographySidecarMetadata> {
    let sidecar_path = bibliography_sidecar_path(bbl_path);
    if !sidecar_path.exists()
        || file_access_gate.check_read(&sidecar_path) == PathAccessDecision::Denied
    {
        return None;
    }

    let bytes = file_access_gate.read_file(&sidecar_path).ok()?;
    let value = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
    let hash = value
        .get("inputFingerprint")
        .and_then(|entry| entry.get("hash"))
        .and_then(serde_json::Value::as_str)?
        .to_string();
    let toolchain = match value.get("toolchain").and_then(serde_json::Value::as_str)? {
        "bibtex" => BibliographyToolchain::Bibtex,
        "biber" => BibliographyToolchain::Biber,
        _ => return None,
    };

    Some(BibliographySidecarMetadata {
        input_fingerprint: BibliographyInputFingerprint { hash },
        toolchain,
    })
}

fn extract_bibliography_declarations(source: &str) -> Vec<String> {
    extract_command_arguments(source, "bibliography", false)
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn extract_addbibresource_declarations(source: &str) -> Vec<String> {
    extract_command_arguments(source, "addbibresource", true)
}

fn extract_bibliography_style(source: &str) -> Option<String> {
    extract_command_arguments(source, "bibliographystyle", false)
        .into_iter()
        .next()
}

fn extract_citation_keys(source: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut seen = BTreeSet::new();
    for value in extract_command_arguments(source, "cite", true) {
        for key in value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            if seen.insert(key.to_string()) {
                keys.push(key.to_string());
            }
        }
    }
    keys
}

fn extract_command_arguments(source: &str, command: &str, skip_optional: bool) -> Vec<String> {
    let mut values = Vec::new();
    let mut cursor = 0usize;
    let needle = format!(r"\{command}");

    while let Some(relative_start) = source[cursor..].find(&needle) {
        let command_start = cursor + relative_start;
        let mut index = command_start + needle.len();
        if source[index..]
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
        {
            cursor = index;
            continue;
        }
        let trimmed = source[index..].trim_start();
        index += source[index..].len() - trimmed.len();

        if skip_optional && source[index..].starts_with('[') {
            let Some(optional_end) = find_matching_delimiter(source, index, '[', ']') else {
                break;
            };
            index = optional_end;
            let trimmed = source[index..].trim_start();
            index += source[index..].len() - trimmed.len();
        }

        if !source[index..].starts_with('{') {
            cursor = (index + 1).min(source.len());
            continue;
        }

        let Some(argument_end) = find_matching_delimiter(source, index, '{', '}') else {
            break;
        };
        let value = source[index + 1..argument_end - 1].trim();
        if !value.is_empty() {
            values.push(value.to_string());
        }
        cursor = argument_end;
    }

    values
}

fn find_matching_delimiter(input: &str, start: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut escaped = false;

    for (offset, ch) in input[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }

        if ch == '\\' {
            escaped = true;
            continue;
        }

        if ch == open {
            depth += 1;
        } else if ch == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(start + offset + ch.len_utf8());
            }
        }
    }

    None
}

fn check_bbl_freshness(
    loaded_bibliography_state: &LoadedBibliographyState,
    bibliography_context: &BibliographyContext,
    project_root: &Path,
    overlay_roots: &[PathBuf],
) -> Option<BibliographyDiagnostic> {
    let current_fingerprint = bibliography_context.current_fingerprint(project_root, overlay_roots);
    if let (Some(sidecar), Some(current_fingerprint)) = (
        &loaded_bibliography_state.sidecar,
        current_fingerprint.as_ref(),
    ) {
        if sidecar.toolchain
            != bibliography_context
                .toolchain()
                .unwrap_or(sidecar.toolchain)
            || sidecar.input_fingerprint != *current_fingerprint
        {
            return Some(BibliographyDiagnostic::StaleBbl {
                reason: format!(
                    "bibliography input fingerprint no longer matches `{}`",
                    loaded_bibliography_state.path.display()
                ),
            });
        }
        return None;
    }

    let bbl_modified = std::fs::metadata(&loaded_bibliography_state.path)
        .ok()?
        .modified()
        .ok()?;

    for candidate in bibliography_input_paths(project_root, overlay_roots, bibliography_context) {
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
                    loaded_bibliography_state.path.display()
                ),
            });
        }
    }

    None
}

fn bibliography_input_paths(
    project_root: &Path,
    overlay_roots: &[PathBuf],
    bibliography_context: &BibliographyContext,
) -> Vec<PathBuf> {
    bibliography_context
        .declarations
        .iter()
        .chain(bibliography_context.addbibresources.iter())
        .filter_map(|name| bibliography_input_path(project_root, overlay_roots, name))
        .collect()
}

fn bibliography_input_path(
    project_root: &Path,
    overlay_roots: &[PathBuf],
    bib_name: &str,
) -> Option<PathBuf> {
    let candidate_name = if Path::new(bib_name).extension().is_some() {
        PathBuf::from(bib_name)
    } else {
        PathBuf::from(format!("{bib_name}.bib"))
    };

    std::iter::once(project_root)
        .chain(overlay_roots.iter().map(PathBuf::as_path))
        .map(|root| root.join(&candidate_name))
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

fn input_dir_for_input(input_file: &Path, project_root: &Path) -> PathBuf {
    input_file
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .map(normalize_existing_path)
        .unwrap_or_else(|| project_root.to_path_buf())
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

fn synctex_data_for(document: &TypesetDocument, source_lines: &[SourceLineTrace]) -> SyncTexData {
    let mut annotated_document = document.clone();
    let annotator = SourceSpanAnnotator::new(source_lines);
    let mut used_source_lines = annotator.used_source_lines_for_document(document);
    used_source_lines.extend(annotator.annotate_pages(&mut annotated_document));
    let mut synctex =
        SyncTexData::build_from_placed_nodes(placed_text_nodes_for(&annotated_document));
    let remaining_source_lines = annotator.source_lines_without(source_lines, &used_source_lines);

    if !remaining_source_lines.is_empty() {
        let mut fallback = SyncTexData::build_line_based(
            &synctex_pages_for_unannotated(&annotated_document),
            &remaining_source_lines,
        );
        let (merged_files, fallback_file_ids) =
            merged_synctex_files(&annotator.files, &fallback.files);
        remap_synctex_file_ids(&mut fallback, &fallback_file_ids);
        synctex.fragments.extend(fallback.fragments);
        synctex.files = merged_files;
    } else if !annotator.files.is_empty() {
        synctex.files = annotator.files;
    }

    synctex
}

fn merged_synctex_files(
    primary_files: &[String],
    fallback_files: &[String],
) -> (Vec<String>, Vec<u32>) {
    let mut files = Vec::with_capacity(primary_files.len() + fallback_files.len());
    for file in primary_files {
        file_id_for_source(&mut files, file);
    }
    let fallback_file_ids = fallback_files
        .iter()
        .map(|file| file_id_for_source(&mut files, file))
        .collect();
    (files, fallback_file_ids)
}

fn remap_synctex_file_ids(synctex: &mut SyncTexData, file_id_map: &[u32]) {
    for fragment in &mut synctex.fragments {
        fragment.span.start.file_id =
            remap_synctex_file_id(fragment.span.start.file_id, file_id_map);
        fragment.span.end.file_id = remap_synctex_file_id(fragment.span.end.file_id, file_id_map);
    }
}

fn remap_synctex_file_id(file_id: u32, file_id_map: &[u32]) -> u32 {
    debug_assert!(
        file_id_map.get(file_id as usize).is_some(),
        "fallback SyncTeX file_id must reference fallback files"
    );
    file_id_map
        .get(file_id as usize)
        .copied()
        .unwrap_or(file_id)
}

fn source_span_contains_span(outer: SourceSpan, inner: SourceSpan) -> bool {
    source_location_lte(outer.start, inner.start) && source_location_lte(inner.end, outer.end)
}

fn source_location_lte(left: SourceLocation, right: SourceLocation) -> bool {
    (left.file_id, left.line, left.column) <= (right.file_id, right.line, right.column)
}

fn placed_text_nodes_for(document: &TypesetDocument) -> Vec<PlacedTextNode> {
    document
        .pages
        .iter()
        .enumerate()
        .flat_map(|(page_index, page)| {
            page.lines
                .iter()
                .chain(
                    page.float_placements
                        .iter()
                        .flat_map(|placement| placement.content.lines.iter()),
                )
                .filter_map(move |line| {
                    let span = line.source_span?;
                    synctex_eligible_line(line).then(|| {
                        PlacedTextNode::from_text_line(
                            line.text.clone(),
                            span,
                            page_index as u32 + 1,
                            line.y,
                        )
                    })
                })
        })
        .collect()
}

fn synctex_pages_for_unannotated(document: &TypesetDocument) -> Vec<RenderedPageTrace> {
    document
        .pages
        .iter()
        .map(|page| RenderedPageTrace {
            lines: page
                .lines
                .iter()
                .chain(
                    page.float_placements
                        .iter()
                        .flat_map(|placement| placement.content.lines.iter()),
                )
                .filter(|line| line.source_span.is_none() && synctex_eligible_line(line))
                .map(|line| RenderedLineTrace {
                    text: line.text.clone(),
                    y: line.y,
                })
                .collect(),
        })
        .collect()
}

fn collect_rendered_line_keys(document: &TypesetDocument) -> Vec<RenderedLineKey> {
    document
        .pages
        .iter()
        .enumerate()
        .flat_map(|(page_index, page)| {
            page.lines
                .iter()
                .enumerate()
                .filter_map(move |(line_index, line)| {
                    if line.source_span.is_some() || !synctex_eligible_line(line) {
                        return None;
                    }
                    let normalized_text = normalized_rendered_text(&line.text);
                    (!normalized_text.is_empty()).then_some(RenderedLineKey {
                        page_index,
                        line_index,
                        placement_index: None,
                        normalized_text,
                    })
                })
                .chain(page.float_placements.iter().enumerate().flat_map(
                    move |(placement_index, placement)| {
                        placement.content.lines.iter().enumerate().filter_map(
                            move |(line_index, line)| {
                                if line.source_span.is_some() || !synctex_eligible_line(line) {
                                    return None;
                                }
                                let normalized_text = normalized_rendered_text(&line.text);
                                (!normalized_text.is_empty()).then_some(RenderedLineKey {
                                    page_index,
                                    line_index,
                                    placement_index: Some(placement_index),
                                    normalized_text,
                                })
                            },
                        )
                    },
                ))
        })
        .collect()
}

fn normalized_rendered_text(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn synctex_eligible_line(line: &TextLine) -> bool {
    !line.text.trim().is_empty() && !is_synthetic_page_furniture_line(line)
}

fn is_synthetic_page_furniture_line(line: &TextLine) -> bool {
    line.links.is_empty()
        && line.font_size == DimensionValue(10 * SCALED_POINTS_PER_POINT)
        && line.text.trim().chars().all(|ch| ch.is_ascii_digit())
}

fn file_id_for_source(files: &mut Vec<String>, file: &str) -> u32 {
    if let Some(index) = files.iter().position(|entry| entry == file) {
        index as u32
    } else {
        files.push(file.to_string());
        (files.len() - 1) as u32
    }
}

fn visible_source_chars(text: &str) -> Vec<VisibleSourceChar> {
    let trimmed = text.trim_start();
    if let Some(command) = leading_control_word(trimmed) {
        if matches!(
            command,
            "documentclass"
                | "begin"
                | "end"
                | "usepackage"
                | "RequirePackage"
                | "InputIfFileExists"
                | "input"
                | "include"
                | "bibliography"
                | "tableofcontents"
                | "listoffigures"
                | "listoftables"
                | "makeindex"
                | "printindex"
                | "hypersetup"
        ) {
            return Vec::new();
        }
    }

    let mut visible = Vec::new();
    let mut chars = text.chars().peekable();
    let mut column = 1u32;

    while let Some(ch) = chars.next() {
        match ch {
            '%' => break,
            '\\' => {
                column += 1;
                while chars.peek().is_some_and(|next| next.is_alphabetic()) {
                    chars.next();
                    column += 1;
                }
            }
            '{' | '}' | '[' | ']' => {
                column += 1;
            }
            _ if !ch.is_control() => {
                if !ch.is_whitespace() {
                    visible.push(VisibleSourceChar { ch, column });
                }
                column += 1;
            }
            _ => {
                column += 1;
            }
        }
    }

    visible
}

fn leading_control_word(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('\\')?;
    let end = rest
        .find(|ch: char| !ch.is_alphabetic())
        .unwrap_or(rest.len());
    (end > 0).then_some(&rest[..end])
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
        ParseError::FontspecNotLoaded { .. }
        | ParseError::SetmainfontInBody { .. }
        | ParseError::ShellEscapeNotAllowed { .. }
        | ParseError::UnimplementedPackage { .. } => Severity::Warning,
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
        ParseError::UnsupportedDocumentClass { .. } => diagnostic
            .with_context(format!(
                "ferritex currently supports: {}",
                ferritex_core::parser::package_loading::SUPPORTED_CLASSES.join(", ")
            ))
            .with_suggestion(
                "change \\documentclass to one of the supported classes, or track kernel compatibility in ROADMAP.md",
            ),
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
        ParseError::UndefinedControlSequence { name, .. } => diagnostic
            .with_context(format!(
                "the parser could not resolve `\\{name}` to a built-in or user-defined command"
            ))
            .with_suggestion(format!(
                "define \\{name} before use or replace it with a supported command"
            )),
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
        ParseError::TikzDiagnostic { .. } => diagnostic
            .with_context("a problem was detected while parsing a tikzpicture environment")
            .with_suggestion("check the TikZ commands in the tikzpicture environment"),
        ParseError::ShellEscapeNotAllowed { .. } => {
            diagnostic.with_suggestion("use --shell-escape to enable external command execution")
        }
        ParseError::ShellEscapeError { .. } => diagnostic,
        ParseError::FileOperationDenied { reason, .. } => diagnostic.with_context(reason),
        ParseError::UnimplementedPackage { name, .. } => diagnostic
            .with_context(format!(
                "ferritex has no implementation or bundled .sty for `{name}`; commands it would define will surface as undefined control sequences"
            ))
            .with_suggestion(format!(
                "remove \\usepackage{{{name}}} or provide a .sty for it"
            )),
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
    let source_path_buf = source_path.to_path_buf();

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
            let candidate = tex_path_candidate(base_dir, &command.value);

            match &command.kind {
                InlineCommandKind::Input => {
                    if let Some((reuse_plan, cached_subtree)) =
                        reusable_cached_input_subtree(reuse_plan, &candidate)
                    {
                        dependency_graph.record_edge(source_path_buf.clone(), candidate.clone());
                        append_cached_source_subtree(
                            &mut expanded,
                            &candidate,
                            reuse_plan,
                            cached_subtree,
                            source_files,
                            labels,
                            citations,
                            dependency_graph,
                            cached_source_subtrees,
                        );
                        cursor = command.end;
                        continue;
                    }

                    let resolved = resolve_input_path(
                        base_dir,
                        workspace_root,
                        overlay_roots,
                        &command.value,
                        service.asset_bundle_loader,
                        asset_bundle_path,
                    );
                    dependency_graph.record_edge(source_path_buf.clone(), resolved.clone());
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
                    if let Some((reuse_plan, cached_subtree)) =
                        reusable_cached_input_subtree(reuse_plan, &candidate)
                    {
                        dependency_graph.record_edge(source_path_buf.clone(), candidate.clone());
                        if !include_guard.insert(candidate.clone()) {
                            cursor = command.end;
                            continue;
                        }
                        append_cached_source_subtree(
                            &mut expanded,
                            &candidate,
                            reuse_plan,
                            cached_subtree,
                            source_files,
                            labels,
                            citations,
                            dependency_graph,
                            cached_source_subtrees,
                        );
                        cursor = command.end;
                        continue;
                    }

                    let resolved = resolve_input_path(
                        base_dir,
                        workspace_root,
                        overlay_roots,
                        &command.value,
                        service.asset_bundle_loader,
                        asset_bundle_path,
                    );
                    dependency_graph.record_edge(source_path_buf.clone(), resolved.clone());
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
                    if let Some((reuse_plan, cached_subtree)) =
                        reusable_cached_input_subtree(reuse_plan, &candidate)
                    {
                        dependency_graph.record_edge(source_path_buf.clone(), candidate.clone());
                        append_cached_source_subtree(
                            &mut expanded,
                            &candidate,
                            reuse_plan,
                            cached_subtree,
                            source_files,
                            labels,
                            citations,
                            dependency_graph,
                            cached_source_subtrees,
                        );
                        // A cached subtree implies the file existed and was successfully expanded in the previous compilation.
                        expanded.append_with_origin(true_branch, &source_file, source_line);
                        cursor = command.end;
                        continue;
                    }

                    let resolved = resolve_input_path(
                        base_dir,
                        workspace_root,
                        overlay_roots,
                        &command.value,
                        service.asset_bundle_loader,
                        asset_bundle_path,
                    );
                    if resolved.exists() {
                        dependency_graph.record_edge(source_path_buf.clone(), resolved.clone());
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

fn reusable_cached_input_subtree<'a>(
    reuse_plan: Option<&'a SourceTreeReusePlan>,
    candidate: &Path,
) -> Option<(&'a SourceTreeReusePlan, &'a CachedSourceSubtree)> {
    let reuse_plan = reuse_plan?;
    if reuse_plan.rebuild_paths.contains(candidate)
        || !reuse_plan
            .cached_dependency_graph
            .nodes
            .contains_key(candidate)
    {
        return None;
    }

    reuse_plan
        .cached_source_subtrees
        .get(candidate)
        .map(|cached| (reuse_plan, cached))
}

#[allow(clippy::too_many_arguments)]
fn append_cached_source_subtree(
    expanded: &mut ExpandedSourceBuilder,
    cached_path: &Path,
    reuse_plan: &SourceTreeReusePlan,
    cached_subtree: &CachedSourceSubtree,
    source_files: &mut BTreeSet<PathBuf>,
    labels: &mut BTreeMap<String, SymbolLocation>,
    citations: &mut BTreeMap<String, SymbolLocation>,
    dependency_graph: &mut DependencyGraph,
    cached_source_subtrees: &mut BTreeMap<PathBuf, CachedSourceSubtree>,
) {
    restore_cached_subtree_graph(
        dependency_graph,
        &reuse_plan.cached_dependency_graph,
        cached_subtree,
    );
    source_files.extend(cached_subtree.source_files.iter().cloned());
    extend_symbol_locations(labels, &cached_subtree.labels);
    extend_symbol_locations(citations, &cached_subtree.citations);
    expanded.append_str_with_source_lines(&cached_subtree.text, &cached_subtree.source_lines);
    cached_source_subtrees.insert(cached_path.to_path_buf(), cached_subtree.clone());
}

fn extend_symbol_locations(
    target: &mut BTreeMap<String, SymbolLocation>,
    additional: &BTreeMap<String, SymbolLocation>,
) {
    for (key, value) in additional {
        target.entry(key.clone()).or_insert_with(|| value.clone());
    }
}

/// Restores dependency graph entries for a cached source subtree. Edges are filtered to subtree-internal targets only.
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
    let mut forms = Vec::new();
    let mut image_indices = std::collections::HashMap::new();
    let mut form_indices = std::collections::HashMap::new();
    let mut page_images = Vec::with_capacity(document.pages.len());
    let mut page_forms = Vec::with_capacity(document.pages.len());

    for page in &document.pages {
        let mut placements = Vec::with_capacity(page.images.len());
        let mut form_placements = Vec::with_capacity(page.images.len());
        for image in &page.images {
            let Some(node) = (image.scene.nodes.len() == 1).then(|| &image.scene.nodes[0]) else {
                continue;
            };
            match node {
                GraphicNode::External(graphic) => {
                    let xobject_index =
                        if let Some(index) = image_indices.get(&graphic.asset_handle.id) {
                            *index
                        } else {
                            let path = Path::new(&graphic.path);
                            let bytes = file_access_gate.read_file(path).map_err(|error| {
                                diagnostic_for_input_error(error, graphic.path.clone())
                            })?;
                            let xobject =
                                build_pdf_image_xobject(&graphic.path, &graphic.metadata, &bytes)?;
                            let index = images.len();
                            images.push(xobject);
                            image_indices.insert(graphic.asset_handle.id.clone(), index);
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
                GraphicNode::Pdf(graphic) => {
                    let xobject_index =
                        if let Some(index) = form_indices.get(&graphic.asset_handle.id) {
                            *index
                        } else {
                            let xobject = build_pdf_form_xobject(&graphic.path, &graphic.metadata)?;
                            let index = forms.len();
                            forms.push(xobject);
                            form_indices.insert(graphic.asset_handle.id.clone(), index);
                            index
                        };

                    form_placements.push(PlacedFormXObject {
                        xobject_index,
                        x: image.x,
                        y: image.y,
                        display_width: image.display_width,
                        display_height: image.display_height,
                    });
                }
                GraphicNode::Group(_) | GraphicNode::Vector(_) | GraphicNode::Text(_) => {}
            }
        }
        page_images.push(placements);
        page_forms.push(form_placements);
    }

    Ok(renderer
        .with_images(images, page_images)
        .with_form_xobjects(forms, page_forms))
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

fn build_pdf_form_xobject(
    path: &str,
    metadata: &PdfGraphicMetadata,
) -> Result<PdfFormXObject, Diagnostic> {
    let [llx, lly, urx, ury] = metadata.media_box;
    if !metadata.page_data.is_empty() && urx > llx && ury > lly {
        return Ok(PdfFormXObject {
            object_id: 0,
            media_box: metadata.media_box,
            data: metadata.page_data.clone(),
            resources_dict: metadata.resources_dict.clone(),
        });
    }

    Err(Diagnostic::new(
        Severity::Error,
        "unsupported PDF input for \\includegraphics",
    )
    .with_file(path.to_string())
    .with_suggestion("use an unencrypted single-page PDF whose first page defines /MediaBox"))
}

fn normalize_existing_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn load_cmr10_metrics(
    file_access_gate: &dyn FileAccessGate,
    asset_bundle_loader: &dyn AssetBundleLoaderPort,
    asset_bundle_path: Option<&Path>,
) -> Option<TfmMetrics> {
    load_named_tfm_metrics(
        file_access_gate,
        asset_bundle_loader,
        asset_bundle_path,
        "cmr10",
        &CMR10_TFM_CANDIDATES,
    )
}

fn load_cmbx12_metrics(
    file_access_gate: &dyn FileAccessGate,
    asset_bundle_loader: &dyn AssetBundleLoaderPort,
    asset_bundle_path: Option<&Path>,
) -> Option<TfmMetrics> {
    load_named_tfm_metrics(
        file_access_gate,
        asset_bundle_loader,
        asset_bundle_path,
        "cmbx12",
        &CMBX12_TFM_CANDIDATES,
    )
}

fn load_named_tfm_metrics(
    file_access_gate: &dyn FileAccessGate,
    asset_bundle_loader: &dyn AssetBundleLoaderPort,
    asset_bundle_path: Option<&Path>,
    font_name: &str,
    candidate_paths: &[&str],
) -> Option<TfmMetrics> {
    let bundle_path = asset_bundle_path?;

    if let Some(candidate) = asset_bundle_loader.resolve_tfm_font(bundle_path, font_name) {
        if let Some(metrics) = load_tfm_metrics_from_path(file_access_gate, font_name, &candidate) {
            return Some(metrics);
        }
    }

    for relative_path in candidate_paths {
        let candidate = bundle_path.join(*relative_path);
        if let Some(metrics) = load_tfm_metrics_from_path(file_access_gate, font_name, &candidate) {
            return Some(metrics);
        }
    }

    None
}

fn load_tfm_metrics_from_path(
    file_access_gate: &dyn FileAccessGate,
    font_name: &str,
    candidate: &Path,
) -> Option<TfmMetrics> {
    if !candidate.is_file() {
        return None;
    }

    if file_access_gate.check_read(candidate) == PathAccessDecision::Denied {
        tracing::warn!(
            path = %candidate.display(),
            font = font_name,
            "TFM access denied; falling back to fixed-width typesetting"
        );
        return None;
    }

    let bytes = match file_access_gate.read_file(candidate) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(
                path = %candidate.display(),
                font = font_name,
                %error,
                "failed to read TFM metrics; falling back to fixed-width typesetting"
            );
            return None;
        }
    };

    match TfmMetrics::parse(&bytes) {
        Ok(metrics) => Some(metrics),
        Err(error) => {
            tracing::warn!(
                path = %candidate.display(),
                font = font_name,
                %error,
                "failed to parse TFM metrics; falling back to fixed-width typesetting"
            );
            None
        }
    }
}

fn resolve_type1_font(
    file_access_gate: &dyn FileAccessGate,
    asset_bundle_loader: &dyn AssetBundleLoaderPort,
    asset_bundle_path: Option<&Path>,
    font_name: &str,
) -> Option<Type1Font> {
    let bundle_path = asset_bundle_path?;
    let mut candidates = Vec::new();
    let mut visited = BTreeSet::new();

    if let Some(tfm_path) = asset_bundle_loader.resolve_tfm_font(bundle_path, font_name) {
        candidates.extend(derived_type1_candidates(bundle_path, &tfm_path));
    }
    candidates.extend(
        CM_PFB_CANDIDATE_TEMPLATES
            .iter()
            .map(|template| bundle_path.join(template.replace("{name}", font_name))),
    );

    for candidate in candidates {
        let normalized = normalize_existing_path(&candidate);
        if !visited.insert(normalized.clone()) {
            continue;
        }
        if let Some(font) = load_type1_font_from_path(file_access_gate, font_name, &normalized) {
            return Some(font);
        }
    }

    None
}

fn derived_type1_candidates(bundle_path: &Path, tfm_path: &Path) -> Vec<PathBuf> {
    let Ok(relative_path) = tfm_path.strip_prefix(bundle_path) else {
        return Vec::new();
    };
    let relative = relative_path.to_string_lossy().replace('\\', "/");
    let mut candidates = vec![bundle_path.join(relative_path).with_extension("pfb")];

    if relative.contains("/fonts/tfm/") {
        let type1_relative = relative.replace("/fonts/tfm/", "/fonts/type1/");
        candidates.push(
            bundle_path
                .join(Path::new(&type1_relative))
                .with_extension("pfb"),
        );

        if type1_relative.contains("/public/cm/") {
            let amsfonts_relative = type1_relative.replace("/public/cm/", "/public/amsfonts/cm/");
            candidates.push(
                bundle_path
                    .join(Path::new(&amsfonts_relative))
                    .with_extension("pfb"),
            );
        }
    }

    candidates
}

fn load_type1_font_from_path(
    file_access_gate: &dyn FileAccessGate,
    font_name: &str,
    candidate: &Path,
) -> Option<Type1Font> {
    if !candidate.is_file() {
        return None;
    }

    if file_access_gate.check_read(candidate) == PathAccessDecision::Denied {
        tracing::warn!(
            path = %candidate.display(),
            font = font_name,
            "Type1 font access denied; continuing without embedded Type1 font"
        );
        return None;
    }

    let bytes = match file_access_gate.read_file(candidate) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(
                path = %candidate.display(),
                font = font_name,
                %error,
                "failed to read Type1 font; continuing without embedded Type1 font"
            );
            return None;
        }
    };

    match Type1Font::parse(&bytes) {
        Ok(font) => Some(font),
        Err(error) => {
            tracing::warn!(
                path = %candidate.display(),
                font = font_name,
                %error,
                "failed to parse Type1 font; continuing without embedded Type1 font"
            );
            None
        }
    }
}

fn load_opentype_font(
    file_access_gate: &dyn FileAccessGate,
    asset_bundle_loader: &dyn AssetBundleLoaderPort,
    asset_bundle_path: Option<&Path>,
) -> Option<LoadedOpenTypeFont> {
    let bundle_path = asset_bundle_path?;

    if let Some(candidate) = asset_bundle_loader.resolve_default_opentype_font(bundle_path) {
        if let Some(font) = load_resolved_font(file_access_gate, &candidate) {
            return Some(LoadedOpenTypeFont {
                base_font: sanitize_pdf_font_name(&font.base_font_name),
                font: font.font,
            });
        }
    }

    for candidate in collect_ttf_candidates(bundle_path) {
        if let Some(font) = load_resolved_font(file_access_gate, &candidate) {
            return Some(LoadedOpenTypeFont {
                base_font: sanitize_pdf_font_name(&font.base_font_name),
                font: font.font,
            });
        }
    }

    None
}

fn resolve_named_font_with_bundle_index(
    font_name: &str,
    input_dir: &Path,
    project_root: &Path,
    overlay_roots: &[PathBuf],
    asset_bundle_loader: &dyn AssetBundleLoaderPort,
    asset_bundle_path: Option<&Path>,
    host_font_roots: &[PathBuf],
    file_access_gate: &dyn FileAccessGate,
) -> Option<ResolvedFont> {
    resolve_named_font(
        font_name,
        input_dir,
        project_root,
        overlay_roots,
        None,
        &[],
        file_access_gate,
    )
    .or_else(|| {
        resolve_bundle_named_font(
            font_name,
            asset_bundle_loader,
            asset_bundle_path,
            file_access_gate,
        )
    })
    .or_else(|| {
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
}

fn resolve_bundle_named_font(
    font_name: &str,
    asset_bundle_loader: &dyn AssetBundleLoaderPort,
    asset_bundle_path: Option<&Path>,
    file_access_gate: &dyn FileAccessGate,
) -> Option<ResolvedFont> {
    let bundle_path = asset_bundle_path?;
    let candidate = asset_bundle_loader.resolve_opentype_font(bundle_path, font_name)?;
    load_resolved_font(file_access_gate, &candidate)
}

fn load_resolved_font(
    file_access_gate: &dyn FileAccessGate,
    candidate: &Path,
) -> Option<ResolvedFont> {
    if file_access_gate.check_read(candidate) == PathAccessDecision::Denied {
        tracing::warn!(
            path = %candidate.display(),
            "ttf access denied; falling back to other font paths"
        );
        return None;
    }

    let bytes = match file_access_gate.read_file(candidate) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(
                path = %candidate.display(),
                %error,
                "failed to read TTF font; falling back to other font paths"
            );
            return None;
        }
    };

    match OpenTypeFont::parse(&bytes) {
        Ok(font) => {
            let stem = candidate
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("FerritexOpenType");
            Some(ResolvedFont {
                path: candidate.to_path_buf(),
                font,
                base_font_name: stem.to_string(),
            })
        }
        Err(error) => {
            tracing::warn!(
                path = %candidate.display(),
                %error,
                "failed to parse TTF font; falling back to other font paths"
            );
            None
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Type1DescriptorMetrics {
    bbox: [i16; 4],
    ascent: i16,
    descent: i16,
    italic_angle: i16,
    stem_v: u16,
    cap_height: i16,
    flags: u32,
}

fn build_type1_font_resources(
    selection: &FontFamilySelection,
    main: &TfmFontBundle,
    bold: Option<&TfmFontBundle>,
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

    let mut resources = vec![None; highest_used_role + 1];
    resources[0] = Some(build_type1_font_resource_with_fallback(
        main,
        &used_characters[0],
        0,
    ));

    let mut tasks: Vec<Box<dyn FnOnce() -> (usize, Option<FontResource>) + Send>> = Vec::new();
    for role in 1..=highest_used_role {
        let Some(font) = selection.font_for_role(role as u8) else {
            resources[role] = Some(builtin_font_resource_for_role(role as u8));
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

    let mut resources = resources.into_iter().flatten().collect::<Vec<_>>();
    if !document.outlines.is_empty() {
        if let Some(font) =
            bold.and_then(|bundle| build_type1_font_resource(bundle, &BTreeMap::new()))
        {
            resources.push(font);
        }
    }

    resources
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

fn build_type1_font_resource_with_fallback(
    bundle: &TfmFontBundle,
    role_characters: &BTreeMap<u8, char>,
    role: u8,
) -> FontResource {
    build_type1_font_resource(bundle, role_characters)
        .unwrap_or_else(|| builtin_font_resource_for_role(role))
}

fn build_type1_font_resource(
    bundle: &TfmFontBundle,
    role_characters: &BTreeMap<u8, char>,
) -> Option<FontResource> {
    let type1 = bundle.type1.as_ref()?;
    let (first_char, last_char) = type1_char_range(role_characters, &bundle.metrics)?;
    let descriptor = type1_descriptor_metrics(bundle);
    let mut font_program = type1.header_segment.clone();
    font_program.extend_from_slice(&type1.binary_segment);
    font_program.extend_from_slice(&type1.trailer_segment);

    Some(FontResource::EmbeddedType1 {
        base_font: format!(
            "FERRTX+{}",
            sanitize_pdf_font_name(&bundle.font_name.to_ascii_uppercase())
        ),
        ascii_length: type1.header_segment.len(),
        binary_length: type1.binary_segment.len(),
        trailer_length: type1.trailer_segment.len(),
        font_program,
        first_char,
        last_char,
        widths: (first_char..=last_char)
            .map(|code| tfm_width_to_pdf_units(&bundle.metrics, code))
            .collect(),
        bbox: descriptor.bbox,
        ascent: descriptor.ascent,
        descent: descriptor.descent,
        italic_angle: descriptor.italic_angle,
        stem_v: descriptor.stem_v,
        cap_height: descriptor.cap_height,
        flags: descriptor.flags,
    })
}

fn type1_char_range(
    role_characters: &BTreeMap<u8, char>,
    metrics: &TfmMetrics,
) -> Option<(u8, u8)> {
    match (
        role_characters.keys().next().copied(),
        role_characters.keys().next_back().copied(),
    ) {
        (Some(first_char), Some(last_char)) => Some((first_char, last_char)),
        _ => {
            let bc = u8::try_from(metrics.bc).ok()?;
            let ec = u8::try_from(metrics.ec).ok()?;
            let printable_start = bc.max(32);
            let printable_end = ec.min(126);
            if printable_start <= printable_end {
                Some((printable_start, printable_end))
            } else if bc <= ec {
                Some((bc, ec))
            } else {
                None
            }
        }
    }
}

fn tfm_width_to_pdf_units(metrics: &TfmMetrics, code: u8) -> u16 {
    let width = metrics
        .width(u16::from(code))
        .unwrap_or_else(|_| DimensionValue::zero());
    let units = tfm_dimension_to_pdf_units(metrics, width).max(0);
    u16::try_from(units.min(i64::from(u16::MAX))).expect("PDF width must fit in u16")
}

fn tfm_dimension_to_pdf_units(metrics: &TfmMetrics, value: DimensionValue) -> i64 {
    if metrics.design_size.0 == 0 {
        return 0;
    }
    value.0 * 1000 / metrics.design_size.0
}

fn type1_descriptor_metrics(bundle: &TfmFontBundle) -> Type1DescriptorMetrics {
    match bundle.font_name.to_ascii_lowercase().as_str() {
        "cmr10" => Type1DescriptorMetrics {
            bbox: [-251, -250, 1009, 750],
            ascent: 683,
            descent: -217,
            italic_angle: 0,
            stem_v: 69,
            cap_height: 683,
            flags: PDF_TYPE1_DEFAULT_FLAGS,
        },
        "cmbx12" => Type1DescriptorMetrics {
            bbox: [-342, -251, 1087, 750],
            ascent: 694,
            descent: -194,
            italic_angle: 0,
            stem_v: 109,
            cap_height: 694,
            flags: PDF_TYPE1_DEFAULT_FLAGS,
        },
        _ => generic_type1_descriptor_metrics(&bundle.metrics),
    }
}

fn generic_type1_descriptor_metrics(metrics: &TfmMetrics) -> Type1DescriptorMetrics {
    let Some((first_char, last_char)) = type1_char_range(&BTreeMap::new(), metrics) else {
        return Type1DescriptorMetrics {
            bbox: [0, 0, 0, 0],
            ascent: 0,
            descent: 0,
            italic_angle: 0,
            stem_v: 80,
            cap_height: 0,
            flags: PDF_TYPE1_DEFAULT_FLAGS,
        };
    };

    let mut max_width = 0i64;
    let mut max_height = 0i64;
    let mut max_depth = 0i64;
    let mut cap_height = 0i64;

    for code in first_char..=last_char {
        let code = u16::from(code);
        let width = metrics
            .width(code)
            .unwrap_or_else(|_| DimensionValue::zero());
        let height = metrics
            .height(code)
            .unwrap_or_else(|_| DimensionValue::zero());
        let depth = metrics
            .depth(code)
            .unwrap_or_else(|_| DimensionValue::zero());

        max_width = max_width.max(tfm_dimension_to_pdf_units(metrics, width));
        max_height = max_height.max(tfm_dimension_to_pdf_units(metrics, height));
        max_depth = max_depth.max(tfm_dimension_to_pdf_units(metrics, depth));
    }

    for code in b'A'..=b'Z' {
        let height = metrics
            .height(u16::from(code))
            .unwrap_or_else(|_| DimensionValue::zero());
        cap_height = cap_height.max(tfm_dimension_to_pdf_units(metrics, height));
    }

    Type1DescriptorMetrics {
        bbox: [
            0,
            clamp_i64_to_i16(-max_depth),
            clamp_i64_to_i16(max_width),
            clamp_i64_to_i16(max_height),
        ],
        ascent: clamp_i64_to_i16(max_height),
        descent: clamp_i64_to_i16(-max_depth),
        italic_angle: 0,
        stem_v: 80,
        cap_height: clamp_i64_to_i16(cap_height.max(max_height)),
        flags: PDF_TYPE1_DEFAULT_FLAGS,
    }
}

fn clamp_i64_to_i16(value: i64) -> i16 {
    value.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
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
    asset_bundle_loader: &dyn AssetBundleLoaderPort,
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
            resolve_named_font_with_bundle_index(
                font_name,
                input_dir,
                project_root,
                overlay_roots,
                asset_bundle_loader,
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
                    || load_opentype_font(file_access_gate, asset_bundle_loader, asset_bundle_path),
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
                || load_opentype_font(file_access_gate, asset_bundle_loader, asset_bundle_path),
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
    asset_bundle_loader: &dyn AssetBundleLoaderPort,
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
        resolve_named_font_with_bundle_index(
            font_name,
            input_dir,
            project_root,
            overlay_roots,
            asset_bundle_loader,
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
    let detail = match diagnostic.file {
        Some(file) => format!("{} ({file})", diagnostic.message),
        None => diagnostic.message,
    };
    Diagnostic::new(
        Severity::Error,
        format!("failed to resolve \\input/\\include target `{input_value}`"),
    )
    .with_file(source_path.to_string_lossy().into_owned())
    .with_line(line)
    .with_context(detail)
    .with_suggestion("verify the referenced file path and access policy")
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use serde_json::json;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    use super::{
        reusable_page_payloads_for_render, run_font_tasks, CompileJobService,
        PartitionTypesetDetail, PartitionTypesetFallbackReason, PartitionTypesetReuseType,
        StageTiming,
    };
    use crate::compile_cache::{
        BlockCheckpoint, BlockLayoutState, CachedSourceSubtree, CachedTypesetFragment, CompileCache,
    };
    use crate::ports::{AssetBundleLoaderPort, ShellCommandGatewayPort, ShellCommandOutput};
    use crate::runtime_options::{InteractionMode, RuntimeOptions, ShellEscapeMode};
    use ferritex_core::compilation::PartitionLocator;
    use ferritex_core::diagnostics::Severity;
    use ferritex_core::font::OpenTypeFont;
    use ferritex_core::graphics::api::ImageColorSpace;
    use ferritex_core::graphics::GraphicsScene;
    use ferritex_core::incremental::DependencyGraph;
    use ferritex_core::kernel::api::{DimensionValue, SourceLocation, SourceSpan};
    use ferritex_core::parser::api::{DocumentNode, MinimalLatexParser, Parser};
    use ferritex_core::pdf::PageRenderPayload;
    use ferritex_core::policy::{FileAccessError, FileAccessGate, PathAccessDecision};
    use ferritex_core::synctex::{SourceLineTrace, SyncTexData};
    use ferritex_core::typesetting::{
        DocumentLayoutFragment, FloatContent, FloatCounters, FloatPlacement, FloatRegion, PageBox,
        RawBlockCheckpoint, TextLine, TypesetDocument, TypesetImage, TypesetNamedDestination,
        TypesetOutline, TypesetPage,
    };
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

    struct ScopedFsFileAccessGate {
        allowed_read_root: PathBuf,
        allowed_write_root: PathBuf,
    }

    impl ScopedFsFileAccessGate {
        fn allows(path: &Path, root: &Path) -> bool {
            let resolved_path = canonicalize_with_missing(path);
            let resolved_root = canonicalize_with_missing(root);
            resolved_path.starts_with(&resolved_root)
        }
    }

    impl FileAccessGate for ScopedFsFileAccessGate {
        fn ensure_directory(&self, path: &Path) -> Result<(), FileAccessError> {
            if self.check_write(path) == PathAccessDecision::Denied {
                return Err(FileAccessError::AccessDenied {
                    path: path.to_path_buf(),
                });
            }

            fs::create_dir_all(path).map_err(FileAccessError::from)
        }

        fn check_read(&self, path: &Path) -> PathAccessDecision {
            if Self::allows(path, &self.allowed_read_root) {
                PathAccessDecision::Allowed
            } else {
                PathAccessDecision::Denied
            }
        }

        fn check_write(&self, path: &Path) -> PathAccessDecision {
            if Self::allows(path, &self.allowed_write_root) {
                PathAccessDecision::Allowed
            } else {
                PathAccessDecision::Denied
            }
        }

        fn check_readback(
            &self,
            path: &Path,
            _primary_input: &Path,
            _jobname: &str,
        ) -> PathAccessDecision {
            self.check_read(path)
        }

        fn read_file(&self, path: &Path) -> Result<Vec<u8>, FileAccessError> {
            if self.check_read(path) == PathAccessDecision::Denied {
                return Err(FileAccessError::AccessDenied {
                    path: path.to_path_buf(),
                });
            }

            fs::read(path).map_err(FileAccessError::from)
        }

        fn write_file(&self, path: &Path, content: &[u8]) -> Result<(), FileAccessError> {
            if self.check_write(path) == PathAccessDecision::Denied {
                return Err(FileAccessError::AccessDenied {
                    path: path.to_path_buf(),
                });
            }

            fs::write(path, content).map_err(FileAccessError::from)
        }

        fn read_readback(
            &self,
            path: &Path,
            _primary_input: &Path,
            _jobname: &str,
        ) -> Result<Vec<u8>, FileAccessError> {
            self.read_file(path)
        }
    }

    fn canonicalize_with_missing(path: &Path) -> PathBuf {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().expect("current dir").join(path)
        };

        let mut missing_suffix = Vec::new();
        let mut cursor = absolute.as_path();
        loop {
            if cursor.exists() {
                let mut resolved = cursor.canonicalize().expect("canonicalize existing path");
                for segment in missing_suffix.iter().rev() {
                    resolved.push(segment);
                }
                return resolved;
            }

            if let Some(name) = cursor.file_name() {
                missing_suffix.push(name.to_os_string());
            }
            cursor = cursor.parent().unwrap_or_else(|| Path::new("."));
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

    struct NoopShellCommandGateway;

    impl ShellCommandGatewayPort for NoopShellCommandGateway {
        fn execute(
            &self,
            _program: &str,
            _args: &[&str],
            _working_dir: &Path,
        ) -> Result<ShellCommandOutput, String> {
            Ok(ShellCommandOutput {
                exit_code: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
    }

    static NOOP_SHELL_COMMAND_GATEWAY: NoopShellCommandGateway = NoopShellCommandGateway;

    #[derive(Default)]
    struct MockShellCommandGateway {
        commands: Mutex<Vec<(String, Vec<String>, PathBuf)>>,
        generated_bbl: Mutex<Option<String>>,
        exit_code: Mutex<i32>,
        error: Mutex<Option<String>>,
    }

    impl MockShellCommandGateway {
        fn with_bbl(bbl: impl Into<String>) -> Self {
            Self {
                generated_bbl: Mutex::new(Some(bbl.into())),
                ..Self::default()
            }
        }

        fn commands(&self) -> Vec<(String, Vec<String>, PathBuf)> {
            self.commands.lock().expect("commands lock").clone()
        }
    }

    impl ShellCommandGatewayPort for MockShellCommandGateway {
        fn execute(
            &self,
            program: &str,
            args: &[&str],
            working_dir: &Path,
        ) -> Result<ShellCommandOutput, String> {
            self.commands.lock().expect("commands lock").push((
                program.to_string(),
                args.iter().map(|arg| (*arg).to_string()).collect(),
                working_dir.to_path_buf(),
            ));

            if let Some(error) = self.error.lock().expect("error lock").clone() {
                return Err(error);
            }

            if let Some(bbl) = self.generated_bbl.lock().expect("bbl lock").clone() {
                let jobname = args.first().copied().unwrap_or("main");
                fs::write(working_dir.join(format!("{jobname}.bbl")), bbl)
                    .expect("write generated bbl");
            }

            Ok(ShellCommandOutput {
                exit_code: *self.exit_code.lock().expect("exit code lock"),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
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
        CompileJobService::new(
            file_access_gate,
            asset_bundle_loader,
            &NOOP_SHELL_COMMAND_GATEWAY,
        )
    }

    fn service_with_shell<'a>(
        file_access_gate: &'a dyn FileAccessGate,
        asset_bundle_loader: &'a dyn AssetBundleLoaderPort,
        shell_command_gateway: &'a dyn ShellCommandGatewayPort,
    ) -> CompileJobService<'a> {
        CompileJobService::new(file_access_gate, asset_bundle_loader, shell_command_gateway)
    }

    fn document(body: &str) -> String {
        format!("\\documentclass{{article}}\n\\begin{{document}}\n{body}\n\\end{{document}}\n")
    }

    fn report_document(body: &str) -> String {
        format!("\\documentclass{{report}}\n\\begin{{document}}\n{body}\n\\end{{document}}\n")
    }

    fn source_lines_for(file: &str, source: &str) -> Vec<SourceLineTrace> {
        source
            .lines()
            .enumerate()
            .map(|(index, text)| SourceLineTrace {
                file: file.to_string(),
                line: u32::try_from(index + 1).expect("line number fits in u32"),
                text: text.to_string(),
            })
            .collect()
    }

    fn cached_source_subtree_for(
        path: &Path,
        source_name: &str,
        text: &str,
    ) -> CachedSourceSubtree {
        CachedSourceSubtree {
            text: text.to_string(),
            source_lines: source_lines_for(source_name, text),
            source_files: vec![path.to_path_buf()],
            labels: BTreeMap::new(),
            citations: BTreeMap::new(),
        }
    }

    fn reuse_plan_with_cached_subtree(
        path: &Path,
        cached_subtree: CachedSourceSubtree,
    ) -> super::SourceTreeReusePlan {
        let mut cached_dependency_graph = DependencyGraph::default();
        cached_dependency_graph.record_node(path.to_path_buf(), 1);
        let mut cached_source_subtrees = BTreeMap::new();
        cached_source_subtrees.insert(path.to_path_buf(), cached_subtree);
        super::SourceTreeReusePlan {
            rebuild_paths: BTreeSet::new(),
            cached_dependency_graph,
            cached_source_subtrees,
        }
    }

    fn expand_inputs_for_test(
        source: &str,
        source_path: &Path,
        reuse_plan: Option<&super::SourceTreeReusePlan>,
    ) -> Result<
        (
            super::ExpandedSource,
            BTreeSet<PathBuf>,
            DependencyGraph,
            BTreeMap<PathBuf, CachedSourceSubtree>,
        ),
        ferritex_core::diagnostics::Diagnostic,
    > {
        let loader = MockAssetBundleLoader::valid();
        let compile_service = service(&FsTestFileAccessGate, &loader);
        let base_dir = source_path.parent().expect("source path parent");
        let mut visited = BTreeSet::new();
        let mut include_guard = BTreeSet::new();
        let mut source_files = BTreeSet::from([source_path.to_path_buf()]);
        let mut labels = BTreeMap::new();
        let mut citations = BTreeMap::new();
        let mut dependency_graph = DependencyGraph::default();
        let mut cached_source_subtrees = BTreeMap::new();
        let expanded = super::expand_inputs(
            &compile_service,
            source,
            source_path,
            base_dir,
            base_dir,
            &[],
            None,
            &mut visited,
            &mut include_guard,
            reuse_plan,
            &mut source_files,
            &mut labels,
            &mut citations,
            &mut dependency_graph,
            &mut cached_source_subtrees,
        )?;
        Ok((
            expanded,
            source_files,
            dependency_graph,
            cached_source_subtrees,
        ))
    }

    fn parse_article_document(source: &str) -> ferritex_core::parser::ParsedDocument {
        MinimalLatexParser
            .parse(source)
            .expect("parse test article document")
    }

    fn fragment_from_typeset_document(
        partition_id: &str,
        document: &TypesetDocument,
    ) -> DocumentLayoutFragment {
        DocumentLayoutFragment {
            partition_id: partition_id.to_string(),
            pages: document.pages.clone(),
            local_label_pages: document
                .named_destinations
                .iter()
                .map(|destination| (destination.name.clone(), destination.page_index))
                .collect(),
            outlines: document.outlines.clone(),
            named_destinations: document.named_destinations.clone(),
        }
    }

    fn dummy_layout_state() -> BlockLayoutState {
        BlockLayoutState {
            content_used: DimensionValue::zero(),
            completed_page_count: 0,
            pending_floats: Vec::new(),
            footnote_count: 0,
            figure_count: 0,
            table_count: 0,
        }
    }

    fn write_partitioned_report_project(root: &Path, chapters: &[(&str, &str)]) -> PathBuf {
        let body = chapters
            .iter()
            .map(|(file_name, _)| {
                let stem = Path::new(file_name)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .expect("utf-8 chapter file stem");
                format!("\\input{{{stem}}}")
            })
            .collect::<Vec<_>>()
            .join("\n\\newpage\n");
        let input_file = root.join("main.tex");
        fs::write(&input_file, report_document(&body)).expect("write main input");
        for (file_name, content) in chapters {
            fs::write(root.join(file_name), content).expect("write chapter input");
        }
        input_file
    }

    fn write_partitioned_article_project(root: &Path, sections: &[(&str, &str)]) -> PathBuf {
        let body = sections
            .iter()
            .map(|(file_name, _)| {
                let stem = Path::new(file_name)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .expect("utf-8 section file stem");
                format!("\\input{{{stem}}}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let input_file = root.join("main.tex");
        fs::write(&input_file, document(&body)).expect("write main input");
        for (file_name, content) in sections {
            fs::write(root.join(file_name), content).expect("write section input");
        }
        input_file
    }

    fn read_pdf(path: &Path) -> String {
        String::from_utf8_lossy(&fs::read(path).expect("read output pdf")).into_owned()
    }

    fn pdf_text_operators(pdf: &str) -> Vec<String> {
        pdf.lines()
            .filter_map(|line| {
                let line = line.trim();
                line.strip_suffix(") Tj")
                    .and_then(|prefix| prefix.strip_prefix('('))
                    .map(str::to_string)
            })
            .collect()
    }

    fn partition_detail<'a>(
        details: &'a [super::PartitionTypesetDetail],
        partition_id: &str,
    ) -> &'a super::PartitionTypesetDetail {
        details
            .iter()
            .find(|detail| detail.partition_id == partition_id)
            .unwrap_or_else(|| panic!("missing detail for {partition_id}"))
    }

    fn partition_detail_diagnostic_lines(details: &[super::PartitionTypesetDetail]) -> Vec<String> {
        let mut counts = BTreeMap::new();
        for detail in details {
            *counts.entry(detail.reuse_type).or_insert(0usize) += 1;
        }

        let mut lines = vec![format!(
            "summary cached={} block_reuse={} suffix_rebuild={} full_rebuild={}",
            counts
                .get(&PartitionTypesetReuseType::Cached)
                .copied()
                .unwrap_or(0),
            counts
                .get(&PartitionTypesetReuseType::BlockReuse)
                .copied()
                .unwrap_or(0),
            counts
                .get(&PartitionTypesetReuseType::SuffixRebuild)
                .copied()
                .unwrap_or(0),
            counts
                .get(&PartitionTypesetReuseType::FullRebuild)
                .copied()
                .unwrap_or(0),
        )];

        lines.extend(details.iter().filter_map(|detail| {
            (detail.reuse_type != PartitionTypesetReuseType::Cached).then(|| {
                format!(
                    "partition={} reuse={:?} suffix={}/{} fallback={:?} elapsed_us={}",
                    detail.partition_id,
                    detail.reuse_type,
                    detail.suffix_block_count,
                    detail.total_block_count,
                    detail.fallback_reason,
                    detail
                        .elapsed
                        .map(|elapsed| elapsed.as_micros())
                        .unwrap_or_default(),
                )
            })
        }));
        lines
    }

    fn read_synctex(path: &Path) -> SyncTexData {
        serde_json::from_slice(&fs::read(path).expect("read output synctex"))
            .expect("parse output synctex")
    }

    fn points(value: i64) -> DimensionValue {
        DimensionValue(value * 65_536)
    }

    #[test]
    fn find_affected_block_single_paragraph_change() {
        let cached_source = "Alpha\n\nBeta\n\nGamma\n";
        let current_source = "Alpha\n\nBeta\n\nDelta\n";
        let cached_source_lines = source_lines_for("main.tex", cached_source);
        let current_source_lines = source_lines_for("main.tex", current_source);
        let cached_nodes = vec![
            DocumentNode::Text("Alpha".to_string(), None),
            DocumentNode::ParBreak,
            DocumentNode::Text("Beta".to_string(), None),
            DocumentNode::ParBreak,
            DocumentNode::Text("Gamma".to_string(), None),
        ];
        let current_nodes = vec![
            DocumentNode::Text("Alpha".to_string(), None),
            DocumentNode::ParBreak,
            DocumentNode::Text("Beta".to_string(), None),
            DocumentNode::ParBreak,
            DocumentNode::Text("Delta".to_string(), None),
        ];
        let cached_checkpoints = vec![
            BlockCheckpoint {
                node_index: 2,
                source_span: None,
                layout_state: dummy_layout_state(),
            },
            BlockCheckpoint {
                node_index: 4,
                source_span: None,
                layout_state: dummy_layout_state(),
            },
        ];
        let current_checkpoints = vec![
            RawBlockCheckpoint {
                node_index: 2,
                source_span: None,
                vlist_position: 2,
            },
            RawBlockCheckpoint {
                node_index: 4,
                source_span: None,
                vlist_position: 4,
            },
        ];

        assert_eq!(
            super::find_affected_block_index(
                &cached_checkpoints,
                &cached_nodes,
                &cached_source_lines,
                &current_checkpoints,
                &current_nodes,
                &current_source_lines,
                cached_source,
                current_source,
            ),
            Some(1)
        );
    }

    #[test]
    fn find_affected_block_no_change_returns_none() {
        let source = "Alpha\n\nBeta\n\nGamma\n";
        let source_lines = source_lines_for("main.tex", source);
        let nodes = vec![
            DocumentNode::Text("Alpha".to_string(), None),
            DocumentNode::ParBreak,
            DocumentNode::Text("Beta".to_string(), None),
            DocumentNode::ParBreak,
            DocumentNode::Text("Gamma".to_string(), None),
        ];
        let checkpoints = vec![BlockCheckpoint {
            node_index: 2,
            source_span: None,
            layout_state: dummy_layout_state(),
        }];
        let current_checkpoints = vec![RawBlockCheckpoint {
            node_index: 2,
            source_span: None,
            vlist_position: 2,
        }];

        assert_eq!(
            super::find_affected_block_index(
                &checkpoints,
                &nodes,
                &source_lines,
                &current_checkpoints,
                &nodes,
                &source_lines,
                source,
                source,
            ),
            None
        );
    }

    #[test]
    fn find_affected_block_heading_added_returns_zero() {
        let cached_source = "Alpha\n\nBeta\n\nGamma\n";
        let current_source = "Intro\n\nAlpha\n\nBeta\n\nGamma\n";
        let cached_source_lines = source_lines_for("main.tex", cached_source);
        let current_source_lines = source_lines_for("main.tex", current_source);
        let cached_nodes = vec![
            DocumentNode::Text("Alpha".to_string(), None),
            DocumentNode::ParBreak,
            DocumentNode::Text("Beta".to_string(), None),
            DocumentNode::ParBreak,
            DocumentNode::Text("Gamma".to_string(), None),
        ];
        let current_nodes = vec![
            DocumentNode::Text("Intro".to_string(), None),
            DocumentNode::ParBreak,
            DocumentNode::Text("Alpha".to_string(), None),
            DocumentNode::ParBreak,
            DocumentNode::Text("Beta".to_string(), None),
            DocumentNode::ParBreak,
            DocumentNode::Text("Gamma".to_string(), None),
        ];
        let cached_checkpoints = vec![
            BlockCheckpoint {
                node_index: 2,
                source_span: None,
                layout_state: dummy_layout_state(),
            },
            BlockCheckpoint {
                node_index: 4,
                source_span: None,
                layout_state: dummy_layout_state(),
            },
        ];
        let current_checkpoints = vec![
            RawBlockCheckpoint {
                node_index: 2,
                source_span: None,
                vlist_position: 2,
            },
            RawBlockCheckpoint {
                node_index: 4,
                source_span: None,
                vlist_position: 4,
            },
            RawBlockCheckpoint {
                node_index: 6,
                source_span: None,
                vlist_position: 6,
            },
        ];

        assert_eq!(
            super::find_affected_block_index(
                &cached_checkpoints,
                &cached_nodes,
                &cached_source_lines,
                &current_checkpoints,
                &current_nodes,
                &current_source_lines,
                cached_source,
                current_source,
            ),
            Some(0)
        );
    }

    #[test]
    fn block_checkpoint_missing_falls_back_to_partition() {
        assert_eq!(
            super::find_affected_block_index(&[], &[], &[], &[], &[], &[], "Alpha", "Beta"),
            Some(0)
        );
    }

    #[test]
    fn suffix_rebuild_produces_correct_pages() {
        let loader = MockAssetBundleLoader::valid();
        let service = service(&FsTestFileAccessGate, &loader);
        let selection = super::CompileFontSelection::Basic;
        let temp = tempdir().expect("create tempdir");
        let overlay_roots: Vec<PathBuf> = Vec::new();
        let resolver = super::CompileGraphicAssetResolver {
            file_access_gate: &FsTestFileAccessGate,
            input_dir: temp.path(),
            project_root: temp.path(),
            overlay_roots: &overlay_roots,
            asset_bundle_path: None,
            diagnostics: RefCell::new(Vec::new()),
        };

        let cached_source = document("Alpha\n\nBeta\n\nGamma.");
        let current_source = document("Alpha\n\nBeta\n\nDelta.");
        let cached_source_lines = source_lines_for("main.tex", &cached_source);
        let current_source_lines = source_lines_for("main.tex", &current_source);
        let cached_document = parse_article_document(&cached_source);
        let current_document = parse_article_document(&current_source);
        let (_annotated_cached_document, cached_nodes) =
            super::annotated_body_nodes(&cached_document, Some(&cached_source_lines))
                .expect("annotate cached document");
        let (_annotated_current_document, current_nodes) =
            super::annotated_body_nodes(&current_document, Some(&current_source_lines))
                .expect("annotate current document");
        let mut cached_raw_checkpoints = Vec::new();
        let cached_vlist = service.build_vlist_from_body_nodes_with_selection(
            &cached_document,
            cached_nodes.clone(),
            &selection,
            &resolver,
            false,
            FloatCounters::default(),
            Some(&mut cached_raw_checkpoints),
        );
        let cached_typeset = service.typeset_document_with_selection(
            &cached_document,
            Some(&cached_source_lines),
            &selection,
            &resolver,
        );
        let cached_fragment = CachedTypesetFragment {
            fragment: fragment_from_typeset_document("main", &cached_typeset),
            source_hash: 1,
            block_checkpoints: Some(super::raw_checkpoints_to_block_checkpoint_data(
                &cached_raw_checkpoints,
                &cached_nodes,
                &cached_vlist,
                1,
                cached_document.body.clone(),
            )),
        };
        let mut current_raw_checkpoints = Vec::new();
        let _ = service.build_vlist_from_body_nodes_with_selection(
            &current_document,
            current_nodes.clone(),
            &selection,
            &resolver,
            false,
            FloatCounters::default(),
            Some(&mut current_raw_checkpoints),
        );
        let affected_block_index = super::find_affected_block_index(
            &cached_fragment
                .block_checkpoints
                .as_ref()
                .expect("cached checkpoints")
                .checkpoints,
            &cached_nodes,
            &cached_source_lines,
            &current_raw_checkpoints,
            &current_nodes,
            &current_source_lines,
            &cached_source,
            &current_source,
        )
        .expect("affected block index");
        assert_eq!(affected_block_index, 1);

        let rebuilt_fragment = super::suffix_rebuild(
            &service,
            &current_document,
            &current_nodes,
            &selection,
            &resolver,
            &cached_fragment,
            cached_fragment
                .block_checkpoints
                .as_ref()
                .expect("cached checkpoint data"),
            affected_block_index,
        )
        .expect("suffix rebuild should succeed");
        let full_current_typeset = service.typeset_document_with_selection(
            &current_document,
            Some(&current_source_lines),
            &selection,
            &resolver,
        );
        let expected_fragment = fragment_from_typeset_document("main", &full_current_typeset);

        assert_eq!(rebuilt_fragment.pages, expected_fragment.pages);
    }

    #[test]
    fn suffix_rebuild_with_footnotes() {
        let loader = MockAssetBundleLoader::valid();
        let service = service(&FsTestFileAccessGate, &loader);
        let selection = super::CompileFontSelection::Basic;
        let temp = tempdir().expect("create tempdir");
        let overlay_roots: Vec<PathBuf> = Vec::new();
        let resolver = super::CompileGraphicAssetResolver {
            file_access_gate: &FsTestFileAccessGate,
            input_dir: temp.path(),
            project_root: temp.path(),
            overlay_roots: &overlay_roots,
            asset_bundle_path: None,
            diagnostics: RefCell::new(Vec::new()),
        };

        let cached_source = document(concat!(
            "Alpha\\footnote{one}\n\n",
            "Beta.\n\n",
            "Gamma\\footnote{two}."
        ));
        let current_source = document(concat!(
            "Alpha\\footnote{one}\n\n",
            "Beta.\n\n",
            "Gamma revised\\footnote{two}."
        ));
        let cached_source_lines = source_lines_for("main.tex", &cached_source);
        let current_source_lines = source_lines_for("main.tex", &current_source);
        let cached_document = parse_article_document(&cached_source);
        let current_document = parse_article_document(&current_source);
        let (_annotated_cached_document, cached_nodes) =
            super::annotated_body_nodes(&cached_document, Some(&cached_source_lines))
                .expect("annotate cached document");
        let (_annotated_current_document, current_nodes) =
            super::annotated_body_nodes(&current_document, Some(&current_source_lines))
                .expect("annotate current document");
        let mut cached_raw_checkpoints = Vec::new();
        let cached_vlist = service.build_vlist_from_body_nodes_with_selection(
            &cached_document,
            cached_nodes.clone(),
            &selection,
            &resolver,
            false,
            FloatCounters::default(),
            Some(&mut cached_raw_checkpoints),
        );
        let cached_typeset = service.typeset_document_with_selection(
            &cached_document,
            Some(&cached_source_lines),
            &selection,
            &resolver,
        );
        let cached_fragment = CachedTypesetFragment {
            fragment: fragment_from_typeset_document("main", &cached_typeset),
            source_hash: 1,
            block_checkpoints: Some(super::raw_checkpoints_to_block_checkpoint_data(
                &cached_raw_checkpoints,
                &cached_nodes,
                &cached_vlist,
                1,
                cached_document.body.clone(),
            )),
        };
        let mut current_raw_checkpoints = Vec::new();
        let _ = service.build_vlist_from_body_nodes_with_selection(
            &current_document,
            current_nodes.clone(),
            &selection,
            &resolver,
            false,
            FloatCounters::default(),
            Some(&mut current_raw_checkpoints),
        );
        let affected_block_index = super::find_affected_block_index(
            &cached_fragment
                .block_checkpoints
                .as_ref()
                .expect("cached checkpoints")
                .checkpoints,
            &cached_nodes,
            &cached_source_lines,
            &current_raw_checkpoints,
            &current_nodes,
            &current_source_lines,
            &cached_source,
            &current_source,
        )
        .expect("affected block index");
        assert_eq!(affected_block_index, 1);

        let rebuilt_fragment = super::suffix_rebuild(
            &service,
            &current_document,
            &current_nodes,
            &selection,
            &resolver,
            &cached_fragment,
            cached_fragment
                .block_checkpoints
                .as_ref()
                .expect("cached checkpoint data"),
            affected_block_index,
        )
        .expect("suffix rebuild should succeed");
        let full_current_typeset = service.typeset_document_with_selection(
            &current_document,
            Some(&current_source_lines),
            &selection,
            &resolver,
        );
        let expected_fragment = fragment_from_typeset_document("main", &full_current_typeset);

        assert_eq!(rebuilt_fragment.pages, expected_fragment.pages);
    }

    #[test]
    fn suffix_rebuild_with_pending_floats() {
        let loader = MockAssetBundleLoader::valid();
        let service = service(&FsTestFileAccessGate, &loader);
        let selection = super::CompileFontSelection::Basic;
        let temp = tempdir().expect("create tempdir");
        let overlay_roots: Vec<PathBuf> = Vec::new();
        let resolver = super::CompileGraphicAssetResolver {
            file_access_gate: &FsTestFileAccessGate,
            input_dir: temp.path(),
            project_root: temp.path(),
            overlay_roots: &overlay_roots,
            asset_bundle_path: None,
            diagnostics: RefCell::new(Vec::new()),
        };

        let trailing_lines = (3..=60)
            .map(|index| format!("Line {index}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let cached_source = document(&format!(
            "Line 1\n\n\\begin{{figure}}[t]Top body\\caption{{Top}}\\end{{figure}}\n\nLine 2\n\n{trailing_lines}"
        ));
        let current_source = document(&format!(
            "Line 1\n\n\\begin{{figure}}[t]Top body\\caption{{Top}}\\end{{figure}}\n\nLine 2 revised\n\n{trailing_lines}"
        ));
        let cached_source_lines = source_lines_for("main.tex", &cached_source);
        let current_source_lines = source_lines_for("main.tex", &current_source);
        let cached_document = parse_article_document(&cached_source);
        let current_document = parse_article_document(&current_source);
        let (_annotated_cached_document, cached_nodes) =
            super::annotated_body_nodes(&cached_document, Some(&cached_source_lines))
                .expect("annotate cached document");
        let (_annotated_current_document, current_nodes) =
            super::annotated_body_nodes(&current_document, Some(&current_source_lines))
                .expect("annotate current document");
        let mut cached_raw_checkpoints = Vec::new();
        let cached_vlist = service.build_vlist_from_body_nodes_with_selection(
            &cached_document,
            cached_nodes.clone(),
            &selection,
            &resolver,
            false,
            FloatCounters::default(),
            Some(&mut cached_raw_checkpoints),
        );
        let cached_typeset = service.typeset_document_with_selection(
            &cached_document,
            Some(&cached_source_lines),
            &selection,
            &resolver,
        );
        assert_eq!(cached_typeset.pages.len(), 2);
        let cached_fragment = CachedTypesetFragment {
            fragment: fragment_from_typeset_document("main", &cached_typeset),
            source_hash: 1,
            block_checkpoints: Some(super::raw_checkpoints_to_block_checkpoint_data(
                &cached_raw_checkpoints,
                &cached_nodes,
                &cached_vlist,
                1,
                cached_document.body.clone(),
            )),
        };
        let mut current_raw_checkpoints = Vec::new();
        let _ = service.build_vlist_from_body_nodes_with_selection(
            &current_document,
            current_nodes.clone(),
            &selection,
            &resolver,
            false,
            FloatCounters::default(),
            Some(&mut current_raw_checkpoints),
        );
        let affected_block_index = super::find_affected_block_index(
            &cached_fragment
                .block_checkpoints
                .as_ref()
                .expect("cached checkpoints")
                .checkpoints,
            &cached_nodes,
            &cached_source_lines,
            &current_raw_checkpoints,
            &current_nodes,
            &current_source_lines,
            &cached_source,
            &current_source,
        )
        .expect("affected block index");
        assert!(affected_block_index > 0);
        let checkpoint = &cached_fragment
            .block_checkpoints
            .as_ref()
            .expect("cached checkpoint data")
            .checkpoints[affected_block_index];
        assert!(!checkpoint.layout_state.pending_floats.is_empty());

        let rebuilt_fragment = super::suffix_rebuild(
            &service,
            &current_document,
            &current_nodes,
            &selection,
            &resolver,
            &cached_fragment,
            cached_fragment
                .block_checkpoints
                .as_ref()
                .expect("cached checkpoint data"),
            affected_block_index,
        )
        .expect("suffix rebuild should succeed");
        let full_current_typeset = service.typeset_document_with_selection(
            &current_document,
            Some(&current_source_lines),
            &selection,
            &resolver,
        );
        let expected_fragment = fragment_from_typeset_document("main", &full_current_typeset);

        assert_eq!(rebuilt_fragment.pages, expected_fragment.pages);
    }

    #[test]
    fn suffix_rebuild_reports_footnote_count_mismatch() {
        let loader = MockAssetBundleLoader::valid();
        let service = service(&FsTestFileAccessGate, &loader);
        let selection = super::CompileFontSelection::Basic;
        let temp = tempdir().expect("create tempdir");
        let overlay_roots: Vec<PathBuf> = Vec::new();
        let resolver = super::CompileGraphicAssetResolver {
            file_access_gate: &FsTestFileAccessGate,
            input_dir: temp.path(),
            project_root: temp.path(),
            overlay_roots: &overlay_roots,
            asset_bundle_path: None,
            diagnostics: RefCell::new(Vec::new()),
        };

        let cached_source = document(concat!(
            "Alpha\\footnote{one}\n\n",
            "Beta.\n\n",
            "Gamma\\footnote{two}."
        ));
        let current_source = document(concat!(
            "Alpha\\footnote{one}\n\n",
            "Beta.\n\n",
            "Gamma revised\\footnote{two}."
        ));
        let cached_source_lines = source_lines_for("main.tex", &cached_source);
        let current_source_lines = source_lines_for("main.tex", &current_source);
        let cached_document = parse_article_document(&cached_source);
        let current_document = parse_article_document(&current_source);
        let (_annotated_cached_document, cached_nodes) =
            super::annotated_body_nodes(&cached_document, Some(&cached_source_lines))
                .expect("annotate cached document");
        let (_annotated_current_document, current_nodes) =
            super::annotated_body_nodes(&current_document, Some(&current_source_lines))
                .expect("annotate current document");
        let mut cached_raw_checkpoints = Vec::new();
        let cached_vlist = service.build_vlist_from_body_nodes_with_selection(
            &cached_document,
            cached_nodes.clone(),
            &selection,
            &resolver,
            false,
            FloatCounters::default(),
            Some(&mut cached_raw_checkpoints),
        );
        let cached_typeset = service.typeset_document_with_selection(
            &cached_document,
            Some(&cached_source_lines),
            &selection,
            &resolver,
        );
        let cached_fragment = CachedTypesetFragment {
            fragment: fragment_from_typeset_document("main", &cached_typeset),
            source_hash: 1,
            block_checkpoints: Some(super::raw_checkpoints_to_block_checkpoint_data(
                &cached_raw_checkpoints,
                &cached_nodes,
                &cached_vlist,
                1,
                cached_document.body.clone(),
            )),
        };
        let mut current_raw_checkpoints = Vec::new();
        let _ = service.build_vlist_from_body_nodes_with_selection(
            &current_document,
            current_nodes.clone(),
            &selection,
            &resolver,
            false,
            FloatCounters::default(),
            Some(&mut current_raw_checkpoints),
        );
        let affected_block_index = super::find_affected_block_index(
            &cached_fragment
                .block_checkpoints
                .as_ref()
                .expect("cached checkpoints")
                .checkpoints,
            &cached_nodes,
            &cached_source_lines,
            &current_raw_checkpoints,
            &current_nodes,
            &current_source_lines,
            &cached_source,
            &current_source,
        )
        .expect("affected block index");
        assert_eq!(affected_block_index, 1);

        let mut checkpoint_data = cached_fragment
            .block_checkpoints
            .as_ref()
            .expect("cached checkpoint data")
            .clone();
        checkpoint_data.checkpoints[affected_block_index]
            .layout_state
            .footnote_count += 1;

        let failure = super::suffix_rebuild(
            &service,
            &current_document,
            &current_nodes,
            &selection,
            &resolver,
            &cached_fragment,
            &checkpoint_data,
            affected_block_index,
        )
        .expect_err("suffix rebuild should report validation failure");

        assert_eq!(failure, super::SuffixRebuildFailure::FootnoteCountMismatch);
    }

    #[test]
    fn suffix_rebuild_allows_page_count_changes() {
        let loader = MockAssetBundleLoader::valid();
        let service = service(&FsTestFileAccessGate, &loader);
        let selection = super::CompileFontSelection::Basic;
        let temp = tempdir().expect("create tempdir");
        let overlay_roots: Vec<PathBuf> = Vec::new();
        let resolver = super::CompileGraphicAssetResolver {
            file_access_gate: &FsTestFileAccessGate,
            input_dir: temp.path(),
            project_root: temp.path(),
            overlay_roots: &overlay_roots,
            asset_bundle_path: None,
            diagnostics: RefCell::new(Vec::new()),
        };

        let long_paragraph = std::iter::repeat_n("Expanded paragraph text.", 1_600)
            .collect::<Vec<_>>()
            .join(" ");
        let cached_source = document("Alpha.\n\nBeta.\n\nGamma.");
        let current_source = document(&format!("Alpha.\n\nBeta.\n\n{long_paragraph}"));
        let cached_source_lines = source_lines_for("main.tex", &cached_source);
        let current_source_lines = source_lines_for("main.tex", &current_source);
        let cached_document = parse_article_document(&cached_source);
        let current_document = parse_article_document(&current_source);
        let (_annotated_cached_document, cached_nodes) =
            super::annotated_body_nodes(&cached_document, Some(&cached_source_lines))
                .expect("annotate cached document");
        let (_annotated_current_document, current_nodes) =
            super::annotated_body_nodes(&current_document, Some(&current_source_lines))
                .expect("annotate current document");
        let mut cached_raw_checkpoints = Vec::new();
        let cached_vlist = service.build_vlist_from_body_nodes_with_selection(
            &cached_document,
            cached_nodes.clone(),
            &selection,
            &resolver,
            false,
            FloatCounters::default(),
            Some(&mut cached_raw_checkpoints),
        );
        let cached_typeset = service.typeset_document_with_selection(
            &cached_document,
            Some(&cached_source_lines),
            &selection,
            &resolver,
        );
        let cached_fragment = CachedTypesetFragment {
            fragment: fragment_from_typeset_document("main", &cached_typeset),
            source_hash: 1,
            block_checkpoints: Some(super::raw_checkpoints_to_block_checkpoint_data(
                &cached_raw_checkpoints,
                &cached_nodes,
                &cached_vlist,
                1,
                cached_document.body.clone(),
            )),
        };
        let mut current_raw_checkpoints = Vec::new();
        let _ = service.build_vlist_from_body_nodes_with_selection(
            &current_document,
            current_nodes.clone(),
            &selection,
            &resolver,
            false,
            FloatCounters::default(),
            Some(&mut current_raw_checkpoints),
        );
        let affected_block_index = super::find_affected_block_index(
            &cached_fragment
                .block_checkpoints
                .as_ref()
                .expect("cached checkpoints")
                .checkpoints,
            &cached_nodes,
            &cached_source_lines,
            &current_raw_checkpoints,
            &current_nodes,
            &current_source_lines,
            &cached_source,
            &current_source,
        )
        .expect("affected block index");
        assert_eq!(affected_block_index, 1);

        let rebuilt_fragment = super::suffix_rebuild(
            &service,
            &current_document,
            &current_nodes,
            &selection,
            &resolver,
            &cached_fragment,
            cached_fragment
                .block_checkpoints
                .as_ref()
                .expect("cached checkpoint data"),
            affected_block_index,
        )
        .expect("suffix rebuild should allow page count change");
        let full_current_typeset = service.typeset_document_with_selection(
            &current_document,
            Some(&current_source_lines),
            &selection,
            &resolver,
        );
        let expected_fragment = fragment_from_typeset_document("main", &full_current_typeset);

        assert_ne!(
            cached_fragment.fragment.pages.len(),
            expected_fragment.pages.len(),
            "test must exercise a page-count-changing edit"
        );
        assert_eq!(rebuilt_fragment.pages, expected_fragment.pages);
    }

    #[derive(Clone)]
    struct CapturingSubscriber {
        messages: Arc<Mutex<Vec<String>>>,
    }

    #[derive(Default)]
    struct MessageVisitor {
        message: Option<String>,
    }

    impl Visit for MessageVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.message = Some(format!("{value:?}").trim_matches('"').to_string());
            }
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            if field.name() == "message" {
                self.message = Some(value.to_string());
            }
        }
    }

    impl Subscriber for CapturingSubscriber {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            metadata.is_event()
        }

        fn new_span(&self, _span: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _span: &Id, _values: &Record<'_>) {}

        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut visitor = MessageVisitor::default();
            event.record(&mut visitor);
            if let Some(message) = visitor.message {
                self.messages
                    .lock()
                    .expect("lock tracing messages")
                    .push(message);
            }
        }

        fn enter(&self, _span: &Id) {}

        fn exit(&self, _span: &Id) {}
    }

    fn compile_with_trace_messages(
        gate: &dyn FileAccessGate,
        loader: &dyn AssetBundleLoaderPort,
        options: &RuntimeOptions,
    ) -> (super::CompileResult, Vec<String>) {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let subscriber = CapturingSubscriber {
            messages: Arc::clone(&messages),
        };
        let result = tracing::subscriber::with_default(subscriber, || {
            service(gate, loader).compile(options)
        });
        let trace_messages = messages.lock().expect("lock tracing messages").clone();

        (result, trace_messages)
    }

    #[test]
    fn stage_timing_total_sums_present_stages() {
        let timing = StageTiming {
            cache_load: Some(Duration::from_micros(100)),
            source_tree_load: Some(Duration::from_micros(200)),
            parse: Some(Duration::from_micros(300)),
            typeset: Some(Duration::from_micros(400)),
            pdf_render: None,
            cache_store: Some(Duration::from_micros(50)),
            typeset_partition_details: None,
            pass_count: None,
        };

        assert_eq!(timing.total(), Duration::from_micros(1050));
    }

    #[test]
    fn stage_timing_default_is_all_none() {
        let timing = StageTiming::default();

        assert_eq!(timing.total(), Duration::ZERO);
        assert!(timing.cache_load.is_none());
        assert!(timing.source_tree_load.is_none());
        assert!(timing.parse.is_none());
        assert!(timing.typeset.is_none());
        assert!(timing.pdf_render.is_none());
        assert!(timing.cache_store.is_none());
        assert!(timing.typeset_partition_details.is_none());
        assert!(timing.pass_count.is_none());
    }

    #[test]
    fn compile_success_populates_stage_timing_on_success_path() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        fs::write(
            &input_file,
            document("\\section{Timing}\nStage timing instrumentation smoke.\n"),
        )
        .expect("write input");

        let mut options = runtime_options(input_file, dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0, "{:?}", result.diagnostics);
        assert!(result.stage_timing.cache_load.is_some());
        assert!(result.stage_timing.source_tree_load.is_some());
        assert!(result.stage_timing.parse.is_some());
        assert!(result.stage_timing.typeset.is_some());
        assert!(result.stage_timing.pdf_render.is_some());
        assert!(result.stage_timing.cache_store.is_some());
        assert!(result.stage_timing.typeset_partition_details.is_none());
        assert!(result.stage_timing.total() > Duration::ZERO);
    }

    struct ParallelFullTypesetCollisionGuard;

    impl Drop for ParallelFullTypesetCollisionGuard {
        fn drop(&mut self) {
            super::FORCE_PARALLEL_FULL_TYPESET_COLLISION.store(false, Ordering::SeqCst);
        }
    }

    fn force_parallel_full_typeset_collision() -> ParallelFullTypesetCollisionGuard {
        super::FORCE_PARALLEL_FULL_TYPESET_COLLISION.store(true, Ordering::SeqCst);
        ParallelFullTypesetCollisionGuard
    }

    fn test_typeset_document(lines: Vec<TextLine>) -> TypesetDocument {
        TypesetDocument {
            pages: vec![TypesetPage {
                lines,
                images: Vec::new(),
                page_box: PageBox {
                    width: points(612),
                    height: points(792),
                },
                float_placements: Vec::new(),
                index_entries: Vec::new(),
            }],
            outlines: Vec::new(),
            named_destinations: Vec::new(),
            title: None,
            author: None,
            navigation: Default::default(),
            index_entries: Vec::new(),
            has_unresolved_index: false,
        }
    }

    #[test]
    fn reusable_page_payloads_skip_pages_with_xobject_resources() {
        let mut document = test_typeset_document(Vec::new());
        document.pages[0].images.push(TypesetImage {
            scene: GraphicsScene {
                nodes: vec![super::GraphicNode::External(super::ExternalGraphic {
                    path: "image.png".to_string(),
                    asset_handle: super::AssetHandle {
                        id: super::LogicalAssetId(super::StableId(1)),
                    },
                    metadata: super::ImageMetadata {
                        width: 1,
                        height: 1,
                        color_space: ImageColorSpace::DeviceRGB,
                        bits_per_component: 8,
                    },
                })],
            },
            x: points(72),
            y: points(600),
            display_width: points(100),
            display_height: points(100),
        });
        document.outlines = vec![TypesetOutline {
            level: 0,
            title: "One".to_string(),
            page_index: 0,
            y: points(720),
        }];
        let partition_id = "chapter:0001:one".to_string();
        let plan = super::DocumentPartitionPlan {
            fallback_partition_id: "document:0000:book".to_string(),
            work_units: vec![super::DocumentWorkUnit {
                partition_id: partition_id.clone(),
                kind: super::PartitionKind::Chapter,
                locator: PartitionLocator {
                    entry_file: "book.tex".into(),
                    level: 0,
                    ordinal: 0,
                    title: "One".to_string(),
                },
                title: "One".to_string(),
            }],
        };
        let partition_details = vec![PartitionTypesetDetail {
            partition_id: partition_id.clone(),
            reuse_type: PartitionTypesetReuseType::Cached,
            suffix_block_count: 0,
            total_block_count: 0,
            elapsed: None,
            fallback_reason: None,
        }];
        let cached_page_payloads = BTreeMap::from([(
            partition_id,
            vec![
                PageRenderPayload::new(0, Vec::new(), BTreeSet::new(), "BT\nET\n".to_string())
                    .into(),
            ],
        )]);

        let reusable = reusable_page_payloads_for_render(
            &document,
            &plan,
            Some(&partition_details),
            &cached_page_payloads,
        );

        assert!(reusable.is_empty());
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

    fn build_test_pfb(font_name: &str) -> Vec<u8> {
        fn segment(segment_type: u8, payload: &[u8]) -> Vec<u8> {
            let mut bytes = vec![0x80, segment_type];
            bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            bytes.extend_from_slice(payload);
            bytes
        }

        let ascii = format!("%!FontType1-1.0: {font_name} 001.000\n/FontName /{font_name} def\n");
        let binary = [0xde, 0xad, 0xbe, 0xef];
        let trailer = b"cleartomark\n";
        let mut bytes = segment(0x01, ascii.as_bytes());
        bytes.extend_from_slice(&segment(0x02, &binary));
        bytes.extend_from_slice(&segment(0x01, trailer));
        bytes.extend_from_slice(&[0x80, 0x03]);
        bytes
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

    fn write_asset_bundle_fixture(bundle_path: &Path) {
        let mut tex_inputs = BTreeMap::new();
        let mut packages = BTreeMap::new();
        let mut opentype_fonts = BTreeMap::new();
        let mut tfm_fonts = BTreeMap::new();
        let mut default_opentype_fonts = Vec::new();

        collect_bundle_assets(
            bundle_path,
            bundle_path,
            &mut tex_inputs,
            &mut packages,
            &mut opentype_fonts,
            &mut tfm_fonts,
            &mut default_opentype_fonts,
        );
        default_opentype_fonts.sort();
        default_opentype_fonts.dedup();

        fs::create_dir_all(bundle_path).expect("create bundle dir");
        fs::write(
            bundle_path.join("manifest.json"),
            serde_json::to_vec(&json!({
                "name": "test-bundle",
                "version": "2026.03.18",
                "min_ferritex_version": "0.1.0",
                "format_version": 1,
                "asset_index_path": "asset-index.json",
            }))
            .expect("serialize manifest"),
        )
        .expect("write manifest");
        fs::write(
            bundle_path.join("asset-index.json"),
            serde_json::to_vec(&json!({
                "tex_inputs": tex_inputs,
                "packages": packages,
                "opentype_fonts": opentype_fonts,
                "tfm_fonts": tfm_fonts,
                "default_opentype_fonts": default_opentype_fonts,
            }))
            .expect("serialize asset index"),
        )
        .expect("write asset index");
    }

    fn collect_bundle_assets(
        bundle_root: &Path,
        current: &Path,
        tex_inputs: &mut BTreeMap<String, String>,
        packages: &mut BTreeMap<String, String>,
        opentype_fonts: &mut BTreeMap<String, String>,
        tfm_fonts: &mut BTreeMap<String, String>,
        default_opentype_fonts: &mut Vec<String>,
    ) {
        let Ok(read_dir) = fs::read_dir(current) else {
            return;
        };

        let mut entries = read_dir
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        entries.sort();

        for path in entries {
            if path.is_dir() {
                collect_bundle_assets(
                    bundle_root,
                    &path,
                    tex_inputs,
                    packages,
                    opentype_fonts,
                    tfm_fonts,
                    default_opentype_fonts,
                );
                continue;
            }

            let Some(relative) = path
                .strip_prefix(bundle_root)
                .ok()
                .map(|path| path.to_string_lossy().replace('\\', "/"))
            else {
                continue;
            };

            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.to_ascii_lowercase());
            match extension.as_deref() {
                Some("tex") => {
                    let logical = relative
                        .strip_prefix("texmf/")
                        .unwrap_or(&relative)
                        .to_string();
                    tex_inputs.insert(logical, relative);
                }
                Some("sty") => {
                    let logical = relative
                        .strip_prefix("texmf/")
                        .unwrap_or(&relative)
                        .to_ascii_lowercase();
                    packages.insert(logical, relative);
                }
                Some("ttf") | Some("otf") => {
                    let key = path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .map(|stem| {
                            stem.chars()
                                .filter(|ch| ch.is_alphanumeric())
                                .flat_map(|ch| ch.to_lowercase())
                                .collect::<String>()
                        })
                        .unwrap_or_default();
                    if !key.is_empty() {
                        opentype_fonts.insert(key, relative.clone());
                        default_opentype_fonts.push(relative);
                    }
                }
                Some("tfm") => {
                    let key = path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .map(|stem| stem.to_ascii_lowercase())
                        .unwrap_or_default();
                    if !key.is_empty() {
                        tfm_fonts.insert(key, relative);
                    }
                }
                _ => {}
            }
        }
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
        assert!(synctex.fragments.iter().any(|fragment| {
            fragment.text == "Hello world"
                && fragment.span.start.line == 3
                && fragment.span.start.column == 1
                && fragment.span.end.column == 12
        }));
    }

    #[test]
    fn compile_with_synctex_preserves_duplicate_visible_text_source_order() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\begin{document}\nAlpha \\href{https://example.com}{Beta}\n\nAlpha Beta\n\\end{document}\n",
        )
        .expect("write input");

        let mut options = runtime_options(input_file.clone(), dir.path().join("out"));
        options.synctex = true;
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let synctex = read_synctex(&options.output_dir.join("main.synctex"));
        let linked_positions = synctex.forward_search(SourceLocation {
            file_id: 0,
            line: 3,
            column: 20,
        });
        let plain_positions = synctex.forward_search(SourceLocation {
            file_id: 0,
            line: 5,
            column: 7,
        });

        assert_eq!(linked_positions.len(), 1);
        assert_eq!(plain_positions.len(), 1);
        assert_ne!(linked_positions[0], plain_positions[0]);
        assert_eq!(
            synctex.inverse_search(linked_positions[0]),
            Some(SourceSpan {
                start: SourceLocation {
                    file_id: 0,
                    line: 3,
                    column: 1,
                },
                end: SourceLocation {
                    file_id: 0,
                    line: 3,
                    column: 38,
                },
            })
        );
        assert_eq!(
            synctex.inverse_search(plain_positions[0]),
            Some(SourceSpan {
                start: SourceLocation {
                    file_id: 0,
                    line: 5,
                    column: 1,
                },
                end: SourceLocation {
                    file_id: 0,
                    line: 5,
                    column: 11,
                },
            })
        );
    }

    #[test]
    fn synctex_data_for_remaps_fallback_fragments_into_merged_files() {
        let main_file = "/tmp/main.tex".to_string();
        let chapter_file = "/tmp/chapter.tex".to_string();
        let source_lines = vec![
            ferritex_core::synctex::SourceLineTrace {
                file: main_file.clone(),
                line: 3,
                text: "Annotated text".to_string(),
            },
            ferritex_core::synctex::SourceLineTrace {
                file: chapter_file.clone(),
                line: 7,
                text: "Original chapter".to_string(),
            },
        ];
        let document = test_typeset_document(vec![
            TextLine {
                text: "Annotated text".to_string(),
                x: DimensionValue::zero(),
                y: points(720),
                links: Vec::new(),
                font_index: 0,
                font_size: points(10),
                source_span: None,
            },
            TextLine {
                text: "Rendered".to_string(),
                x: DimensionValue::zero(),
                y: points(702),
                links: Vec::new(),
                font_index: 0,
                font_size: points(10),
                source_span: None,
            },
        ]);

        let synctex = super::synctex_data_for(&document, &source_lines);

        assert_eq!(synctex.files, vec![main_file, chapter_file]);
        assert!(
            synctex
                .forward_search(SourceLocation {
                    file_id: 1,
                    line: 7,
                    column: 1,
                })
                .len()
                >= 1
        );
        assert!(
            synctex
                .forward_search(SourceLocation {
                    file_id: 0,
                    line: 3,
                    column: 1,
                })
                .len()
                >= 1
        );
        assert!(synctex
            .forward_search(SourceLocation {
                file_id: 0,
                line: 7,
                column: 1,
            })
            .is_empty());

        let annotated_fragment = synctex
            .fragments
            .iter()
            .find(|fragment| fragment.text == "Annotated text")
            .expect("annotated fragment");
        assert_eq!(annotated_fragment.span.start.file_id, 0);
        assert_eq!(annotated_fragment.span.end.file_id, 0);

        let fallback_fragment = synctex
            .fragments
            .iter()
            .find(|fragment| fragment.text == "Rendered")
            .expect("fallback fragment");
        assert_eq!(fallback_fragment.span.start.file_id, 1);
        assert_eq!(fallback_fragment.span.end.file_id, 1);
        assert_eq!(fallback_fragment.span.start.line, 7);
    }

    #[test]
    fn synctex_data_for_includes_unannotated_float_lines_in_fallback() {
        let source_lines = vec![ferritex_core::synctex::SourceLineTrace {
            file: "/tmp/main.tex".to_string(),
            line: 5,
            text: "Float caption coverage".to_string(),
        }];
        let mut document = test_typeset_document(Vec::new());
        document.pages[0].float_placements.push(FloatPlacement {
            region: FloatRegion::Top,
            content: FloatContent {
                lines: vec![TextLine {
                    text: "Float caption coverage".to_string(),
                    x: DimensionValue::zero(),
                    y: DimensionValue::zero(),
                    links: Vec::new(),
                    font_index: 0,
                    font_size: points(10),
                    source_span: None,
                }],
                images: Vec::new(),
                height: points(10),
            },
            y_position: points(720),
        });

        let synctex = super::synctex_data_for(&document, &source_lines);

        assert!(synctex
            .fragments
            .iter()
            .any(|fragment| fragment.text == "Float caption coverage"));
        assert_eq!(
            synctex
                .forward_search(SourceLocation {
                    file_id: 0,
                    line: 5,
                    column: 1,
                })
                .len(),
            1
        );
    }

    #[test]
    fn synctex_data_for_excludes_synthetic_page_number_lines() {
        let source_lines = vec![ferritex_core::synctex::SourceLineTrace {
            file: "/tmp/main.tex".to_string(),
            line: 3,
            text: "Body".to_string(),
        }];
        let document = test_typeset_document(vec![
            TextLine {
                text: "Body".to_string(),
                x: DimensionValue::zero(),
                y: points(720),
                links: Vec::new(),
                font_index: 0,
                font_size: points(10),
                source_span: None,
            },
            TextLine {
                text: "1".to_string(),
                x: DimensionValue::zero(),
                y: points(139),
                links: Vec::new(),
                font_index: 0,
                font_size: points(10),
                source_span: None,
            },
        ]);

        let synctex = super::synctex_data_for(&document, &source_lines);

        assert!(synctex
            .fragments
            .iter()
            .any(|fragment| fragment.text == "Body"));
        assert!(!synctex
            .fragments
            .iter()
            .any(|fragment| fragment.text == "1"));
        assert_eq!(
            synctex
                .forward_search(SourceLocation {
                    file_id: 0,
                    line: 3,
                    column: 1,
                })
                .len(),
            1
        );
    }

    #[test]
    fn source_span_annotator_marks_wrapped_lines_with_same_span() {
        let source_lines = vec![ferritex_core::synctex::SourceLineTrace {
            file: "/tmp/main.tex".to_string(),
            line: 3,
            text: "Hello world".to_string(),
        }];
        let mut document = test_typeset_document(vec![
            TextLine {
                text: "Hello".to_string(),
                x: DimensionValue::zero(),
                y: points(720),
                links: Vec::new(),
                font_index: 0,
                font_size: points(10),
                source_span: None,
            },
            TextLine {
                text: "world".to_string(),
                x: DimensionValue::zero(),
                y: points(702),
                links: Vec::new(),
                font_index: 0,
                font_size: points(10),
                source_span: None,
            },
        ]);

        let annotator = super::SourceSpanAnnotator::new(&source_lines);
        let used_source_lines = annotator.annotate_pages(&mut document);
        let expected_span = SourceSpan {
            start: SourceLocation {
                file_id: 0,
                line: 3,
                column: 1,
            },
            end: SourceLocation {
                file_id: 0,
                line: 3,
                column: 12,
            },
        };

        assert_eq!(used_source_lines, BTreeSet::from([0]));
        assert_eq!(document.pages[0].lines[0].source_span, Some(expected_span));
        assert_eq!(document.pages[0].lines[1].source_span, Some(expected_span));
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

        let cache_record_dir = fs::read_dir(options.output_dir.join(".ferritex-cache"))
            .expect("read cache dir")
            .map(|entry| entry.expect("cache entry").path())
            .next()
            .expect("cache record dir");
        let cache_file = cache_record_dir.join("index.bin");
        fs::write(&cache_file, b"not-valid-bincode").expect("corrupt cache metadata");

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
                    .contains("compile cache index is invalid")
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
        assert_eq!(
            pdf_text_operators(&String::from_utf8_lossy(&incremental_pdf)),
            pdf_text_operators(&String::from_utf8_lossy(&full_pdf))
        );
    }

    #[test]
    fn partial_recompile_single_file_edit_matches_full() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let chapter_one = dir.path().join("chapter-one.tex");
        let chapter_two = dir.path().join("chapter-two.tex");
        let chapter_three = dir.path().join("chapter-three.tex");
        fs::write(
            &input_file,
            report_document(
                "\\input{chapter-one}\n\\newpage\n\\input{chapter-two}\n\\newpage\n\\input{chapter-three}",
            ),
        )
        .expect("write main input");
        fs::write(
            &chapter_one,
            "\\chapter{One}\\label{chap:one}\nAlpha body.\n",
        )
        .expect("write chapter one");
        fs::write(
            &chapter_two,
            "\\chapter{Two}\\label{chap:two}\nOriginal body text.\n",
        )
        .expect("write chapter two");
        fs::write(
            &chapter_three,
            "\\chapter{Three}\\label{chap:three}\nSee Chapter \\ref{chap:two}.\n",
        )
        .expect("write chapter three");

        let mut options = runtime_options(input_file.clone(), dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let first = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(first.exit_code, 0);

        fs::write(
            &chapter_two,
            "\\chapter{Two}\\label{chap:two}\nEdited body text after cache warmup.\n",
        )
        .expect("update chapter two");

        let (second, trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &options);
        let second_state = second
            .stable_compile_state
            .clone()
            .expect("second stable state");
        assert_eq!(second.exit_code, 0);
        assert!(
            trace_messages
                .iter()
                .any(|message| message.contains("partial typeset reuse applied")),
            "{trace_messages:?}"
        );
        assert_eq!(second_state.snapshot.pass_number, 1);

        let incremental_pdf =
            fs::read(options.output_dir.join("main.pdf")).expect("read incremental pdf");
        let incremental_pdf_text = String::from_utf8_lossy(&incremental_pdf);
        assert!(incremental_pdf_text.contains("Edited body text after cache warmup."));

        let mut full_options = runtime_options(input_file, dir.path().join("out-full"));
        full_options.no_cache = true;
        let full = service(&FsTestFileAccessGate, &loader).compile(&full_options);
        assert_eq!(full.exit_code, 0);

        let full_pdf = fs::read(full_options.output_dir.join("main.pdf")).expect("read full pdf");
        assert_eq!(
            pdf_text_operators(&String::from_utf8_lossy(&incremental_pdf)),
            pdf_text_operators(&String::from_utf8_lossy(&full_pdf))
        );
    }

    #[test]
    fn block_checkpoint_single_paragraph_edit_parity() {
        let dir = tempdir().expect("create tempdir");
        let input_file = write_partitioned_report_project(
            dir.path(),
            &[
                (
                    "chapter-one.tex",
                    "\\chapter{One}\\label{chap:one}\nStable opening chapter.\n",
                ),
                (
                    "chapter-two.tex",
                    "\\chapter{Two}\\label{chap:two}\nAlpha paragraph.\n\nBeta paragraph.\n\nGamma paragraph.\n",
                ),
            ],
        );
        let mut options = runtime_options(input_file.clone(), dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let warmup = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(warmup.exit_code, 0);

        fs::write(
            dir.path().join("chapter-two.tex"),
            "\\chapter{Two}\\label{chap:two}\nAlpha paragraph.\n\nBeta paragraph after block reuse.\n\nGamma paragraph.\n",
        )
        .expect("update chapter two");

        let (incremental, trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &options);
        assert_eq!(incremental.exit_code, 0);
        assert!(
            trace_messages
                .iter()
                .any(|message| message.contains("partial typeset reuse applied")),
            "{trace_messages:?}"
        );
        assert!(
            trace_messages
                .iter()
                .all(|message| !message.contains("partial typeset fallback to full typeset")),
            "{trace_messages:?}"
        );

        let incremental_pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(incremental_pdf.contains("Beta paragraph after block reuse."));

        let mut full_options = runtime_options(input_file, dir.path().join("out-full"));
        full_options.no_cache = true;
        let full = service(&FsTestFileAccessGate, &loader).compile(&full_options);
        assert_eq!(full.exit_code, 0);

        let full_pdf = read_pdf(&full_options.output_dir.join("main.pdf"));
        assert_eq!(incremental_pdf, full_pdf);
    }

    #[test]
    fn block_checkpoint_page_count_change_uses_suffix_rebuild() {
        let dir = tempdir().expect("create tempdir");
        let input_file = write_partitioned_report_project(
            dir.path(),
            &[
                (
                    "chapter-one.tex",
                    "\\chapter{One}\\label{chap:one}\nStable opening chapter.\n",
                ),
                (
                    "chapter-two.tex",
                    "\\chapter{Two}\\label{chap:two}\nAlpha paragraph.\n\nBeta paragraph.\n\nGamma paragraph.\n",
                ),
            ],
        );
        let mut options = runtime_options(input_file.clone(), dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let warmup = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(warmup.exit_code, 0);

        let expanded_paragraph = std::iter::repeat("Expanded body text for page growth.")
            .take(1_600)
            .collect::<Vec<_>>()
            .join(" ");
        fs::write(
            dir.path().join("chapter-two.tex"),
            format!(
                "\\chapter{{Two}}\\label{{chap:two}}\nAlpha paragraph.\n\nBeta paragraph.\n\n{expanded_paragraph}\n"
            ),
        )
        .expect("update chapter two");

        let (incremental, trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &options);
        assert_eq!(incremental.exit_code, 0, "{:?}", incremental.diagnostics);
        assert!(
            trace_messages
                .iter()
                .any(|message| message.contains("partial typeset reuse applied")),
            "{trace_messages:?}"
        );
        assert!(
            trace_messages
                .iter()
                .all(|message| !message.contains("partial typeset fallback to full typeset")),
            "{trace_messages:?}"
        );

        let details = incremental
            .stage_timing
            .typeset_partition_details
            .as_ref()
            .expect("partition details should be recorded");
        let rebuilt_detail = partition_detail(details, "chapter:0002:2-two");
        assert_eq!(
            rebuilt_detail.reuse_type,
            PartitionTypesetReuseType::SuffixRebuild
        );
        assert_eq!(rebuilt_detail.fallback_reason, None);

        let incremental_pdf = read_pdf(&options.output_dir.join("main.pdf"));
        let mut full_options = runtime_options(input_file, dir.path().join("out-full"));
        full_options.no_cache = true;
        let full = service(&FsTestFileAccessGate, &loader).compile(&full_options);
        assert_eq!(full.exit_code, 0, "{:?}", full.diagnostics);

        let full_pdf = read_pdf(&full_options.output_dir.join("main.pdf"));
        assert_eq!(incremental_pdf, full_pdf);
    }

    #[test]
    fn block_checkpoint_single_paragraph_edit_records_partition_details() {
        let dir = tempdir().expect("create tempdir");
        let input_file = write_partitioned_report_project(
            dir.path(),
            &[
                (
                    "chapter-one.tex",
                    "\\chapter{One}\\label{chap:one}\nStable opening chapter.\n",
                ),
                (
                    "chapter-two.tex",
                    "\\chapter{Two}\\label{chap:two}\nAlpha paragraph.\n\nBeta paragraph.\n\nGamma paragraph.\n",
                ),
            ],
        );
        let mut options = runtime_options(input_file, dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let warmup = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(warmup.exit_code, 0);

        fs::write(
            dir.path().join("chapter-two.tex"),
            "\\chapter{Two}\\label{chap:two}\nAlpha paragraph.\n\nBeta paragraph after diagnostic edit.\n\nGamma paragraph.\n",
        )
        .expect("update chapter two");

        let incremental = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(incremental.exit_code, 0, "{:?}", incremental.diagnostics);

        let details = incremental
            .stage_timing
            .typeset_partition_details
            .as_ref()
            .expect("partition details should be recorded");
        eprintln!("pageref details: {details:?}");
        assert_eq!(details.len(), 2);

        let cached_detail = partition_detail(details, "chapter:0001:1-one");
        assert_eq!(cached_detail.reuse_type, PartitionTypesetReuseType::Cached);
        assert_eq!(cached_detail.suffix_block_count, 0);
        assert!(cached_detail.total_block_count >= 1, "{cached_detail:?}");
        assert_eq!(cached_detail.elapsed, Some(Duration::ZERO));
        assert_eq!(cached_detail.fallback_reason, None);

        let rebuilt_detail = partition_detail(details, "chapter:0002:2-two");
        assert_eq!(
            rebuilt_detail.reuse_type,
            PartitionTypesetReuseType::SuffixRebuild
        );
        assert!(rebuilt_detail.total_block_count >= 2, "{rebuilt_detail:?}");
        assert!(rebuilt_detail.suffix_block_count > 0, "{rebuilt_detail:?}");
        assert!(
            rebuilt_detail.suffix_block_count < rebuilt_detail.total_block_count,
            "{rebuilt_detail:?}"
        );
        assert_eq!(rebuilt_detail.fallback_reason, None);
        assert!(
            rebuilt_detail.elapsed.unwrap_or_default() > Duration::ZERO,
            "{rebuilt_detail:?}"
        );
    }

    #[test]
    fn block_checkpoint_single_paragraph_edit_with_packages() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let chapter_one = dir.path().join("chapter-one.tex");
        let chapter_two = dir.path().join("chapter-two.tex");
        fs::write(
            &input_file,
            concat!(
                "\\documentclass{report}\n",
                "\\usepackage{amsmath}\n",
                "\\def\\mymacro{Custom macro text.}\n",
                "\\begin{document}\n",
                "\\input{chapter-one}\n",
                "\\newpage\n",
                "\\input{chapter-two}\n",
                "\\end{document}\n",
            ),
        )
        .expect("write main input");
        fs::write(
            &chapter_one,
            concat!(
                "\\chapter{One}\\label{chap:one}\n",
                "Stable opening chapter.\n",
            ),
        )
        .expect("write chapter one input");
        fs::write(
            &chapter_two,
            concat!(
                "\\chapter{Two}\\label{chap:two}\n",
                "Alpha paragraph.\n\n",
                "\\mymacro\n\n",
                "\\begin{align}\n",
                "E &= mc^2\n",
                "\\end{align}\n\n",
                "Beta paragraph.\n",
            ),
        )
        .expect("write chapter two input");

        let mut options = runtime_options(input_file.clone(), dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let warmup = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(warmup.exit_code, 0);

        fs::write(
            &chapter_two,
            concat!(
                "\\chapter{Two}\\label{chap:two}\n",
                "Alpha paragraph.\n\n",
                "\\mymacro\n\n",
                "\\begin{align}\n",
                "E &= mc^2\n",
                "\\end{align}\n\n",
                "Beta paragraph after block reuse.\n",
            ),
        )
        .expect("update chapter two input");

        let (incremental, trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &options);
        assert_eq!(incremental.exit_code, 0);
        assert!(
            trace_messages
                .iter()
                .any(|message| message.contains("partial typeset reuse applied")),
            "{trace_messages:?}"
        );
        assert!(
            trace_messages
                .iter()
                .all(|message| !message.contains("partial typeset fallback to full typeset")),
            "{trace_messages:?}"
        );

        let incremental_pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(incremental_pdf.contains("Beta paragraph after block reuse."));

        let mut full_options = runtime_options(input_file, dir.path().join("out-full"));
        full_options.no_cache = true;
        let full = service(&FsTestFileAccessGate, &loader).compile(&full_options);
        assert_eq!(full.exit_code, 0);

        let full_pdf = read_pdf(&full_options.output_dir.join("main.pdf"));
        assert_eq!(incremental_pdf, full_pdf);
    }

    #[test]
    fn block_checkpoint_heading_addition_fallback() {
        let dir = tempdir().expect("create tempdir");
        let input_file = write_partitioned_report_project(
            dir.path(),
            &[
                (
                    "chapter-one.tex",
                    "\\chapter{One}\\label{chap:one}\nStable opening chapter.\n",
                ),
                (
                    "chapter-two.tex",
                    "\\chapter{Two}\\label{chap:two}\nStable opening paragraph.\n\nStable closing paragraph.\n",
                ),
            ],
        );
        let mut options = runtime_options(input_file.clone(), dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let warmup = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(warmup.exit_code, 0);

        fs::write(
            dir.path().join("chapter-two.tex"),
            "\\chapter{Two}\\label{chap:two}\nStable opening paragraph.\n\n\\section{Inserted heading}\nInserted paragraph after heading.\n\nStable closing paragraph.\n",
        )
        .expect("update chapter two with heading");

        let (incremental, trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &options);
        assert_eq!(incremental.exit_code, 0);
        assert!(
            trace_messages
                .iter()
                .any(|message| message.contains("partial typeset reuse applied")),
            "{trace_messages:?}"
        );
        assert!(
            trace_messages
                .iter()
                .all(|message| !message.contains("partial typeset fallback to full typeset")),
            "{trace_messages:?}"
        );

        let incremental_pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(incremental_pdf.contains("Inserted heading"));
        assert!(incremental_pdf.contains("Inserted paragraph after heading."));
        let details = incremental
            .stage_timing
            .typeset_partition_details
            .as_ref()
            .expect("partition details should be recorded");
        let rebuilt_detail = partition_detail(details, "chapter:0002:2-two");
        assert_eq!(
            rebuilt_detail.reuse_type,
            PartitionTypesetReuseType::FullRebuild
        );
        assert_eq!(
            rebuilt_detail.fallback_reason,
            Some(PartitionTypesetFallbackReason::BlockStructureChanged)
        );

        let mut full_options = runtime_options(input_file, dir.path().join("out-full"));
        full_options.no_cache = true;
        let full = service(&FsTestFileAccessGate, &loader).compile(&full_options);
        assert_eq!(full.exit_code, 0);

        let full_pdf = read_pdf(&full_options.output_dir.join("main.pdf"));
        assert_eq!(incremental_pdf, full_pdf);
    }

    #[test]
    fn incremental_pageref_records_full_rebuild_reason() {
        let dir = tempdir().expect("create tempdir");
        let input_file = write_partitioned_report_project(
            dir.path(),
            &[
                (
                    "chapter-one.tex",
                    "\\chapter{One}\\label{chap:one}\nSee Chapter Two on page \\pageref{chap:two}.\n",
                ),
                (
                    "chapter-two.tex",
                    "\\chapter{Two}\\label{chap:two}\nStable target chapter.\n",
                ),
            ],
        );
        let mut options = runtime_options(input_file, dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let warmup = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(warmup.exit_code, 0, "{:?}", warmup.diagnostics);

        fs::write(
            dir.path().join("chapter-one.tex"),
            "\\chapter{One}\\label{chap:one}\nEdited body with \\pageref{chap:two} still present.\n",
        )
        .expect("update chapter one");

        let incremental = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(incremental.exit_code, 0, "{:?}", incremental.diagnostics);

        let details = incremental
            .stage_timing
            .typeset_partition_details
            .as_ref()
            .expect("partition details should be recorded");
        assert_eq!(details.len(), 2);
        assert!(
            details.iter().any(|detail| {
                detail.reuse_type == PartitionTypesetReuseType::FullRebuild
                    && detail.fallback_reason
                        == Some(PartitionTypesetFallbackReason::TypesetCallbackCount)
            }),
            "{details:?}"
        );
        assert!(
            details
                .iter()
                .all(|detail| { detail.reuse_type != PartitionTypesetReuseType::SuffixRebuild }),
            "{details:?}"
        );
    }

    #[test]
    #[ignore = "diagnostic profiling output for 1000-section staged input"]
    fn typeset_dominance_diagnostic() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let mut body = String::new();
        for index in 1..=1000 {
            let stem = format!("section-{index:04}");
            body.push_str(&format!("\\input{{{stem}}}\n"));
            fs::write(
                dir.path().join(format!("{stem}.tex")),
                format!(
                    "\\section{{Section {index:04}}}\\label{{sec:{index:04}}}\nAlpha paragraph {index:04}.\n\nBeta paragraph {index:04}.\n\nGamma paragraph {index:04}.\n"
                ),
            )
            .expect("write section input");
        }
        fs::write(&input_file, document(&body)).expect("write main input");

        let mut options = runtime_options(input_file, dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let warmup = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(warmup.exit_code, 0, "{:?}", warmup.diagnostics);

        fs::write(
            dir.path().join("section-0900.tex"),
            "\\section{Section 0900}\\label{sec:0900}\nAlpha paragraph 0900.\n\nBeta paragraph 0900 after diagnostic edit.\n\nGamma paragraph 0900.\n",
        )
        .expect("update staged section");

        let incremental = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(incremental.exit_code, 0, "{:?}", incremental.diagnostics);

        let details = incremental
            .stage_timing
            .typeset_partition_details
            .as_ref()
            .expect("partition details should be recorded");
        assert_eq!(details.len(), 1000);

        for line in partition_detail_diagnostic_lines(details) {
            println!("{line}");
        }

        let non_cached = details
            .iter()
            .filter(|detail| detail.reuse_type != PartitionTypesetReuseType::Cached)
            .collect::<Vec<_>>();
        assert!(!non_cached.is_empty(), "{details:?}");
        assert!(
            details.iter().all(|detail| {
                !matches!(
                    detail.fallback_reason,
                    Some(PartitionTypesetFallbackReason::SuffixValidationFailed(_))
                )
            }),
            "{:?}",
            partition_detail_diagnostic_lines(details)
        );
    }

    #[test]
    #[ignore = "REQ-NF-002 stage breakdown: 5-run median profiling (~6 min)"]
    fn incremental_stage_timing_5run_median() {
        fn median_duration(values: &[Duration]) -> Duration {
            let mut sorted = values.to_vec();
            sorted.sort_unstable();
            sorted[2]
        }

        fn run_list_ms(values: &[Duration]) -> String {
            values
                .iter()
                .map(|value| format!("{}ms", value.as_millis()))
                .collect::<Vec<_>>()
                .join(", ")
        }

        fn run_list_pass_counts(values: &[Option<u32>]) -> String {
            values
                .iter()
                .map(|pass_count| match pass_count {
                    Some(count) => format!("{count}"),
                    None => "N/A".to_string(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        }

        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let mut body = String::new();
        for index in 1..=1000 {
            let stem = format!("section-{index:04}");
            body.push_str(&format!("\\input{{{stem}}}\n"));
            fs::write(
                dir.path().join(format!("{stem}.tex")),
                format!(
                    "\\section{{Section {index:04}}}\\label{{sec:{index:04}}}\nAlpha paragraph {index:04}.\n\nBeta paragraph {index:04}.\n\nGamma paragraph {index:04}.\n"
                ),
            )
            .expect("write section input");
        }
        fs::write(&input_file, document(&body)).expect("write main input");

        let mut options = runtime_options(input_file, dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let warmup = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(warmup.exit_code, 0, "{:?}", warmup.diagnostics);

        let mut cache_load_runs = Vec::with_capacity(5);
        let mut source_tree_load_runs = Vec::with_capacity(5);
        let mut parse_runs = Vec::with_capacity(5);
        let mut typeset_runs = Vec::with_capacity(5);
        let mut pdf_render_runs = Vec::with_capacity(5);
        let mut cache_store_runs = Vec::with_capacity(5);
        let mut total_runs = Vec::with_capacity(5);
        let mut pass_count_runs = Vec::with_capacity(5);

        for i in 1..=5 {
            fs::write(
                dir.path().join("section-0900.tex"),
                format!(
                    "\\section{{Section 0900}}\\label{{sec:0900}}\nAlpha paragraph 0900.\n\nBeta paragraph 0900 after benchmark run {i} edit.\n\nGamma paragraph 0900.\n"
                ),
            )
            .expect("update staged section");

            let incremental = service(&FsTestFileAccessGate, &loader).compile(&options);
            assert_eq!(incremental.exit_code, 0, "{:?}", incremental.diagnostics);

            let timing = incremental.stage_timing;
            cache_load_runs.push(timing.cache_load.unwrap_or(Duration::ZERO));
            source_tree_load_runs.push(timing.source_tree_load.unwrap_or(Duration::ZERO));
            parse_runs.push(timing.parse.unwrap_or(Duration::ZERO));
            typeset_runs.push(timing.typeset.unwrap_or(Duration::ZERO));
            pdf_render_runs.push(timing.pdf_render.unwrap_or(Duration::ZERO));
            cache_store_runs.push(timing.cache_store.unwrap_or(Duration::ZERO));
            total_runs.push(timing.total());
            pass_count_runs.push(timing.pass_count);
        }

        let cache_load_median = median_duration(&cache_load_runs);
        let source_tree_load_median = median_duration(&source_tree_load_runs);
        let parse_median = median_duration(&parse_runs);
        let typeset_median = median_duration(&typeset_runs);
        let pdf_render_median = median_duration(&pdf_render_runs);
        let cache_store_median = median_duration(&cache_store_runs);
        let total_median = median_duration(&total_runs);

        let dominant_stage = [
            ("cache_load", cache_load_median),
            ("source_tree_load", source_tree_load_median),
            ("parse", parse_median),
            ("typeset", typeset_median),
            ("pdf_render", pdf_render_median),
            ("cache_store", cache_store_median),
        ]
        .into_iter()
        .max_by_key(|(_, duration)| *duration)
        .expect("stage medians should not be empty");
        let dominant_percentage = if total_median.is_zero() {
            0.0
        } else {
            dominant_stage.1.as_secs_f64() / total_median.as_secs_f64() * 100.0
        };

        println!("[REQ-NF-002 STAGE BREAKDOWN]");
        println!(
            "cache_load:       median {}ms  (runs: {})",
            cache_load_median.as_millis(),
            run_list_ms(&cache_load_runs)
        );
        println!(
            "source_tree_load: median {}ms  (runs: {})",
            source_tree_load_median.as_millis(),
            run_list_ms(&source_tree_load_runs)
        );
        println!(
            "parse:            median {}ms  (runs: {})",
            parse_median.as_millis(),
            run_list_ms(&parse_runs)
        );
        println!(
            "typeset:          median {}ms  (runs: {})",
            typeset_median.as_millis(),
            run_list_ms(&typeset_runs)
        );
        println!(
            "pdf_render:       median {}ms  (runs: {})",
            pdf_render_median.as_millis(),
            run_list_ms(&pdf_render_runs)
        );
        println!(
            "cache_store:      median {}ms  (runs: {})",
            cache_store_median.as_millis(),
            run_list_ms(&cache_store_runs)
        );
        println!(
            "total:            median {}ms  (runs: {})",
            total_median.as_millis(),
            run_list_ms(&total_runs)
        );
        println!(
            "pass_count:       (runs: {})",
            run_list_pass_counts(&pass_count_runs)
        );
        println!(
            "dominant_stage:   {} ({:.1}%)",
            dominant_stage.0, dominant_percentage
        );
    }

    #[test]
    #[ignore = "REQ-NF-002 convergence analysis: 5-run with cross-references (~6 min)"]
    fn incremental_stage_timing_with_refs_5run() {
        fn median_duration(values: &[Duration]) -> Duration {
            let mut sorted = values.to_vec();
            sorted.sort_unstable();
            sorted[2]
        }

        fn run_list_ms(values: &[Duration]) -> String {
            values
                .iter()
                .map(|value| format!("{}ms", value.as_millis()))
                .collect::<Vec<_>>()
                .join(", ")
        }

        fn run_list_pass_counts(values: &[Option<u32>]) -> String {
            values
                .iter()
                .map(|pass_count| match pass_count {
                    Some(count) => format!("{count}"),
                    None => "N/A".to_string(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        }

        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let mut body = String::from("\\tableofcontents\n");
        for index in 1..=1000 {
            let stem = format!("section-{index:04}");
            body.push_str(&format!("\\input{{{stem}}}\n"));
            fs::write(
                dir.path().join(format!("{stem}.tex")),
                format!(
                    "\\section{{Section {index:04}}}\\label{{sec:{index:04}}}\n{}Alpha paragraph {index:04}.\n\nBeta paragraph {index:04}.\n\nGamma paragraph {index:04}.\n",
                    if index > 1 {
                        format!("See Section~\\ref{{sec:{:04}}}.\n\n", index - 1)
                    } else {
                        String::new()
                    }
                ),
            )
            .expect("write section input");
        }
        fs::write(&input_file, document(&body)).expect("write main input");

        let mut options = runtime_options(input_file, dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let warmup = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(warmup.exit_code, 0, "{:?}", warmup.diagnostics);

        let mut cache_load_runs = Vec::with_capacity(5);
        let mut source_tree_load_runs = Vec::with_capacity(5);
        let mut parse_runs = Vec::with_capacity(5);
        let mut typeset_runs = Vec::with_capacity(5);
        let mut pdf_render_runs = Vec::with_capacity(5);
        let mut cache_store_runs = Vec::with_capacity(5);
        let mut total_runs = Vec::with_capacity(5);
        let mut pass_count_runs = Vec::with_capacity(5);

        for i in 1..=5 {
            fs::write(
                dir.path().join("section-0900.tex"),
                format!(
                    "\\section{{Section 0900}}\\label{{sec:0900}}\nSee Section~\\ref{{sec:0899}}.\n\nAlpha paragraph 0900.\n\nBeta paragraph 0900 after benchmark run {i} edit.\n\nGamma paragraph 0900.\n"
                ),
            )
            .expect("update staged section");

            let incremental = service(&FsTestFileAccessGate, &loader).compile(&options);
            assert_eq!(incremental.exit_code, 0, "{:?}", incremental.diagnostics);

            let timing = incremental.stage_timing;
            cache_load_runs.push(timing.cache_load.unwrap_or(Duration::ZERO));
            source_tree_load_runs.push(timing.source_tree_load.unwrap_or(Duration::ZERO));
            parse_runs.push(timing.parse.unwrap_or(Duration::ZERO));
            typeset_runs.push(timing.typeset.unwrap_or(Duration::ZERO));
            pdf_render_runs.push(timing.pdf_render.unwrap_or(Duration::ZERO));
            cache_store_runs.push(timing.cache_store.unwrap_or(Duration::ZERO));
            total_runs.push(timing.total());
            pass_count_runs.push(timing.pass_count);
        }

        let cache_load_median = median_duration(&cache_load_runs);
        let source_tree_load_median = median_duration(&source_tree_load_runs);
        let parse_median = median_duration(&parse_runs);
        let typeset_median = median_duration(&typeset_runs);
        let pdf_render_median = median_duration(&pdf_render_runs);
        let cache_store_median = median_duration(&cache_store_runs);
        let total_median = median_duration(&total_runs);

        let dominant_stage = [
            ("cache_load", cache_load_median),
            ("source_tree_load", source_tree_load_median),
            ("parse", parse_median),
            ("typeset", typeset_median),
            ("pdf_render", pdf_render_median),
            ("cache_store", cache_store_median),
        ]
        .into_iter()
        .max_by_key(|(_, duration)| *duration)
        .expect("stage medians should not be empty");
        let dominant_percentage = if total_median.is_zero() {
            0.0
        } else {
            dominant_stage.1.as_secs_f64() / total_median.as_secs_f64() * 100.0
        };

        println!("[REQ-NF-002 STAGE BREAKDOWN WITH REFS]");
        println!(
            "cache_load:       median {}ms  (runs: {})",
            cache_load_median.as_millis(),
            run_list_ms(&cache_load_runs)
        );
        println!(
            "source_tree_load: median {}ms  (runs: {})",
            source_tree_load_median.as_millis(),
            run_list_ms(&source_tree_load_runs)
        );
        println!(
            "parse:            median {}ms  (runs: {})",
            parse_median.as_millis(),
            run_list_ms(&parse_runs)
        );
        println!(
            "typeset:          median {}ms  (runs: {})",
            typeset_median.as_millis(),
            run_list_ms(&typeset_runs)
        );
        println!(
            "pdf_render:       median {}ms  (runs: {})",
            pdf_render_median.as_millis(),
            run_list_ms(&pdf_render_runs)
        );
        println!(
            "cache_store:      median {}ms  (runs: {})",
            cache_store_median.as_millis(),
            run_list_ms(&cache_store_runs)
        );
        println!(
            "total:            median {}ms  (runs: {})",
            total_median.as_millis(),
            run_list_ms(&total_runs)
        );
        println!(
            "pass_count:       (runs: {})",
            run_list_pass_counts(&pass_count_runs)
        );
        println!(
            "dominant_stage:   {} ({:.1}%)",
            dominant_stage.0, dominant_percentage
        );
    }

    #[test]
    fn parallel_partial_typeset_produces_same_output_as_sequential() {
        let loader = MockAssetBundleLoader::valid();

        let sequential_dir = tempdir().expect("create sequential tempdir");
        let sequential_input = write_partitioned_report_project(
            sequential_dir.path(),
            &[
                (
                    "chapter-one.tex",
                    "\\chapter{One}\\label{chap:one}\nOriginal chapter one body.\n",
                ),
                (
                    "chapter-two.tex",
                    "\\chapter{Two}\\label{chap:two}\nOriginal chapter two body.\n",
                ),
                (
                    "chapter-three.tex",
                    "\\chapter{Three}\\label{chap:three}\nStable chapter three body.\n",
                ),
            ],
        );
        let mut sequential_options =
            runtime_options(sequential_input, sequential_dir.path().join("out"));
        sequential_options.no_cache = false;
        sequential_options.parallelism = 1;

        let warmup = service(&FsTestFileAccessGate, &loader).compile(&sequential_options);
        assert_eq!(warmup.exit_code, 0);

        fs::write(
            sequential_dir.path().join("chapter-one.tex"),
            "\\chapter{One}\\label{chap:one}\nEdited chapter one body.\n",
        )
        .expect("update sequential chapter one");
        fs::write(
            sequential_dir.path().join("chapter-two.tex"),
            "\\chapter{Two}\\label{chap:two}\nEdited chapter two body.\n",
        )
        .expect("update sequential chapter two");

        let (sequential_result, sequential_trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &sequential_options);
        assert_eq!(sequential_result.exit_code, 0);
        assert!(sequential_trace_messages
            .iter()
            .any(|message| message.contains("partial typeset reuse applied")));
        assert!(sequential_trace_messages
            .iter()
            .all(|message| !message.contains("partial typeset rebuild executing in parallel")));
        let sequential_pdf =
            fs::read(sequential_options.output_dir.join("main.pdf")).expect("read sequential pdf");

        let parallel_dir = tempdir().expect("create parallel tempdir");
        let parallel_input = write_partitioned_report_project(
            parallel_dir.path(),
            &[
                (
                    "chapter-one.tex",
                    "\\chapter{One}\\label{chap:one}\nOriginal chapter one body.\n",
                ),
                (
                    "chapter-two.tex",
                    "\\chapter{Two}\\label{chap:two}\nOriginal chapter two body.\n",
                ),
                (
                    "chapter-three.tex",
                    "\\chapter{Three}\\label{chap:three}\nStable chapter three body.\n",
                ),
            ],
        );
        let mut parallel_options = runtime_options(parallel_input, parallel_dir.path().join("out"));
        parallel_options.no_cache = false;
        parallel_options.parallelism = 4;

        let warmup = service(&FsTestFileAccessGate, &loader).compile(&parallel_options);
        assert_eq!(warmup.exit_code, 0);

        fs::write(
            parallel_dir.path().join("chapter-one.tex"),
            "\\chapter{One}\\label{chap:one}\nEdited chapter one body.\n",
        )
        .expect("update parallel chapter one");
        fs::write(
            parallel_dir.path().join("chapter-two.tex"),
            "\\chapter{Two}\\label{chap:two}\nEdited chapter two body.\n",
        )
        .expect("update parallel chapter two");

        let (parallel_result, parallel_trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &parallel_options);
        assert_eq!(parallel_result.exit_code, 0);
        assert!(parallel_trace_messages
            .iter()
            .any(|message| message.contains("partial typeset reuse applied")));
        assert!(parallel_trace_messages
            .iter()
            .any(|message| message.contains("partial typeset rebuild executing in parallel")));
        let parallel_pdf =
            fs::read(parallel_options.output_dir.join("main.pdf")).expect("read parallel pdf");

        assert_eq!(sequential_pdf, parallel_pdf);
    }

    #[test]
    fn coalesce_full_typeset_partitions_balances_chunk_sizes() {
        let partition_plan = super::DocumentPartitionPlan {
            fallback_partition_id: "document:0000:root".to_string(),
            work_units: (0..10)
                .map(|ordinal| super::DocumentWorkUnit {
                    partition_id: format!("partition-{ordinal}"),
                    kind: ferritex_core::compilation::PartitionKind::Chapter,
                    locator: ferritex_core::compilation::PartitionLocator {
                        entry_file: PathBuf::from("main.tex"),
                        level: 0,
                        ordinal,
                        title: format!("Chapter {ordinal}"),
                    },
                    title: format!("Chapter {ordinal}"),
                })
                .collect(),
        };
        let body_ranges = partition_plan
            .work_units
            .iter()
            .enumerate()
            .map(|(index, work_unit)| {
                (
                    work_unit.partition_id.clone(),
                    (index * 10, (index + 1) * 10),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let section_ranges = partition_plan
            .work_units
            .iter()
            .enumerate()
            .map(|(index, work_unit)| (work_unit.partition_id.clone(), (index, index + 1)))
            .collect::<BTreeMap<_, _>>();

        let (coalesced_plan, coalesced_body_ranges, coalesced_section_ranges) =
            super::coalesce_full_typeset_partitions(
                &partition_plan,
                &body_ranges,
                &section_ranges,
                4,
            )
            .expect("coalesce should succeed");

        assert_eq!(coalesced_plan.work_units.len(), 4);
        assert_eq!(
            coalesced_plan
                .work_units
                .iter()
                .map(|work_unit| work_unit.partition_id.as_str())
                .collect::<Vec<_>>(),
            vec!["partition-0", "partition-3", "partition-6", "partition-8"]
        );

        let chunk_sizes = coalesced_plan
            .work_units
            .iter()
            .map(|work_unit| {
                let range = coalesced_section_ranges
                    .get(&work_unit.partition_id)
                    .expect("section range should exist");
                range.1 - range.0
            })
            .collect::<Vec<_>>();
        assert_eq!(chunk_sizes, vec![3, 3, 2, 2]);

        let min_chunk_size = *chunk_sizes.iter().min().expect("min chunk size");
        let max_chunk_size = *chunk_sizes.iter().max().expect("max chunk size");
        assert!(
            max_chunk_size <= min_chunk_size * 2,
            "expected balanced chunk sizes, got {chunk_sizes:?}"
        );

        assert_eq!(coalesced_body_ranges.get("partition-0"), Some(&(0, 30)));
        assert_eq!(coalesced_body_ranges.get("partition-3"), Some(&(30, 60)));
        assert_eq!(coalesced_body_ranges.get("partition-6"), Some(&(60, 80)));
        assert_eq!(coalesced_body_ranges.get("partition-8"), Some(&(80, 100)));
    }

    #[test]
    fn parallel_full_typeset_produces_same_output_as_sequential() {
        let loader = MockAssetBundleLoader::valid();

        let sequential_dir = tempdir().expect("create sequential tempdir");
        let sequential_input = write_partitioned_report_project(
            sequential_dir.path(),
            &[
                (
                    "chapter-one.tex",
                    "\\chapter{One}\\label{chap:one}\nOriginal chapter one body.\n",
                ),
                (
                    "chapter-two.tex",
                    "\\chapter{Two}\\label{chap:two}\nOriginal chapter two body.\n",
                ),
                (
                    "chapter-three.tex",
                    "\\chapter{Three}\\label{chap:three}\nStable chapter three body.\n",
                ),
            ],
        );
        let mut sequential_options =
            runtime_options(sequential_input, sequential_dir.path().join("out"));
        sequential_options.no_cache = true;
        sequential_options.parallelism = 1;

        let sequential = service(&FsTestFileAccessGate, &loader).compile(&sequential_options);
        assert_eq!(sequential.exit_code, 0);
        let sequential_pdf =
            fs::read(sequential_options.output_dir.join("main.pdf")).expect("read sequential pdf");

        let parallel_dir = tempdir().expect("create parallel tempdir");
        let parallel_input = write_partitioned_report_project(
            parallel_dir.path(),
            &[
                (
                    "chapter-one.tex",
                    "\\chapter{One}\\label{chap:one}\nOriginal chapter one body.\n",
                ),
                (
                    "chapter-two.tex",
                    "\\chapter{Two}\\label{chap:two}\nOriginal chapter two body.\n",
                ),
                (
                    "chapter-three.tex",
                    "\\chapter{Three}\\label{chap:three}\nStable chapter three body.\n",
                ),
            ],
        );
        let mut parallel_options = runtime_options(parallel_input, parallel_dir.path().join("out"));
        parallel_options.no_cache = true;
        parallel_options.parallelism = 4;

        let (parallel, trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &parallel_options);
        assert_eq!(parallel.exit_code, 0);
        assert!(trace_messages
            .iter()
            .any(|message| message.contains("full typeset executing in parallel")));
        let parallel_pdf =
            fs::read(parallel_options.output_dir.join("main.pdf")).expect("read parallel pdf");

        assert_eq!(sequential_pdf, parallel_pdf);
    }

    #[test]
    fn parallel_full_typeset_produces_same_output_as_sequential_for_section_partitions() {
        let loader = MockAssetBundleLoader::valid();

        let sequential_dir = tempdir().expect("create sequential tempdir");
        let sequential_input = write_partitioned_article_project(
            sequential_dir.path(),
            &[
                (
                    "section-one.tex",
                    "\\section{One}\\label{sec:one}\nSection one body stays on the first page.\n",
                ),
                (
                    "section-two.tex",
                    "\\section{Two}\\label{sec:two}\nSection two should continue on the same page without an injected page break.\n",
                ),
                (
                    "section-three.tex",
                    "\\section{Three}\\label{sec:three}\nSection three keeps the article flow continuous.\n",
                ),
            ],
        );
        let mut sequential_options =
            runtime_options(sequential_input, sequential_dir.path().join("out"));
        sequential_options.no_cache = true;
        sequential_options.parallelism = 1;

        let sequential = service(&FsTestFileAccessGate, &loader).compile(&sequential_options);
        assert_eq!(sequential.exit_code, 0);
        let sequential_pdf =
            fs::read(sequential_options.output_dir.join("main.pdf")).expect("read sequential pdf");

        let parallel_dir = tempdir().expect("create parallel tempdir");
        let parallel_input = write_partitioned_article_project(
            parallel_dir.path(),
            &[
                (
                    "section-one.tex",
                    "\\section{One}\\label{sec:one}\nSection one body stays on the first page.\n",
                ),
                (
                    "section-two.tex",
                    "\\section{Two}\\label{sec:two}\nSection two should continue on the same page without an injected page break.\n",
                ),
                (
                    "section-three.tex",
                    "\\section{Three}\\label{sec:three}\nSection three keeps the article flow continuous.\n",
                ),
            ],
        );
        let mut parallel_options = runtime_options(parallel_input, parallel_dir.path().join("out"));
        parallel_options.no_cache = true;
        parallel_options.parallelism = 4;

        let (parallel, trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &parallel_options);
        assert_eq!(parallel.exit_code, 0);
        assert!(trace_messages
            .iter()
            .any(|message| message.contains("full typeset executing in parallel")));
        let parallel_pdf =
            fs::read(parallel_options.output_dir.join("main.pdf")).expect("read parallel pdf");

        assert_eq!(sequential_pdf, parallel_pdf);
    }

    #[test]
    fn parallel_full_typeset_preserves_clearpage_boundaries_for_section_partitions() {
        let loader = MockAssetBundleLoader::valid();

        let sequential_dir = tempdir().expect("create sequential tempdir");
        let sequential_input = write_partitioned_article_project(
            sequential_dir.path(),
            &[
                (
                    "section-one.tex",
                    "\\section{One}\\label{sec:one}\nSection one fills the first partition before forcing a page break.\n\\clearpage\n",
                ),
                (
                    "section-two.tex",
                    "\\section{Two}\\label{sec:two}\nSection two must start on a fresh page and preserve the explicit clearpage boundary.\n\\clearpage\n",
                ),
                (
                    "section-three.tex",
                    "\\section{Three}\\label{sec:three}\nSection three should also remain on its own page after the second clearpage.\n",
                ),
            ],
        );
        let mut sequential_options =
            runtime_options(sequential_input, sequential_dir.path().join("out"));
        sequential_options.no_cache = true;
        sequential_options.parallelism = 1;

        let sequential = service(&FsTestFileAccessGate, &loader).compile(&sequential_options);
        assert_eq!(sequential.exit_code, 0);
        let sequential_pdf =
            fs::read(sequential_options.output_dir.join("main.pdf")).expect("read sequential pdf");

        let parallel_dir = tempdir().expect("create parallel tempdir");
        let parallel_input = write_partitioned_article_project(
            parallel_dir.path(),
            &[
                (
                    "section-one.tex",
                    "\\section{One}\\label{sec:one}\nSection one fills the first partition before forcing a page break.\n\\clearpage\n",
                ),
                (
                    "section-two.tex",
                    "\\section{Two}\\label{sec:two}\nSection two must start on a fresh page and preserve the explicit clearpage boundary.\n\\clearpage\n",
                ),
                (
                    "section-three.tex",
                    "\\section{Three}\\label{sec:three}\nSection three should also remain on its own page after the second clearpage.\n",
                ),
            ],
        );
        let mut parallel_options = runtime_options(parallel_input, parallel_dir.path().join("out"));
        parallel_options.no_cache = true;
        parallel_options.parallelism = 4;

        let (parallel, trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &parallel_options);
        assert_eq!(parallel.exit_code, 0);
        assert!(trace_messages
            .iter()
            .any(|message| message.contains("full typeset executing in parallel")));
        let parallel_pdf =
            fs::read(parallel_options.output_dir.join("main.pdf")).expect("read parallel pdf");

        assert_eq!(sequential_pdf, parallel_pdf);
    }

    #[test]
    fn parallel_full_typeset_falls_back_on_single_partition() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        fs::write(
            &input_file,
            report_document("\\chapter{Only}\\label{chap:only}\nSingle partition body.\n"),
        )
        .expect("write input");

        let mut options = runtime_options(input_file, dir.path().join("out"));
        options.no_cache = true;
        options.parallelism = 4;
        let loader = MockAssetBundleLoader::valid();

        let (result, trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &options);

        assert_eq!(result.exit_code, 0);
        assert!(trace_messages
            .iter()
            .all(|message| !message.contains("full typeset executing in parallel")));
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("Single partition body."));
    }

    #[test]
    fn parallel_full_typeset_collision_fallback_produces_sequential_result() {
        let loader = MockAssetBundleLoader::valid();

        let sequential_dir = tempdir().expect("create sequential tempdir");
        let sequential_input = write_partitioned_report_project(
            sequential_dir.path(),
            &[
                (
                    "chapter-one.tex",
                    "\\chapter{One}\\label{chap:one}\nFirst chapter body.\n",
                ),
                (
                    "chapter-two.tex",
                    "\\chapter{Two}\\label{chap:two}\nSecond chapter body.\n",
                ),
            ],
        );
        let mut sequential_options =
            runtime_options(sequential_input, sequential_dir.path().join("out"));
        sequential_options.no_cache = true;
        sequential_options.parallelism = 1;

        let sequential = service(&FsTestFileAccessGate, &loader).compile(&sequential_options);
        assert_eq!(sequential.exit_code, 0);
        let sequential_pdf =
            fs::read(sequential_options.output_dir.join("main.pdf")).expect("read sequential pdf");

        let parallel_dir = tempdir().expect("create parallel tempdir");
        let parallel_input = write_partitioned_report_project(
            parallel_dir.path(),
            &[
                (
                    "chapter-one.tex",
                    "\\chapter{One}\\label{chap:one}\nFirst chapter body.\n",
                ),
                (
                    "chapter-two.tex",
                    "\\chapter{Two}\\label{chap:two}\nSecond chapter body.\n",
                ),
            ],
        );
        let mut parallel_options = runtime_options(parallel_input, parallel_dir.path().join("out"));
        parallel_options.no_cache = true;
        parallel_options.parallelism = 4;

        let _collision_guard = force_parallel_full_typeset_collision();
        let (parallel, trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &parallel_options);
        assert_eq!(parallel.exit_code, 0);
        assert!(
            trace_messages
                .iter()
                .any(|message| message.contains("full typeset executing in parallel")),
            "{trace_messages:?}"
        );
        assert!(
            trace_messages.iter().any(|message| {
                message.contains(
                    "parallel full typeset authority key collision; falling back to sequential",
                )
            }),
            "{trace_messages:?}"
        );
        let parallel_pdf =
            fs::read(parallel_options.output_dir.join("main.pdf")).expect("read parallel pdf");

        assert_eq!(
            pdf_text_operators(&String::from_utf8_lossy(&sequential_pdf)),
            pdf_text_operators(&String::from_utf8_lossy(&parallel_pdf))
        );
    }

    #[test]
    fn parallel_typeset_falls_back_on_single_partition() {
        let dir = tempdir().expect("create tempdir");
        let loader = MockAssetBundleLoader::valid();
        let input_file = write_partitioned_report_project(
            dir.path(),
            &[
                (
                    "chapter-one.tex",
                    "\\chapter{One}\\label{chap:one}\nStable chapter one body.\n",
                ),
                (
                    "chapter-two.tex",
                    "\\chapter{Two}\\label{chap:two}\nOriginal chapter two body.\n",
                ),
                (
                    "chapter-three.tex",
                    "\\chapter{Three}\\label{chap:three}\nStable chapter three body.\n",
                ),
            ],
        );
        let mut options = runtime_options(input_file, dir.path().join("out"));
        options.no_cache = false;
        options.parallelism = 4;

        let warmup = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(warmup.exit_code, 0);

        fs::write(
            dir.path().join("chapter-two.tex"),
            "\\chapter{Two}\\label{chap:two}\nEdited chapter two body.\n",
        )
        .expect("update chapter two");

        let (incremental, trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &options);
        assert_eq!(incremental.exit_code, 0);
        assert!(trace_messages
            .iter()
            .any(|message| message.contains("partial typeset reuse applied")));
        assert!(trace_messages
            .iter()
            .all(|message| !message.contains("partial typeset rebuild executing in parallel")));

        let incremental_pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(incremental_pdf.contains("Edited chapter two body."));

        let mut full_options =
            runtime_options(dir.path().join("main.tex"), dir.path().join("out-full"));
        full_options.no_cache = true;
        let full = service(&FsTestFileAccessGate, &loader).compile(&full_options);
        assert_eq!(full.exit_code, 0);
        let full_pdf = read_pdf(&full_options.output_dir.join("main.pdf"));
        assert_eq!(
            pdf_text_operators(&incremental_pdf),
            pdf_text_operators(&full_pdf)
        );
    }

    #[test]
    fn parallel_typeset_collision_fallback() {
        // Current compile-level duplicate labels are normalized before fragment extraction, so a
        // black-box incremental compile does not reliably surface two colliding fragment-local
        // authorities. Validate the fallback guard with a manual overlapping-fragment scenario.
        let fragments = [
            DocumentLayoutFragment {
                partition_id: "chapter:0001:one".to_string(),
                pages: Vec::new(),
                local_label_pages: BTreeMap::from([("shared".to_string(), 0)]),
                outlines: Vec::new(),
                named_destinations: vec![TypesetNamedDestination {
                    name: "shared".to_string(),
                    page_index: 0,
                    y: points(700),
                }],
            },
            DocumentLayoutFragment {
                partition_id: "chapter:0002:two".to_string(),
                pages: Vec::new(),
                local_label_pages: BTreeMap::from([("shared".to_string(), 0)]),
                outlines: Vec::new(),
                named_destinations: vec![TypesetNamedDestination {
                    name: "shared".to_string(),
                    page_index: 0,
                    y: points(680),
                }],
            },
        ];

        assert!(super::has_cross_partition_layout_collision(
            fragments.iter()
        ));
    }

    #[test]
    fn incremental_cache_is_reusable_on_subsequent_run() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let chapter_one = dir.path().join("chapter-one.tex");
        let chapter_two = dir.path().join("chapter-two.tex");
        let chapter_three = dir.path().join("chapter-three.tex");
        fs::write(
            &input_file,
            report_document(
                "\\input{chapter-one}\n\\newpage\n\\input{chapter-two}\n\\newpage\n\\input{chapter-three}",
            ),
        )
        .expect("write main input");
        fs::write(
            &chapter_one,
            "\\chapter{One}\\label{chap:one}\nOriginal chapter one body.\n",
        )
        .expect("write chapter one");
        fs::write(
            &chapter_two,
            "\\chapter{Two}\\label{chap:two}\nOriginal chapter two body.\n",
        )
        .expect("write chapter two");
        fs::write(
            &chapter_three,
            "\\chapter{Three}\\label{chap:three}\nSee Chapter \\ref{chap:one} and Chapter \\ref{chap:two}.\n",
        )
        .expect("write chapter three");

        let mut options = runtime_options(input_file.clone(), dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let warmup = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(warmup.exit_code, 0);

        fs::write(
            &chapter_two,
            "\\chapter{Two}\\label{chap:two}\nEdited chapter two body after first incremental run.\n",
        )
        .expect("update chapter two");

        let (first_incremental, first_trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &options);
        assert_eq!(first_incremental.exit_code, 0);
        assert!(
            first_trace_messages
                .iter()
                .any(|message| message.contains("partial typeset reuse applied")),
            "{first_trace_messages:?}"
        );

        let first_incremental_pdf =
            fs::read(options.output_dir.join("main.pdf")).expect("read first incremental pdf");
        let mut first_full_options =
            runtime_options(input_file.clone(), dir.path().join("out-full-first"));
        first_full_options.no_cache = true;
        let first_full = service(&FsTestFileAccessGate, &loader).compile(&first_full_options);
        assert_eq!(first_full.exit_code, 0);
        let first_full_pdf =
            fs::read(first_full_options.output_dir.join("main.pdf")).expect("read first full pdf");
        assert_eq!(first_incremental_pdf, first_full_pdf);

        fs::write(
            &chapter_one,
            "\\chapter{One}\\label{chap:one}\nEdited chapter one body after cache persist.\n",
        )
        .expect("update chapter one");

        let (second_incremental, second_trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &options);
        assert_eq!(second_incremental.exit_code, 0);
        assert!(
            second_trace_messages
                .iter()
                .any(|message| message.contains("partial typeset reuse applied")),
            "{second_trace_messages:?}"
        );

        let second_incremental_pdf =
            fs::read(options.output_dir.join("main.pdf")).expect("read second incremental pdf");
        let mut final_full_options = runtime_options(input_file, dir.path().join("out-full-final"));
        final_full_options.no_cache = true;
        let final_full = service(&FsTestFileAccessGate, &loader).compile(&final_full_options);
        assert_eq!(final_full.exit_code, 0);
        let final_full_pdf =
            fs::read(final_full_options.output_dir.join("main.pdf")).expect("read final full pdf");
        assert_eq!(second_incremental_pdf, final_full_pdf);
    }

    #[test]
    fn per_page_payload_reuse_matches_full_and_reduces_pdf_render_stage() {
        let dir = tempdir().expect("create tempdir");
        let loader = MockAssetBundleLoader::valid();
        let chapters = (0..40)
            .map(|index| {
                let body = (0..12)
                    .map(|line| format!("Stable chapter {index:02} line {line:02}.\n"))
                    .collect::<String>();
                (
                    format!("chapter-{index:02}.tex"),
                    format!("\\chapter{{Chapter {index:02}}}\\label{{chap:{index:02}}}\n{body}"),
                )
            })
            .collect::<Vec<_>>();
        let chapter_refs = chapters
            .iter()
            .map(|(file_name, content)| (file_name.as_str(), content.as_str()))
            .collect::<Vec<_>>();
        let input_file = write_partitioned_report_project(dir.path(), &chapter_refs);
        let mut options = runtime_options(input_file.clone(), dir.path().join("out"));
        options.no_cache = false;

        let warmup = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(warmup.exit_code, 0, "{:?}", warmup.diagnostics);

        let edited_chapter = dir.path().join("chapter-06.tex");
        fs::write(
            &edited_chapter,
            "\\chapter{Chapter 06}\\label{chap:06}\nEdited chapter 06 line 00.\nEdited chapter 06 line 01.\nEdited chapter 06 line 02.\nEdited chapter 06 line 03.\nEdited chapter 06 line 04.\nEdited chapter 06 line 05.\nEdited chapter 06 line 06.\nEdited chapter 06 line 07.\nEdited chapter 06 line 08.\nEdited chapter 06 line 09.\nEdited chapter 06 line 10.\nEdited chapter 06 line 11.\n",
        )
        .expect("update chapter");
        let lookup = CompileCache::new(
            &FsTestFileAccessGate,
            &options.output_dir,
            &input_file,
            "main",
        )
        .lookup(&[]);
        assert_eq!(
            lookup.cached_page_payloads.len(),
            40,
            "{:?}",
            lookup.cached_page_payloads.keys().collect::<Vec<_>>()
        );
        assert!(lookup
            .cached_page_payloads
            .values()
            .flat_map(|payloads| payloads.iter())
            .all(|payload| payload.to_page_render_payload(0).is_some()));

        let (incremental, trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &options);
        assert_eq!(incremental.exit_code, 0, "{:?}", incremental.diagnostics);
        let partition_details = incremental
            .stage_timing
            .typeset_partition_details
            .as_ref()
            .expect("partition details");
        assert!(
            partition_details
                .iter()
                .any(|detail| detail.reuse_type == PartitionTypesetReuseType::Cached),
            "{partition_details:?}"
        );
        assert!(
            trace_messages
                .iter()
                .any(|message| message.contains("partial typeset reuse applied")),
            "{trace_messages:?}"
        );
        assert!(
            trace_messages.iter().any(|message| {
                message.contains("pdf page payload reuse applied")
                    && message.contains("reused_pages=39")
                    && message.contains("rendered_pages=1")
            }),
            "{trace_messages:?}"
        );

        let incremental_pdf =
            fs::read(options.output_dir.join("main.pdf")).expect("read incremental pdf");
        let mut full_options = runtime_options(input_file, dir.path().join("out-full"));
        full_options.no_cache = true;
        let full = service(&FsTestFileAccessGate, &loader).compile(&full_options);
        assert_eq!(full.exit_code, 0, "{:?}", full.diagnostics);
        let full_pdf = fs::read(full_options.output_dir.join("main.pdf")).expect("read full pdf");

        assert_eq!(incremental_pdf, full_pdf);
        assert!(incremental.stage_timing.pdf_render.is_some());
        assert!(full.stage_timing.pdf_render.is_some());
    }

    #[test]
    fn incremental_recompile_with_toc_matches_full() {
        let current_dir = std::env::current_dir().expect("current dir");
        let dir = tempfile::tempdir_in(&current_dir).expect("create tempdir");
        let relative_root = dir
            .path()
            .strip_prefix(&current_dir)
            .expect("relative tempdir root");
        let input_file = relative_root.join("main.tex");
        let chapter_one = relative_root.join("chapter-one.tex");
        let chapter_two = relative_root.join("chapter-two.tex");
        let chapter_three = relative_root.join("chapter-three.tex");
        fs::write(
            &input_file,
            report_document(
                "\\tableofcontents\n\\newpage\n\\input{chapter-one}\n\\newpage\n\\input{chapter-two}\n\\newpage\n\\input{chapter-three}",
            ),
        )
        .expect("write main input");
        fs::write(
            &chapter_one,
            "\\chapter{One}\\label{chap:one}\nAlpha chapter body.\n",
        )
        .expect("write chapter one");
        fs::write(
            &chapter_two,
            "\\chapter{Two}\\label{chap:two}\nOriginal middle chapter body text.\n",
        )
        .expect("write chapter two");
        fs::write(
            &chapter_three,
            "\\chapter{Three}\\label{chap:three}\nSee Chapter \\ref{chap:two}.\n",
        )
        .expect("write chapter three");

        let mut options = runtime_options(input_file.clone(), relative_root.join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let warmup = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(warmup.exit_code, 0);

        fs::write(
            &chapter_two,
            "\\chapter{Two}\\label{chap:two}\nEdited middle chapter body text.\n",
        )
        .expect("update chapter two");

        let (incremental, trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &options);
        assert_eq!(incremental.exit_code, 0);
        assert!(
            trace_messages
                .iter()
                .any(|message| message.contains("partial typeset reuse applied")),
            "{trace_messages:?}"
        );

        let incremental_pdf = read_pdf(&options.output_dir.join("main.pdf"));
        let mut full_options = runtime_options(input_file, relative_root.join("out-full"));
        full_options.no_cache = true;
        let full = service(&FsTestFileAccessGate, &loader).compile(&full_options);
        assert_eq!(full.exit_code, 0);

        let full_pdf = read_pdf(&full_options.output_dir.join("main.pdf"));
        // TOC coverage here is about visible merged output; destination metadata differs in this
        // harness even when the rendered text stream is equivalent to a clean full compile.
        assert_eq!(
            pdf_text_operators(&incremental_pdf),
            pdf_text_operators(&full_pdf)
        );
        assert!(incremental_pdf.contains("(Edited middle chapter body text.) Tj"));
        assert!(incremental_pdf.matches("(1 One) Tj").count() >= 2);
        assert!(incremental_pdf.matches("(2 Two) Tj").count() >= 2);
        assert!(incremental_pdf.matches("(3 Three) Tj").count() >= 2);
        assert!(!incremental_pdf.contains("??"));
    }

    #[test]
    fn preamble_change_forces_full_rebuild() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let chapter_one = dir.path().join("chapter-one.tex");
        let chapter_two = dir.path().join("chapter-two.tex");
        fs::write(
            &input_file,
            report_document(
                "\\tableofcontents\n\\newpage\n\\input{chapter-one}\n\\newpage\n\\input{chapter-two}",
            ),
        )
        .expect("write main input");
        fs::write(
            &chapter_one,
            "\\chapter{One}\\label{chap:one}\nAlpha body.\n",
        )
        .expect("write chapter one");
        fs::write(
            &chapter_two,
            "\\chapter{Two}\\label{chap:two}\nSee Chapter \\ref{chap:one}.\n",
        )
        .expect("write chapter two");

        let mut options = runtime_options(input_file.clone(), dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let first = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(first.exit_code, 0);

        fs::write(
            &input_file,
            "\\documentclass{report}\n\\usepackage{xcolor}\n\\begin{document}\n\\tableofcontents\n\\newpage\n\\input{chapter-one}\n\\newpage\n\\input{chapter-two}\n\\end{document}\n",
        )
        .expect("update main input");

        let (second, trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &options);
        assert_eq!(second.exit_code, 0);
        assert!(trace_messages
            .iter()
            .any(|message| message.contains("partial typeset fallback to full typeset")));
        assert!(trace_messages
            .iter()
            .all(|message| !message.contains("partial typeset reuse applied")));

        let incremental_pdf =
            fs::read(options.output_dir.join("main.pdf")).expect("read incremental pdf");
        let mut full_options = runtime_options(input_file, dir.path().join("out-full"));
        full_options.no_cache = true;
        let full = service(&FsTestFileAccessGate, &loader).compile(&full_options);
        assert_eq!(full.exit_code, 0);

        let full_pdf = fs::read(full_options.output_dir.join("main.pdf")).expect("read full pdf");
        assert_eq!(
            pdf_text_operators(&String::from_utf8_lossy(&incremental_pdf)),
            pdf_text_operators(&String::from_utf8_lossy(&full_pdf))
        );
    }

    #[test]
    fn stale_page_numbers_trigger_full_fallback() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let chapter_one = dir.path().join("chapter-one.tex");
        let chapter_two = dir.path().join("chapter-two.tex");
        let appendix = dir.path().join("appendix.tex");
        fs::write(
            &input_file,
            report_document(
                "\\input{chapter-one}\n\\newpage\n\\input{chapter-two}\n\\newpage\n\\input{appendix}",
            ),
        )
        .expect("write main input");
        fs::write(
            &chapter_one,
            "\\chapter{One}\\label{chap:one}\nIntro body.\n",
        )
        .expect("write chapter one");
        fs::write(
            &chapter_two,
            "\\chapter{Two}\\label{chap:two}\nAppendix starts on page \\pageref{chap:appendix}.\n",
        )
        .expect("write chapter two");
        fs::write(
            &appendix,
            "\\chapter{Appendix}\\label{chap:appendix}\nAppendix body.\n",
        )
        .expect("write appendix");

        let mut options = runtime_options(input_file.clone(), dir.path().join("out"));
        options.no_cache = false;
        let loader = MockAssetBundleLoader::valid();

        let first = service(&FsTestFileAccessGate, &loader).compile(&options);
        assert_eq!(first.exit_code, 0);
        let first_pdf_text = read_pdf(&options.output_dir.join("main.pdf"));

        fs::write(
            &chapter_one,
            "\\chapter{One}\\label{chap:one}\nIntro body.\n\\newpage\nInserted page.\n",
        )
        .expect("update chapter one");

        let (second, trace_messages) =
            compile_with_trace_messages(&FsTestFileAccessGate, &loader, &options);
        let second_state = second
            .stable_compile_state
            .clone()
            .expect("second stable state");
        assert_eq!(second.exit_code, 0);
        assert!(trace_messages
            .iter()
            .any(|message| message.contains("partial typeset fallback to full typeset")));
        assert!(trace_messages
            .iter()
            .all(|message| !message.contains("partial typeset reuse applied")));
        assert!(second_state.snapshot.pass_number >= 2);

        let incremental_pdf =
            fs::read(options.output_dir.join("main.pdf")).expect("read incremental pdf");
        let incremental_pdf_text = String::from_utf8_lossy(&incremental_pdf);
        assert_ne!(incremental_pdf_text, first_pdf_text);

        let mut full_options = runtime_options(input_file, dir.path().join("out-full"));
        full_options.no_cache = true;
        let full = service(&FsTestFileAccessGate, &loader).compile(&full_options);
        assert_eq!(full.exit_code, 0);

        let full_pdf = fs::read(full_options.output_dir.join("main.pdf")).expect("read full pdf");
        let full_pdf_text = String::from_utf8_lossy(&full_pdf);
        assert_eq!(
            pdf_text_operators(&incremental_pdf_text),
            pdf_text_operators(&full_pdf_text)
        );
        assert_eq!(incremental_pdf_text, full_pdf_text);
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
    fn append_str_with_source_lines_preserves_open_line_origin() {
        let mut builder = super::ExpandedSourceBuilder::default();
        builder.append_with_origin("ROOT ", "main.tex", 3);
        builder.append_str_with_source_lines(
            "cached line\nnext cached",
            &[
                SourceLineTrace {
                    file: "child.tex".to_string(),
                    line: 7,
                    text: "cached line".to_string(),
                },
                SourceLineTrace {
                    file: "child.tex".to_string(),
                    line: 8,
                    text: "next cached".to_string(),
                },
            ],
        );

        let expanded = builder.finish();

        assert_eq!(
            expanded.source_lines,
            vec![
                SourceLineTrace {
                    file: "main.tex".to_string(),
                    line: 3,
                    text: "ROOT cached line".to_string(),
                },
                SourceLineTrace {
                    file: "child.tex".to_string(),
                    line: 8,
                    text: "next cached".to_string(),
                },
            ]
        );
    }

    #[test]
    fn expand_inputs_reuses_cached_subtree_for_input() {
        let dir = tempdir().expect("create tempdir");
        let source_path = dir.path().join("main.tex");
        let candidate = super::tex_path_candidate(dir.path(), "child");
        let cached_subtree =
            cached_source_subtree_for(&candidate, "child.tex", "CACHED INPUT CONTENT");
        let reuse_plan = reuse_plan_with_cached_subtree(&candidate, cached_subtree);

        let (expanded, source_files, dependency_graph, cached_source_subtrees) =
            expand_inputs_for_test("\\input{child}\n", &source_path, Some(&reuse_plan))
                .expect("reuse cached subtree");

        assert!(expanded.text.contains("CACHED INPUT CONTENT"));
        assert!(source_files.contains(&candidate));
        assert!(dependency_graph
            .edges
            .get(&source_path)
            .is_some_and(|edges| edges.contains(&candidate)));
        assert_eq!(
            cached_source_subtrees
                .get(&candidate)
                .map(|entry| entry.text.as_str()),
            Some("CACHED INPUT CONTENT")
        );
    }

    #[test]
    fn expand_inputs_reuses_cached_subtree_for_include_with_guard() {
        let dir = tempdir().expect("create tempdir");
        let source_path = dir.path().join("main.tex");
        let candidate = super::tex_path_candidate(dir.path(), "child");
        let cached_subtree =
            cached_source_subtree_for(&candidate, "child.tex", "CACHED INCLUDE CONTENT");
        let reuse_plan = reuse_plan_with_cached_subtree(&candidate, cached_subtree);

        let (expanded, _, _, _) = expand_inputs_for_test(
            "\\include{child}\\include{child}\n",
            &source_path,
            Some(&reuse_plan),
        )
        .expect("reuse cached include");

        assert_eq!(expanded.text.matches("CACHED INCLUDE CONTENT").count(), 1);
    }

    #[test]
    fn expand_inputs_reuses_cached_subtree_for_input_if_file_exists() {
        let dir = tempdir().expect("create tempdir");
        let source_path = dir.path().join("main.tex");
        let candidate = super::tex_path_candidate(dir.path(), "child");
        let cached_subtree =
            cached_source_subtree_for(&candidate, "child.tex", "CACHED CONDITIONAL");
        let reuse_plan = reuse_plan_with_cached_subtree(&candidate, cached_subtree);

        let (expanded, _, _, _) = expand_inputs_for_test(
            "\\InputIfFileExists{child}{TRUE BRANCH}{FALSE BRANCH}\n",
            &source_path,
            Some(&reuse_plan),
        )
        .expect("reuse cached conditional input");

        assert!(expanded.text.contains("CACHED CONDITIONAL"));
        assert!(expanded.text.contains("TRUE BRANCH"));
        assert!(!expanded.text.contains("FALSE BRANCH"));
    }

    #[test]
    fn expand_inputs_falls_back_to_disk_when_path_rebuild_is_required() {
        let dir = tempdir().expect("create tempdir");
        let source_path = dir.path().join("main.tex");
        let candidate = super::tex_path_candidate(dir.path(), "child");
        fs::write(&candidate, "DISK CONTENT").expect("write child");
        let cached_subtree = cached_source_subtree_for(&candidate, "child.tex", "CACHED CONTENT");
        let mut reuse_plan = reuse_plan_with_cached_subtree(&candidate, cached_subtree);
        reuse_plan.rebuild_paths.insert(candidate.clone());

        let (expanded, _, _, _) =
            expand_inputs_for_test("\\input{child}\n", &source_path, Some(&reuse_plan))
                .expect("fallback to disk");

        assert!(expanded.text.contains("DISK CONTENT"));
        assert!(!expanded.text.contains("CACHED CONTENT"));
    }

    #[test]
    fn expand_inputs_falls_back_to_disk_when_cached_node_is_missing_from_graph() {
        let dir = tempdir().expect("create tempdir");
        let source_path = dir.path().join("main.tex");
        let candidate = super::tex_path_candidate(dir.path(), "child");
        fs::write(&candidate, "DISK ONLY CONTENT").expect("write child");
        let cached_subtree = cached_source_subtree_for(&candidate, "child.tex", "CACHED CONTENT");
        let mut reuse_plan = reuse_plan_with_cached_subtree(&candidate, cached_subtree);
        reuse_plan.cached_dependency_graph.nodes.clear();

        let (expanded, _, _, _) =
            expand_inputs_for_test("\\input{child}\n", &source_path, Some(&reuse_plan))
                .expect("fallback when cached node missing");

        assert!(expanded.text.contains("DISK ONLY CONTENT"));
        assert!(!expanded.text.contains("CACHED CONTENT"));
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
    fn bibliography_candidate_paths_include_input_dir_before_project_root() {
        let dir = tempdir().expect("create tempdir");
        let project_root = dir.path().join("project");
        let input_dir = project_root.join("fixtures");
        let artifact_root = project_root.join("out");
        let overlay_root = dir.path().join("overlay");
        fs::create_dir_all(&input_dir).expect("create input dir");
        fs::create_dir_all(&artifact_root).expect("create artifact root");
        fs::create_dir_all(&overlay_root).expect("create overlay root");

        let candidates = super::bibliography_candidate_paths(
            &project_root,
            &input_dir,
            std::slice::from_ref(&overlay_root),
            &artifact_root,
            "main",
        );

        assert_eq!(
            candidates,
            vec![
                artifact_root.join("main.bbl"),
                input_dir.join("main.bbl"),
                project_root.join("main.bbl"),
                overlay_root.join("main.bbl"),
            ]
        );
    }

    #[test]
    fn loads_bbl_from_input_dir_when_project_root_differs() {
        let dir = tempdir().expect("create tempdir");
        let project_root = dir.path().join("project");
        let input_dir = project_root.join("fixtures/chapters");
        fs::create_dir_all(project_root.join(".git")).expect("create git marker");
        fs::create_dir_all(&input_dir).expect("create input dir");

        let input_file = input_dir.join("main.tex");
        fs::write(
            &input_file,
            document("See \\cite{key}.\n\\bibliography{refs}"),
        )
        .expect("write input");
        fs::write(
            input_dir.join("main.bbl"),
            "\\begin{thebibliography}{99}\n\\bibitem{key} Adjacent reference\n\\end{thebibliography}\n",
        )
        .expect("write adjacent bbl");

        let options = runtime_options(input_file, project_root.join("out"));
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        assert!(result.diagnostics.is_empty());
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("See [1]."));
        assert!(pdf.contains("[1] Adjacent reference"));
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
    fn prefers_artifact_root_bbl_over_project_root_bbl() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(
            &input_file,
            document("See \\cite{key}.\n\\bibliography{refs}"),
        )
        .expect("write input");
        fs::write(
            dir.path().join("main.bbl"),
            "\\begin{thebibliography}{99}\n\\bibitem{key} Project reference\n\\end{thebibliography}\n",
        )
        .expect("write project bbl");
        fs::write(
            output_dir.join("main.bbl"),
            "\\begin{thebibliography}{99}\n\\bibitem{key} Artifact reference\n\\end{thebibliography}\n",
        )
        .expect("write artifact bbl");

        let options = runtime_options(input_file, output_dir.clone());
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&output_dir.join("main.pdf"));
        assert!(pdf.contains("[1] Artifact reference"));
        assert!(!pdf.contains("[1] Project reference"));
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
    fn stale_bbl_emits_warning_when_sidecar_fingerprint_mismatches() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let bbl_path = dir.path().join("main.bbl");
        let bib_path = dir.path().join("refs.bib");
        let sidecar_path = dir.path().join("main.bbl.ferritex.json");
        fs::write(
            &input_file,
            document("See \\cite{key}.\n\\bibliography{refs}"),
        )
        .expect("write input");
        fs::write(&bib_path, "@book{key,\n  title = {Reference text}\n}\n").expect("write bib");
        fs::write(
            &bbl_path,
            "\\begin{thebibliography}{99}\n\\bibitem{key} Reference text\n\\end{thebibliography}\n",
        )
        .expect("write bbl");
        fs::write(
            &sidecar_path,
            r#"{"inputFingerprint":{"hash":"deadbeef"},"toolchain":"bibtex"}"#,
        )
        .expect("write sidecar");

        let options = runtime_options(input_file, dir.path().join("out"));
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 1);
        let stale = result
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message == "bibliography .bbl file appears stale")
            .expect("stale bibliography diagnostic");
        assert!(stale
            .context
            .as_deref()
            .unwrap_or_default()
            .contains("fingerprint"));
    }

    #[test]
    fn shell_escape_runs_bibtex_when_bbl_is_missing() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let bib_path = dir.path().join("refs.bib");
        fs::write(
            &input_file,
            document("See \\cite{key}.\n\\bibliographystyle{plain}\n\\bibliography{refs}"),
        )
        .expect("write input");
        fs::write(
            &bib_path,
            "@book{key,\n  title = {Generated reference}\n}\n",
        )
        .expect("write bib");

        let mut options = runtime_options(input_file, dir.path().join("out"));
        options.shell_escape = ShellEscapeMode::Enabled;
        let loader = MockAssetBundleLoader::valid();
        let shell_gateway = MockShellCommandGateway::with_bbl(
            "\\begin{thebibliography}{99}\n\\bibitem{key} Generated reference\n\\end{thebibliography}\n",
        );

        let result =
            service_with_shell(&FsTestFileAccessGate, &loader, &shell_gateway).compile(&options);

        assert_eq!(result.exit_code, 0);
        assert!(result.diagnostics.is_empty());
        assert_eq!(
            shell_gateway.commands(),
            vec![(
                "bibtex".to_string(),
                vec!["main".to_string()],
                options.output_dir.clone()
            )]
        );
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("See [1]."));
        assert!(pdf.contains("[1] Generated reference"));
        let sidecar = fs::read_to_string(options.output_dir.join("main.bbl.ferritex.json"))
            .expect("read sidecar");
        assert!(sidecar.contains("\"toolchain\": \"bibtex\""));
    }

    #[test]
    fn write18_emits_warning_when_shell_escape_is_disabled() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        fs::write(&input_file, document("\\write18{echo ok}\nHello")).expect("write input");

        let options = runtime_options(input_file, dir.path().join("out"));
        let loader = MockAssetBundleLoader::valid();
        let shell_gateway = MockShellCommandGateway::default();

        let result =
            service_with_shell(&FsTestFileAccessGate, &loader, &shell_gateway).compile(&options);

        assert_eq!(result.exit_code, 1);
        assert!(shell_gateway.commands().is_empty());
        let diagnostic = result
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("\\write18{echo ok}"))
            .expect("shell escape diagnostic");
        assert_eq!(diagnostic.severity, Severity::Warning);
        assert_eq!(
            diagnostic.suggestion.as_deref(),
            Some("use --shell-escape to enable external command execution")
        );
    }

    #[test]
    fn pdf_encoding_warning_is_propagated_to_compile_diagnostics() {
        // '漢' has no WinAnsi mapping and no Symbol-font mapping, so it still
        // triggers the encoding warning. (Greek/math glyphs are now routed
        // through the Symbol font rather than the WinAnsi text path.)
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        fs::write(&input_file, document("Hello 漢")).expect("write input");

        let options = runtime_options(input_file, dir.path().join("out"));
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 1);
        let diagnostic = result
            .diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic.severity == Severity::Warning
                    && diagnostic.message.contains('漢')
                    && diagnostic.message.contains("WinAnsiEncoding")
            })
            .expect("pdf encoding warning diagnostic");
        assert!(diagnostic.message.contains("replaced with '?'"));
    }

    #[test]
    fn pdf_math_mode_unicode_glyphs_emit_no_winansi_warning() {
        // Regression for Issue #9: math-mode Unicode glyphs (\alpha, \pi, \int,
        // \sqrt{}, \infty, thin-space) must render through a math-capable font
        // rather than via WinAnsi '?' substitution.
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        fs::write(
            &input_file,
            document(
                "Inline math: $\\alpha + \\beta = \\gamma$.\n\nDisplay math:\n\\[\n  \\int_0^\\infty e^{-x^2} \\, dx = \\frac{\\sqrt{\\pi}}{2}\n\\]",
            ),
        )
        .expect("write input");

        let options = runtime_options(input_file, dir.path().join("out"));
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        let winansi_messages: Vec<String> = result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.message.contains("WinAnsiEncoding"))
            .map(|diagnostic| diagnostic.message.clone())
            .collect();

        assert!(
            winansi_messages.is_empty(),
            "expected no WinAnsi encoding warnings for math-mode glyphs, got: {winansi_messages:?}",
        );
    }

    #[test]
    fn write18_executes_through_shell_gateway_when_enabled() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        fs::write(&input_file, document("\\write18{echo ok}\nHello")).expect("write input");

        let mut options = runtime_options(input_file, dir.path().join("out"));
        options.shell_escape = ShellEscapeMode::Enabled;
        let loader = MockAssetBundleLoader::valid();
        let shell_gateway = MockShellCommandGateway::default();

        let result =
            service_with_shell(&FsTestFileAccessGate, &loader, &shell_gateway).compile(&options);

        assert_eq!(result.exit_code, 0);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let expected_working_dir = super::normalize_existing_path(dir.path());
        assert_eq!(
            shell_gateway.commands(),
            vec![(
                "echo".to_string(),
                vec!["ok".to_string()],
                expected_working_dir
            )]
        );
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
        assert_eq!(result.diagnostics.len(), 2);
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message == "bibliography .bbl file not found"));
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message == "unresolved citation `missing`"));
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("See [?]."));
    }

    #[test]
    fn unresolved_citation_emits_warning_when_bbl_lacks_requested_key() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        fs::write(
            &input_file,
            document("See \\cite{missing}.\n\\bibliography{refs}"),
        )
        .expect("write input");
        fs::write(
            dir.path().join("main.bbl"),
            "\\begin{thebibliography}{99}\n\\bibitem{other} Other reference\n\\end{thebibliography}\n",
        )
        .expect("write bbl");

        let options = runtime_options(input_file, dir.path().join("out"));
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 1);
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message == "unresolved citation `missing`"));
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("See [?]."));
    }

    #[test]
    fn nocite_with_adjacent_bbl_renders_bibliography_without_parse_error() {
        let dir = tempdir().expect("create tempdir");
        let project_root = dir.path().join("project");
        let input_dir = project_root.join("fixtures/chapters");
        fs::create_dir_all(project_root.join(".git")).expect("create git marker");
        fs::create_dir_all(&input_dir).expect("create input dir");

        let input_file = input_dir.join("main.tex");
        fs::write(&input_file, document("\\nocite{key}\n\\bibliography{refs}"))
            .expect("write input");
        fs::write(
            input_dir.join("main.bbl"),
            "\\begin{thebibliography}{99}\n\\bibitem{key} Hidden reference\n\\end{thebibliography}\n",
        )
        .expect("write adjacent bbl");

        let options = runtime_options(input_file, project_root.join("out"));
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        assert!(!result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("undefined control sequence")));
        let pdf = read_pdf(&options.output_dir.join("main.pdf"));
        assert!(pdf.contains("[1] Hidden reference"));
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
        fs::write(&input_file, document("AB")).expect("write input");
        let font_bytes = build_test_ttf();
        fs::write(font_dir.join("TestSans.ttf"), &font_bytes).expect("write font");
        write_asset_bundle_fixture(&bundle_path);

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
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmainfont{ChosenSans}\n\\begin{document}\nAB\n\\end{document}\n",
        )
        .expect("write input");
        fs::write(font_dir.join("AFirst.ttf"), build_test_ttf()).expect("write first font");
        fs::write(font_dir.join("ChosenSans.ttf"), build_test_ttf()).expect("write chosen font");
        write_asset_bundle_fixture(&bundle_path);

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
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmainfont{MainFace}\n\\setsansfont{SansFace}\n\\setmonofont{MonoFace}\n\\begin{document}\nAB\\par\n\\textsf{AB}\\par\n\\texttt{AB}\n\\end{document}\n",
        )
        .expect("write input");
        fs::write(font_dir.join("MainFace.ttf"), build_test_ttf()).expect("write main font");
        fs::write(font_dir.join("SansFace.ttf"), build_test_ttf()).expect("write sans font");
        fs::write(font_dir.join("MonoFace.ttf"), build_test_ttf()).expect("write mono font");
        write_asset_bundle_fixture(&bundle_path);

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
        assert!(pdf.contains("/F2 10 Tf"));
        assert!(pdf.contains("/F3 10 Tf"));
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
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmainfont{MainFace}\n\\setsansfont{SansFace}\n\\setmonofont{MonoFace}\n\\begin{document}\nAB\\par\n\\textsf{AB}\\par\n\\texttt{AB}\n\\end{document}\n",
        )
        .expect("write input");
        fs::write(font_dir.join("MainFace.ttf"), build_test_ttf()).expect("write main font");
        fs::write(font_dir.join("SansFace.ttf"), build_test_ttf()).expect("write sans font");
        fs::write(font_dir.join("MonoFace.ttf"), build_test_ttf()).expect("write mono font");
        write_asset_bundle_fixture(&bundle_path);

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
        assert!(pdf.contains("/F2 10 Tf"));
        assert!(pdf.contains("/F3 10 Tf"));
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
        fs::write(&tfm_path, build_test_tfm()).expect("write tfm");
        write_asset_bundle_fixture(&bundle_path);
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
        assert!(pdf.contains("/F2 10 Tf"));
        assert!(pdf.contains("/F3 10 Tf"));
    }

    #[test]
    fn embeds_type1_font_when_bundle_contains_matching_pfb() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        let bundle_path = dir.path().join("bundle");
        let tfm_path = bundle_path.join("texmf/fonts/tfm/public/cm/cmr10.tfm");
        let pfb_path = bundle_path.join("texmf/fonts/type1/public/amsfonts/cm/cmr10.pfb");
        fs::create_dir_all(tfm_path.parent().expect("cmr10 parent")).expect("create tfm dir");
        fs::create_dir_all(pfb_path.parent().expect("cmr10 pfb parent")).expect("create pfb dir");
        fs::write(&tfm_path, build_test_tfm()).expect("write tfm");
        fs::write(&pfb_path, build_test_pfb("CMR10")).expect("write pfb");
        write_asset_bundle_fixture(&bundle_path);
        fs::write(&input_file, document("Hello")).expect("write input");

        let mut options = runtime_options(input_file, output_dir.clone());
        options.asset_bundle = Some(bundle_path);
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&output_dir.join("main.pdf"));
        assert!(pdf.contains("/Subtype /Type1 /BaseFont /FERRTX+CMR10"));
        assert!(pdf.contains("/FontFile "));
        assert!(pdf.contains("/Length2 4"));
        assert!(pdf.contains("/Length3 12"));
        assert!(pdf.contains("/Flags 6"));
        assert!(!pdf.contains("/BaseFont /Helvetica"));
    }

    #[test]
    fn embeds_cmbx12_type1_font_when_document_produces_outlines() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        let bundle_path = dir.path().join("bundle");
        let cmr10_tfm = bundle_path.join("texmf/fonts/tfm/public/cm/cmr10.tfm");
        let cmbx12_tfm = bundle_path.join("texmf/fonts/tfm/public/cm/cmbx12.tfm");
        let cmr10_pfb = bundle_path.join("texmf/fonts/type1/public/amsfonts/cm/cmr10.pfb");
        let cmbx12_pfb = bundle_path.join("texmf/fonts/type1/public/amsfonts/cm/cmbx12.pfb");
        for path in [&cmr10_tfm, &cmbx12_tfm] {
            fs::create_dir_all(path.parent().expect("tfm parent")).expect("create tfm dir");
            fs::write(path, build_test_tfm()).expect("write tfm");
        }
        for (path, font_name) in [(&cmr10_pfb, "CMR10"), (&cmbx12_pfb, "CMBX12")] {
            fs::create_dir_all(path.parent().expect("pfb parent")).expect("create pfb dir");
            fs::write(path, build_test_pfb(font_name)).expect("write pfb");
        }
        write_asset_bundle_fixture(&bundle_path);
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\begin{document}\n\\section{Intro}\nHello\n\\end{document}\n",
        )
        .expect("write input");

        let mut options = runtime_options(input_file, output_dir.clone());
        options.asset_bundle = Some(bundle_path);
        let loader = MockAssetBundleLoader::valid();

        let result = service(&FsTestFileAccessGate, &loader).compile(&options);

        assert_eq!(result.exit_code, 0);
        let pdf = read_pdf(&output_dir.join("main.pdf"));
        assert!(pdf.contains("/BaseFont /FERRTX+CMR10"));
        assert!(pdf.contains("/BaseFont /FERRTX+CMBX12"));
    }

    #[test]
    fn compile_with_setmainfont_not_found_emits_diagnostic() {
        let dir = tempdir().expect("create tempdir");
        let input_file = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        let bundle_path = dir.path().join("bundle");
        let font_dir = bundle_path.join("texmf/fonts/truetype/public/test");
        fs::create_dir_all(&font_dir).expect("create font dir");
        fs::write(
            &input_file,
            "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmainfont{MissingFont}\n\\begin{document}\nAB\n\\end{document}\n",
        )
        .expect("write input");
        fs::write(font_dir.join("FallbackSans.ttf"), build_test_ttf())
            .expect("write fallback font");
        write_asset_bundle_fixture(&bundle_path);

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
        fs::write(&input_file, document("AB")).expect("write input");
        fs::write(font_dir.join("AFirst.ttf"), build_test_ttf()).expect("write first font");
        fs::write(font_dir.join("ChosenSans.ttf"), build_test_ttf()).expect("write second font");
        write_asset_bundle_fixture(&bundle_path);

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
        let tfm_path = bundle_path.join("texmf/fonts/tfm/public/cm/cmr10.tfm");
        fs::create_dir_all(&bundle_path).expect("create bundle dir");
        fs::create_dir_all(tfm_path.parent().expect("cmr10 parent")).expect("create tfm dir");
        fs::write(&tfm_path, build_test_tfm()).expect("write cmr10.tfm");
        write_asset_bundle_fixture(&bundle_path);
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
    fn nested_input_access_denied_context_includes_denied_path() {
        let dir = tempdir().expect("create tempdir");
        let src = dir.path().join("src");
        let outside = dir.path().join("outside");
        let out = dir.path().join("out");
        fs::create_dir_all(&src).expect("create src");
        fs::create_dir_all(&outside).expect("create outside");
        fs::write(src.join("main.tex"), document("\\input{../outside/secret}"))
            .expect("write main");
        let denied_path = outside.join("secret.tex");
        fs::write(&denied_path, "SECRET\n").expect("write denied input");

        let gate = ScopedFsFileAccessGate {
            allowed_read_root: src.clone(),
            allowed_write_root: out.clone(),
        };
        let loader = MockAssetBundleLoader::valid();
        let options = runtime_options(src.join("main.tex"), out);

        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 2);
        let diagnostic = result
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("\\input/\\include target"))
            .expect("nested input diagnostic");
        let context = diagnostic.context.as_deref().expect("nested input context");
        assert!(context.contains("input file access denied"));
        assert!(context.contains(denied_path.to_string_lossy().as_ref()));
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
            &project_root.join("main.tex"),
            &project_root,
            &[],
            None,
            false,
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
            &project_root.join("main.tex"),
            &project_root,
            &[overlay_root],
            None,
            false,
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
        let tfm_path = bundle_root.join("texmf/fonts/tfm/public/cm/cmr10.tfm");
        fs::create_dir_all(bundled_file.parent().expect("bundle texmf parent"))
            .expect("create bundle texmf");
        fs::create_dir_all(tfm_path.parent().expect("cmr10 parent")).expect("create tfm dir");
        fs::write(&bundled_file, "BUNDLED FILE CONTENT\n").expect("write bundled file");
        fs::write(&tfm_path, build_test_tfm()).expect("write cmr10.tfm");
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
        let tfm_path = bundle_root.join("texmf/fonts/tfm/public/cm/cmr10.tfm");
        fs::create_dir_all(bundled_file.parent().expect("bundle texmf parent"))
            .expect("create bundle texmf");
        fs::create_dir_all(tfm_path.parent().expect("cmr10 parent")).expect("create tfm dir");
        fs::write(&bundled_file, "BUNDLED CONTENT\n").expect("write bundled file");
        fs::write(&tfm_path, build_test_tfm()).expect("write cmr10.tfm");
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
    fn openout_outside_allowed_root_emits_access_denied_diagnostic() {
        let dir = tempdir().expect("create tempdir");
        let src = dir.path().join("src");
        let outside = dir.path().join("outside");
        let out = dir.path().join("out");
        fs::create_dir_all(&src).expect("create src");
        fs::create_dir_all(&outside).expect("create outside");
        fs::write(
            src.join("main.tex"),
            document("\\openout1=../outside/output.txt\nHello"),
        )
        .expect("write main");

        let gate = ScopedFsFileAccessGate {
            allowed_read_root: src.clone(),
            allowed_write_root: out.clone(),
        };
        let loader = MockAssetBundleLoader::valid();
        let options = runtime_options(src.join("main.tex"), out);

        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 2);
        let diagnostic = result
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("file access denied: \\openout"))
            .expect("openout denied diagnostic");
        assert_eq!(
            diagnostic.context.as_deref(),
            Some("outside allowed read/write roots")
        );
        assert!(diagnostic.message.contains("outside/output.txt"));
    }

    #[test]
    fn fails_when_bundle_has_no_cmr10_tfm() {
        let dir = tempdir().expect("create tempdir");
        let bundle_root = dir.path().join("bundle");
        fs::create_dir_all(&bundle_root).expect("create bundle");
        fs::write(dir.path().join("main.tex"), document("AA")).expect("write main");

        let mut options = runtime_options(dir.path().join("main.tex"), dir.path().join("out"));
        options.asset_bundle = Some(bundle_root);

        let gate = FsTestFileAccessGate;
        let loader = MockAssetBundleLoader::valid();

        let result = service(&gate, &loader).compile(&options);

        assert_eq!(result.exit_code, 2);
        assert_eq!(result.output_pdf, None);
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.message
                    == "required asset bundle font metrics \"cmr10\" could not be resolved"
        }));
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
        assert!(pdf.contains("(A) Tj\n0 -12 Td\n(A) Tj"));
    }
}
