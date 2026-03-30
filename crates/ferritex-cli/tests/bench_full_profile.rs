use std::{path::PathBuf, str};

use ferritex_bench::{
    bench_fixtures_root, full_bench_cases, BenchCase, BenchHarness, BenchProfile, BenchRunConfig,
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

fn stage_full_bench_cases() -> (tempfile::TempDir, Vec<BenchCase>) {
    let bench_fixtures = bench_fixtures_root();
    let base_cases = full_bench_cases(&bench_fixtures);
    let bench_dir = bench_fixtures.join("bench");
    let temp_dir = tempfile::tempdir().expect("create tempdir");

    std::fs::copy(
        bench_dir.join("pixel.png"),
        temp_dir.path().join("pixel.png"),
    )
    .expect("copy pixel asset to temp dir");

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

    (temp_dir, cases)
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn count_pdf_pages(pdf_bytes: &[u8]) -> usize {
    count_occurrences(pdf_bytes, b"/Type /Page")
        .saturating_sub(count_occurrences(pdf_bytes, b"/Type /Pages"))
}

#[test]
fn full_bench_compiles_with_jobs_1_and_jobs_4() {
    let (_temp_dir, cases) = stage_full_bench_cases();

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
        "full bench compilation failed: {:?}",
        report.failures
    );
    assert_eq!(report.results.len(), 2);
    assert!(report
        .results
        .iter()
        .all(|result| result.case.profile == BenchProfile::FullBench));
    assert!(report
        .results
        .iter()
        .all(|result| !result.timings.is_empty()));
    assert!(report
        .results
        .iter()
        .flat_map(|result| result.timings.iter())
        .all(|timing| timing.output_hash.is_some()));

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
        "full bench output should stay identical across jobs=1 and jobs=4"
    );
}

#[test]
fn full_bench_produces_at_least_100_pages() {
    let (_temp_dir, cases) = stage_full_bench_cases();
    let case = cases
        .into_iter()
        .find(|case| case.jobs == 1)
        .expect("jobs=1 case should exist");

    let backend = CliCompileBackend::new(ferritex_bin());
    let harness = BenchHarness::new(
        vec![case.clone()],
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
        "full bench jobs=1 compilation failed: {:?}",
        report.failures
    );

    let pdf_path = case.input_fixture.with_extension("pdf");
    let pdf_bytes = std::fs::read(&pdf_path)
        .unwrap_or_else(|e| panic!("read output PDF {}: {e}", pdf_path.display()));
    let pdf_text = String::from_utf8_lossy(&pdf_bytes);
    let extracted = extract_pdf_text(&pdf_text);
    let page_count = count_pdf_pages(&pdf_bytes);

    assert!(
        extracted.contains("Benchmark") || pdf_text.contains("Benchmark"),
        "compiled PDF should contain benchmark text"
    );
    assert!(
        page_count >= 100,
        "expected at least 100 pages, got {page_count}"
    );
}

#[test]
fn full_bench_report_captures_timing_in_json() {
    let (_temp_dir, cases) = stage_full_bench_cases();

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
        "full bench compilation failed: {:?}",
        report.failures
    );

    let json: serde_json::Value =
        serde_json::from_str(&report.to_json()).expect("bench report JSON should parse");
    let results = json["results"]
        .as_array()
        .expect("results should be an array");

    assert_eq!(results.len(), 2);

    for result in results {
        let timings = result["timings"]
            .as_array()
            .expect("timings should be an array");
        assert!(!timings.is_empty(), "timings should not be empty");
        assert_eq!(result["case"]["profile"].as_str(), Some("full-bench"));

        for timing in timings {
            let duration = timing["duration"]
                .as_f64()
                .or_else(|| timing["duration"].as_u64().map(|value| value as f64))
                .expect("duration should be numeric");
            let output_hash = timing["output_hash"]
                .as_str()
                .expect("output_hash should be a string");

            assert!(duration > 0.0, "duration should be positive");
            assert!(!output_hash.is_empty(), "output_hash should not be empty");
        }
    }
}
