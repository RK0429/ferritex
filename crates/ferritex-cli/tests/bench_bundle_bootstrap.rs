use std::path::PathBuf;

use ferritex_bench::{
    bench_fixtures_root, bundle_bootstrap_cases, BenchCase, BenchHarness, BenchProfile,
    BenchRunConfig, CliCompileBackend,
};

fn ferritex_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ferritex"))
}

#[test]
fn bundle_bootstrap_compiles_layout_core_article_with_real_backend() {
    let bench_fixtures = bench_fixtures_root();
    let base_cases = bundle_bootstrap_cases(&bench_fixtures);

    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let mut cases = Vec::with_capacity(base_cases.len());
    for case in &base_cases {
        let fixture_name = case
            .input_fixture
            .file_name()
            .expect("fixture should have filename");
        let temp_input = temp_dir.path().join(fixture_name);
        std::fs::copy(&case.input_fixture, &temp_input).expect("copy fixture to temp dir");
        cases.push(BenchCase {
            name: case.name.clone(),
            profile: case.profile.clone(),
            input_fixture: temp_input,
            asset_bundle: case.asset_bundle.clone(),
            jobs: case.jobs,
        });
    }

    let backend = CliCompileBackend::new(ferritex_bin());
    let harness = BenchHarness::new(
        cases,
        BenchRunConfig {
            warmup_runs: 0,
            measured_runs: 1,
            compare_output_identity: false,
        },
    )
    .with_backend(backend);

    let report = harness.run();

    assert!(
        report.failures.is_empty(),
        "bundle bootstrap compilation failed: {:?}",
        report.failures
    );
    assert_eq!(report.results.len(), 1);
    assert_eq!(
        report.results[0].case.profile,
        BenchProfile::BundleBootstrap
    );
    assert!(
        report.results[0].case.asset_bundle.is_some(),
        "asset_bundle must be set for BundleBootstrap"
    );
    assert!(
        !report.results[0].timings.is_empty(),
        "should have at least one timing"
    );
    assert!(
        report.results[0]
            .timings
            .iter()
            .all(|timing| timing.output_hash.is_some()),
        "all timings should have output hashes from real compilation"
    );
}
