use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ferritex_core::compilation::SymbolLocation;
use ferritex_core::diagnostics::{Diagnostic, Severity};
use ferritex_core::incremental::{DependencyGraph, RecompilationScope};
use ferritex_core::policy::{FileAccessError, FileAccessGate};
use ferritex_core::synctex::SourceLineTrace;
use serde::{Deserialize, Serialize};

use crate::stable_compile_state::StableCompileState;

const CACHE_VERSION: u32 = 2;
const CACHE_DIR_NAME: &str = ".ferritex-cache";

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
            scope: None,
        }
    }

    pub fn store(
        &self,
        dependency_graph: &DependencyGraph,
        stable_compile_state: &StableCompileState,
        output_pdf_hash: u64,
        cached_source_subtrees: &BTreeMap<PathBuf, CachedSourceSubtree>,
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

    use ferritex_core::compilation::{CompilationSnapshot, DocumentState};
    use ferritex_core::incremental::RecompilationScope;

    use super::{fingerprint_bytes, CompileCache};
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

    trait ExpectNone<T> {
        fn expect_none(self, message: &str);
    }

    impl<T> ExpectNone<T> for Option<T> {
        fn expect_none(self, message: &str) {
            assert!(self.is_none(), "{message}");
        }
    }
}
