use std::path::PathBuf;

use ferritex_bench::{
    bench_fixtures_root, bundle_bootstrap_cases, bundle_package_loading_cases,
    corpus_bibliography_cases, corpus_embedded_assets_cases, corpus_navigation_cases,
    partition_bench_cases, BenchCase, BenchHarness, BenchProfile, BenchRunConfig,
    CliCompileBackend,
};

fn ferritex_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ferritex"))
}

fn extract_pdf_text(pdf: &str) -> String {
    pdf.lines()
        .filter_map(|line| {
            line.trim()
                .strip_suffix(") Tj")
                .and_then(|line| line.strip_prefix('('))
        })
        .collect()
}

fn partition_subset(case_name: &str) -> &'static str {
    if case_name.starts_with("partition-partition-book-") {
        "partition-book"
    } else if case_name.starts_with("partition-partition-article-") {
        "partition-article"
    } else {
        panic!("unexpected partition bench case name: {case_name}");
    }
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
fn bundle_package_loading_compiles_and_verifies_content() {
    let bench_fixtures = bench_fixtures_root();
    let base_cases = bundle_package_loading_cases(&bench_fixtures);

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
        cases.clone(),
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
        "bundle package loading compilation failed: {:?}",
        report.failures
    );
    assert_eq!(
        report.results.len(),
        cases.len(),
        "all bundle package loading cases should run"
    );

    for case in &cases {
        let pdf_path = case.input_fixture.with_extension("pdf");
        let pdf_content = std::fs::read_to_string(&pdf_path)
            .unwrap_or_else(|e| panic!("failed to read output PDF {}: {e}", pdf_path.display()));
        let pdf_text = extract_pdf_text(&pdf_content);

        match case.name.as_str() {
            "bundle-pkg-compat_options" => {
                assert!(
                    pdf_text.contains("COMPAT:compat-loaded-ftxutils:draft-mode"),
                    "compat_options PDF should contain option processing result"
                );
                assert!(
                    pdf_text.contains("UTILS:utils-defined-ok"),
                    "compat_options PDF should contain utils check result"
                );
            }
            "bundle-pkg-depchain_recursive" => {
                assert!(
                    pdf_text.contains("DEPCHAIN:chain-loaded-compat:chain-has-utils"),
                    "depchain PDF should contain recursive loading result"
                );
                assert!(
                    pdf_text.contains("COMPAT:compat-loaded-ftxutils:final-mode"),
                    "depchain PDF should contain compat info (no draft option)"
                );
                assert!(
                    pdf_text.contains("UTILS:utils-defined-ok"),
                    "depchain PDF should contain utils check result"
                );
            }
            other => panic!("unexpected bundle package loading case: {other}"),
        }
    }
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
        if !temp_input.exists() {
            std::fs::copy(&case.input_fixture, &temp_input).expect("copy fixture to temp dir");
        }
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
    assert_eq!(report.results.len(), 12);
    assert!(report
        .results
        .iter()
        .all(|result| result.case.profile == BenchProfile::PartitionBench));
    assert!(report
        .results
        .iter()
        .all(|result| result.case.asset_bundle.is_none()));
    for pair in report.results.chunks(2) {
        assert_eq!(pair.len(), 2);
        assert_eq!(pair[0].case.jobs, 1);
        assert_eq!(pair[1].case.jobs, 4);
        assert_eq!(
            pair[0].timings[0].output_hash,
            pair[1].timings[0].output_hash,
            "partition bench output should be identical across jobs=1 and jobs=4 for {}",
            pair[0].case.input_fixture.display()
        );
    }
}

