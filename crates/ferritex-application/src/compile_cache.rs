use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use ferritex_core::compilation::SymbolLocation;
use ferritex_core::diagnostics::{Diagnostic, Severity};
use ferritex_core::incremental::{DependencyGraph, RecompilationScope};
use ferritex_core::kernel::api::{DimensionValue, SourceSpan};
use ferritex_core::pdf::{OpacityGraphicsStateKey, PageRenderPayload, PdfLinkAnnotation};
use ferritex_core::policy::{FileAccessError, FileAccessGate};
use ferritex_core::synctex::SourceLineTrace;
use ferritex_core::typesetting::{DocumentLayoutFragment, FloatContent, PlacementSpec};
use serde::{Deserialize, Serialize};

use crate::stable_compile_state::StableCompileState;

const CACHE_VERSION: u32 = 7;
const PREVIOUS_SPLIT_CACHE_VERSION: u32 = 6;
const LEGACY_CACHE_VERSION: u32 = 4;
const CACHE_DIR_NAME: &str = ".ferritex-cache";
const CACHE_INDEX_FILENAME: &str = "index.json";
const CACHE_PARTITIONS_DIR_NAME: &str = "partitions";
const CACHE_RECORD_EXTENSION: &str = "json";
const MAX_CACHE_RECORD_FILES: usize = 64;

