use std::path::PathBuf;

use ferritex_bench::{
    bench_fixtures_root, bundle_bootstrap_cases, partition_bench_cases, BenchCase, BenchHarness,
    BenchProfile, BenchRunConfig, CliCompileBackend,
};

fn ferritex_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ferritex"))
}

#[test]
fn bundle_bootstrap_compiles_layout_core_classes_with_real_backend() {
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
    assert!(
        report.results.len() == 4,
        "expected all layout-core bundle bootstrap cases to run"
    );
    assert!(report
        .results
        .iter()
        .all(|result| result.case.profile == BenchProfile::BundleBootstrap));
    assert!(report
        .results
        .iter()
        .all(|result| result.case.asset_bundle.is_some()));
    assert!(report
        .results
        .iter()
        .all(|result| !result.timings.is_empty()));
    assert!(
        report
            .results
            .iter()
            .flat_map(|result| result.timings.iter())
            .all(|timing| timing.output_hash.is_some()),
        "all timings should have output hashes from real compilation"
    );
    assert!(report
        .results
        .iter()
        .map(|result| result.case.name.as_str())
        .eq([
            "layout-core-article-bundle",
            "layout-core-book-bundle",
            "layout-core-report-bundle",
            "layout-core-letter-bundle",
        ]
        .into_iter()));
}

#[test]
fn partition_bench_output_identity_across_jobs_1_and_jobs_4() {
    let bench_fixtures = bench_fixtures_root();
    let base_cases = partition_bench_cases(&bench_fixtures);

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
            compare_output_identity: true,
        },
    )
    .with_backend(backend);

    let report = harness.run();

    assert!(
        report.failures.is_empty(),
        "partition bench compilation failed: {:?}",
        report.failures
    );
    assert_eq!(report.results.len(), 2);
    assert!(report
        .results
        .iter()
        .all(|result| result.case.profile == BenchProfile::PartitionBench));
    assert!(report
        .results
        .iter()
        .all(|result| result.case.asset_bundle.is_none()));
    assert!(report
        .results
        .iter()
        .all(|result| result.case.input_fixture.ends_with("multi_section.tex")));

    let sequential = report
        .results
        .iter()
        .find(|result| result.case.jobs == 1)
        .expect("jobs=1 result should exist");
    let parallel = report
        .results
        .iter()
        .find(|result| result.case.jobs == 4)
        .expect("jobs=4 result should exist");

    assert_eq!(sequential.timings.len(), 1);
    assert_eq!(parallel.timings.len(), 1);
    assert_eq!(
        sequential.timings[0].output_hash, parallel.timings[0].output_hash,
        "partition bench output should stay identical across jobs=1 and jobs=4"
    );
}
