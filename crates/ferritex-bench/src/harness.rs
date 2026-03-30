use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use serde::Serialize;
use thiserror::Error;

pub trait CompileBackend: Send + Sync {
    fn compile(
        &self,
        input: &Path,
        asset_bundle: Option<&Path>,
        jobs: u32,
    ) -> Result<CompileOutput, String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileOutput {
    pub duration: Duration,
    pub output_bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BenchProfile {
    FullBench,
    PartitionBench,
    BundleBootstrap,
    CorpusCompat,
}

impl BenchProfile {
    pub fn stable_id(&self) -> &'static str {
        match self {
            Self::FullBench => "FTX-BENCH-001",
            Self::PartitionBench => "FTX-PARTITION-BENCH-001",
            Self::BundleBootstrap => "bundle-bootstrap-smoke",
            Self::CorpusCompat => "FTX-CORPUS-COMPAT-001",
        }
    }
}

pub fn bench_fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

pub fn bundle_bootstrap_cases(fixture_base: &Path) -> Vec<BenchCase> {
    let bundle_dir = fixture_base.join("bundle");

    ["article", "book", "report", "letter"]
        .into_iter()
        .map(|class| BenchCase {
            name: format!("layout-core-{class}-bundle"),
            profile: BenchProfile::BundleBootstrap,
            input_fixture: fixture_base.join(format!("layout-core/{class}.tex")),
            asset_bundle: Some(bundle_dir.clone()),
            jobs: 1,
        })
        .collect()
}

pub fn bundle_package_loading_cases(fixture_base: &Path) -> Vec<BenchCase> {
    let bundle_dir = fixture_base.join("bundle");
    let pkg_dir = fixture_base.join("bundle-packages");

    let mut fixtures = fs::read_dir(&pkg_dir)
        .unwrap_or_else(|error| {
            panic!(
                "failed to read bundle-packages fixtures from {}: {error}",
                pkg_dir.display()
            )
        })
        .map(|entry| {
            entry
                .unwrap_or_else(|error| {
                    panic!("failed to enumerate bundle-packages entry: {error}")
                })
                .path()
        })
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("tex"))
        .collect::<Vec<_>>();
    fixtures.sort();

    fixtures
        .into_iter()
        .map(|input_fixture| {
            let stem = input_fixture
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_else(|| {
                    panic!(
                        "invalid UTF-8 bundle-packages fixture name: {}",
                        input_fixture.display()
                    )
                });
            BenchCase {
                name: format!("bundle-pkg-{stem}"),
                profile: BenchProfile::BundleBootstrap,
                input_fixture,
                asset_bundle: Some(bundle_dir.clone()),
                jobs: 1,
            }
        })
        .collect()
}

pub fn partition_bench_cases(fixture_base: &Path) -> Vec<BenchCase> {
    let input = fixture_base.join("multi_section.tex");
    [1, 4]
        .into_iter()
        .map(|jobs| BenchCase {
            name: format!("partition-multi-section-jobs{jobs}"),
            profile: BenchProfile::PartitionBench,
            input_fixture: input.clone(),
            asset_bundle: None,
            jobs,
        })
        .collect()
}

pub fn corpus_compat_cases(fixture_base: &Path) -> Vec<BenchCase> {
    corpus_subset_cases(fixture_base, "layout-core")
}

pub fn corpus_navigation_cases(fixture_base: &Path) -> Vec<BenchCase> {
    corpus_subset_cases(fixture_base, "navigation-features")
}

pub fn corpus_embedded_assets_cases(fixture_base: &Path) -> Vec<BenchCase> {
    corpus_subset_cases(fixture_base, "embedded-assets")
}

pub fn corpus_bibliography_cases(fixture_base: &Path) -> Vec<BenchCase> {
    corpus_subset_cases(fixture_base, "bibliography")
}