pub struct CompileCache<'a> {
    file_access_gate: &'a dyn FileAccessGate,
    primary_input: PathBuf,
    jobname: String,
    output_pdf: PathBuf,
    cache_dir: PathBuf,
    record_dir: PathBuf,
    partitions_dir: PathBuf,
    metadata_path: PathBuf,
    legacy_metadata_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedCompileArtifact {
    pub stable_compile_state: StableCompileState,
    pub output_pdf: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheLookupResult {
    pub artifact: Option<CachedCompileArtifact>,
    pub baseline_state: Option<StableCompileState>,
    pub diagnostics: Vec<Diagnostic>,
    pub changed_paths: Vec<PathBuf>,
    pub rebuild_paths: BTreeSet<PathBuf>,
    pub cached_dependency_graph: Option<DependencyGraph>,
    pub cached_source_subtrees: BTreeMap<PathBuf, CachedSourceSubtree>,
    pub cached_typeset_fragments: BTreeMap<String, CachedTypesetFragment>,
    pub cached_page_payloads: BTreeMap<String, Vec<CachedPagePayload>>,
    pub scope: Option<RecompilationScope>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedSourceSubtree {
    pub text: String,
    pub source_lines: Vec<SourceLineTrace>,
    pub source_files: Vec<PathBuf>,
    pub labels: BTreeMap<String, SymbolLocation>,
    pub citations: BTreeMap<String, SymbolLocation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockCheckpoint {
    pub node_index: usize,
    pub source_span: Option<SourceSpan>,
    pub layout_state: BlockLayoutState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockLayoutState {
    pub content_used: DimensionValue,
    pub completed_page_count: usize,
    pub pending_floats: Vec<PendingFloat>,
    pub footnote_count: usize,
    pub figure_count: u32,
    pub table_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingFloat {
    pub spec: PlacementSpec,
    pub content: FloatContent,
    pub defer_count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockCheckpointData {
    pub checkpoints: Vec<BlockCheckpoint>,
    pub source_hash: u64,
    #[serde(default)]
    pub partition_body: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedTypesetFragment {
    pub fragment: DocumentLayoutFragment,
    pub source_hash: u64,
    #[serde(default)]
    pub block_checkpoints: Option<BlockCheckpointData>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedPagePayload {
    pub stream_hash: u64,
    pub stream: String,
    pub annotations: Vec<PdfLinkAnnotation>,
    pub opacity_graphics_states: BTreeSet<OpacityGraphicsStateKey>,
}

impl From<PageRenderPayload> for CachedPagePayload {
    fn from(payload: PageRenderPayload) -> Self {
        Self {
            stream_hash: payload.stream_hash,
            stream: payload.stream,
            annotations: payload.annotations,
            opacity_graphics_states: payload.opacity_graphics_states,
        }
    }
}

impl CachedPagePayload {
    pub fn to_page_render_payload(&self, page_index: usize) -> Option<PageRenderPayload> {
        PageRenderPayload::try_from_cached(
            page_index,
            self.stream_hash,
            self.stream.clone(),
            self.annotations.clone(),
            self.opacity_graphics_states.clone(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LegacyCompileCacheRecord {
    version: u32,
    primary_input: PathBuf,
    jobname: String,
    output_pdf: PathBuf,
    output_pdf_hash: u64,
    dependency_graph: DependencyGraph,
    stable_compile_state: StableCompileState,
    #[serde(default)]
    cached_source_subtrees: BTreeMap<PathBuf, CachedSourceSubtree>,
    #[serde(default)]
    cached_typeset_fragments: BTreeMap<String, CachedTypesetFragment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CacheIndex {
    version: u32,
    primary_input: PathBuf,
    jobname: String,
    output_pdf: PathBuf,
    output_pdf_hash: u64,
    dependency_graph: DependencyGraph,
    stable_compile_state: StableCompileState,
    partition_keys: Vec<String>,
    #[serde(default)]
    partition_hashes: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct PartitionBlob {
    #[serde(default)]
    cached_source_subtrees: BTreeMap<PathBuf, CachedSourceSubtree>,
    #[serde(default)]
    cached_typeset_fragments: BTreeMap<String, CachedTypesetFragment>,
    #[serde(default)]
    cached_page_payloads: Option<Vec<CachedPagePayload>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LoadedCacheRecord {
    primary_input: PathBuf,
    jobname: String,
    output_pdf: PathBuf,
    output_pdf_hash: u64,
    dependency_graph: DependencyGraph,
    stable_compile_state: StableCompileState,
    cached_source_subtrees: BTreeMap<PathBuf, CachedSourceSubtree>,
    cached_typeset_fragments: BTreeMap<String, CachedTypesetFragment>,
    cached_page_payloads: BTreeMap<String, Vec<CachedPagePayload>>,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnedCacheRecordFile {
    path: PathBuf,
    modified: std::time::SystemTime,
}

impl<'a> CompileCache<'a> {
    pub fn new(
        file_access_gate: &'a dyn FileAccessGate,
        output_dir: &Path,
        primary_input: &Path,
        jobname: &str,
    ) -> Self {
        let cache_dir = output_dir.join(CACHE_DIR_NAME);
        let cache_key = format!(
            "{}-{:016x}",
            sanitize_cache_key(jobname),
            fingerprint_bytes(primary_input.to_string_lossy().as_bytes())
        );
        let record_dir = cache_dir.join(&cache_key);

        Self {
            file_access_gate,
            primary_input: primary_input.to_path_buf(),
            jobname: jobname.to_string(),
            output_pdf: output_dir.join(format!("{jobname}.pdf")),
            record_dir: record_dir.clone(),
            partitions_dir: record_dir.join(CACHE_PARTITIONS_DIR_NAME),
            metadata_path: record_dir.join(CACHE_INDEX_FILENAME),
            legacy_metadata_path: cache_dir.join(format!("{cache_key}.json")),
            cache_dir,
        }
    }

    pub fn lookup(&self, changed_paths_hint: &[PathBuf]) -> CacheLookupResult {
        let record = match self.load_record() {
            Ok(Some(record)) => record,
            Ok(None) => return empty_lookup_result(Vec::new()),
            Err(diagnostic) => return empty_lookup_result(vec![diagnostic]),
        };

        if record.primary_input != self.primary_input
            || record.jobname != self.jobname
            || record.output_pdf != self.output_pdf
        {
            return empty_lookup_result(Vec::new());
        }

        let baseline_state = record.stable_compile_state.clone();

        let change_summary = self.detect_changes(&record.dependency_graph, changed_paths_hint);
        if !change_summary.changed_paths.is_empty() {
            let scope = if change_summary.scope == RecompilationScope::LocalRegion
                && record.cached_typeset_fragments.values().any(|fragment| {
                    fragment
                        .block_checkpoints
                        .as_ref()
                        .map(|data| !data.partition_body.is_empty())
                        .unwrap_or(false)
                }) {
                // TODO: Thread partition ids through detect_changes so this promotion can
                // populate affected_partitions instead of falling back to an empty list.
                RecompilationScope::BlockLevel {
                    affected_partitions: Vec::new(),
                    references_affected: false,
                    pagination_affected: false,
                }
            } else {
                change_summary.scope
            };
            return CacheLookupResult {
                artifact: None,
                baseline_state: Some(baseline_state),
                diagnostics: record.diagnostics,
                changed_paths: change_summary.changed_paths,
                rebuild_paths: change_summary.rebuild_paths,
                cached_dependency_graph: Some(record.dependency_graph),
                cached_source_subtrees: record.cached_source_subtrees,
                cached_typeset_fragments: record.cached_typeset_fragments,
                cached_page_payloads: record.cached_page_payloads,
                scope: Some(scope),
            };
        }

        let output_pdf_hash = match self.file_access_gate.read_file(&self.output_pdf) {
            Ok(bytes) => fingerprint_bytes(&bytes),
            Err(error) => {
                let mut diagnostics = record.diagnostics;
                diagnostics.push(cache_info_diagnostic(
                    format!("cached PDF artifact is unavailable: {error}"),
                    &self.output_pdf,
                ));
                return CacheLookupResult {
                    artifact: None,
                    baseline_state: Some(baseline_state),
                    diagnostics,
                    changed_paths: Vec::new(),
                    rebuild_paths: BTreeSet::new(),
                    cached_dependency_graph: None,
                    cached_source_subtrees: BTreeMap::new(),
                    cached_typeset_fragments: BTreeMap::new(),
                    cached_page_payloads: BTreeMap::new(),
                    scope: None,
                };
            }
        };

        if output_pdf_hash != record.output_pdf_hash {
            let mut diagnostics = record.diagnostics;
            diagnostics.push(cache_info_diagnostic(
                "cached PDF artifact hash mismatch; falling back to full compile",
                &self.output_pdf,
            ));
            return CacheLookupResult {
                artifact: None,
                baseline_state: Some(baseline_state),
                diagnostics,
                changed_paths: Vec::new(),
                rebuild_paths: BTreeSet::new(),
                cached_dependency_graph: None,
                cached_source_subtrees: BTreeMap::new(),
                cached_typeset_fragments: BTreeMap::new(),
                cached_page_payloads: BTreeMap::new(),
                scope: None,
            };
        }

        CacheLookupResult {
            artifact: Some(CachedCompileArtifact {
                stable_compile_state: record.stable_compile_state,
                output_pdf: record.output_pdf,
            }),
            baseline_state: Some(baseline_state),
            diagnostics: record.diagnostics,
            changed_paths: Vec::new(),
            rebuild_paths: BTreeSet::new(),
            cached_dependency_graph: Some(record.dependency_graph),
            cached_source_subtrees: record.cached_source_subtrees,
            cached_typeset_fragments: record.cached_typeset_fragments,
            cached_page_payloads: record.cached_page_payloads,
            scope: None,
        }
    }

    pub fn store(
        &self,
        dependency_graph: &DependencyGraph,
        stable_compile_state: &StableCompileState,
        output_pdf_hash: u64,
        cached_source_subtrees: &BTreeMap<PathBuf, CachedSourceSubtree>,
        cached_typeset_fragments: &BTreeMap<String, CachedTypesetFragment>,
    ) -> Option<Diagnostic> {
        self.store_with_page_payloads(
            dependency_graph,
            stable_compile_state,
            output_pdf_hash,
            cached_source_subtrees,
            cached_typeset_fragments,
            &BTreeMap::new(),
        )
    }

    pub fn store_with_page_payloads(
        &self,
        dependency_graph: &DependencyGraph,
        stable_compile_state: &StableCompileState,
        output_pdf_hash: u64,
        cached_source_subtrees: &BTreeMap<PathBuf, CachedSourceSubtree>,
        cached_typeset_fragments: &BTreeMap<String, CachedTypesetFragment>,
        cached_page_payloads: &BTreeMap<String, Vec<CachedPagePayload>>,
    ) -> Option<Diagnostic> {
        if let Err(error) = self.file_access_gate.ensure_directory(&self.cache_dir) {
            return Some(cache_info_diagnostic(
                format!("failed to prepare compile cache directory: {error}"),
                &self.cache_dir,
            ));
        }

        if let Err(error) = self.file_access_gate.ensure_directory(&self.record_dir) {
            return Some(cache_info_diagnostic(
                format!("failed to prepare compile cache record directory: {error}"),
                &self.record_dir,
            ));
        }

        if let Err(error) = self.file_access_gate.ensure_directory(&self.partitions_dir) {
            return Some(cache_info_diagnostic(
                format!("failed to prepare compile cache partition directory: {error}"),
                &self.partitions_dir,
            ));
        }

        let previous_hashes: BTreeMap<String, u64> = self
            .file_access_gate
            .read_file(&self.metadata_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<CacheIndex>(&bytes).ok())
            .map(|idx| idx.partition_hashes)
            .unwrap_or_default();

        let partition_blobs = partition_blobs_for(
            cached_source_subtrees,
            cached_typeset_fragments,
            cached_page_payloads,
        );
        let mut new_hashes = BTreeMap::new();
        for (partition_key, blob) in &partition_blobs {
            let path = self.partition_blob_path(partition_key);
            let bytes = match serde_json::to_vec(&blob) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return Some(cache_info_diagnostic(
                        format!("failed to serialize compile cache partition blob: {error}"),
                        &path,
                    ));
                }
            };
            let hash = fingerprint_bytes(&bytes);

            if previous_hashes.get(partition_key) == Some(&hash) && path.exists() {
                new_hashes.insert(partition_key.clone(), hash);
                continue;
            }

            if let Err(error) = self.file_access_gate.write_file(&path, &bytes) {
                return Some(cache_info_diagnostic(
                    format!("failed to persist compile cache partition blob: {error}"),
                    &path,
                ));
            }

            new_hashes.insert(partition_key.clone(), hash);
        }

        let index = CacheIndex {
            version: CACHE_VERSION,
            primary_input: self.primary_input.clone(),
            jobname: self.jobname.clone(),
            output_pdf: self.output_pdf.clone(),
            output_pdf_hash,
            dependency_graph: dependency_graph.clone(),
            stable_compile_state: stable_compile_state.clone(),
            partition_keys: partition_blobs.keys().cloned().collect(),
            partition_hashes: new_hashes,
        };

        let bytes = match serde_json::to_vec_pretty(&index) {
            Ok(bytes) => bytes,
            Err(error) => {
                return Some(cache_info_diagnostic(
                    format!("failed to serialize compile cache index: {error}"),
                    &self.metadata_path,
                ));
            }
        };

        if let Err(error) = self
            .file_access_gate
            .write_file(&self.metadata_path, &bytes)
        {
            return Some(cache_info_diagnostic(
                format!("failed to persist compile cache index: {error}"),
                &self.metadata_path,
            ));
        }

        let cleanup_diagnostic = self.cleanup_orphaned_partitions(&index.partition_keys);
        let legacy_diagnostic = self.remove_legacy_record_if_present();
        let eviction_diagnostic = self.evict_excess_records().err().map(|error| {
            cache_cleanup_diagnostic(
                format!("failed to evict old compile cache records: {error}"),
                &self.cache_dir,
            )
        });

        cleanup_diagnostic
            .or(legacy_diagnostic)
            .or(eviction_diagnostic)
    }

    fn detect_changes(
        &self,
        dependency_graph: &DependencyGraph,
        changed_paths_hint: &[PathBuf],
    ) -> ChangeSummary {
        let mut changed_paths = Vec::new();

        if changed_paths_hint.is_empty() {
            for (path, node) in &dependency_graph.nodes {
                if self.path_has_changed(path, node.content_hash) {
                    changed_paths.push(path.clone());
                }
            }
        } else {
            // Fast path: trust the watcher hint to be a complete set of changed paths.
            // If a file changes after poll_changes() but before this check, we can miss it
            // for this cycle; that short race is acceptable because the next poll will
            // observe it. Preserve the same assumption when migrating to inotify/kqueue.
            let mut checked_paths = BTreeSet::new();
            for path in changed_paths_hint {
                if !checked_paths.insert(path.clone()) {
                    continue;
                }

                let Some(node) = dependency_graph.nodes.get(path) else {
                    continue;
                };

                if self.path_has_changed(path, node.content_hash) {
                    changed_paths.push(path.clone());
                }
            }
        }

        let rebuild_paths = dependency_graph.affected_paths(changed_paths.iter());
        let scope = if changed_paths.len() <= 1
            || (!rebuild_paths.is_empty() && rebuild_paths.len() < dependency_graph.nodes.len())
        {
            RecompilationScope::LocalRegion
        } else {
            RecompilationScope::FullDocument
        };

        ChangeSummary {
            changed_paths,
            rebuild_paths,
            scope,
        }
    }

    fn path_has_changed(&self, path: &Path, expected_hash: u64) -> bool {
        let current_hash = match self.file_access_gate.read_file(path) {
            Ok(bytes) => fingerprint_bytes(&bytes),
            Err(_) => return true,
        };

        current_hash != expected_hash
    }

    fn load_record(&self) -> Result<Option<LoadedCacheRecord>, Diagnostic> {
        match self.file_access_gate.read_file(&self.metadata_path) {
            Ok(bytes) => self.load_split_record(&bytes),
            Err(FileAccessError::Io { source })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                self.load_legacy_record()
            }
            Err(error) => Err(cache_info_diagnostic(
                format!("failed to read compile cache index: {error}"),
                &self.metadata_path,
            )),
        }
    }

    fn load_split_record(&self, bytes: &[u8]) -> Result<Option<LoadedCacheRecord>, Diagnostic> {
        let index: CacheIndex = serde_json::from_slice(bytes).map_err(|error| {
            cache_info_diagnostic(
                format!("compile cache index is invalid: {error}"),
                &self.metadata_path,
            )
        })?;

        if !matches!(index.version, CACHE_VERSION | PREVIOUS_SPLIT_CACHE_VERSION) {
            return Err(cache_info_diagnostic(
                format!(
                    "compile cache version mismatch (found {}, expected {CACHE_VERSION} or {PREVIOUS_SPLIT_CACHE_VERSION})",
                    index.version,
                ),
                &self.metadata_path,
            ));
        }

        let mut cached_source_subtrees = BTreeMap::new();
        let mut cached_typeset_fragments = BTreeMap::new();
        let mut cached_page_payloads = BTreeMap::new();
        let mut diagnostics = Vec::new();

        for partition_key in &index.partition_keys {
            let path = self.partition_blob_path(partition_key);
            let bytes = match self.file_access_gate.read_file(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    diagnostics.push(cache_info_diagnostic(
                        format!(
                            "failed to read compile cache partition blob `{partition_key}`: {error}"
                        ),
                        &path,
                    ));
                    continue;
                }
            };

            let blob: PartitionBlob = match serde_json::from_slice(&bytes) {
                Ok(blob) => blob,
                Err(error) => {
                    diagnostics.push(cache_info_diagnostic(
                        format!(
                            "compile cache partition blob `{partition_key}` is invalid: {error}"
                        ),
                        &path,
                    ));
                    continue;
                }
            };

            cached_source_subtrees.extend(blob.cached_source_subtrees);
            if let Some(page_payloads) = blob.cached_page_payloads {
                if let Some(partition_id) = blob.cached_typeset_fragments.keys().next().cloned() {
                    cached_page_payloads.insert(partition_id, page_payloads);
                }
            }
            cached_typeset_fragments.extend(blob.cached_typeset_fragments);
        }

        Ok(Some(LoadedCacheRecord {
            primary_input: index.primary_input,
            jobname: index.jobname,
            output_pdf: index.output_pdf,
            output_pdf_hash: index.output_pdf_hash,
            dependency_graph: index.dependency_graph,
            stable_compile_state: index.stable_compile_state,
            cached_source_subtrees,
            cached_typeset_fragments,
            cached_page_payloads,
            diagnostics,
        }))
    }

    fn load_legacy_record(&self) -> Result<Option<LoadedCacheRecord>, Diagnostic> {
        let bytes = match self.file_access_gate.read_file(&self.legacy_metadata_path) {
            Ok(bytes) => bytes,
            Err(FileAccessError::Io { source })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(cache_info_diagnostic(
                    format!("failed to read compile cache metadata: {error}"),
                    &self.legacy_metadata_path,
                ));
            }
        };

        let record: LegacyCompileCacheRecord = serde_json::from_slice(&bytes).map_err(|error| {
            cache_info_diagnostic(
                format!("compile cache metadata is invalid: {error}"),
                &self.legacy_metadata_path,
            )
        })?;

        if record.version != LEGACY_CACHE_VERSION {
            return Err(cache_info_diagnostic(
                format!(
                    "compile cache legacy version mismatch (found {}, expected {LEGACY_CACHE_VERSION})",
                    record.version
                ),
                &self.legacy_metadata_path,
            ));
        }

        Ok(Some(LoadedCacheRecord {
            primary_input: record.primary_input,
            jobname: record.jobname,
            output_pdf: record.output_pdf,
            output_pdf_hash: record.output_pdf_hash,
            dependency_graph: record.dependency_graph,
            stable_compile_state: record.stable_compile_state,
            cached_source_subtrees: record.cached_source_subtrees,
            cached_typeset_fragments: record.cached_typeset_fragments,
            cached_page_payloads: BTreeMap::new(),
            diagnostics: Vec::new(),
        }))
    }

    fn remove_legacy_record_if_present(&self) -> Option<Diagnostic> {
        match fs::remove_file(&self.legacy_metadata_path) {
            Ok(()) => None,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => Some(cache_cleanup_diagnostic(
                format!("failed to remove legacy compile cache metadata: {error}"),
                &self.legacy_metadata_path,
            )),
        }
    }

    fn cleanup_orphaned_partitions(&self, valid_keys: &[String]) -> Option<Diagnostic> {
        let valid_filenames: BTreeSet<_> = valid_keys
            .iter()
            .map(|key| format!("{key}.{CACHE_RECORD_EXTENSION}"))
            .collect();
        let entries = match fs::read_dir(&self.partitions_dir) {
            Ok(entries) => entries,
            Err(_) => return None,
        };

        let mut first_error = None;
        for entry in entries.flatten() {
            let path = entry.path();
            if !entry
                .file_type()
                .map(|file_type| file_type.is_file())
                .unwrap_or(false)
            {
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str())
                != Some(CACHE_RECORD_EXTENSION)
            {
                continue;
            }

            let filename = entry.file_name().to_string_lossy().into_owned();
            if valid_filenames.contains(&filename) {
                continue;
            }

            if let Err(error) = fs::remove_file(&path) {
                first_error.get_or_insert_with(|| {
                    cache_partition_cleanup_diagnostic(
                        format!(
                            "failed to remove orphaned compile cache partition blob `{filename}`: {error}"
                        ),
                        &path,
                    )
                });
            }
        }

        first_error
    }

    fn partition_blob_path(&self, partition_key: &str) -> PathBuf {
        self.partitions_dir
            .join(format!("{partition_key}.{CACHE_RECORD_EXTENSION}"))
    }

    fn evict_excess_records(&self) -> std::io::Result<()> {
        let records = Self::owned_cache_record_files(&self.cache_dir)?;
        Self::evict_oldest_records(records, MAX_CACHE_RECORD_FILES)
    }

    fn evict_oldest_records(
        mut records: Vec<OwnedCacheRecordFile>,
        max_records: usize,
    ) -> std::io::Result<()> {
        let excess = records.len().saturating_sub(max_records);
        if excess == 0 {
            return Ok(());
        }

        records.sort_by(|left, right| {
            left.modified
                .cmp(&right.modified)
                .then_with(|| left.path.cmp(&right.path))
        });

        let mut first_error = None;
        for record in records.into_iter().take(excess) {
            let delete_result = if record.path.is_dir() {
                fs::remove_dir_all(&record.path)
            } else {
                fs::remove_file(&record.path)
            };
            if let Err(error) = delete_result {
                first_error.get_or_insert(error);
            }
        }

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn owned_cache_record_files(cache_dir: &Path) -> std::io::Result<Vec<OwnedCacheRecordFile>> {
        let mut records = Vec::new();
        for entry in fs::read_dir(cache_dir)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };

            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };

            let path = entry.path();
            let (owned, modified) = if file_type.is_dir() {
                let index_path = path.join(CACHE_INDEX_FILENAME);
                let bytes = match fs::read(&index_path) {
                    Ok(bytes) => bytes,
                    Err(_) => continue,
                };
                let owned = serde_json::from_slice::<CacheIndex>(&bytes)
                    .map(|index| {
                        matches!(index.version, CACHE_VERSION | PREVIOUS_SPLIT_CACHE_VERSION)
                    })
                    .unwrap_or(false);
                let modified = fs::metadata(&index_path)
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(UNIX_EPOCH);
                (owned, modified)
            } else if file_type.is_file()
                && path.extension().and_then(|extension| extension.to_str())
                    == Some(CACHE_RECORD_EXTENSION)
            {
                let bytes = match fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(_) => continue,
                };
                let owned = serde_json::from_slice::<LegacyCompileCacheRecord>(&bytes)
                    .map(|record| record.version == LEGACY_CACHE_VERSION)
                    .unwrap_or(false);
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(UNIX_EPOCH);
                (owned, modified)
            } else {
                continue;
            };

            if !owned {
                continue;
            }

            records.push(OwnedCacheRecordFile { path, modified });
        }

        Ok(records)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChangeSummary {
    changed_paths: Vec<PathBuf>,
    rebuild_paths: BTreeSet<PathBuf>,
    scope: RecompilationScope,
}

fn sanitize_cache_key(jobname: &str) -> String {
    let sanitized = jobname
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();

    if sanitized.is_empty() {
        "texput".to_string()
    } else {
        sanitized
    }
}

fn sanitize_partition_key(raw: &str) -> String {
    let sanitized = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .take(64)
        .collect::<String>();

    let stem = if sanitized.is_empty() {
        "partition"
    } else {
        sanitized.as_str()
    };
    format!("{stem}-{:016x}", fingerprint_bytes(raw.as_bytes()))
}

fn partition_blobs_for(
    cached_source_subtrees: &BTreeMap<PathBuf, CachedSourceSubtree>,
    cached_typeset_fragments: &BTreeMap<String, CachedTypesetFragment>,
    cached_page_payloads: &BTreeMap<String, Vec<CachedPagePayload>>,
) -> BTreeMap<String, PartitionBlob> {
    let mut partitions = BTreeMap::new();

    for (path, subtree) in cached_source_subtrees {
        let raw_key = format!("source:{}", path.to_string_lossy());
        partitions.insert(
            sanitize_partition_key(&raw_key),
            PartitionBlob {
                cached_source_subtrees: BTreeMap::from([(path.clone(), subtree.clone())]),
                cached_typeset_fragments: BTreeMap::new(),
                cached_page_payloads: None,
            },
        );
    }

    for (partition_id, fragment) in cached_typeset_fragments {
        let raw_key = format!("fragment:{partition_id}");
        partitions.insert(
            sanitize_partition_key(&raw_key),
            PartitionBlob {
                cached_source_subtrees: BTreeMap::new(),
                cached_typeset_fragments: BTreeMap::from([(
                    partition_id.clone(),
                    fragment.clone(),
                )]),
                cached_page_payloads: cached_page_payloads.get(partition_id).cloned(),
            },
        );
    }

    partitions
}

fn empty_lookup_result(diagnostics: Vec<Diagnostic>) -> CacheLookupResult {
    CacheLookupResult {
        artifact: None,
        baseline_state: None,
        diagnostics,
        changed_paths: Vec::new(),
        rebuild_paths: BTreeSet::new(),
        cached_dependency_graph: None,
        cached_source_subtrees: BTreeMap::new(),
        cached_typeset_fragments: BTreeMap::new(),
        cached_page_payloads: BTreeMap::new(),
        scope: None,
    }
}

fn cache_info_diagnostic(message: impl Into<String>, path: &Path) -> Diagnostic {
    Diagnostic::new(Severity::Info, message.into())
        .with_file(path.to_string_lossy().into_owned())
        .with_suggestion("Ferritex will ignore this cache entry and run a full compile")
}

fn cache_cleanup_diagnostic(message: impl Into<String>, path: &Path) -> Diagnostic {
    Diagnostic::new(Severity::Info, message.into())
        .with_file(path.to_string_lossy().into_owned())
        .with_suggestion(
            "Ferritex kept the current cache entry but could not clean up older cache records",
        )
}

fn cache_partition_cleanup_diagnostic(message: impl Into<String>, path: &Path) -> Diagnostic {
    Diagnostic::new(Severity::Info, message.into())
        .with_file(path.to_string_lossy().into_owned())
        .with_suggestion(
            "Ferritex kept the current cache entry but could not clean up stale partition blobs",
        )
}

pub fn fingerprint_bytes(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use ferritex_core::compilation::{CompilationSnapshot, DocumentState};
    use ferritex_core::diagnostics::Severity;
    use ferritex_core::incremental::RecompilationScope;
    use ferritex_core::kernel::api::{DimensionValue, SourceLocation, SourceSpan};
    use ferritex_core::pdf::{PageRenderPayload, PdfLinkAnnotation, PdfLinkTarget};
    use ferritex_core::typesetting::{
        DocumentLayoutFragment, FloatContent, FloatRegion, PageBox, PlacementSpec, TextLine,
        TypesetNamedDestination, TypesetOutline, TypesetPage,
    };

    use super::{
        fingerprint_bytes, BlockCheckpoint, BlockCheckpointData, BlockLayoutState, CacheIndex,
        CachedPagePayload, CachedSourceSubtree, CachedTypesetFragment, CompileCache,
        LegacyCompileCacheRecord, OwnedCacheRecordFile, PartitionBlob, PendingFloat,
        MAX_CACHE_RECORD_FILES,
    };
    use crate::stable_compile_state::StableCompileState;

    struct FsGate;

    impl ferritex_core::policy::FileAccessGate for FsGate {
        fn ensure_directory(
            &self,
            path: &std::path::Path,
        ) -> Result<(), ferritex_core::policy::FileAccessError> {
            fs::create_dir_all(path).map_err(Into::into)
        }

        fn check_read(&self, _path: &std::path::Path) -> ferritex_core::policy::PathAccessDecision {
            ferritex_core::policy::PathAccessDecision::Allowed
        }

        fn check_write(
            &self,
            _path: &std::path::Path,
        ) -> ferritex_core::policy::PathAccessDecision {
            ferritex_core::policy::PathAccessDecision::Allowed
        }

        fn check_readback(
            &self,
            _path: &std::path::Path,
            _primary_input: &std::path::Path,
            _jobname: &str,
        ) -> ferritex_core::policy::PathAccessDecision {
            ferritex_core::policy::PathAccessDecision::Allowed
        }

        fn read_file(
            &self,
            path: &std::path::Path,
        ) -> Result<Vec<u8>, ferritex_core::policy::FileAccessError> {
            fs::read(path).map_err(Into::into)
        }

        fn write_file(
            &self,
            path: &std::path::Path,
            content: &[u8],
        ) -> Result<(), ferritex_core::policy::FileAccessError> {
            fs::write(path, content).map_err(Into::into)
        }

        fn read_readback(
            &self,
            path: &std::path::Path,
            _primary_input: &std::path::Path,
            _jobname: &str,
        ) -> Result<Vec<u8>, ferritex_core::policy::FileAccessError> {
            fs::read(path).map_err(Into::into)
        }
    }

    struct CountingFsGate {
        read_counts: Mutex<BTreeMap<PathBuf, usize>>,
    }

    impl CountingFsGate {
        fn new() -> Self {
            Self {
                read_counts: Mutex::new(BTreeMap::new()),
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

    impl ferritex_core::policy::FileAccessGate for CountingFsGate {
        fn ensure_directory(
            &self,
            path: &std::path::Path,
        ) -> Result<(), ferritex_core::policy::FileAccessError> {
            fs::create_dir_all(path).map_err(Into::into)
        }

        fn check_read(&self, _path: &std::path::Path) -> ferritex_core::policy::PathAccessDecision {
            ferritex_core::policy::PathAccessDecision::Allowed
        }

        fn check_write(
            &self,
            _path: &std::path::Path,
        ) -> ferritex_core::policy::PathAccessDecision {
            ferritex_core::policy::PathAccessDecision::Allowed
        }

        fn check_readback(
            &self,
            _path: &std::path::Path,
            _primary_input: &std::path::Path,
            _jobname: &str,
        ) -> ferritex_core::policy::PathAccessDecision {
            ferritex_core::policy::PathAccessDecision::Allowed
        }

        fn read_file(
            &self,
            path: &std::path::Path,
        ) -> Result<Vec<u8>, ferritex_core::policy::FileAccessError> {
            *self
                .read_counts
                .lock()
                .expect("lock read counts")
                .entry(path.to_path_buf())
                .or_default() += 1;
            fs::read(path).map_err(Into::into)
        }

        fn write_file(
            &self,
            path: &std::path::Path,
            content: &[u8],
        ) -> Result<(), ferritex_core::policy::FileAccessError> {
            fs::write(path, content).map_err(Into::into)
        }

        fn read_readback(
            &self,
            path: &std::path::Path,
            _primary_input: &std::path::Path,
            _jobname: &str,
        ) -> Result<Vec<u8>, ferritex_core::policy::FileAccessError> {
            fs::read(path).map_err(Into::into)
        }
    }

    struct RecordingFsGate {
        writes: Mutex<Vec<PathBuf>>,
    }

    impl RecordingFsGate {
        fn new() -> Self {
            Self {
                writes: Mutex::new(Vec::new()),
            }
        }

        fn writes(&self) -> Vec<PathBuf> {
            self.writes.lock().expect("lock writes").clone()
        }
    }

    impl ferritex_core::policy::FileAccessGate for RecordingFsGate {
        fn ensure_directory(
            &self,
            path: &std::path::Path,
        ) -> Result<(), ferritex_core::policy::FileAccessError> {
            fs::create_dir_all(path).map_err(Into::into)
        }

        fn check_read(&self, _path: &std::path::Path) -> ferritex_core::policy::PathAccessDecision {
            ferritex_core::policy::PathAccessDecision::Allowed
        }

        fn check_write(
            &self,
            _path: &std::path::Path,
        ) -> ferritex_core::policy::PathAccessDecision {
            ferritex_core::policy::PathAccessDecision::Allowed
        }

        fn check_readback(
            &self,
            _path: &std::path::Path,
            _primary_input: &std::path::Path,
            _jobname: &str,
        ) -> ferritex_core::policy::PathAccessDecision {
            ferritex_core::policy::PathAccessDecision::Allowed
        }

        fn read_file(
            &self,
            path: &std::path::Path,
        ) -> Result<Vec<u8>, ferritex_core::policy::FileAccessError> {
            fs::read(path).map_err(Into::into)
        }

        fn write_file(
            &self,
            path: &std::path::Path,
            content: &[u8],
        ) -> Result<(), ferritex_core::policy::FileAccessError> {
            fs::write(path, content).map_err(ferritex_core::policy::FileAccessError::from)?;
            self.writes
                .lock()
                .expect("lock writes")
                .push(path.to_path_buf());
            Ok(())
        }

        fn read_readback(
            &self,
            path: &std::path::Path,
            _primary_input: &std::path::Path,
            _jobname: &str,
        ) -> Result<Vec<u8>, ferritex_core::policy::FileAccessError> {
            fs::read(path).map_err(Into::into)
        }
    }

    fn stable_state(input: &std::path::Path) -> StableCompileState {
        StableCompileState {
            snapshot: CompilationSnapshot {
                pass_number: 1,
                primary_input: input.to_path_buf(),
                jobname: "main".to_string(),
                confirmed_registers: Default::default(),
                confirmed_commands: Default::default(),
                confirmed_environments: Default::default(),
                confirmed_document_state: Default::default(),
            },
            document_state: DocumentState::default(),
            cross_reference_seed: Default::default(),
            page_count: 1,
            success: true,
            diagnostics: Vec::new(),
        }
    }

    fn test_source_span() -> SourceSpan {
        SourceSpan {
            start: SourceLocation {
                file_id: 7,
                line: 3,
                column: 1,
            },
            end: SourceLocation {
                file_id: 7,
                line: 3,
                column: 18,
            },
        }
    }

    fn test_block_checkpoint_data() -> BlockCheckpointData {
        BlockCheckpointData {
            checkpoints: vec![BlockCheckpoint {
                node_index: 4,
                source_span: Some(test_source_span()),
                layout_state: BlockLayoutState {
                    content_used: DimensionValue(12 * 65_536),
                    completed_page_count: 2,
                    pending_floats: vec![PendingFloat {
                        spec: PlacementSpec {
                            priority_order: vec![FloatRegion::Top, FloatRegion::Page],
                            force: true,
                        },
                        content: FloatContent {
                            lines: vec![TextLine {
                                text: "pending float".to_string(),
                                x: DimensionValue::zero(),
                                y: DimensionValue(0),
                                links: Vec::new(),
                                font_index: 0,
                                font_size: DimensionValue(10 * 65_536),
                                source_span: Some(test_source_span()),
                            }],
                            images: Vec::new(),
                            height: DimensionValue(18 * 65_536),
                        },
                        defer_count: 3,
                    }],
                    footnote_count: 5,
                    figure_count: 2,
                    table_count: 1,
                },
            }],
            source_hash: 99,
            partition_body: "Body paragraph.\n\nSecond paragraph.".to_string(),
        }
    }

    #[test]
    fn block_checkpoint_data_serializes_roundtrip() {
        let data = test_block_checkpoint_data();

        let serialized = serde_json::to_string(&data).expect("serialize checkpoint data");
        let restored: BlockCheckpointData =
            serde_json::from_str(&serialized).expect("deserialize checkpoint data");

        assert_eq!(restored, data);
    }

    #[test]
    fn block_checkpoint_data_without_partition_body_defaults_empty() {
        let payload = serde_json::json!({
            "checkpoints": [],
            "source_hash": 77_u64,
        });

        let restored: BlockCheckpointData =
            serde_json::from_value(payload).expect("deserialize legacy checkpoint data");

        assert!(restored.checkpoints.is_empty());
        assert_eq!(restored.source_hash, 77);
        assert!(restored.partition_body.is_empty());
    }

    #[test]
    fn cached_fragment_v1_without_checkpoints_compatible() {
        let payload = serde_json::json!({
            "fragment": test_fragment("section:0001:intro"),
            "source_hash": 77_u64,
        });

        let restored: CachedTypesetFragment =
            serde_json::from_value(payload).expect("deserialize cached typeset fragment");

        assert_eq!(restored.fragment, test_fragment("section:0001:intro"));
        assert_eq!(restored.source_hash, 77);
        assert_eq!(restored.block_checkpoints, None);
    }

    #[test]
    fn detects_local_region_change_for_single_dependency() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let input = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(&input, "before").expect("write input");
        let pdf_path = output_dir.join("main.pdf");
        fs::write(&pdf_path, b"%PDF-1.4\n").expect("write pdf");

        let mut graph = ferritex_core::incremental::DependencyGraph::default();
        graph.record_node(input.clone(), fingerprint_bytes(b"before"));

        let cache = CompileCache::new(&FsGate, &output_dir, &input, "main");
        cache
            .store(
                &graph,
                &stable_state(&input),
                fingerprint_bytes(b"%PDF-1.4\n"),
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .expect_none("cache stored");

        fs::write(&input, "after").expect("update input");

        let lookup = cache.lookup(&[]);

        assert!(lookup.artifact.is_none());
        assert_eq!(lookup.baseline_state, Some(stable_state(&input)));
        assert_eq!(lookup.changed_paths, vec![input]);
        assert_eq!(
            lookup.rebuild_paths,
            [dir.path().join("main.tex")]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        );
        assert_eq!(lookup.scope, Some(RecompilationScope::LocalRegion));
    }

    #[test]
    fn reuses_cache_when_dependencies_and_pdf_match() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let input = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(&input, "stable").expect("write input");
        let pdf_bytes = b"%PDF-1.4\ncached\n";
        fs::write(output_dir.join("main.pdf"), pdf_bytes).expect("write pdf");

        let mut graph = ferritex_core::incremental::DependencyGraph::default();
        graph.record_node(input.clone(), fingerprint_bytes(b"stable"));

        let expected_state = stable_state(&input);
        let cache = CompileCache::new(&FsGate, &output_dir, &input, "main");
        cache
            .store(
                &graph,
                &expected_state,
                fingerprint_bytes(pdf_bytes),
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .expect_none("cache stored");

        let lookup = cache.lookup(&[]);

        assert!(lookup.diagnostics.is_empty());
        assert_eq!(lookup.baseline_state, Some(expected_state.clone()));
        assert_eq!(
            lookup
                .artifact
                .expect("cached artifact")
                .stable_compile_state,
            expected_state
        );
    }

    #[test]
    fn rebuild_paths_include_transitive_parents_only() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let input = dir.path().join("main.tex");
        let chapter = dir.path().join("chapter.tex");
        let appendix = dir.path().join("appendix.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(&input, "\\input{chapter}\\input{appendix}").expect("write input");
        fs::write(&chapter, "before").expect("write chapter");
        fs::write(&appendix, "stable").expect("write appendix");
        let pdf_bytes = b"%PDF-1.4\ncached\n";
        fs::write(output_dir.join("main.pdf"), pdf_bytes).expect("write pdf");

        let mut graph = ferritex_core::incremental::DependencyGraph::default();
        graph.record_node(
            input.clone(),
            fingerprint_bytes(b"\\input{chapter}\\input{appendix}"),
        );
        graph.record_node(chapter.clone(), fingerprint_bytes(b"before"));
        graph.record_node(appendix.clone(), fingerprint_bytes(b"stable"));
        graph.record_edge(input.clone(), chapter.clone());
        graph.record_edge(input.clone(), appendix.clone());

        let cache = CompileCache::new(&FsGate, &output_dir, &input, "main");
        cache
            .store(
                &graph,
                &stable_state(&input),
                fingerprint_bytes(pdf_bytes),
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .expect_none("cache stored");

        fs::write(&chapter, "after").expect("update chapter");

        let lookup = cache.lookup(&[]);

        assert_eq!(lookup.changed_paths, vec![chapter.clone()]);
        assert_eq!(
            lookup.rebuild_paths,
            [input, chapter]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        );
        assert_eq!(lookup.scope, Some(RecompilationScope::LocalRegion));
    }

    #[test]
    fn lookup_promotes_local_region_to_block_level_when_partition_bodies_are_cached() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let input = dir.path().join("main.tex");
        let chapter = dir.path().join("chapter.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(&input, "\\input{chapter}").expect("write input");
        fs::write(&chapter, "before").expect("write chapter");
        let pdf_bytes = b"%PDF-1.4\ncached\n";
        fs::write(output_dir.join("main.pdf"), pdf_bytes).expect("write pdf");

        let mut graph = ferritex_core::incremental::DependencyGraph::default();
        graph.record_node(input.clone(), fingerprint_bytes(b"\\input{chapter}"));
        graph.record_node(chapter.clone(), fingerprint_bytes(b"before"));
        graph.record_edge(input.clone(), chapter.clone());

        let cached_typeset_fragments = BTreeMap::from([(
            "chapter:0001:intro".to_string(),
            CachedTypesetFragment {
                fragment: test_fragment("chapter:0001:intro"),
                source_hash: 42,
                block_checkpoints: Some(test_block_checkpoint_data()),
            },
        )]);

        let cache = CompileCache::new(&FsGate, &output_dir, &input, "main");
        cache
            .store(
                &graph,
                &stable_state(&input),
                fingerprint_bytes(pdf_bytes),
                &BTreeMap::new(),
                &cached_typeset_fragments,
            )
            .expect_none("cache stored");

        fs::write(&chapter, "after").expect("update chapter");

        let lookup = cache.lookup(&[]);

        assert_eq!(lookup.changed_paths, vec![chapter.clone()]);
        assert_eq!(
            lookup.scope,
            Some(RecompilationScope::BlockLevel {
                affected_partitions: Vec::new(),
                references_affected: false,
                pagination_affected: false,
            })
        );
    }

    #[test]
    fn stores_and_restores_cached_typeset_fragments() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let input = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(&input, "stable").expect("write input");
        let pdf_bytes = b"%PDF-1.4\ncached\n";
        fs::write(output_dir.join("main.pdf"), pdf_bytes).expect("write pdf");

        let mut graph = ferritex_core::incremental::DependencyGraph::default();
        graph.record_node(input.clone(), fingerprint_bytes(b"stable"));
        let cached_typeset_fragments = BTreeMap::from([(
            "document:0000:main".to_string(),
            CachedTypesetFragment {
                fragment: test_fragment("document:0000:main"),
                source_hash: 42,
                block_checkpoints: None,
            },
        )]);

        let cache = CompileCache::new(&FsGate, &output_dir, &input, "main");
        cache
            .store(
                &graph,
                &stable_state(&input),
                fingerprint_bytes(pdf_bytes),
                &BTreeMap::new(),
                &cached_typeset_fragments,
            )
            .expect_none("cache stored");

        let lookup = cache.lookup(&[]);

        assert_eq!(lookup.cached_typeset_fragments, cached_typeset_fragments);
    }

    #[test]
    fn stores_and_restores_cached_page_payloads() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let input = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(&input, "stable").expect("write input");
        let pdf_bytes = b"%PDF-1.4\ncached\n";
        fs::write(output_dir.join("main.pdf"), pdf_bytes).expect("write pdf");

        let mut graph = ferritex_core::incremental::DependencyGraph::default();
        graph.record_node(input.clone(), fingerprint_bytes(b"stable"));
        let cached_typeset_fragments = BTreeMap::from([(
            "document:0000:main".to_string(),
            CachedTypesetFragment {
                fragment: test_fragment("document:0000:main"),
                source_hash: 42,
                block_checkpoints: None,
            },
        )]);
        let cached_page_payloads = BTreeMap::from([(
            "document:0000:main".to_string(),
            vec![test_cached_page_payload("cached page 1")],
        )]);

        let cache = CompileCache::new(&FsGate, &output_dir, &input, "main");
        cache
            .store_with_page_payloads(
                &graph,
                &stable_state(&input),
                fingerprint_bytes(pdf_bytes),
                &BTreeMap::new(),
                &cached_typeset_fragments,
                &cached_page_payloads,
            )
            .expect_none("cache stored");

        let lookup = cache.lookup(&[]);

        assert_eq!(lookup.cached_page_payloads, cached_page_payloads);
    }

    #[test]
    fn split_cache_v6_without_page_payloads_is_compatible() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let input = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(&input, "stable").expect("write input");
        let pdf_bytes = b"%PDF-1.4\ncached\n";
        fs::write(output_dir.join("main.pdf"), pdf_bytes).expect("write pdf");
        let cache = CompileCache::new(&FsGate, &output_dir, &input, "main");
        let record_dir = cache.record_dir.clone();
        fs::create_dir_all(record_dir.join(super::CACHE_PARTITIONS_DIR_NAME))
            .expect("create record dir");

        let cached_typeset_fragments = BTreeMap::from([(
            "document:0000:main".to_string(),
            CachedTypesetFragment {
                fragment: test_fragment("document:0000:main"),
                source_hash: 42,
                block_checkpoints: None,
            },
        )]);
        let partition_key = super::sanitize_partition_key("fragment:document:0000:main");
        let index = CacheIndex {
            version: super::PREVIOUS_SPLIT_CACHE_VERSION,
            primary_input: input.clone(),
            jobname: "main".to_string(),
            output_pdf: output_dir.join("main.pdf"),
            output_pdf_hash: fingerprint_bytes(pdf_bytes),
            dependency_graph: dependency_graph_for(&input, "stable"),
            stable_compile_state: stable_state(&input),
            partition_keys: vec![partition_key.clone()],
            partition_hashes: BTreeMap::new(),
        };
        fs::write(
            record_dir.join(super::CACHE_INDEX_FILENAME),
            serde_json::to_vec_pretty(&index).expect("serialize v6 index"),
        )
        .expect("write v6 index");
        let blob = serde_json::json!({
            "cached_source_subtrees": {},
            "cached_typeset_fragments": {
                "document:0000:main": {
                    "fragment": test_fragment("document:0000:main"),
                    "source_hash": 42_u64,
                    "block_checkpoints": null
                }
            }
        });
        fs::write(
            record_dir
                .join(super::CACHE_PARTITIONS_DIR_NAME)
                .join(format!("{partition_key}.json")),
            serde_json::to_vec_pretty(&blob).expect("serialize v6 blob"),
        )
        .expect("write v6 blob");

        let lookup = cache.lookup(&[]);

        assert_eq!(lookup.cached_typeset_fragments, cached_typeset_fragments);
        assert!(lookup.cached_page_payloads.is_empty());
    }

    #[test]
    fn split_cache_round_trip_persists_index_and_partition_blobs() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let input = dir.path().join("main.tex");
        let chapter = dir.path().join("chapter.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(&input, "\\input{chapter}").expect("write input");
        fs::write(&chapter, "chapter body").expect("write chapter");
        let pdf_bytes = b"%PDF-1.4\ncached\n";
        fs::write(output_dir.join("main.pdf"), pdf_bytes).expect("write pdf");

        let mut graph = ferritex_core::incremental::DependencyGraph::default();
        graph.record_node(input.clone(), fingerprint_bytes(b"\\input{chapter}"));
        graph.record_node(chapter.clone(), fingerprint_bytes(b"chapter body"));
        graph.record_edge(input.clone(), chapter.clone());

        let cached_source_subtrees = BTreeMap::from([
            (
                input.clone(),
                test_cached_subtree(&input, "\\input{chapter}"),
            ),
            (
                chapter.clone(),
                test_cached_subtree(&chapter, "chapter body"),
            ),
        ]);
        let cached_typeset_fragments = BTreeMap::from([
            (
                "document:0000:main".to_string(),
                CachedTypesetFragment {
                    fragment: test_fragment("document:0000:main"),
                    source_hash: 11,
                    block_checkpoints: None,
                },
            ),
            (
                "chapter:0001:intro".to_string(),
                CachedTypesetFragment {
                    fragment: test_fragment("chapter:0001:intro"),
                    source_hash: 22,
                    block_checkpoints: None,
                },
            ),
        ]);

        let cache = CompileCache::new(&FsGate, &output_dir, &input, "main");
        cache
            .store(
                &graph,
                &stable_state(&input),
                fingerprint_bytes(pdf_bytes),
                &cached_source_subtrees,
                &cached_typeset_fragments,
            )
            .expect_none("cache stored");

        assert!(
            cache.record_dir.exists(),
            "split cache directory should exist"
        );
        assert!(cache.metadata_path.exists(), "cache index should exist");
        assert!(
            cache.partitions_dir.exists(),
            "partition blob directory should exist"
        );

        let index: CacheIndex =
            serde_json::from_slice(&fs::read(&cache.metadata_path).expect("read cache index"))
                .expect("deserialize cache index");
        assert_eq!(index.version, super::CACHE_VERSION);
        assert_eq!(
            index.partition_keys.len(),
            cached_source_subtrees.len() + cached_typeset_fragments.len()
        );
        for partition_key in &index.partition_keys {
            assert!(
                cache.partition_blob_path(partition_key).exists(),
                "partition blob should exist for {partition_key}"
            );
        }

        let lookup = cache.lookup(&[]);

        assert!(lookup.diagnostics.is_empty());
        assert_eq!(lookup.cached_source_subtrees, cached_source_subtrees);
        assert_eq!(lookup.cached_typeset_fragments, cached_typeset_fragments);
    }

    #[test]
    fn store_writes_partition_blobs_before_index_commit_point() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let input = dir.path().join("main.tex");
        let chapter = dir.path().join("chapter.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(&input, "\\input{chapter}").expect("write input");
        fs::write(&chapter, "chapter body").expect("write chapter");
        let pdf_bytes = b"%PDF-1.4\ncached\n";
        fs::write(output_dir.join("main.pdf"), pdf_bytes).expect("write pdf");

        let mut graph = ferritex_core::incremental::DependencyGraph::default();
        graph.record_node(input.clone(), fingerprint_bytes(b"\\input{chapter}"));
        graph.record_node(chapter.clone(), fingerprint_bytes(b"chapter body"));
        graph.record_edge(input.clone(), chapter.clone());

        let cached_source_subtrees = BTreeMap::from([
            (
                input.clone(),
                test_cached_subtree(&input, "\\input{chapter}"),
            ),
            (
                chapter.clone(),
                test_cached_subtree(&chapter, "chapter body"),
            ),
        ]);
        let cached_typeset_fragments = BTreeMap::from([(
            "document:0000:main".to_string(),
            CachedTypesetFragment {
                fragment: test_fragment("document:0000:main"),
                source_hash: 42,
                block_checkpoints: None,
            },
        )]);

        let gate = RecordingFsGate::new();
        let cache = CompileCache::new(&gate, &output_dir, &input, "main");
        cache
            .store(
                &graph,
                &stable_state(&input),
                fingerprint_bytes(pdf_bytes),
                &cached_source_subtrees,
                &cached_typeset_fragments,
            )
            .expect_none("cache stored");

        let writes = gate.writes();
        assert_eq!(writes.last(), Some(&cache.metadata_path));
        let index_position = writes
            .iter()
            .position(|path| *path == cache.metadata_path)
            .expect("index write recorded");
        assert_eq!(index_position, writes.len() - 1);
        assert!(writes[..index_position]
            .iter()
            .all(|path| path.starts_with(&cache.partitions_dir)));
        assert_eq!(
            writes
                .iter()
                .filter(|path| path.starts_with(&cache.partitions_dir))
                .count(),
            cached_source_subtrees.len() + cached_typeset_fragments.len()
        );
    }

    #[test]
    fn delta_write_skips_unchanged_partition_blobs() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let input = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(&input, "stable").expect("write input");
        let pdf_bytes = b"%PDF-1.4\ncached\n";
        fs::write(output_dir.join("main.pdf"), pdf_bytes).expect("write pdf");

        let cached_typeset_fragments = BTreeMap::from([
            (
                "document:0000:main".to_string(),
                CachedTypesetFragment {
                    fragment: test_fragment("document:0000:main"),
                    source_hash: 11,
                    block_checkpoints: None,
                },
            ),
            (
                "document:0001:appendix".to_string(),
                CachedTypesetFragment {
                    fragment: test_fragment("document:0001:appendix"),
                    source_hash: 22,
                    block_checkpoints: None,
                },
            ),
        ]);

        let gate = RecordingFsGate::new();
        let cache = CompileCache::new(&gate, &output_dir, &input, "main");
        let graph = dependency_graph_for(&input, "stable");
        let state = stable_state(&input);

        cache
            .store(
                &graph,
                &state,
                fingerprint_bytes(pdf_bytes),
                &BTreeMap::new(),
                &cached_typeset_fragments,
            )
            .expect_none("initial cache stored");

        let initial_writes = gate.writes();
        let initial_partition_writes = initial_writes
            .iter()
            .filter(|path| path.starts_with(&cache.partitions_dir))
            .count();
        assert_eq!(initial_partition_writes, cached_typeset_fragments.len());

        cache
            .store(
                &graph,
                &state,
                fingerprint_bytes(pdf_bytes),
                &BTreeMap::new(),
                &cached_typeset_fragments,
            )
            .expect_none("unchanged cache stored");

        let writes_after_second_store = gate.writes();
        let second_store_writes = &writes_after_second_store[initial_writes.len()..];
        assert_eq!(second_store_writes, &[cache.metadata_path.clone()]);

        let updated_fragments = BTreeMap::from([
            (
                "document:0000:main".to_string(),
                CachedTypesetFragment {
                    fragment: test_fragment("document:0000:main"),
                    source_hash: 99,
                    block_checkpoints: None,
                },
            ),
            (
                "document:0001:appendix".to_string(),
                CachedTypesetFragment {
                    fragment: test_fragment("document:0001:appendix"),
                    source_hash: 22,
                    block_checkpoints: None,
                },
            ),
        ]);
        let changed_partition = cache.partition_blob_path(&super::sanitize_partition_key(
            "fragment:document:0000:main",
        ));

        cache
            .store(
                &graph,
                &state,
                fingerprint_bytes(pdf_bytes),
                &BTreeMap::new(),
                &updated_fragments,
            )
            .expect_none("updated cache stored");

        let writes_after_third_store = gate.writes();
        let third_store_writes = &writes_after_third_store[writes_after_second_store.len()..];
        assert_eq!(
            third_store_writes,
            &[changed_partition, cache.metadata_path.clone()]
        );
    }

    #[test]
    fn store_removes_orphaned_partition_blobs_after_index_commit() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let input = dir.path().join("main.tex");
        let chapter = dir.path().join("chapter.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(&input, "\\input{chapter}").expect("write input");
        fs::write(&chapter, "chapter body").expect("write chapter");
        let pdf_bytes = b"%PDF-1.4\ncached\n";
        fs::write(output_dir.join("main.pdf"), pdf_bytes).expect("write pdf");

        let mut graph = ferritex_core::incremental::DependencyGraph::default();
        graph.record_node(input.clone(), fingerprint_bytes(b"\\input{chapter}"));
        graph.record_node(chapter.clone(), fingerprint_bytes(b"chapter body"));
        graph.record_edge(input.clone(), chapter.clone());

        let initial_subtrees = BTreeMap::from([
            (
                input.clone(),
                test_cached_subtree(&input, "\\input{chapter}"),
            ),
            (
                chapter.clone(),
                test_cached_subtree(&chapter, "chapter body"),
            ),
        ]);
        let fragments = BTreeMap::from([(
            "document:0000:main".to_string(),
            CachedTypesetFragment {
                fragment: test_fragment("document:0000:main"),
                source_hash: 11,
                block_checkpoints: None,
            },
        )]);

        let cache = CompileCache::new(&FsGate, &output_dir, &input, "main");
        cache
            .store(
                &graph,
                &stable_state(&input),
                fingerprint_bytes(pdf_bytes),
                &initial_subtrees,
                &fragments,
            )
            .expect_none("initial cache stored");

        let initial_index: CacheIndex =
            serde_json::from_slice(&fs::read(&cache.metadata_path).expect("read initial index"))
                .expect("deserialize initial index");
        let initial_keys = initial_index
            .partition_keys
            .into_iter()
            .collect::<BTreeSet<_>>();

        let updated_subtrees = BTreeMap::from([(
            input.clone(),
            test_cached_subtree(&input, "\\input{chapter}"),
        )]);
        cache
            .store(
                &graph,
                &stable_state(&input),
                fingerprint_bytes(pdf_bytes),
                &updated_subtrees,
                &fragments,
            )
            .expect_none("updated cache stored");

        let updated_index: CacheIndex =
            serde_json::from_slice(&fs::read(&cache.metadata_path).expect("read updated index"))
                .expect("deserialize updated index");
        let updated_keys = updated_index
            .partition_keys
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let orphaned_keys = initial_keys
            .difference(&updated_keys)
            .cloned()
            .collect::<Vec<_>>();

        assert!(
            !orphaned_keys.is_empty(),
            "expected at least one partition blob to become orphaned"
        );
        for partition_key in orphaned_keys {
            assert!(
                !cache.partition_blob_path(&partition_key).exists(),
                "orphaned partition blob should be removed: {partition_key}"
            );
        }

        let partition_blob_count = fs::read_dir(&cache.partitions_dir)
            .expect("read partition dir")
            .flatten()
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    == Some(super::CACHE_RECORD_EXTENSION)
            })
            .count();
        assert_eq!(partition_blob_count, updated_keys.len());
    }

    #[test]
    fn lookup_reads_legacy_v4_cache_record_as_fallback() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let input = dir.path().join("main.tex");
        let chapter = dir.path().join("chapter.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(&input, "\\input{chapter}").expect("write input");
        fs::write(&chapter, "chapter body").expect("write chapter");
        let pdf_bytes = b"%PDF-1.4\ncached\n";
        fs::write(output_dir.join("main.pdf"), pdf_bytes).expect("write pdf");

        let mut graph = ferritex_core::incremental::DependencyGraph::default();
        graph.record_node(input.clone(), fingerprint_bytes(b"\\input{chapter}"));
        graph.record_node(chapter.clone(), fingerprint_bytes(b"chapter body"));
        graph.record_edge(input.clone(), chapter.clone());

        let cached_source_subtrees = BTreeMap::from([(
            chapter.clone(),
            test_cached_subtree(&chapter, "chapter body"),
        )]);
        let cached_typeset_fragments = BTreeMap::from([(
            "chapter:0001:intro".to_string(),
            CachedTypesetFragment {
                fragment: test_fragment("chapter:0001:intro"),
                source_hash: 22,
                block_checkpoints: None,
            },
        )]);

        let cache = CompileCache::new(&FsGate, &output_dir, &input, "main");
        write_legacy_cache_record(
            &cache.legacy_metadata_path,
            LegacyCompileCacheRecord {
                version: super::LEGACY_CACHE_VERSION,
                primary_input: input.clone(),
                jobname: "main".to_string(),
                output_pdf: output_dir.join("main.pdf"),
                output_pdf_hash: fingerprint_bytes(pdf_bytes),
                dependency_graph: graph,
                stable_compile_state: stable_state(&input),
                cached_source_subtrees: cached_source_subtrees.clone(),
                cached_typeset_fragments: cached_typeset_fragments.clone(),
            },
        );

        let lookup = cache.lookup(&[]);

        assert!(lookup.diagnostics.is_empty());
        assert!(lookup.artifact.is_some());
        assert_eq!(lookup.cached_source_subtrees, cached_source_subtrees);
        assert_eq!(lookup.cached_typeset_fragments, cached_typeset_fragments);
    }

    #[test]
    fn corrupted_partition_blob_only_drops_that_partition() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let input = dir.path().join("main.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(&input, "stable").expect("write input");
        let pdf_bytes = b"%PDF-1.4\ncached\n";
        fs::write(output_dir.join("main.pdf"), pdf_bytes).expect("write pdf");

        let mut graph = ferritex_core::incremental::DependencyGraph::default();
        graph.record_node(input.clone(), fingerprint_bytes(b"stable"));
        let cached_typeset_fragments = BTreeMap::from([
            (
                "document:0000:one".to_string(),
                CachedTypesetFragment {
                    fragment: test_fragment("document:0000:one"),
                    source_hash: 1,
                    block_checkpoints: None,
                },
            ),
            (
                "document:0001:two".to_string(),
                CachedTypesetFragment {
                    fragment: test_fragment("document:0001:two"),
                    source_hash: 2,
                    block_checkpoints: None,
                },
            ),
        ]);

        let cache = CompileCache::new(&FsGate, &output_dir, &input, "main");
        cache
            .store(
                &graph,
                &stable_state(&input),
                fingerprint_bytes(pdf_bytes),
                &BTreeMap::new(),
                &cached_typeset_fragments,
            )
            .expect_none("cache stored");

        let corrupt_partition_key = super::sanitize_partition_key("fragment:document:0000:one");
        fs::write(
            cache.partition_blob_path(&corrupt_partition_key),
            b"{broken",
        )
        .expect("corrupt partition blob");

        let lookup = cache.lookup(&[]);

        assert!(lookup.artifact.is_some());
        assert!(
            lookup
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("partition blob")),
            "expected partition corruption diagnostic, got {:?}",
            lookup.diagnostics
        );
        assert!(!lookup
            .cached_typeset_fragments
            .contains_key("document:0000:one"));
        assert_eq!(
            lookup
                .cached_typeset_fragments
                .get("document:0001:two")
                .expect("healthy partition retained"),
            cached_typeset_fragments
                .get("document:0001:two")
                .expect("expected fragment")
        );
    }

    #[test]
    fn fast_path_detects_change_for_hinted_path() {
        let gate = CountingFsGate::new();
        let dir = tempfile::tempdir().expect("create tempdir");
        let input = dir.path().join("main.tex");
        let chapter = dir.path().join("chapter.tex");
        let appendix = dir.path().join("appendix.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(&input, "\\input{chapter}\\input{appendix}").expect("write input");
        fs::write(&chapter, "before").expect("write chapter");
        fs::write(&appendix, "stable").expect("write appendix");
        let pdf_bytes = b"%PDF-1.4\ncached\n";
        fs::write(output_dir.join("main.pdf"), pdf_bytes).expect("write pdf");

        let mut graph = ferritex_core::incremental::DependencyGraph::default();
        graph.record_node(
            input.clone(),
            fingerprint_bytes(b"\\input{chapter}\\input{appendix}"),
        );
        graph.record_node(chapter.clone(), fingerprint_bytes(b"before"));
        graph.record_node(appendix.clone(), fingerprint_bytes(b"stable"));
        graph.record_edge(input.clone(), chapter.clone());
        graph.record_edge(input.clone(), appendix.clone());

        let cache = CompileCache::new(&gate, &output_dir, &input, "main");
        cache
            .store(
                &graph,
                &stable_state(&input),
                fingerprint_bytes(pdf_bytes),
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .expect_none("cache stored");

        fs::write(&chapter, "after").expect("update chapter");
        gate.reset();

        let lookup = cache.lookup(std::slice::from_ref(&chapter));

        assert!(lookup.artifact.is_none());
        assert_eq!(lookup.changed_paths, vec![chapter.clone()]);
        assert_eq!(
            lookup.rebuild_paths,
            [input.clone(), chapter.clone()]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        );
        assert_eq!(lookup.scope, Some(RecompilationScope::LocalRegion));
        assert_eq!(gate.read_count(&input), 0);
        assert_eq!(gate.read_count(&chapter), 1);
        assert_eq!(gate.read_count(&appendix), 0);
    }

    #[test]
    fn fast_path_with_empty_hint_falls_back_to_full_scan() {
        let gate = CountingFsGate::new();
        let dir = tempfile::tempdir().expect("create tempdir");
        let input = dir.path().join("main.tex");
        let chapter = dir.path().join("chapter.tex");
        let appendix = dir.path().join("appendix.tex");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(&input, "\\input{chapter}\\input{appendix}").expect("write input");
        fs::write(&chapter, "before").expect("write chapter");
        fs::write(&appendix, "stable").expect("write appendix");
        let pdf_bytes = b"%PDF-1.4\ncached\n";
        fs::write(output_dir.join("main.pdf"), pdf_bytes).expect("write pdf");

        let mut graph = ferritex_core::incremental::DependencyGraph::default();
        graph.record_node(
            input.clone(),
            fingerprint_bytes(b"\\input{chapter}\\input{appendix}"),
        );
        graph.record_node(chapter.clone(), fingerprint_bytes(b"before"));
        graph.record_node(appendix.clone(), fingerprint_bytes(b"stable"));
        graph.record_edge(input.clone(), chapter.clone());
        graph.record_edge(input.clone(), appendix.clone());

        let cache = CompileCache::new(&gate, &output_dir, &input, "main");
        cache
            .store(
                &graph,
                &stable_state(&input),
                fingerprint_bytes(pdf_bytes),
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .expect_none("cache stored");

        fs::write(&chapter, "after").expect("update chapter");
        gate.reset();

        let lookup = cache.lookup(&[]);

        assert!(lookup.artifact.is_none());
        assert_eq!(lookup.changed_paths, vec![chapter.clone()]);
        assert_eq!(
            lookup.rebuild_paths,
            [input.clone(), chapter.clone()]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        );
        assert_eq!(lookup.scope, Some(RecompilationScope::LocalRegion));
        assert_eq!(gate.read_count(&input), 1);
        assert_eq!(gate.read_count(&chapter), 1);
        assert_eq!(gate.read_count(&appendix), 1);
    }

    #[test]
    fn fast_path_ignores_hint_paths_not_in_dependency_graph() {
        let gate = CountingFsGate::new();
        let dir = tempfile::tempdir().expect("create tempdir");
        let input = dir.path().join("main.tex");
        let chapter = dir.path().join("chapter.tex");
        let appendix = dir.path().join("appendix.tex");
        let unrelated = dir.path().join("notes.txt");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        fs::write(&input, "\\input{chapter}\\input{appendix}").expect("write input");
        fs::write(&chapter, "before").expect("write chapter");
        fs::write(&appendix, "stable").expect("write appendix");
        fs::write(&unrelated, "updated").expect("write unrelated");
        let pdf_bytes = b"%PDF-1.4\ncached\n";
        fs::write(output_dir.join("main.pdf"), pdf_bytes).expect("write pdf");

        let mut graph = ferritex_core::incremental::DependencyGraph::default();
        graph.record_node(
            input.clone(),
            fingerprint_bytes(b"\\input{chapter}\\input{appendix}"),
        );
        graph.record_node(chapter.clone(), fingerprint_bytes(b"before"));
        graph.record_node(appendix.clone(), fingerprint_bytes(b"stable"));
        graph.record_edge(input.clone(), chapter.clone());
        graph.record_edge(input.clone(), appendix.clone());

        let expected_state = stable_state(&input);
        let cache = CompileCache::new(&gate, &output_dir, &input, "main");
        cache
            .store(
                &graph,
                &expected_state,
                fingerprint_bytes(pdf_bytes),
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .expect_none("cache stored");

        gate.reset();
        let lookup = cache.lookup(std::slice::from_ref(&unrelated));

        assert!(lookup.changed_paths.is_empty());
        assert!(lookup.rebuild_paths.is_empty());
        assert_eq!(lookup.scope, None);
        assert_eq!(
            lookup
                .artifact
                .expect("cached artifact")
                .stable_compile_state,
            expected_state
        );
        assert_eq!(gate.read_count(&input), 0);
        assert_eq!(gate.read_count(&chapter), 0);
        assert_eq!(gate.read_count(&appendix), 0);
        assert_eq!(gate.read_count(&unrelated), 0);
    }

    #[test]
    fn evicts_oldest_owned_cache_records_and_keeps_newer_entries() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        let cache_dir = output_dir.join(super::CACHE_DIR_NAME);
        let base_time = UNIX_EPOCH + Duration::from_secs(10_000);
        let mut record_paths = Vec::new();

        fs::create_dir_all(&cache_dir).expect("create cache dir");
        fs::write(cache_dir.join("notes.txt"), "keep me").expect("write unrelated text file");
        fs::write(cache_dir.join("foreign.json"), br#"{"kind":"foreign"}"#)
            .expect("write unrelated json file");

        for index in 0..=MAX_CACHE_RECORD_FILES {
            let input = dir.path().join(format!("input-{index}.tex"));
            fs::write(&input, format!("content-{index}")).expect("write input");
            let cache = CompileCache::new(&FsGate, &output_dir, &input, &format!("job-{index:03}"));
            cache
                .store(
                    &dependency_graph_for(&input, &format!("content-{index}")),
                    &stable_state(&input),
                    fingerprint_bytes(format!("pdf-{index}").as_bytes()),
                    &BTreeMap::new(),
                    &BTreeMap::new(),
                )
                .expect_none("cache stored");
            record_paths.push(cache.record_dir.clone());

            if index < MAX_CACHE_RECORD_FILES {
                set_modified_time(
                    &cache.metadata_path,
                    base_time + Duration::from_secs(index as u64),
                );
            }
        }

        assert!(
            !record_paths[0].exists(),
            "oldest cache record should be evicted"
        );
        for path in record_paths.iter().skip(1) {
            assert!(path.exists(), "newer cache records should be retained");
        }
        assert!(
            cache_dir.join("notes.txt").exists(),
            "unrelated text file must remain"
        );
        assert!(
            cache_dir.join("foreign.json").exists(),
            "unrelated json file must remain"
        );
        assert_eq!(
            CompileCache::owned_cache_record_files(&cache_dir)
                .expect("owned cache record listing")
                .len(),
            MAX_CACHE_RECORD_FILES
        );
    }

    #[test]
    fn eviction_continues_after_individual_delete_failure() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let missing_path = dir.path().join("missing.json");
        let retained_a = dir.path().join("retained-a.json");
        let retained_b = dir.path().join("retained-b.json");
        fs::write(&retained_a, "{}").expect("write record a");
        fs::write(&retained_b, "{}").expect("write record b");

        let error = CompileCache::evict_oldest_records(
            vec![
                OwnedCacheRecordFile {
                    path: missing_path,
                    modified: UNIX_EPOCH + Duration::from_secs(1),
                },
                OwnedCacheRecordFile {
                    path: retained_a.clone(),
                    modified: UNIX_EPOCH + Duration::from_secs(2),
                },
                OwnedCacheRecordFile {
                    path: retained_b.clone(),
                    modified: UNIX_EPOCH + Duration::from_secs(3),
                },
            ],
            0,
        )
        .expect_err("missing record should surface a delete error");

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(
            !retained_a.exists(),
            "later eviction candidates should still be attempted"
        );
        assert!(
            !retained_b.exists(),
            "all excess candidates should still be attempted"
        );
    }

    #[cfg(unix)]
    #[test]
    fn owned_cache_record_listing_skips_unreadable_entries() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("create tempdir");
        let cache_dir = dir.path().join("cache");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&cache_dir).expect("create cache dir");
        fs::create_dir_all(&output_dir).expect("create output dir");

        let valid_a_input = dir.path().join("valid-a.tex");
        let unreadable_input = dir.path().join("unreadable.tex");
        let valid_b_input = dir.path().join("valid-b.tex");
        fs::write(&valid_a_input, "valid-a").expect("write valid input a");
        fs::write(&unreadable_input, "unreadable").expect("write unreadable input");
        fs::write(&valid_b_input, "valid-b").expect("write valid input b");

        let valid_a_path = cache_dir.join("valid-a");
        let unreadable_path = cache_dir.join("unreadable");
        let valid_b_path = cache_dir.join("valid-b");

        write_owned_cache_record_dir(
            &valid_a_path,
            CacheIndex {
                version: super::CACHE_VERSION,
                primary_input: valid_a_input.clone(),
                jobname: "valid-a".to_string(),
                output_pdf: output_dir.join("valid-a.pdf"),
                output_pdf_hash: fingerprint_bytes(b"valid-a-pdf"),
                dependency_graph: dependency_graph_for(&valid_a_input, "valid-a"),
                stable_compile_state: stable_state(&valid_a_input),
                partition_keys: Vec::new(),
                partition_hashes: BTreeMap::new(),
            },
            BTreeMap::new(),
        );
        write_owned_cache_record_dir(
            &unreadable_path,
            CacheIndex {
                version: super::CACHE_VERSION,
                primary_input: unreadable_input.clone(),
                jobname: "unreadable".to_string(),
                output_pdf: output_dir.join("unreadable.pdf"),
                output_pdf_hash: fingerprint_bytes(b"unreadable-pdf"),
                dependency_graph: dependency_graph_for(&unreadable_input, "unreadable"),
                stable_compile_state: stable_state(&unreadable_input),
                partition_keys: Vec::new(),
                partition_hashes: BTreeMap::new(),
            },
            BTreeMap::new(),
        );
        write_owned_cache_record_dir(
            &valid_b_path,
            CacheIndex {
                version: super::CACHE_VERSION,
                primary_input: valid_b_input.clone(),
                jobname: "valid-b".to_string(),
                output_pdf: output_dir.join("valid-b.pdf"),
                output_pdf_hash: fingerprint_bytes(b"valid-b-pdf"),
                dependency_graph: dependency_graph_for(&valid_b_input, "valid-b"),
                stable_compile_state: stable_state(&valid_b_input),
                partition_keys: Vec::new(),
                partition_hashes: BTreeMap::new(),
            },
            BTreeMap::new(),
        );

        let unreadable_index_path = unreadable_path.join(super::CACHE_INDEX_FILENAME);
        let mut permissions = fs::metadata(&unreadable_index_path)
            .expect("unreadable record metadata")
            .permissions();
        let original_mode = permissions.mode();
        permissions.set_mode(0o000);
        fs::set_permissions(&unreadable_index_path, permissions).expect("set unreadable record");

        let records =
            CompileCache::owned_cache_record_files(&cache_dir).expect("owned cache record listing");

        let mut restored = fs::metadata(&unreadable_index_path)
            .expect("restore unreadable record metadata")
            .permissions();
        restored.set_mode(original_mode);
        fs::set_permissions(&unreadable_index_path, restored).expect("restore unreadable record");

        let listed_paths = records
            .into_iter()
            .map(|record| record.path)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(listed_paths.len(), 2);
        assert!(listed_paths.contains(&valid_a_path));
        assert!(listed_paths.contains(&valid_b_path));
        assert!(!listed_paths.contains(&unreadable_path));
    }

    #[cfg(unix)]
    #[test]
    fn eviction_failure_is_reported_without_failing_store() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("create tempdir");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        let cache_dir = output_dir.join(super::CACHE_DIR_NAME);
        let input = dir.path().join("main.tex");
        fs::write(&input, "stable").expect("write input");
        let cache = CompileCache::new(&FsGate, &output_dir, &input, "main");

        cache
            .store(
                &dependency_graph_for(&input, "stable"),
                &stable_state(&input),
                fingerprint_bytes(b"pdf-main"),
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .expect_none("initial cache stored");

        for index in 0..MAX_CACHE_RECORD_FILES {
            let extra_input = dir.path().join(format!("extra-{index}.tex"));
            let record_path = cache_dir.join(format!("manual-{index:03}"));
            fs::write(&extra_input, format!("extra-{index}")).expect("write extra input");
            write_owned_cache_record_dir(
                &record_path,
                CacheIndex {
                    version: super::CACHE_VERSION,
                    primary_input: extra_input.clone(),
                    jobname: format!("extra-{index:03}"),
                    output_pdf: output_dir.join(format!("extra-{index:03}.pdf")),
                    output_pdf_hash: fingerprint_bytes(format!("pdf-extra-{index}").as_bytes()),
                    dependency_graph: dependency_graph_for(&extra_input, &format!("extra-{index}")),
                    stable_compile_state: stable_state(&extra_input),
                    partition_keys: Vec::new(),
                    partition_hashes: BTreeMap::new(),
                },
                BTreeMap::new(),
            );
            set_modified_time(
                &record_path.join(super::CACHE_INDEX_FILENAME),
                UNIX_EPOCH + Duration::from_secs(index as u64),
            );
        }

        let mut permissions = fs::metadata(&cache_dir)
            .expect("cache dir metadata")
            .permissions();
        let original_mode = permissions.mode();
        permissions.set_mode(0o555);
        fs::set_permissions(&cache_dir, permissions.clone()).expect("set read-only cache dir");

        let diagnostic = cache.store(
            &dependency_graph_for(&input, "stable"),
            &stable_state(&input),
            fingerprint_bytes(b"pdf-main-updated"),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );

        permissions.set_mode(original_mode);
        fs::set_permissions(&cache_dir, permissions).expect("restore cache dir permissions");

        let diagnostic = diagnostic.expect("eviction failure diagnostic");
        assert_eq!(diagnostic.severity, Severity::Info);
        assert!(
            diagnostic
                .message
                .contains("failed to evict old compile cache records"),
            "expected eviction failure diagnostic, got {:?}",
            diagnostic.message
        );
        assert!(
            cache.metadata_path.exists(),
            "current cache record should still exist"
        );
    }

    fn dependency_graph_for(
        input: &std::path::Path,
        content: &str,
    ) -> ferritex_core::incremental::DependencyGraph {
        let mut graph = ferritex_core::incremental::DependencyGraph::default();
        graph.record_node(input.to_path_buf(), fingerprint_bytes(content.as_bytes()));
        graph
    }

    fn set_modified_time(path: &std::path::Path, modified: SystemTime) {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open file for timestamp update");
        file.set_times(fs::FileTimes::new().set_modified(modified))
            .expect("set file modified time");
    }

    fn write_owned_cache_record_dir(
        path: &std::path::Path,
        index: CacheIndex,
        partition_blobs: BTreeMap<String, PartitionBlob>,
    ) {
        fs::create_dir_all(path.join(super::CACHE_PARTITIONS_DIR_NAME))
            .expect("create cache record dir");
        let bytes = serde_json::to_vec_pretty(&index).expect("serialize cache index");
        fs::write(path.join(super::CACHE_INDEX_FILENAME), bytes).expect("write cache index");
        for (partition_key, blob) in partition_blobs {
            let bytes = serde_json::to_vec_pretty(&blob).expect("serialize partition blob");
            fs::write(
                path.join(super::CACHE_PARTITIONS_DIR_NAME)
                    .join(format!("{partition_key}.json")),
                bytes,
            )
            .expect("write partition blob");
        }
    }

    fn write_legacy_cache_record(path: &std::path::Path, record: LegacyCompileCacheRecord) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create legacy cache dir");
        }
        let bytes = serde_json::to_vec_pretty(&record).expect("serialize legacy cache record");
        fs::write(path, bytes).expect("write cache record");
    }

    fn test_cached_subtree(path: &Path, text: &str) -> CachedSourceSubtree {
        CachedSourceSubtree {
            text: text.to_string(),
            source_lines: Vec::new(),
            source_files: vec![path.to_path_buf()],
            labels: BTreeMap::new(),
            citations: BTreeMap::new(),
        }
    }

    fn test_fragment(partition_id: &str) -> DocumentLayoutFragment {
        DocumentLayoutFragment {
            partition_id: partition_id.to_string(),
            pages: vec![TypesetPage {
                lines: vec![TextLine {
                    text: "cached".to_string(),
                    x: DimensionValue::zero(),
                    y: DimensionValue(0),
                    links: Vec::new(),
                    font_index: 0,
                    font_size: DimensionValue(10 * 65_536),
                    source_span: None,
                }],
                images: Vec::new(),
                page_box: PageBox {
                    width: DimensionValue(100),
                    height: DimensionValue(200),
                },
                float_placements: Vec::new(),
                index_entries: Vec::new(),
            }],
            local_label_pages: BTreeMap::from([("intro".to_string(), 0)]),
            outlines: vec![TypesetOutline {
                level: 0,
                title: "Intro".to_string(),
                page_index: 0,
                y: DimensionValue(0),
            }],
            named_destinations: vec![TypesetNamedDestination {
                name: "intro".to_string(),
                page_index: 0,
                y: DimensionValue(0),
            }],
        }
    }

    fn test_cached_page_payload(text: &str) -> CachedPagePayload {
        PageRenderPayload::new(
            0,
            vec![PdfLinkAnnotation {
                object_id: 0,
                target: PdfLinkTarget::Uri("https://example.com".to_string()),
                x_start: DimensionValue::zero(),
                x_end: DimensionValue(10),
                y_bottom: DimensionValue(20),
                y_top: DimensionValue(30),
            }],
            BTreeSet::new(),
            text.to_string(),
        )
        .into()
    }

    trait ExpectNone<T> {
        fn expect_none(self, message: &str);
    }

    impl<T> ExpectNone<T> for Option<T> {
        fn expect_none(self, message: &str) {
            assert!(self.is_none(), "{message}");
        }
    }
}