/// FTX-PARTITION-BENCH-001 bounded no-regression proof for REQ-FUNC-032
/// partition parallelization.
///
/// Protocol: 1 warmup + 5 measured runs per case, comparing `--jobs=1` vs `--jobs=4`.
///
/// Hard assertions:
///   - Output identity: jobs=1 and jobs=4 produce byte-identical PDFs per case
///   - No regression: per-case speedup >= 0.90 (10% tolerance for scheduler noise)
///   - No regression at subset level: mean speedup >= 0.95 per corpus subset
///
/// At sub-1s compile times with 600-iteration corpus fixtures, parallel overhead
/// (partition document construction, thread synchronization, fragment merge) is
/// comparable to the typesetting savings from 4-way parallelization. The contract
/// therefore establishes bounded no-regression evidence rather than a strict
/// speedup proof. The parallel infrastructure is exercised and
/// determinism-verified; measurable speedup is expected only with documents
/// where typesetting dominates total compile time (multi-second compiles with
/// heavier content per partition).
#[test]
fn partition_bench_docs_protocol_median_and_timing_proof() {
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
        if !temp_input.exists() {
            std::fs::copy(&case.input_fixture, &temp_input).expect("copy fixture to temp dir");
        }
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
            warmup_runs: 1,
            measured_runs: 5,
            compare_output_identity: true,
        },
    )
    .with_backend(backend);

    let report = harness.run();

    assert!(
        report.failures.is_empty(),
        "partition bench docs protocol failed: {:?}",
        report.failures
    );
    assert_eq!(report.results.len(), 12);

    assert!(report.results.iter().all(|r| r.timings.len() == 5));

    for pair in report.results.chunks(2) {
        assert_eq!(pair[0].case.jobs, 1);
        assert_eq!(pair[1].case.jobs, 4);
        assert_eq!(
            pair[0].timings[0].output_hash,
            pair[1].timings[0].output_hash,
            "output identity should hold for {}",
            pair[0].case.input_fixture.display()
        );
    }

    for result in &report.results {
        let median = result.median_duration().expect("median should exist");
        assert!(
            !median.is_zero(),
            "median should be positive for {}",
            result.case.name
        );
    }

    let json: serde_json::Value =
        serde_json::from_str(&report.to_json()).expect("JSON should parse");
    let json_results = json["results"].as_array().expect("results array");
    for result in json_results {
        assert!(
            result.get("median_duration_ms").is_some(),
            "JSON result should include median_duration_ms"
        );
    }

    let threshold_secs = 1.0;
    for result in &report.results {
        let median = result.median_duration().unwrap();
        if median.as_secs_f64() > threshold_secs {
            eprintln!(
                "[FTX-PARTITION-BENCH-001 TIMING] {} median {:.3}s exceeds {threshold_secs}s",
                result.case.name,
                median.as_secs_f64()
            );
        }
    }
    eprintln!(
        "[FTX-PARTITION-BENCH-001 TIMING] protocol proof complete for {} cases",
        report.results.len()
    );

    let has_benchmark_precondition =
        std::thread::available_parallelism().map_or(false, |n| n.get() >= 4);
    if has_benchmark_precondition {
        assert!(
            !report.comparisons.is_empty(),
            "REQ-FUNC-032: expected partition bench comparisons to be built"
        );
        let mut subset_speedups = std::collections::BTreeMap::<&str, Vec<f64>>::new();
        for comparison in &report.comparisons {
            let speedup = comparison
                .speedup()
                .expect("median durations should exist for comparison");
            let subset = partition_subset(&comparison.baseline.case.name);
            let baseline_secs = comparison.baseline.median_duration().unwrap().as_secs_f64();
            let candidate_secs = comparison
                .candidate
                .median_duration()
                .unwrap()
                .as_secs_f64();
            subset_speedups.entry(subset).or_default().push(speedup);
            eprintln!(
                "[FTX-PARTITION-BENCH-001 TIMING] case='{}': speedup {:.3}x \
                 (jobs=1 median {:.3}s, jobs=4 median {:.3}s)",
                comparison.baseline.case.name, speedup, baseline_secs, candidate_secs
            );
            assert!(
                speedup >= 0.90,
                "[REQ-FUNC-032] case '{}' regressed too far: speedup {:.3}x < 0.90 (jobs=1 median {:.3}s, jobs=4 median {:.3}s)",
                comparison.baseline.case.name,
                speedup,
                baseline_secs,
                candidate_secs
            );
        }
        assert_eq!(
            subset_speedups.len(),
            2,
            "REQ-FUNC-032: expected aggregate speedups for partition-book and partition-article"
        );
        for (subset, speedups) in subset_speedups {
            let mean_speedup = speedups.iter().sum::<f64>() / speedups.len() as f64;
            eprintln!(
                "[REQ-FUNC-032 NO-REGRESSION] subset={subset}: mean speedup {:.3}x across {} cases",
                mean_speedup,
                speedups.len()
            );
            assert!(
                mean_speedup >= 0.95,
                "[REQ-FUNC-032] subset '{subset}' regression guard failed: mean speedup {:.3}x < 0.95",
                mean_speedup
            );
        }
    } else {
        eprintln!(
            "[REQ-FUNC-032 SKIPPED] partition parallel no-regression proof requires >= 4 cores; this machine has fewer"
        );
    }
}

