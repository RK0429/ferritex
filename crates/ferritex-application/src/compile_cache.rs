use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use ferritex_core::compilation::SymbolLocation;
use ferritex_core::diagnostics::{Diagnostic, Severity};
use ferritex_core::incremental::{DependencyGraph, RecompilationScope};
use ferritex_core::policy::{FileAccessError, FileAccessGate};
use ferritex_core::synctex::SourceLineTrace;
use ferritex_core::typesetting::DocumentLayoutFragment;
use serde::{Deserialize, Serialize};

use crate::stable_compile_state::StableCompileState;

const CACHE_VERSION: u32 = 4;
const CACHE_DIR_NAME: &str = ".ferritex-cache";
const CACHE_RECORD_EXTENSION: &str = "json";
const MAX_CACHE_RECORD_FILES: usize = 64;

pub struct CompileCache<'a> {
    file_access_gate: &'a dyn FileAccessGate,
    primary_input: PathBuf,
    jobname: String,
    output_pdf: PathBuf,
    cache_dir: PathBuf,
    metadata_path: PathBuf,
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedTypesetFragment {
    pub fragment: DocumentLayoutFragment,
    pub source_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CompileCacheRecord {
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

        Self {
            file_access_gate,
            primary_input: primary_input.to_path_buf(),
            jobname: jobname.to_string(),
            output_pdf: output_dir.join(format!("{jobname}.pdf")),
            metadata_path: cache_dir.join(format!("{cache_key}.json")),
            cache_dir,
        }
    }

    pub fn lookup(&self) -> CacheLookupResult {
        let bytes = match self.file_access_gate.read_file(&self.metadata_path) {
            Ok(bytes) => bytes,
            Err(FileAccessError::Io { source })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                return CacheLookupResult {
                    artifact: None,
                    baseline_state: None,
                    diagnostics: Vec::new(),
                    changed_paths: Vec::new(),
                    rebuild_paths: BTreeSet::new(),
                    cached_dependency_graph: None,
                    cached_source_subtrees: BTreeMap::new(),
                    cached_typeset_fragments: BTreeMap::new(),
                    scope: None,
                };
            }
            Err(error) => {
                return CacheLookupResult {
                    artifact: None,
                    baseline_state: None,
                    diagnostics: vec![cache_info_diagnostic(
                        format!("failed to read compile cache metadata: {error}"),
                        &self.metadata_path,
                    )],
                    changed_paths: Vec::new(),
                    rebuild_paths: BTreeSet::new(),
                    cached_dependency_graph: None,
                    cached_source_subtrees: BTreeMap::new(),
                    cached_typeset_fragments: BTreeMap::new(),
                    scope: None,
                };
            }
        };

        let record: CompileCacheRecord = match serde_json::from_slice(&bytes) {
            Ok(record) => record,
            Err(error) => {
                return CacheLookupResult {
                    artifact: None,
                    baseline_state: None,
                    diagnostics: vec![cache_info_diagnostic(
                        format!("compile cache metadata is invalid: {error}"),
                        &self.metadata_path,
                    )],
                    changed_paths: Vec::new(),
                    rebuild_paths: BTreeSet::new(),
                    cached_dependency_graph: None,
                    cached_source_subtrees: BTreeMap::new(),
                    cached_typeset_fragments: BTreeMap::new(),
                    scope: None,
                };
            }
        };

        if record.version != CACHE_VERSION {
            return CacheLookupResult {
                artifact: None,
                baseline_state: None,
                diagnostics: vec![cache_info_diagnostic(
                    format!(
                        "compile cache version mismatch (found {}, expected {CACHE_VERSION})",
                        record.version
                    ),
                    &self.metadata_path,
                )],
                changed_paths: Vec::new(),
                rebuild_paths: BTreeSet::new(),
                cached_dependency_graph: None,
                cached_source_subtrees: BTreeMap::new(),
                cached_typeset_fragments: BTreeMap::new(),
                scope: None,
            };
        }

        if record.primary_input != self.primary_input
            || record.jobname != self.jobname
            || record.output_pdf != self.output_pdf
        {
            return CacheLookupResult {
                artifact: None,
                baseline_state: None,
                diagnostics: Vec::new(),
                changed_paths: Vec::new(),
                rebuild_paths: BTreeSet::new(),
                cached_dependency_graph: None,
                cached_source_subtrees: BTreeMap::new(),
                cached_typeset_fragments: BTreeMap::new(),
                scope: None,
            };
        }

        let baseline_state = record.stable_compile_state.clone();

        let change_summary = self.detect_changes(&record.dependency_graph);
        if !change_summary.changed_paths.is_empty() {
            return CacheLookupResult {
                artifact: None,
                baseline_state: Some(baseline_state),
                diagnostics: Vec::new(),
                changed_paths: change_summary.changed_paths,
                rebuild_paths: change_summary.rebuild_paths,
                cached_dependency_graph: Some(record.dependency_graph),
                cached_source_subtrees: record.cached_source_subtrees,
                cached_typeset_fragments: record.cached_typeset_fragments,
                scope: Some(change_summary.scope),
            };
        }

        let output_pdf_hash = match self.file_access_gate.read_file(&self.output_pdf) {
            Ok(bytes) => fingerprint_bytes(&bytes),
            Err(error) => {
                return CacheLookupResult {
                    artifact: None,
                    baseline_state: Some(baseline_state),
                    diagnostics: vec![cache_info_diagnostic(
                        format!("cached PDF artifact is unavailable: {error}"),
                        &self.output_pdf,
                    )],
                    changed_paths: Vec::new(),
                    rebuild_paths: BTreeSet::new(),
                    cached_dependency_graph: None,
                    cached_source_subtrees: BTreeMap::new(),
                    cached_typeset_fragments: BTreeMap::new(),
                    scope: None,
                };
            }
        };

        if output_pdf_hash != record.output_pdf_hash {
            return CacheLookupResult {
                artifact: None,
                baseline_state: Some(baseline_state),
                diagnostics: vec![cache_info_diagnostic(
                    "cached PDF artifact hash mismatch; falling back to full compile",
                    &self.output_pdf,
                )],
                changed_paths: Vec::new(),
                rebuild_paths: BTreeSet::new(),
                cached_dependency_graph: None,
                cached_source_subtrees: BTreeMap::new(),
                cached_typeset_fragments: BTreeMap::new(),
                scope: None,
            };
        }

        CacheLookupResult {
            artifact: Some(CachedCompileArtifact {
                stable_compile_state: record.stable_compile_state,
                output_pdf: record.output_pdf,
            }),
            baseline_state: Some(baseline_state),
            diagnostics: Vec::new(),
            changed_paths: Vec::new(),
            rebuild_paths: BTreeSet::new(),
            cached_dependency_graph: Some(record.dependency_graph),
            cached_source_subtrees: record.cached_source_subtrees,
            cached_typeset_fragments: record.cached_typeset_fragments,
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
        if let Err(error) = self.file_access_gate.ensure_directory(&self.cache_dir) {
            return Some(cache_info_diagnostic(
                format!("failed to prepare compile cache directory: {error}"),
                &self.cache_dir,
            ));
        }

        let record = CompileCacheRecord {
            version: CACHE_VERSION,
            primary_input: self.primary_input.clone(),
            jobname: self.jobname.clone(),
            output_pdf: self.output_pdf.clone(),
            output_pdf_hash,
            dependency_graph: dependency_graph.clone(),
            stable_compile_state: stable_compile_state.clone(),
            cached_source_subtrees: cached_source_subtrees.clone(),
            cached_typeset_fragments: cached_typeset_fragments.clone(),
        };

        let bytes = match serde_json::to_vec_pretty(&record) {
            Ok(bytes) => bytes,
            Err(error) => {
                return Some(cache_info_diagnostic(
                    format!("failed to serialize compile cache metadata: {error}"),
                    &self.metadata_path,
                ));
            }
        };

        self.file_access_gate
            .write_file(&self.metadata_path, &bytes)
            .err()
            .map(|error| {
                cache_info_diagnostic(
                    format!("failed to persist compile cache metadata: {error}"),
                    &self.metadata_path,
                )
            })
            .or_else(|| {
                self.evict_excess_records().err().map(|error| {
                    cache_cleanup_diagnostic(
                        format!("failed to evict old compile cache metadata: {error}"),
                        &self.cache_dir,
                    )
                })
            })
    }

    fn detect_changes(&self, dependency_graph: &DependencyGraph) -> ChangeSummary {
        let mut changed_paths = Vec::new();

        for (path, node) in &dependency_graph.nodes {
            let current_hash = match self.file_access_gate.read_file(path) {
                Ok(bytes) => fingerprint_bytes(&bytes),
                Err(_) => {
                    changed_paths.push(path.clone());
                    continue;
                }
            };

            if current_hash != node.content_hash {
                changed_paths.push(path.clone());
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
            if let Err(error) = fs::remove_file(record.path) {
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
            if !file_type.is_file() {
                continue;
            }

            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str())
                != Some(CACHE_RECORD_EXTENSION)
            {
                continue;
            }

            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            if serde_json::from_slice::<CompileCacheRecord>(&bytes).is_err() {
                continue;
            }

            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH);
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

fn cache_info_diagnostic(message: impl Into<String>, path: &Path) -> Diagnostic {
    Diagnostic::new(Severity::Info, message.into())
        .with_file(path.to_string_lossy().into_owned())
        .with_suggestion("Ferritex will ignore this cache entry and run a full compile")
}

fn cache_cleanup_diagnostic(message: impl Into<String>, path: &Path) -> Diagnostic {
    Diagnostic::new(Severity::Info, message.into())
        .with_file(path.to_string_lossy().into_owned())
        .with_suggestion(
            "Ferritex kept the current cache entry but could not clean up older metadata files",
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
    use std::collections::BTreeMap;
    use std::fs;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use ferritex_core::compilation::{CompilationSnapshot, DocumentState};
    use ferritex_core::diagnostics::Severity;
    use ferritex_core::incremental::RecompilationScope;
    use ferritex_core::kernel::api::DimensionValue;
    use ferritex_core::typesetting::{
        DocumentLayoutFragment, PageBox, TextLine, TypesetNamedDestination, TypesetOutline,
        TypesetPage,
    };

    use super::{
        fingerprint_bytes, CachedTypesetFragment, CompileCache, CompileCacheRecord,
        OwnedCacheRecordFile, MAX_CACHE_RECORD_FILES,
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

        let lookup = cache.lookup();

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

        let lookup = cache.lookup();

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

        let lookup = cache.lookup();

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

        let lookup = cache.lookup();

        assert_eq!(lookup.cached_typeset_fragments, cached_typeset_fragments);
    }

    #[test]
    fn evicts_oldest_owned_cache_records_and_keeps_newer_entries() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let output_dir = dir.path().join("out");
        fs::create_dir_all(&output_dir).expect("create output dir");
        let cache_dir = output_dir.join(super::CACHE_DIR_NAME);
        let base_time = UNIX_EPOCH + Duration::from_secs(10_000);
        let mut metadata_paths = Vec::new();

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
            metadata_paths.push(cache.metadata_path.clone());

            if index < MAX_CACHE_RECORD_FILES {
                set_modified_time(
                    &cache.metadata_path,
                    base_time + Duration::from_secs(index as u64),
                );
            }
        }

        assert!(
            !metadata_paths[0].exists(),
            "oldest cache record should be evicted"
        );
        for path in metadata_paths.iter().skip(1) {
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

        let valid_a_path = cache_dir.join("valid-a.json");
        let unreadable_path = cache_dir.join("unreadable.json");
        let valid_b_path = cache_dir.join("valid-b.json");

        write_owned_cache_record(
            &valid_a_path,
            CompileCacheRecord {
                version: super::CACHE_VERSION,
                primary_input: valid_a_input.clone(),
                jobname: "valid-a".to_string(),
                output_pdf: output_dir.join("valid-a.pdf"),
                output_pdf_hash: fingerprint_bytes(b"valid-a-pdf"),
                dependency_graph: dependency_graph_for(&valid_a_input, "valid-a"),
                stable_compile_state: stable_state(&valid_a_input),
                cached_source_subtrees: BTreeMap::new(),
                cached_typeset_fragments: BTreeMap::new(),
            },
        );
        write_owned_cache_record(
            &unreadable_path,
            CompileCacheRecord {
                version: super::CACHE_VERSION,
                primary_input: unreadable_input.clone(),
                jobname: "unreadable".to_string(),
                output_pdf: output_dir.join("unreadable.pdf"),
                output_pdf_hash: fingerprint_bytes(b"unreadable-pdf"),
                dependency_graph: dependency_graph_for(&unreadable_input, "unreadable"),
                stable_compile_state: stable_state(&unreadable_input),
                cached_source_subtrees: BTreeMap::new(),
                cached_typeset_fragments: BTreeMap::new(),
            },
        );
        write_owned_cache_record(
            &valid_b_path,
            CompileCacheRecord {
                version: super::CACHE_VERSION,
                primary_input: valid_b_input.clone(),
                jobname: "valid-b".to_string(),
                output_pdf: output_dir.join("valid-b.pdf"),
                output_pdf_hash: fingerprint_bytes(b"valid-b-pdf"),
                dependency_graph: dependency_graph_for(&valid_b_input, "valid-b"),
                stable_compile_state: stable_state(&valid_b_input),
                cached_source_subtrees: BTreeMap::new(),
                cached_typeset_fragments: BTreeMap::new(),
            },
        );

        let mut permissions = fs::metadata(&unreadable_path)
            .expect("unreadable record metadata")
            .permissions();
        let original_mode = permissions.mode();
        permissions.set_mode(0o000);
        fs::set_permissions(&unreadable_path, permissions).expect("set unreadable record");

        let records =
            CompileCache::owned_cache_record_files(&cache_dir).expect("owned cache record listing");

        let mut restored = fs::metadata(&unreadable_path)
            .expect("restore unreadable record metadata")
            .permissions();
        restored.set_mode(original_mode);
        fs::set_permissions(&unreadable_path, restored).expect("restore unreadable record");

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
            let record_path = cache_dir.join(format!("manual-{index:03}.json"));
            fs::write(&extra_input, format!("extra-{index}")).expect("write extra input");
            write_owned_cache_record(
                &record_path,
                CompileCacheRecord {
                    version: super::CACHE_VERSION,
                    primary_input: extra_input.clone(),
                    jobname: format!("extra-{index:03}"),
                    output_pdf: output_dir.join(format!("extra-{index:03}.pdf")),
                    output_pdf_hash: fingerprint_bytes(format!("pdf-extra-{index}").as_bytes()),
                    dependency_graph: dependency_graph_for(&extra_input, &format!("extra-{index}")),
                    stable_compile_state: stable_state(&extra_input),
                    cached_source_subtrees: BTreeMap::new(),
                    cached_typeset_fragments: BTreeMap::new(),
                },
            );
            set_modified_time(&record_path, UNIX_EPOCH + Duration::from_secs(index as u64));
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
                .contains("failed to evict old compile cache metadata"),
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

    fn write_owned_cache_record(path: &std::path::Path, record: CompileCacheRecord) {
        let bytes = serde_json::to_vec_pretty(&record).expect("serialize cache record");
        fs::write(path, bytes).expect("write cache record");
    }

    fn test_fragment(partition_id: &str) -> DocumentLayoutFragment {
        DocumentLayoutFragment {
            partition_id: partition_id.to_string(),
            pages: vec![TypesetPage {
                lines: vec![TextLine {
                    text: "cached".to_string(),
                    y: DimensionValue(0),
                    links: Vec::new(),
                    font_index: 0,
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

    trait ExpectNone<T> {
        fn expect_none(self, message: &str);
    }

    impl<T> ExpectNone<T> for Option<T> {
        fn expect_none(self, message: &str) {
            assert!(self.is_none(), "{message}");
        }
    }
}