fn corpus_subset_cases(fixture_base: &Path, subset: &str) -> Vec<BenchCase> {
    let corpus_dir = fixture_base.join(format!("corpus/{subset}"));
    let mut fixtures = fs::read_dir(&corpus_dir)
        .unwrap_or_else(|error| {
            panic!(
                "failed to read corpus fixtures from {}: {error}",
                corpus_dir.display()
            )
        })
        .map(|entry| {
            entry
                .unwrap_or_else(|error| panic!("failed to enumerate corpus fixture entry: {error}"))
                .path()
        })
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("tex"))
        .collect::<Vec<_>>();
    fixtures.sort();

    fixtures
        .into_iter()
        .map(|input_fixture| {
            let stem = input_fixture
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_else(|| {
                    panic!(
                        "invalid UTF-8 corpus fixture name: {}",
                        input_fixture.display()
                    )
                });
            BenchCase {
                name: format!("corpus-{subset}-{stem}"),
                profile: BenchProfile::CorpusCompat,
                input_fixture,
                asset_bundle: None,
                jobs: 1,
            }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BenchCase {
    pub name: String,
    pub profile: BenchProfile,
    pub input_fixture: PathBuf,
    pub asset_bundle: Option<PathBuf>,
    pub jobs: u32,
}

impl BenchCase {
    fn comparison_key(&self) -> (String, String, String, String) {
        (
            self.name.clone(),
            self.profile.stable_id().to_string(),
            self.input_fixture.display().to_string(),
            self.asset_bundle
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BenchRunConfig {
    pub warmup_runs: u32,
    pub measured_runs: u32,
    pub compare_output_identity: bool,
}

impl Default for BenchRunConfig {
    fn default() -> Self {
        Self {
            warmup_runs: 1,
            measured_runs: 5,
            compare_output_identity: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BenchTiming {
    #[serde(with = "duration_ms")]
    pub duration: Duration,
    pub output_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BenchResult {
    pub case: BenchCase,
    pub config: BenchRunConfig,
    pub timings: Vec<BenchTiming>,
}

impl BenchResult {
    pub fn median_duration(&self) -> Option<Duration> {
        let len = self.timings.len();
        if len == 0 {
            return None;
        }

        let mut durations = self
            .timings
            .iter()
            .map(|timing| timing.duration)
            .collect::<Vec<_>>();
        durations.sort_unstable();

        let mid = len / 2;
        if len % 2 == 1 {
            Some(durations[mid])
        } else {
            Some(Duration::from_secs_f64(
                (durations[mid - 1].as_secs_f64() + durations[mid].as_secs_f64()) / 2.0,
            ))
        }
    }

    pub fn is_output_identical(&self) -> bool {
        let mut timings = self.timings.iter();
        let Some(first) = timings.next() else {
            return true;
        };

        match &first.output_hash {
            Some(expected) => timings.all(|timing| timing.output_hash.as_ref() == Some(expected)),
            None => timings.all(|timing| timing.output_hash.is_none()),
        }
    }

    fn representative_output_hash(&self) -> Option<&str> {
        self.timings
            .first()
            .and_then(|timing| timing.output_hash.as_deref())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BenchComparison {
    pub baseline: BenchResult,
    pub candidate: BenchResult,
}

impl BenchComparison {
    pub fn speedup(&self) -> Option<f64> {
        let baseline = self.baseline.median_duration()?;
        let candidate = self.candidate.median_duration()?;
        if candidate.is_zero() {
            return None;
        }

        Some(baseline.as_secs_f64() / candidate.as_secs_f64())
    }

    pub fn output_identity_preserved(&self) -> bool {
        if !self.baseline.is_output_identical() || !self.candidate.is_output_identical() {
            return false;
        }

        match (
            self.baseline.representative_output_hash(),
            self.candidate.representative_output_hash(),
        ) {
            (Some(expected), Some(actual)) => expected == actual,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BenchFailure {
    #[error("output mismatch for {case_name}: expected {expected_hash}, got {actual_hash}")]
    OutputMismatch {
        case_name: String,
        expected_hash: String,
        actual_hash: String,
    },
    #[error("timeout while compiling {case_name} after {limit:?}")]
    Timeout {
        case_name: String,
        #[serde(with = "duration_ms")]
        limit: Duration,
    },
    #[error("compile error for {case_name}: {message}")]
    CompileError { case_name: String, message: String },
    #[error("missing fixture: {}", path.display())]
    MissingFixture { path: PathBuf },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct BenchReport {
    pub results: Vec<BenchResult>,
    pub comparisons: Vec<BenchComparison>,
    pub failures: Vec<BenchFailure>,
}

impl BenchReport {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("bench report serialization should succeed")
    }

    pub fn summary(&self) -> String {
        let mut lines = vec![format!(
            "results: {}, comparisons: {}, failures: {}",
            self.results.len(),
            self.comparisons.len(),
            self.failures.len()
        )];

        lines.extend(self.failures.iter().map(|failure| failure.to_string()));
        lines.join("\n")
    }
}

pub struct BenchHarness {
    cases: Vec<BenchCase>,
    config: BenchRunConfig,
    backend: Arc<dyn CompileBackend>,
}

impl BenchHarness {
    pub fn new(cases: Vec<BenchCase>, config: BenchRunConfig) -> Self {
        Self {
            cases,
            config,
            backend: Arc::new(UnconfiguredBackend),
        }
    }

    pub fn with_backend<B>(mut self, backend: B) -> Self
    where
        B: CompileBackend + 'static,
    {
        self.backend = Arc::new(backend);
        self
    }

    pub fn run(&self) -> BenchReport {
        let mut results = Vec::new();
        let mut failures = Vec::new();

        for case in &self.cases {
            match self.run_case(case) {
                Ok(result) => results.push(result),
                Err(failure) => failures.push(failure),
            }
        }

        let comparisons = self.build_comparisons(&results);
        BenchReport {
            results,
            comparisons,
            failures,
        }
    }

    pub fn run_case(&self, case: &BenchCase) -> Result<BenchResult, BenchFailure> {
        self.ensure_fixture(case.input_fixture.as_path())?;
        if let Some(bundle) = case.asset_bundle.as_deref() {
            self.ensure_fixture(bundle)?;
        }

        for _ in 0..self.config.warmup_runs {
            self.backend
                .compile(
                    case.input_fixture.as_path(),
                    case.asset_bundle.as_deref(),
                    case.jobs,
                )
                .map_err(|message| BenchFailure::CompileError {
                    case_name: case.name.clone(),
                    message,
                })?;
        }

        let mut timings = Vec::with_capacity(self.config.measured_runs as usize);
        for _ in 0..self.config.measured_runs {
            let output = self
                .backend
                .compile(
                    case.input_fixture.as_path(),
                    case.asset_bundle.as_deref(),
                    case.jobs,
                )
                .map_err(|message| BenchFailure::CompileError {
                    case_name: case.name.clone(),
                    message,
                })?;

            timings.push(BenchTiming {
                duration: output.duration,
                output_hash: Some(normalize_output_hash(&output.output_bytes)),
            });
        }

        if self.config.compare_output_identity {
            if let Some((expected_hash, actual_hash)) = find_output_mismatch(&timings) {
                return Err(BenchFailure::OutputMismatch {
                    case_name: case.name.clone(),
                    expected_hash,
                    actual_hash,
                });
            }
        }

        Ok(BenchResult {
            case: case.clone(),
            config: self.config.clone(),
            timings,
        })
    }

    pub fn compare(&self, baseline: &BenchResult, candidate: &BenchResult) -> BenchComparison {
        BenchComparison {
            baseline: baseline.clone(),
            candidate: candidate.clone(),
        }
    }

    fn build_comparisons(&self, results: &[BenchResult]) -> Vec<BenchComparison> {
        let mut groups = BTreeMap::<(String, String, String, String), Vec<BenchResult>>::new();
        for result in results {
            groups
                .entry(result.case.comparison_key())
                .or_default()
                .push(result.clone());
        }

        let mut comparisons = Vec::new();
        for mut group in groups.into_values() {
            group.sort_by_key(|result| result.case.jobs);
            let Some(baseline) = group.iter().find(|result| result.case.jobs == 1).cloned() else {
                continue;
            };

            for candidate in group.into_iter().filter(|result| result.case.jobs != 1) {
                comparisons.push(self.compare(&baseline, &candidate));
            }
        }

        comparisons
    }

    fn ensure_fixture(&self, path: &Path) -> Result<(), BenchFailure> {
        if path.exists() {
            Ok(())
        } else {
            Err(BenchFailure::MissingFixture {
                path: path.to_path_buf(),
            })
        }
    }
}

pub struct CliCompileBackend {
    binary_path: PathBuf,
}

impl CliCompileBackend {
    pub fn new(binary_path: PathBuf) -> Self {
        Self { binary_path }
    }
}

impl CompileBackend for CliCompileBackend {
    fn compile(
        &self,
        input: &Path,
        asset_bundle: Option<&Path>,
        jobs: u32,
    ) -> Result<CompileOutput, String> {
        let start = std::time::Instant::now();
        let mut cmd = std::process::Command::new(&self.binary_path);
        cmd.arg("compile").arg(input);
        if let Some(bundle) = asset_bundle {
            cmd.args(["--asset-bundle", &bundle.to_string_lossy()]);
        }
        cmd.args(["--jobs", &jobs.to_string()]);

        let output = cmd
            .output()
            .map_err(|e| format!("failed to run ferritex: {e}"))?;
        let duration = start.elapsed();
        if !output.status.success() {
            return Err(format!(
                "ferritex exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let pdf_path = input.with_extension("pdf");
        let output_bytes = std::fs::read(&pdf_path)
            .map_err(|e| format!("failed to read output PDF {}: {e}", pdf_path.display()))?;
        Ok(CompileOutput {
            duration,
            output_bytes,
        })
    }
}

struct UnconfiguredBackend;

impl CompileBackend for UnconfiguredBackend {
    fn compile(
        &self,
        _input: &Path,
        _asset_bundle: Option<&Path>,
        _jobs: u32,
    ) -> Result<CompileOutput, String> {
        Err("compile backend is not configured".to_string())
    }
}

fn normalize_output_hash(output_bytes: &[u8]) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let hash = output_bytes.iter().fold(FNV_OFFSET_BASIS, |acc, byte| {
        let hashed = acc ^ u64::from(*byte);
        hashed.wrapping_mul(FNV_PRIME)
    });

    format!("{hash:016x}")
}

fn find_output_mismatch(timings: &[BenchTiming]) -> Option<(String, String)> {
    let mut timings = timings.iter();
    let first = timings.next()?;
    let expected = first
        .output_hash
        .clone()
        .unwrap_or_else(|| "<missing>".to_string());

    timings.find_map(|timing| {
        let actual = timing
            .output_hash
            .clone()
            .unwrap_or_else(|| "<missing>".to_string());
        (actual != expected).then(|| (expected.clone(), actual))
    })
}

mod duration_ms {
    use std::time::Duration;

    use serde::{ser::Error as _, Serializer};

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let millis = duration.as_secs_f64() * 1000.0;
        if !millis.is_finite() {
            return Err(S::Error::custom("duration must be finite"));
        }

        serializer.serialize_f64(millis)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
        time::Duration,
    };

    use ferritex_application::{
        compile_job_service::CompileJobService,
        ports::{AssetBundleLoaderPort, ShellCommandGatewayPort, ShellCommandOutput},
    };
    use ferritex_core::{
        diagnostics::Severity,
        policy::{FileAccessError, FileAccessGate, PathAccessDecision},
    };
    use tempfile::tempdir;

    use super::{
        bundle_bootstrap_cases, bundle_package_loading_cases, corpus_bibliography_cases,
        corpus_compat_cases, corpus_embedded_assets_cases, corpus_navigation_cases, BenchCase,
        BenchComparison, BenchFailure, BenchHarness, BenchProfile, BenchResult, BenchRunConfig,
        BenchTiming, CompileBackend, CompileOutput,
    };

    const EXPECTED_BUNDLE_TFM: [u8; 64] = [
        0x00, 0x10, 0x00, 0x02, 0x00, 0x41, 0x00, 0x42, 0x00, 0x02, 0x00, 0x02, 0x00, 0x01, 0x00,
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xab, 0xcd, 0x12, 0x34, 0x00, 0xa0,
        0x00, 0x00, 0x01, 0x10, 0x00, 0x00, 0x01, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x05, 0x55, 0x55, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x99, 0x9a, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
    ];

    #[derive(Clone, Default)]
    struct MockCompileBackend {
        outputs: Arc<Mutex<VecDeque<Result<CompileOutput, String>>>>,
        calls: Arc<Mutex<Vec<(PathBuf, Option<PathBuf>, u32)>>>,
    }

    impl MockCompileBackend {
        fn with_outputs(outputs: Vec<Result<CompileOutput, String>>) -> Self {
            Self {
                outputs: Arc::new(Mutex::new(outputs.into())),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().expect("calls lock").len()
        }
    }

    impl CompileBackend for MockCompileBackend {
        fn compile(
            &self,
            input: &Path,
            asset_bundle: Option<&Path>,
            jobs: u32,
        ) -> Result<CompileOutput, String> {
            self.calls.lock().expect("calls lock").push((
                input.to_path_buf(),
                asset_bundle.map(Path::to_path_buf),
                jobs,
            ));

            self.outputs
                .lock()
                .expect("outputs lock")
                .pop_front()
                .unwrap_or_else(|| Err("mock backend ran out of outputs".to_string()))
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

    struct NoopAssetBundleLoader;

    impl AssetBundleLoaderPort for NoopAssetBundleLoader {
        fn validate(&self, _bundle_path: &Path) -> Result<(), String> {
            Ok(())
        }

        fn resolve_tex_input(&self, _bundle_path: &Path, _relative_path: &str) -> Option<PathBuf> {
            None
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

    #[test]
    fn test_single_case_timing() {
        let case = sample_case(
            "single-case",
            "minimal_article.tex",
            BenchProfile::FullBench,
            1,
        );
        let backend = MockCompileBackend::with_outputs(vec![
            Ok(output(5, b"warmup")),
            Ok(output(12, b"stable-output")),
            Ok(output(18, b"stable-output")),
        ]);
        let harness = BenchHarness::new(
            vec![case.clone()],
            BenchRunConfig {
                warmup_runs: 1,
                measured_runs: 2,
                compare_output_identity: true,
            },
        )
        .with_backend(backend.clone());

        let result = harness.run_case(&case).expect("case should succeed");

        assert_eq!(result.timings.len(), 2);
        assert_eq!(result.timings[0].duration, Duration::from_millis(12));
        assert_eq!(result.timings[1].duration, Duration::from_millis(18));
        assert_eq!(backend.call_count(), 3);
    }

    #[test]
    fn test_median_calculation() {
        let odd_result = sample_result(
            "odd",
            vec![timing(30, "same"), timing(10, "same"), timing(20, "same")],
        );
        let even_result = sample_result(
            "even",
            vec![
                timing(10, "same"),
                timing(40, "same"),
                timing(20, "same"),
                timing(30, "same"),
            ],
        );

        assert_eq!(
            odd_result.median_duration(),
            Some(Duration::from_millis(20))
        );
        assert_eq!(
            even_result.median_duration(),
            Some(Duration::from_millis(25))
        );
    }

    #[test]
    fn test_output_identity_check() {
        let identical = sample_result(
            "identical",
            vec![timing(10, "hash-a"), timing(20, "hash-a")],
        );
        let different = sample_result(
            "different",
            vec![timing(10, "hash-a"), timing(20, "hash-b")],
        );

        assert!(identical.is_output_identical());
        assert!(!different.is_output_identical());

        let case = sample_case(
            "mismatch",
            "minimal_article.tex",
            BenchProfile::PartitionBench,
            4,
        );
        let backend = MockCompileBackend::with_outputs(vec![
            Ok(output(5, b"warmup")),
            Ok(output(10, b"first")),
            Ok(output(11, b"second")),
        ]);
        let harness = BenchHarness::new(
            vec![case.clone()],
            BenchRunConfig {
                warmup_runs: 1,
                measured_runs: 2,
                compare_output_identity: true,
            },
        )
        .with_backend(backend);

        let failure = harness
            .run_case(&case)
            .expect_err("output mismatch should fail");

        assert!(matches!(
            failure,
            BenchFailure::OutputMismatch { case_name, .. } if case_name == "mismatch"
        ));
    }

    #[test]
    fn test_comparison_speedup() {
        let baseline = sample_result(
            "partition-book",
            vec![
                timing(400, "same"),
                timing(300, "same"),
                timing(500, "same"),
            ],
        );
        let candidate = BenchResult {
            case: BenchCase {
                jobs: 4,
                ..baseline.case.clone()
            },
            config: baseline.config.clone(),
            timings: vec![
                timing(200, "same"),
                timing(150, "same"),
                timing(250, "same"),
            ],
        };
        let comparison = BenchComparison {
            baseline,
            candidate,
        };

        let speedup = comparison.speedup().expect("speedup should exist");
        assert!((speedup - 2.0).abs() < f64::EPSILON);
        assert!(comparison.output_identity_preserved());
    }

    #[test]
    fn test_missing_fixture_failure() {
        let temp_dir = tempdir().expect("tempdir should be created");
        let missing_fixture = temp_dir.path().join("missing.tex");
        let case = BenchCase {
            name: "missing".to_string(),
            profile: BenchProfile::BundleBootstrap,
            input_fixture: missing_fixture.clone(),
            asset_bundle: None,
            jobs: 1,
        };
        let harness = BenchHarness::new(vec![case.clone()], BenchRunConfig::default())
            .with_backend(MockCompileBackend::default());

        let failure = harness
            .run_case(&case)
            .expect_err("missing fixture should fail");

        assert!(matches!(
            failure,
            BenchFailure::MissingFixture { path } if path == missing_fixture
        ));
    }

    #[test]
    fn test_bundle_bootstrap_case_with_layout_core_fixture() {
        let fixture_base = fixtures_root();
        let bundle_dir = fixture_base.join("bundle");
        let bundle_paths = [
            bundle_dir.join("manifest.json"),
            bundle_dir.join("asset-index.json"),
            bundle_dir.join("texmf/tex/latex/benchstub.sty"),
            bundle_dir.join("texmf/fonts/tfm/public/cm/cmr10.tfm"),
        ];
        let layout_paths = [
            fixture_base.join("layout-core/article.tex"),
            fixture_base.join("layout-core/report.tex"),
            fixture_base.join("layout-core/book.tex"),
            fixture_base.join("layout-core/letter.tex"),
        ];

        for path in bundle_paths.iter().chain(layout_paths.iter()) {
            assert!(path.exists(), "fixture should exist: {}", path.display());
        }

        let manifest: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(bundle_dir.join("manifest.json"))
                .expect("manifest fixture should be readable"),
        )
        .expect("manifest fixture should be valid json");
        assert_eq!(manifest["asset_index_path"], "asset-index.json");

        let asset_index: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(bundle_dir.join("asset-index.json"))
                .expect("asset index fixture should be readable"),
        )
        .expect("asset index fixture should be valid json");
        assert_eq!(
            asset_index["tfm_fonts"]["cmr10"],
            "texmf/fonts/tfm/public/cm/cmr10.tfm"
        );

        let tfm_bytes = fs::read(bundle_dir.join("texmf/fonts/tfm/public/cm/cmr10.tfm"))
            .expect("tfm fixture should be readable");
        assert_eq!(tfm_bytes.as_slice(), EXPECTED_BUNDLE_TFM);

        let cases = bundle_bootstrap_cases(&fixture_base);
        assert_eq!(cases.len(), 4);
        assert_eq!(
            cases
                .iter()
                .map(|case| case.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "layout-core-article-bundle",
                "layout-core-book-bundle",
                "layout-core-report-bundle",
                "layout-core-letter-bundle",
            ]
        );
        assert!(cases
            .iter()
            .all(|case| case.profile == BenchProfile::BundleBootstrap));
        assert!(cases
            .iter()
            .all(|case| case.asset_bundle.as_deref() == Some(bundle_dir.as_path())));

        let harness = BenchHarness::new(
            cases,
            BenchRunConfig {
                warmup_runs: 0,
                measured_runs: 1,
                compare_output_identity: true,
            },
        )
        .with_backend(MockCompileBackend::with_outputs(vec![
            Ok(output(7, b"bundle-output-article")),
            Ok(output(8, b"bundle-output-book")),
            Ok(output(9, b"bundle-output-report")),
            Ok(output(10, b"bundle-output-letter")),
        ]));

        let report = harness.run();

        assert!(report.failures.is_empty());
        assert_eq!(report.results.len(), 4);
        assert!(report
            .results
            .iter()
            .all(|result| result.case.profile == BenchProfile::BundleBootstrap));
        assert!(report
            .results
            .iter()
            .all(|result| result.case.asset_bundle.as_deref() == Some(bundle_dir.as_path())));
    }

    #[test]
    fn test_partition_bench_cases_generate_paired_jobs() {
        let fixture_base = fixtures_root();
        let cases = super::partition_bench_cases(&fixture_base);

        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].name, "partition-multi-section-jobs1");
        assert_eq!(cases[1].name, "partition-multi-section-jobs4");
        assert_eq!(cases[0].jobs, 1);
        assert_eq!(cases[1].jobs, 4);
        assert!(cases
            .iter()
            .all(|c| c.profile == BenchProfile::PartitionBench));
        assert!(cases.iter().all(|c| c.asset_bundle.is_none()));
        assert!(cases[0].input_fixture.ends_with("multi_section.tex"));
        assert_eq!(cases[0].input_fixture, cases[1].input_fixture);
    }

    #[test]
    fn test_partition_bench_harness_passes_jobs_to_backend() {
        let cases = vec![
            sample_case(
                "partition-seq",
                "multi_section.tex",
                BenchProfile::PartitionBench,
                1,
            ),
            sample_case(
                "partition-par",
                "multi_section.tex",
                BenchProfile::PartitionBench,
                4,
            ),
        ];
        let backend = MockCompileBackend::with_outputs(vec![
            Ok(output(10, b"stable")),
            Ok(output(10, b"stable")),
            Ok(output(8, b"stable")),
            Ok(output(8, b"stable")),
        ]);
        let harness = BenchHarness::new(
            cases,
            BenchRunConfig {
                warmup_runs: 0,
                measured_runs: 2,
                compare_output_identity: true,
            },
        )
        .with_backend(backend.clone());

        let report = harness.run();

        assert!(report.failures.is_empty());
        assert_eq!(report.results.len(), 2);

        let calls = backend.calls.lock().expect("calls lock");
        let jobs_values: Vec<u32> = calls.iter().map(|(_, _, jobs)| *jobs).collect();
        assert_eq!(jobs_values, vec![1, 1, 4, 4]);
    }

    #[test]
    fn test_bundle_package_loading_cases_enumerate_fixtures() {
        let fixture_base = fixtures_root();
        let cases = bundle_package_loading_cases(&fixture_base);

        assert_eq!(cases.len(), 2);
        assert!(cases
            .iter()
            .all(|case| case.profile == BenchProfile::BundleBootstrap));
        assert!(cases.iter().all(|case| case.asset_bundle.is_some()));
        assert!(cases.iter().all(|case| case.jobs == 1));
        assert!(cases
            .iter()
            .all(|case| case.name.starts_with("bundle-pkg-")));
        assert!(cases.iter().all(|case| case.input_fixture.exists()));

        let names: Vec<_> = cases.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"bundle-pkg-compat_options"));
        assert!(names.contains(&"bundle-pkg-depchain_recursive"));
    }

    #[test]
    fn test_corpus_compat_cases_enumerate_layout_core_directory() {
        let fixture_base = fixtures_root();
        let cases = corpus_compat_cases(&fixture_base);

        assert!(cases.len() >= 10);
        assert!(cases
            .iter()
            .all(|case| case.profile == BenchProfile::CorpusCompat));
        assert!(cases.iter().all(|case| case.asset_bundle.is_none()));
        assert!(cases.iter().all(|case| case.jobs == 1));
        assert!(cases
            .iter()
            .all(|case| case.name.starts_with("corpus-layout-core-")));
        assert!(cases.iter().all(|case| case.input_fixture.exists()));
        assert!(cases.iter().all(|case| {
            case.input_fixture.extension().and_then(|ext| ext.to_str()) == Some("tex")
        }));

        let names = cases
            .iter()
            .map(|case| case.name.clone())
            .collect::<Vec<_>>();
        let mut sorted_names = names.clone();
        sorted_names.sort();
        assert_eq!(names, sorted_names);
        assert!(names.contains(&"corpus-layout-core-sectioning_article".to_string()));
        assert!(names.contains(&"corpus-layout-core-sectioning_report".to_string()));
        assert!(names.contains(&"corpus-layout-core-sectioning_book".to_string()));
        assert!(names.contains(&"corpus-layout-core-letter_basic".to_string()));
        assert!(names.contains(&"corpus-layout-core-compat_primitives".to_string()));
    }

    #[test]
    fn corpus_layout_core_documents_compile_successfully() {
        let fixture_base = fixtures_root();
        let cases = corpus_compat_cases(&fixture_base);
        let gate = FsTestFileAccessGate;
        let loader = NoopAssetBundleLoader;
        let service = CompileJobService::new(&gate, &loader, &NOOP_SHELL_COMMAND_GATEWAY);

        assert!(cases.len() >= 10);

        for case in cases {
            let source = fs::read_to_string(&case.input_fixture).unwrap_or_else(|error| {
                panic!("failed to read {}: {error}", case.input_fixture.display())
            });
            let input_path = case.input_fixture.canonicalize().unwrap_or_else(|error| {
                panic!(
                    "failed to canonicalize {}: {error}",
                    case.input_fixture.display()
                )
            });
            let uri = format!("file://{}", input_path.display());
            let state = service.compile_from_source(&source, &uri);
            let error_diagnostics = state
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == Severity::Error)
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>();

            assert!(
                state.success,
                "{} should compile successfully, diagnostics: {:?}",
                case.name, state.diagnostics
            );
            assert!(
                error_diagnostics.is_empty(),
                "{} emitted error diagnostics: {:?}",
                case.name,
                error_diagnostics
            );
            assert!(
                state.page_count >= 1,
                "{} should produce at least one page, got {}",
                case.name,
                state.page_count
            );
        }
    }

    #[test]
    fn test_corpus_navigation_cases_enumerate_fixtures() {
        let fixture_base = fixtures_root();
        let cases = corpus_navigation_cases(&fixture_base);

        assert!(cases.len() >= 3);
        assert!(cases
            .iter()
            .all(|case| case.profile == BenchProfile::CorpusCompat));
        assert!(cases.iter().all(|case| case.asset_bundle.is_none()));
        assert!(cases.iter().all(|case| case.jobs == 1));
        assert!(cases
            .iter()
            .all(|case| case.name.starts_with("corpus-navigation-features-")));
        assert!(cases.iter().all(|case| case.input_fixture.exists()));
    }

    #[test]
    fn corpus_navigation_documents_compile_successfully() {
        let fixture_base = fixtures_root();
        let cases = corpus_navigation_cases(&fixture_base);
        let gate = FsTestFileAccessGate;
        let loader = NoopAssetBundleLoader;
        let service = CompileJobService::new(&gate, &loader, &NOOP_SHELL_COMMAND_GATEWAY);

        assert!(cases.len() >= 3);

        for case in cases {
            let source = fs::read_to_string(&case.input_fixture).unwrap_or_else(|error| {
                panic!("failed to read {}: {error}", case.input_fixture.display())
            });
            let input_path = case.input_fixture.canonicalize().unwrap_or_else(|error| {
                panic!(
                    "failed to canonicalize {}: {error}",
                    case.input_fixture.display()
                )
            });
            let uri = format!("file://{}", input_path.display());
            let state = service.compile_from_source(&source, &uri);
            let error_diagnostics = state
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == Severity::Error)
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>();

            assert!(
                state.success,
                "{} should compile successfully, diagnostics: {:?}",
                case.name, state.diagnostics
            );
            assert!(
                error_diagnostics.is_empty(),
                "{} emitted error diagnostics: {:?}",
                case.name,
                error_diagnostics
            );
            assert!(
                state.page_count >= 1,
                "{} should produce at least one page, got {}",
                case.name,
                state.page_count
            );
        }
    }

    #[test]
    fn test_corpus_embedded_assets_cases_enumerate_fixtures() {
        let fixture_base = fixtures_root();
        let cases = corpus_embedded_assets_cases(&fixture_base);

        assert!(cases.len() >= 2);
        assert!(cases
            .iter()
            .all(|case| case.profile == BenchProfile::CorpusCompat));
        assert!(cases.iter().all(|case| case.asset_bundle.is_none()));
        assert!(cases.iter().all(|case| case.jobs == 1));
        assert!(cases
            .iter()
            .all(|case| case.name.starts_with("corpus-embedded-assets-")));
        assert!(cases.iter().all(|case| case.input_fixture.exists()));
    }

    #[test]
    fn corpus_embedded_assets_documents_compile_successfully() {
        let fixture_base = fixtures_root();
        let cases = corpus_embedded_assets_cases(&fixture_base);
        let gate = FsTestFileAccessGate;
        let loader = NoopAssetBundleLoader;
        let service = CompileJobService::new(&gate, &loader, &NOOP_SHELL_COMMAND_GATEWAY);

        assert!(cases.len() >= 2);

        for case in cases {
            let source = fs::read_to_string(&case.input_fixture).unwrap_or_else(|error| {
                panic!("failed to read {}: {error}", case.input_fixture.display())
            });
            let input_path = case.input_fixture.canonicalize().unwrap_or_else(|error| {
                panic!(
                    "failed to canonicalize {}: {error}",
                    case.input_fixture.display()
                )
            });
            let uri = format!("file://{}", input_path.display());
            let state = service.compile_from_source(&source, &uri);
            let error_diagnostics = state
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == Severity::Error)
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>();

            assert!(
                state.success,
                "{} should compile successfully, diagnostics: {:?}",
                case.name, state.diagnostics
            );
            assert!(
                error_diagnostics.is_empty(),
                "{} emitted error diagnostics: {:?}",
                case.name,
                error_diagnostics
            );
            assert!(
                state.page_count >= 1,
                "{} should produce at least one page, got {}",
                case.name,
                state.page_count
            );
        }
    }

    #[test]
    fn test_corpus_bibliography_cases_enumerate_fixtures() {
        let fixture_base = fixtures_root();
        let cases = corpus_bibliography_cases(&fixture_base);

        assert!(cases.len() >= 2);
        assert!(cases
            .iter()
            .all(|case| case.profile == BenchProfile::CorpusCompat));
        assert!(cases.iter().all(|case| case.asset_bundle.is_none()));
        assert!(cases.iter().all(|case| case.jobs == 1));
        assert!(cases
            .iter()
            .all(|case| case.name.starts_with("corpus-bibliography-")));
        assert!(cases.iter().all(|case| case.input_fixture.exists()));
    }

    #[test]
    fn corpus_bibliography_documents_compile_successfully() {
        let fixture_base = fixtures_root();
        let cases = corpus_bibliography_cases(&fixture_base);
        let gate = FsTestFileAccessGate;
        let loader = NoopAssetBundleLoader;
        let service = CompileJobService::new(&gate, &loader, &NOOP_SHELL_COMMAND_GATEWAY);

        assert!(cases.len() >= 2);

        for case in cases {
            let source = fs::read_to_string(&case.input_fixture).unwrap_or_else(|error| {
                panic!("failed to read {}: {error}", case.input_fixture.display())
            });
            let input_path = case.input_fixture.canonicalize().unwrap_or_else(|error| {
                panic!(
                    "failed to canonicalize {}: {error}",
                    case.input_fixture.display()
                )
            });
            let uri = format!("file://{}", input_path.display());
            let state = service.compile_from_source(&source, &uri);
            let error_diagnostics = state
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == Severity::Error)
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>();

            assert!(
                state.success,
                "{} should compile successfully, diagnostics: {:?}",
                case.name, state.diagnostics
            );
            assert!(
                error_diagnostics.is_empty(),
                "{} emitted error diagnostics: {:?}",
                case.name,
                error_diagnostics
            );
            assert!(
                state.page_count >= 1,
                "{} should produce at least one page, got {}",
                case.name,
                state.page_count
            );
        }
    }

    #[test]
    fn test_report_serialization_and_comparison_generation() {
        let sequential = sample_case(
            "partition-article",
            "multi_section.tex",
            BenchProfile::PartitionBench,
            1,
        );
        let parallel = BenchCase {
            jobs: 4,
            ..sequential.clone()
        };
        let backend = MockCompileBackend::with_outputs(vec![
            Ok(output(20, b"warmup")),
            Ok(output(30, b"stable")),
            Ok(output(32, b"stable")),
            Ok(output(18, b"warmup")),
            Ok(output(15, b"stable")),
            Ok(output(14, b"stable")),
        ]);
        let harness = BenchHarness::new(
            vec![sequential, parallel],
            BenchRunConfig {
                warmup_runs: 1,
                measured_runs: 2,
                compare_output_identity: true,
            },
        )
        .with_backend(backend);

        let report = harness.run();

        assert_eq!(report.results.len(), 2);
        assert_eq!(report.comparisons.len(), 1);
        assert!(report.failures.is_empty());
        assert!(report.to_json().contains("partition-article"));
        assert!(report.summary().contains("results: 2"));
    }

    fn sample_case(name: &str, fixture_name: &str, profile: BenchProfile, jobs: u32) -> BenchCase {
        BenchCase {
            name: name.to_string(),
            profile,
            input_fixture: fixture_path(fixture_name),
            asset_bundle: None,
            jobs,
        }
    }

    fn sample_result(name: &str, timings: Vec<BenchTiming>) -> BenchResult {
        BenchResult {
            case: sample_case(name, "minimal_article.tex", BenchProfile::FullBench, 1),
            config: BenchRunConfig::default(),
            timings,
        }
    }

    fn fixtures_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
    }

    fn fixture_path(name: &str) -> PathBuf {
        fixtures_root().join(name)
    }

    fn output(duration_ms: u64, bytes: &[u8]) -> CompileOutput {
        CompileOutput {
            duration: Duration::from_millis(duration_ms),
            output_bytes: bytes.to_vec(),
        }
    }

    fn timing(duration_ms: u64, hash: &str) -> BenchTiming {
        BenchTiming {
            duration: Duration::from_millis(duration_ms),
            output_hash: Some(hash.to_string()),
        }
    }
}