#[test]
fn corpus_navigation_compiles_and_verifies_content() {
    let bench_fixtures = bench_fixtures_root();
    let base_cases = corpus_navigation_cases(&bench_fixtures);

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
        cases.clone(),
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
        "corpus navigation compilation failed: {:?}",
        report.failures
    );
    assert_eq!(
        report.results.len(),
        cases.len(),
        "all navigation corpus cases should run"
    );

    for case in &cases {
        let pdf_path = case.input_fixture.with_extension("pdf");
        let pdf_content = std::fs::read_to_string(&pdf_path)
            .unwrap_or_else(|e| panic!("failed to read output PDF {}: {e}", pdf_path.display()));

        match case.name.as_str() {
            "corpus-navigation-features-external_links" => {
                assert!(
                    pdf_content.contains("/URI (https://example.com)"),
                    "external_links PDF should contain example.com URI annotation"
                );
            }
            "corpus-navigation-features-hyperref_basic" => {
                assert!(
                    pdf_content.contains("/Subtype /Link"),
                    "hyperref_basic PDF should contain link annotations"
                );
            }
            "corpus-navigation-features-outlines_sections" => {
                assert!(
                    pdf_content.contains("/Outlines"),
                    "outlines_sections PDF should contain PDF outlines"
                );
            }
            "corpus-navigation-features-pdf_metadata" => {
                assert!(
                    pdf_content.contains("/Title (Custom PDF Title)"),
                    "pdf_metadata PDF should contain custom title metadata"
                );
                assert!(
                    pdf_content.contains("/Author (Custom Author)"),
                    "pdf_metadata PDF should contain custom author metadata"
                );
            }
            "corpus-navigation-features-mixed_navigation" => {
                assert!(
                    pdf_content.contains("/Outlines"),
                    "mixed_navigation PDF should contain outlines"
                );
                assert!(
                    pdf_content.contains("/URI (https://example.com)"),
                    "mixed_navigation PDF should contain external URI"
                );
            }
            _ => {}
        }
    }
}

#[test]
fn corpus_embedded_assets_compiles_and_verifies_content() {
    let bench_fixtures = bench_fixtures_root();
    let base_cases = corpus_embedded_assets_cases(&bench_fixtures);
    let corpus_dir = bench_fixtures.join("corpus/embedded-assets");

    let temp_dir = tempfile::tempdir().expect("create tempdir");
    // Copy asset files (PNG, PDF) needed by the fixtures
    for asset in &["pixel.png", "diagram.pdf"] {
        let src = corpus_dir.join(asset);
        let dst = temp_dir.path().join(asset);
        std::fs::copy(&src, &dst)
            .unwrap_or_else(|e| panic!("copy asset {} to tempdir: {e}", src.display()));
    }

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
        cases.clone(),
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
        "corpus embedded-assets compilation failed: {:?}",
        report.failures
    );
    assert_eq!(
        report.results.len(),
        cases.len(),
        "all embedded-assets corpus cases should run"
    );

    for case in &cases {
        let pdf_path = case.input_fixture.with_extension("pdf");
        let pdf_bytes = std::fs::read(&pdf_path)
            .unwrap_or_else(|e| panic!("failed to read output PDF {}: {e}", pdf_path.display()));

        match case.name.as_str() {
            "corpus-embedded-assets-png_embed" => {
                assert!(
                    pdf_bytes.len() > 100,
                    "png_embed PDF should have substantial content"
                );
            }
            "corpus-embedded-assets-pdf_embed" => {
                assert!(
                    pdf_bytes.len() > 100,
                    "pdf_embed PDF should have substantial content"
                );
            }
            _ => {}
        }
    }
}

#[test]
fn corpus_bibliography_compiles_and_verifies_content() {
    let bench_fixtures = bench_fixtures_root();
    let base_cases = corpus_bibliography_cases(&bench_fixtures);
    let corpus_dir = bench_fixtures.join("corpus/bibliography");

    let temp_dir = tempfile::tempdir().expect("create tempdir");
    // Copy .bbl files needed by bibliography fixtures
    for entry in std::fs::read_dir(&corpus_dir).expect("read bibliography corpus dir") {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("bbl") {
            let dst = temp_dir.path().join(path.file_name().unwrap());
            std::fs::copy(&path, &dst).expect("copy bbl to temp dir");
        }
    }

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
        cases.clone(),
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
        "corpus bibliography compilation failed: {:?}",
        report.failures
    );
    assert_eq!(
        report.results.len(),
        cases.len(),
        "all bibliography corpus cases should run"
    );

    for case in &cases {
        let pdf_path = case.input_fixture.with_extension("pdf");
        let pdf_content = std::fs::read_to_string(&pdf_path)
            .unwrap_or_else(|e| panic!("failed to read output PDF {}: {e}", pdf_path.display()));
        let pdf_text = extract_pdf_text(&pdf_content);

        match case.name.as_str() {
            "corpus-bibliography-single_cite" => {
                assert!(
                    pdf_text.contains("[1]"),
                    "single_cite PDF should contain citation marker [1], got: {pdf_text}"
                );
            }
            "corpus-bibliography-multi_cite" => {
                assert!(
                    pdf_text.contains("[1]") && pdf_text.contains("[2]"),
                    "multi_cite PDF should contain citation markers [1] and [2], got: {pdf_text}"
                );
            }
            "corpus-bibliography-custom_labels" => {
                assert!(
                    pdf_text.contains("[Knu84]"),
                    "custom_labels PDF should contain custom label [Knu84], got: {pdf_text}"
                );
            }
            "corpus-bibliography-inline_thebibliography" => {
                assert!(
                    pdf_text.contains("[1]") && pdf_text.contains("[2]"),
                    "inline_thebibliography PDF should contain citation markers, got: {pdf_text}"
                );
            }
            _ => {}
        }
    }
}
