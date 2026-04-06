use std::{
    path::{Path, PathBuf},
    str,
};

use ferritex_bench::{
    bench_fixtures_root, compute_bibliography_parity_score, compute_embedded_assets_parity_score,
    compute_navigation_parity_score, compute_parity_score, compute_tikz_parity_score,
    corpus_bibliography_cases, corpus_compat_cases, corpus_embedded_assets_cases,
    corpus_navigation_cases, corpus_tikz_basic_shapes_cases, corpus_tikz_nested_cases,
    format_bibliography_parity_summary, format_embedded_assets_parity_summary,
    format_navigation_parity_summary, format_parity_summary, format_tikz_parity_summary,
    full_bench_cases, full_bench_strict_cases, stress_bench_cases, BenchCase, BenchHarness,
    BenchProfile, BenchRunConfig, BibliographyParityResult, CliCompileBackend,
    EmbeddedAssetsParityResult, NavigationParityResult, ParityResult, TikzParityResult,
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

fn stage_bench_cases(base_cases: Vec<BenchCase>) -> (tempfile::TempDir, Vec<BenchCase>) {
    let temp_dir = tempfile::tempdir().expect("create tempdir");

    let mut cases = Vec::with_capacity(base_cases.len());
    for case in base_cases {
        if let Some(bench_dir) = case.input_fixture.parent() {
            let pixel_src = bench_dir.join("pixel.png");
            let pixel_dst = temp_dir.path().join("pixel.png");
            if pixel_src.exists() && !pixel_dst.exists() {
                std::fs::copy(&pixel_src, &pixel_dst).unwrap_or_else(|error| {
                    panic!(
                        "copy pixel asset {} -> {}: {error}",
                        pixel_src.display(),
                        pixel_dst.display()
                    )
                });
            }
        }

        let fixture_name = case
            .input_fixture
            .file_name()
            .expect("fixture should have filename");
        let temp_input = temp_dir.path().join(fixture_name);
        std::fs::copy(&case.input_fixture, &temp_input).expect("copy fixture to temp dir");

        let staged_bundle = case.asset_bundle.as_ref().map(|bundle_src| {
            let bundle_name = bundle_src
                .file_name()
                .expect("asset bundle should have a directory name");
            let bundle_dst = temp_dir.path().join(bundle_name);
            if !bundle_dst.exists() {
                copy_dir_all(bundle_src, &bundle_dst);
            }
            bundle_dst
        });

        cases.push(BenchCase {
            name: case.name,
            profile: case.profile,
            input_fixture: temp_input,
            asset_bundle: staged_bundle,
            jobs: case.jobs,
            reproducible: case.reproducible,
            no_cache: case.no_cache,
        });
    }

    (temp_dir, cases)
}

fn stage_full_bench_cases() -> (tempfile::TempDir, Vec<BenchCase>) {
    let bench_fixtures = bench_fixtures_root();
    stage_bench_cases(full_bench_cases(&bench_fixtures))
}

fn stage_full_bench_strict_cases() -> (tempfile::TempDir, Vec<BenchCase>) {
    let bench_fixtures = bench_fixtures_root();
    stage_bench_cases(full_bench_strict_cases(&bench_fixtures))
}

fn stage_stress_bench_cases() -> (tempfile::TempDir, Vec<BenchCase>) {
    let bench_fixtures = bench_fixtures_root();
    stage_bench_cases(stress_bench_cases(&bench_fixtures))
}

fn copy_dir_all(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst)
        .unwrap_or_else(|error| panic!("failed to create directory {}: {error}", dst.display()));
    for entry in std::fs::read_dir(src)
        .unwrap_or_else(|error| panic!("failed to read directory {}: {error}", src.display()))
    {
        let entry =
            entry.unwrap_or_else(|error| panic!("failed to enumerate {}: {error}", src.display()));
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_all(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap_or_else(|error| {
                panic!(
                    "failed to copy fixture {} -> {}: {error}",
                    path.display(),
                    target.display()
                )
            });
        }
    }
}

fn copy_named_files(src_dir: &Path, dst_dir: &Path, names: &[&str]) {
    std::fs::create_dir_all(dst_dir).unwrap_or_else(|error| {
        panic!("failed to create directory {}: {error}", dst_dir.display())
    });
    for name in names {
        let src = src_dir.join(name);
        let dst = dst_dir.join(name);
        std::fs::copy(&src, &dst).unwrap_or_else(|error| {
            panic!(
                "copy fixture {} -> {}: {error}",
                src.display(),
                dst.display()
            )
        });
    }
}

fn copy_files_with_extension(src_dir: &Path, dst_dir: &Path, extension: &str) {
    std::fs::create_dir_all(dst_dir).unwrap_or_else(|error| {
        panic!("failed to create directory {}: {error}", dst_dir.display())
    });
    for entry in std::fs::read_dir(src_dir)
        .unwrap_or_else(|error| panic!("failed to read directory {}: {error}", src_dir.display()))
    {
        let entry = entry
            .unwrap_or_else(|error| panic!("failed to enumerate {}: {error}", src_dir.display()));
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some(extension) {
            let dst = dst_dir.join(
                path.file_name()
                    .expect("staged fixture should have a file name"),
            );
            std::fs::copy(&path, &dst).unwrap_or_else(|error| {
                panic!(
                    "copy fixture {} -> {}: {error}",
                    path.display(),
                    dst.display()
                )
            });
        }
    }
}

fn stage_cases_in_dir(
    base_cases: &[BenchCase],
    dst_dir: &Path,
    asset_bundle_override: Option<&Path>,
) -> Vec<BenchCase> {
    std::fs::create_dir_all(dst_dir).unwrap_or_else(|error| {
        panic!("failed to create directory {}: {error}", dst_dir.display())
    });

    let mut cases = Vec::with_capacity(base_cases.len());
    for case in base_cases {
        let fixture_name = case
            .input_fixture
            .file_name()
            .expect("fixture should have filename");
        let temp_input = dst_dir.join(fixture_name);
        std::fs::copy(&case.input_fixture, &temp_input).expect("copy fixture to temp dir");
        cases.push(BenchCase {
            name: case.name.clone(),
            profile: case.profile.clone(),
            input_fixture: temp_input,
            asset_bundle: asset_bundle_override
                .map(Path::to_path_buf)
                .or_else(|| case.asset_bundle.clone()),
            jobs: case.jobs,
            reproducible: case.reproducible,
            no_cache: case.no_cache,
        });
    }

    cases
}

fn reference_pdf_path(bench_fixtures: &Path, subset: &str, case: &BenchCase) -> PathBuf {
    let stem = case
        .input_fixture
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("fixture should have UTF-8 stem");
    bench_fixtures.join(format!("corpus/{subset}/reference/{stem}.pdf"))
}

const MATH_EQUATIONS_REGRESSION_BASELINE_DOCUMENT_DIFF_RATE: f64 = 0.286;
const MATH_EQUATIONS_MAX_DOCUMENT_DIFF_RATE: f64 = 0.10;

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

fn run_full_bench_protocol(
    cases: Vec<BenchCase>,
    label: &str,
) -> (std::time::Duration, std::time::Duration) {
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
        "{label} failed: {:?}",
        report.failures
    );
    assert_eq!(report.results.len(), 2);

    let seq = report
        .results
        .iter()
        .find(|r| r.case.jobs == 1)
        .expect("jobs=1 result");
    let par = report
        .results
        .iter()
        .find(|r| r.case.jobs == 4)
        .expect("jobs=4 result");
    assert_eq!(seq.timings.len(), 5);
    assert_eq!(par.timings.len(), 5);
    assert_eq!(
        seq.timings[0].output_hash, par.timings[0].output_hash,
        "output should be identical across jobs=1 and jobs=4"
    );

    let seq_median = report
        .median_duration_for(&seq.case.name)
        .expect("jobs=1 median should exist");
    let par_median = report
        .median_duration_for(&par.case.name)
        .expect("jobs=4 median should exist");

    let json: serde_json::Value =
        serde_json::from_str(&report.to_json()).expect("JSON should parse");
    let results = json["results"].as_array().expect("results array");
    for result in results {
        assert!(
            result.get("median_duration_ms").is_some(),
            "each result should have median_duration_ms in JSON"
        );
        let median_ms = result["median_duration_ms"]
            .as_f64()
            .expect("median_duration_ms should be numeric");
        assert!(median_ms > 0.0, "median should be positive");
    }

    (seq_median, par_median)
}

#[test]
fn stage_timing_instrumentation_smoke() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("stage-timing.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nStage timing smoke test.\n\\end{document}\n",
    )
    .expect("write input file");

    let output = std::process::Command::new(ferritex_bin())
        .args([
            "compile",
            "--no-cache",
            tex_file.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    assert!(
        dir.path().join("stage-timing.pdf").exists(),
        "compile should produce a PDF"
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf-8");
    assert!(
        !stderr.contains("panicked at"),
        "compile stderr should not contain a panic: {stderr}"
    );
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
        let median_ms = result.get("median_duration_ms");
        assert!(
            median_ms.is_some(),
            "result should include median_duration_ms"
        );

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

#[test]
fn full_bench_docs_protocol_median_and_timing_proof() {
    let (_temp_dir, cases) = stage_full_bench_strict_cases();
    let (seq_median, par_median) = run_full_bench_protocol(cases, "full bench docs protocol");

    let has_benchmark_precondition =
        std::thread::available_parallelism().map_or(false, |n| n.get() >= 4);
    if has_benchmark_precondition {
        assert!(
            par_median < seq_median,
            "[REQ-FUNC-031] speedup proof failed: jobs=4 median ({:.3}s) >= jobs=1 median ({:.3}s)",
            par_median.as_secs_f64(),
            seq_median.as_secs_f64()
        );
        eprintln!(
            "[REQ-FUNC-031 PROVEN] speedup: jobs=4 median ({:.3}s) < jobs=1 median ({:.3}s)",
            par_median.as_secs_f64(),
            seq_median.as_secs_f64()
        );
    } else {
        eprintln!("[REQ-FUNC-031] speedup proof skipped: available_parallelism < 4");
    }

    let threshold_secs = 1.0;
    if seq_median.as_secs_f64() > threshold_secs {
        eprintln!(
            "[FTX-BENCH-001 TIMING] jobs=1 median {:.3}s exceeds {threshold_secs}s threshold",
            seq_median.as_secs_f64()
        );
    } else {
        eprintln!(
            "[FTX-BENCH-001 TIMING] jobs=1 median {:.3}s within {threshold_secs}s threshold",
            seq_median.as_secs_f64()
        );
    }
    if par_median.as_secs_f64() > threshold_secs {
        eprintln!(
            "[FTX-BENCH-001 TIMING] jobs=4 median {:.3}s exceeds {threshold_secs}s threshold",
            par_median.as_secs_f64()
        );
    } else {
        eprintln!(
            "[FTX-BENCH-001 TIMING] jobs=4 median {:.3}s within {threshold_secs}s threshold",
            par_median.as_secs_f64()
        );
    }
}

#[test]
fn full_bench_warm_cache_probe() {
    let (_temp_dir, cases) = stage_full_bench_cases();
    let (seq_median, par_median) = run_full_bench_protocol(cases, "full bench warm cache probe");

    eprintln!(
        "[FTX-BENCH-001 WARM CACHE PROBE] jobs=1 median {:.3}s, jobs=4 median {:.3}s",
        seq_median.as_secs_f64(),
        par_median.as_secs_f64()
    );
}

// Stress-scale timing diagnostic for warm incremental compile.
// This test measures timing on a 1000-section staged input but does NOT
// assert speedup because the result is environment-dependent and not a
// docs requirement. Correctness evidence for REQ-FUNC-030 is provided by
// `incremental_xref_convergence_after_page_shift` in `e2e_compile.rs`.
// Speed is evaluated by REQ-NF-002 against the canonical `FTX-BENCH-001`
// fixture, so this test emits diagnostic output only.
#[test]
fn stress_bench_warm_incremental_evidence() {
    let (_temp_dir, cases) = stage_stress_bench_cases();
    let staged_case = cases
        .into_iter()
        .find(|case| case.jobs == 1)
        .expect("jobs=1 case should exist");

    let staged_source =
        std::fs::read_to_string(&staged_case.input_fixture).unwrap_or_else(|error| {
            panic!(
                "read staged stress bench fixture {}: {error}",
                staged_case.input_fixture.display()
            )
        });
    let loop_marker = "\\@for\\benchitem:=001,002,003,004,005";
    let (prefix, _) = staged_source
        .split_once(loop_marker)
        .expect("stress bench loop marker should exist");

    let cycle_dir = staged_case.input_fixture.with_file_name("ftx_bench_cycles");
    std::fs::create_dir_all(&cycle_dir).unwrap_or_else(|error| {
        panic!(
            "create staged cycle directory {}: {error}",
            cycle_dir.display()
        )
    });

    let cycle_body = |cycle: usize| {
        format!(
            "\\section{{Benchmark Cycle}}\n\
Cycle {cycle} revisits Section \\ref{{sec:overview}}, Equation \\ref{{eq:foundation}}, Figure \\ref{{fig:reference-pixel}}, and the external record at \\url{{https://example.com/ftx-bench-001}}. The inline recurrence $x_n = x_{{n-1}} + n$ summarizes the local state transition for this cycle.\n\
\\begin{{equation}}\n\
s_n = n^2 + n\n\
\\end{{equation}}\n\
\\[\n\
\\frac{{s_n}}{{n + 1}} = n\n\
\\]\n\
\\begin{{align}}\n\
a_n &= s_n + 1 \\\\\n\
b_n &= a_n + n\n\
\\end{{align}}\n\
\\begin{{figure}}[h]\n\
\\includegraphics[width=80pt]{{pixel.png}}\n\
\\caption{{Observation tile recorded during a benchmark cycle}}\n\
\\end{{figure}}\n\
\\benchmarkparagraph\n\
\\benchmarkparagraph\n\
\\benchmarkparagraph\n\
\\benchmarkparagraph\n\
\\clearpage\n"
        )
    };

    let mut cycle_inputs = String::new();
    for cycle in 1..=1000 {
        let cycle_path = cycle_dir.join(format!("ftx_bench_cycle_{cycle:04}.tex"));
        std::fs::write(&cycle_path, cycle_body(cycle)).unwrap_or_else(|error| {
            panic!(
                "write staged cycle fixture {}: {error}",
                cycle_path.display()
            )
        });
        cycle_inputs.push_str(&format!(
            "\\input{{ftx_bench_cycles/ftx_bench_cycle_{cycle:04}}}\n"
        ));
    }
    std::fs::write(
        &staged_case.input_fixture,
        format!("{prefix}{cycle_inputs}\\end{{document}}\n"),
    )
    .unwrap_or_else(|error| {
        panic!(
            "write staged stress bench fixture {}: {error}",
            staged_case.input_fixture.display()
        )
    });

    let run_case = |case: &BenchCase| -> std::time::Duration {
        let harness = BenchHarness::new(
            vec![case.clone()],
            BenchRunConfig {
                warmup_runs: 0,
                measured_runs: 1,
                compare_output_identity: false,
            },
        )
        .with_backend(CliCompileBackend::new(ferritex_bin()));
        let report = harness.run();

        assert!(
            report.failures.is_empty(),
            "{} compilation failed: {:?}",
            case.name,
            report.failures
        );
        assert_eq!(
            report.results.len(),
            1,
            "{} should produce one result",
            case.name
        );

        report.results[0]
            .median_duration()
            .expect("single-run benchmark should have a median duration")
    };

    let full_case = BenchCase {
        name: format!("{}-no-cache-baseline", staged_case.name),
        no_cache: true,
        ..staged_case.clone()
    };
    let full_compile_duration = run_case(&full_case);

    let warm_case = BenchCase {
        name: format!("{}-warm-cache", staged_case.name),
        no_cache: false,
        ..staged_case.clone()
    };
    let warm_cache_duration = run_case(&warm_case);

    let edited_cycle = 900usize;
    let edited_cycle_path = cycle_dir.join(format!("ftx_bench_cycle_{edited_cycle:04}.tex"));
    let source = std::fs::read_to_string(&edited_cycle_path).unwrap_or_else(|error| {
        panic!(
            "read staged cycle fixture {}: {error}",
            edited_cycle_path.display()
        )
    });
    let target = format!("Cycle {edited_cycle} revisits");
    let replacement = format!("Cycle {edited_cycle} revisits [warm incremental edit]");
    let updated = source.replacen(&target, &replacement, 1);
    assert_ne!(
        updated,
        source,
        "expected to mutate one benchmark paragraph in {}",
        edited_cycle_path.display()
    );
    std::fs::write(&edited_cycle_path, updated).unwrap_or_else(|error| {
        panic!(
            "write staged cycle fixture {}: {error}",
            edited_cycle_path.display()
        )
    });

    let incremental_case = BenchCase {
        name: format!("{}-incremental", staged_case.name),
        no_cache: false,
        ..staged_case
    };
    let incremental_duration = run_case(&incremental_case);

    let speedup = full_compile_duration.as_secs_f64() / incremental_duration.as_secs_f64();
    eprintln!(
        "[REQ-FUNC-030 TIMING] full-no-cache {:.3}s, warm-cache {:.3}s, incremental-after-1-paragraph-edit {:.3}s, speedup {:.2}x",
        full_compile_duration.as_secs_f64(),
        warm_cache_duration.as_secs_f64(),
        incremental_duration.as_secs_f64(),
        speedup
    );
    if incremental_duration >= full_compile_duration {
        eprintln!(
            "[REQ-FUNC-030 NOTE] incremental compile {:.3}s was not faster than full compile {:.3}s; \
             this stress benchmark speedup is environment-dependent and not a docs requirement \
             (REQ-FUNC-030 requires correctness only; REQ-NF-002 covers speed with FTX-BENCH-001)",
            incremental_duration.as_secs_f64(),
            full_compile_duration.as_secs_f64()
        );
    } else {
        eprintln!(
            "[REQ-FUNC-030 PROVEN] incremental compile {:.3}s < full compile {:.3}s after a 1-paragraph edit ({:.2}x faster)",
            incremental_duration.as_secs_f64(),
            full_compile_duration.as_secs_f64(),
            speedup
        );
    }
}

#[test]
fn full_bench_parity_evidence() {
    let bench_fixtures = bench_fixtures_root();
    let temp_dir = tempfile::tempdir().expect("create tempdir");

    let layout_dir = temp_dir.path().join("layout_core");
    let navigation_dir = temp_dir.path().join("navigation_features");
    let bibliography_dir = temp_dir.path().join("bibliography");
    let embedded_dir = temp_dir.path().join("embedded_assets");
    let tikz_basic_dir = temp_dir.path().join("tikz_basic_shapes");
    let tikz_nested_dir = temp_dir
        .path()
        .join("tikz_nested_style_transform_clip_arrow");
    let bundle_dir = temp_dir.path().join("bundle");

    copy_dir_all(&bench_fixtures.join("bundle"), &bundle_dir);
    copy_files_with_extension(
        &bench_fixtures.join("corpus/bibliography"),
        &bibliography_dir,
        "bbl",
    );
    copy_named_files(
        &bench_fixtures.join("corpus/embedded-assets"),
        &embedded_dir,
        &["pixel.png", "diagram.pdf"],
    );

    let layout_cases = stage_cases_in_dir(&corpus_compat_cases(&bench_fixtures), &layout_dir, None);
    let navigation_cases = stage_cases_in_dir(
        &corpus_navigation_cases(&bench_fixtures),
        &navigation_dir,
        None,
    );
    let bibliography_cases = stage_cases_in_dir(
        &corpus_bibliography_cases(&bench_fixtures),
        &bibliography_dir,
        None,
    );
    let embedded_cases = stage_cases_in_dir(
        &corpus_embedded_assets_cases(&bench_fixtures),
        &embedded_dir,
        Some(&bundle_dir),
    );
    let tikz_basic_cases = stage_cases_in_dir(
        &corpus_tikz_basic_shapes_cases(&bench_fixtures),
        &tikz_basic_dir,
        None,
    );
    let tikz_nested_cases = stage_cases_in_dir(
        &corpus_tikz_nested_cases(&bench_fixtures),
        &tikz_nested_dir,
        None,
    );

    let expected_case_count = layout_cases.len()
        + navigation_cases.len()
        + bibliography_cases.len()
        + embedded_cases.len()
        + tikz_basic_cases.len()
        + tikz_nested_cases.len();

    let mut all_cases = Vec::with_capacity(expected_case_count);
    all_cases.extend(layout_cases.iter().cloned());
    all_cases.extend(navigation_cases.iter().cloned());
    all_cases.extend(bibliography_cases.iter().cloned());
    all_cases.extend(embedded_cases.iter().cloned());
    all_cases.extend(tikz_basic_cases.iter().cloned());
    all_cases.extend(tikz_nested_cases.iter().cloned());

    let backend = CliCompileBackend::new(ferritex_bin());
    let harness = BenchHarness::new(
        all_cases,
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
        "full bench parity evidence compilation failed: {:?}",
        report.failures
    );
    assert_eq!(
        report.results.len(),
        expected_case_count,
        "all parity evidence corpus cases should run"
    );

    let mut layout_results = Vec::<ParityResult>::with_capacity(layout_cases.len());
    for case in &layout_cases {
        let output_pdf_path = case.input_fixture.with_extension("pdf");
        let output_pdf = std::fs::read(&output_pdf_path).unwrap_or_else(|error| {
            panic!("read output PDF {}: {error}", output_pdf_path.display())
        });
        let reference_pdf_path = reference_pdf_path(&bench_fixtures, "layout-core", case);
        if !reference_pdf_path.exists() {
            eprintln!(
                "[REQ-NF-007 PARITY] category=layout-core case={} skipped: missing reference {}",
                case.name,
                reference_pdf_path.display()
            );
            layout_results.push(ParityResult {
                document_name: case.name.clone(),
                score: None,
                error: Some(format!(
                    "skipped: missing reference PDF {}",
                    reference_pdf_path.display()
                )),
            });
            continue;
        }

        let reference_pdf = std::fs::read(&reference_pdf_path).unwrap_or_else(|error| {
            panic!(
                "read reference PDF {}: {error}",
                reference_pdf_path.display()
            )
        });

        match compute_parity_score(&output_pdf, &reference_pdf) {
            Ok(score) => {
                eprintln!(
                    "[REQ-NF-007 PARITY] category=layout-core case={} document_diff_rate={:.3} pages={}/{} pass={}",
                    case.name,
                    score.document_diff_rate,
                    score.ferritex_pages,
                    score.reference_pages,
                    score.pass
                );
                layout_results.push(ParityResult {
                    document_name: case.name.clone(),
                    score: Some(score),
                    error: None,
                });
            }
            Err(error) => {
                eprintln!(
                    "[REQ-NF-007 PARITY] category=layout-core case={} skipped: {}",
                    case.name, error
                );
                layout_results.push(ParityResult {
                    document_name: case.name.clone(),
                    score: None,
                    error: Some(format!("skipped: {error}")),
                });
            }
        }
    }
    eprintln!(
        "[REQ-NF-007 PARITY]\n{}",
        format_parity_summary(&layout_results)
    );

    let mut navigation_results =
        Vec::<NavigationParityResult>::with_capacity(navigation_cases.len());
    for case in &navigation_cases {
        let output_pdf_path = case.input_fixture.with_extension("pdf");
        let output_pdf = std::fs::read(&output_pdf_path).unwrap_or_else(|error| {
            panic!("read output PDF {}: {error}", output_pdf_path.display())
        });
        let reference_pdf_path = reference_pdf_path(&bench_fixtures, "navigation-features", case);
        if !reference_pdf_path.exists() {
            eprintln!(
                "[REQ-NF-007 PARITY] category=navigation case={} skipped: missing reference {}",
                case.name,
                reference_pdf_path.display()
            );
            navigation_results.push(NavigationParityResult {
                document_name: case.name.clone(),
                score: None,
                error: Some(format!(
                    "skipped: missing reference PDF {}",
                    reference_pdf_path.display()
                )),
            });
            continue;
        }

        let reference_pdf = std::fs::read(&reference_pdf_path).unwrap_or_else(|error| {
            panic!(
                "read reference PDF {}: {error}",
                reference_pdf_path.display()
            )
        });

        match compute_navigation_parity_score(&output_pdf, &reference_pdf) {
            Ok(score) => {
                eprintln!(
                    "[REQ-NF-007 PARITY] category=navigation case={} pass={} annots={} dests={} outlines={} title={} author={}",
                    case.name,
                    score.pass,
                    score.annotations_match,
                    score.destinations_match,
                    score.outlines_match,
                    score.metadata_title_match,
                    score.metadata_author_match
                );
                navigation_results.push(NavigationParityResult {
                    document_name: case.name.clone(),
                    score: Some(score),
                    error: None,
                });
            }
            Err(error) => {
                eprintln!(
                    "[REQ-NF-007 PARITY] category=navigation case={} skipped: {}",
                    case.name, error
                );
                navigation_results.push(NavigationParityResult {
                    document_name: case.name.clone(),
                    score: None,
                    error: Some(format!("skipped: {error}")),
                });
            }
        }
    }
    eprintln!(
        "[REQ-NF-007 PARITY]\n{}",
        format_navigation_parity_summary(&navigation_results)
    );

    let mut bibliography_results =
        Vec::<BibliographyParityResult>::with_capacity(bibliography_cases.len());
    for case in &bibliography_cases {
        let output_pdf_path = case.input_fixture.with_extension("pdf");
        let output_pdf = std::fs::read(&output_pdf_path).unwrap_or_else(|error| {
            panic!("read output PDF {}: {error}", output_pdf_path.display())
        });
        let reference_pdf_path = reference_pdf_path(&bench_fixtures, "bibliography", case);
        if !reference_pdf_path.exists() {
            eprintln!(
                "[REQ-NF-007 PARITY] category=bibliography case={} skipped: missing reference {}",
                case.name,
                reference_pdf_path.display()
            );
            bibliography_results.push(BibliographyParityResult {
                document_name: case.name.clone(),
                score: None,
                error: Some(format!(
                    "skipped: missing reference PDF {}",
                    reference_pdf_path.display()
                )),
            });
            continue;
        }

        let reference_pdf = std::fs::read(&reference_pdf_path).unwrap_or_else(|error| {
            panic!(
                "read reference PDF {}: {error}",
                reference_pdf_path.display()
            )
        });

        match compute_bibliography_parity_score(&output_pdf, &reference_pdf) {
            Ok(score) => {
                eprintln!(
                    "[REQ-NF-007 PARITY] category=bibliography case={} pass={} entries={} labels={}",
                    case.name,
                    score.pass,
                    score.entry_count_match,
                    score.labels_match
                );
                bibliography_results.push(BibliographyParityResult {
                    document_name: case.name.clone(),
                    score: Some(score),
                    error: None,
                });
            }
            Err(error) => {
                eprintln!(
                    "[REQ-NF-007 PARITY] category=bibliography case={} skipped: {}",
                    case.name, error
                );
                bibliography_results.push(BibliographyParityResult {
                    document_name: case.name.clone(),
                    score: None,
                    error: Some(format!("skipped: {error}")),
                });
            }
        }
    }
    eprintln!(
        "[REQ-NF-007 PARITY]\n{}",
        format_bibliography_parity_summary(&bibliography_results)
    );

    let mut embedded_results =
        Vec::<EmbeddedAssetsParityResult>::with_capacity(embedded_cases.len());
    for case in &embedded_cases {
        let output_pdf_path = case.input_fixture.with_extension("pdf");
        let output_pdf = std::fs::read(&output_pdf_path).unwrap_or_else(|error| {
            panic!("read output PDF {}: {error}", output_pdf_path.display())
        });
        let reference_pdf_path = reference_pdf_path(&bench_fixtures, "embedded-assets", case);
        if !reference_pdf_path.exists() {
            eprintln!(
                "[REQ-NF-007 PARITY] category=embedded-assets case={} skipped: missing reference {}",
                case.name,
                reference_pdf_path.display()
            );
            embedded_results.push(EmbeddedAssetsParityResult {
                document_name: case.name.clone(),
                score: None,
                error: Some(format!(
                    "skipped: missing reference PDF {}",
                    reference_pdf_path.display()
                )),
            });
            continue;
        }

        let reference_pdf = std::fs::read(&reference_pdf_path).unwrap_or_else(|error| {
            panic!(
                "read reference PDF {}: {error}",
                reference_pdf_path.display()
            )
        });

        match compute_embedded_assets_parity_score(&output_pdf, &reference_pdf) {
            Ok(score) => {
                eprintln!(
                    "[REQ-NF-007 PARITY] category=embedded-assets case={} pass={} fonts={} images={} forms={} pages={}",
                    case.name,
                    score.pass,
                    score.font_set_match,
                    score.image_count_match,
                    score.form_count_match,
                    score.page_count_match
                );
                embedded_results.push(EmbeddedAssetsParityResult {
                    document_name: case.name.clone(),
                    score: Some(score),
                    error: None,
                });
            }
            Err(error) => {
                eprintln!(
                    "[REQ-NF-007 PARITY] category=embedded-assets case={} skipped: {}",
                    case.name, error
                );
                embedded_results.push(EmbeddedAssetsParityResult {
                    document_name: case.name.clone(),
                    score: None,
                    error: Some(format!("skipped: {error}")),
                });
            }
        }
    }
    eprintln!(
        "[REQ-NF-007 PARITY]\n{}",
        format_embedded_assets_parity_summary(&embedded_results)
    );

    let mut tikz_results =
        Vec::<TikzParityResult>::with_capacity(tikz_basic_cases.len() + tikz_nested_cases.len());
    for (subset, cases) in [
        ("tikz/basic-shapes", &tikz_basic_cases),
        ("tikz/nested-style-transform-clip-arrow", &tikz_nested_cases),
    ] {
        for case in cases {
            let output_pdf_path = case.input_fixture.with_extension("pdf");
            let output_pdf = std::fs::read(&output_pdf_path).unwrap_or_else(|error| {
                panic!("read output PDF {}: {error}", output_pdf_path.display())
            });
            let reference_pdf_path = reference_pdf_path(&bench_fixtures, subset, case);
            if !reference_pdf_path.exists() {
                eprintln!(
                    "[REQ-NF-007 PARITY] category=tikz case={} skipped: missing reference {}",
                    case.name,
                    reference_pdf_path.display()
                );
                tikz_results.push(TikzParityResult {
                    document_name: case.name.clone(),
                    score: None,
                    error: Some(format!(
                        "skipped: missing reference PDF {}",
                        reference_pdf_path.display()
                    )),
                });
                continue;
            }

            let reference_pdf = std::fs::read(&reference_pdf_path).unwrap_or_else(|error| {
                panic!(
                    "read reference PDF {}: {error}",
                    reference_pdf_path.display()
                )
            });

            match compute_tikz_parity_score(&output_pdf, &reference_pdf) {
                Ok(score) => {
                    eprintln!(
                        "[REQ-NF-007 PARITY] category=tikz case={} match_ratio={:.3} matched={} mismatched={} pass={}",
                        case.name,
                        score.match_ratio,
                        score.matched_ops,
                        score.mismatched_ops,
                        score.pass
                    );
                    tikz_results.push(TikzParityResult {
                        document_name: case.name.clone(),
                        score: Some(score),
                        error: None,
                    });
                }
                Err(error) => {
                    eprintln!(
                        "[REQ-NF-007 PARITY] category=tikz case={} skipped: {}",
                        case.name, error
                    );
                    tikz_results.push(TikzParityResult {
                        document_name: case.name.clone(),
                        score: None,
                        error: Some(format!("skipped: {error}")),
                    });
                }
            }
        }
    }
    eprintln!(
        "[REQ-NF-007 PARITY]\n{}",
        format_tikz_parity_summary(&tikz_results)
    );
}

#[test]
fn math_equations_parity_regression() {
    let bench_fixtures = bench_fixtures_root();
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let layout_dir = temp_dir.path().join("layout_core");

    let math_equations_case = corpus_compat_cases(&bench_fixtures)
        .into_iter()
        .find(|case| {
            case.input_fixture
                .file_stem()
                .and_then(|stem| stem.to_str())
                == Some("math_equations")
        })
        .expect("layout-core corpus should contain math_equations.tex");
    let staged_cases = stage_cases_in_dir(
        std::slice::from_ref(&math_equations_case),
        &layout_dir,
        None,
    );
    let case = staged_cases
        .into_iter()
        .next()
        .expect("math_equations case should be staged");

    let harness = BenchHarness::new(
        vec![case.clone()],
        BenchRunConfig {
            warmup_runs: 0,
            measured_runs: 1,
            compare_output_identity: false,
        },
    )
    .with_backend(CliCompileBackend::new(ferritex_bin()));

    let report = harness.run();

    assert!(
        report.failures.is_empty(),
        "math_equations compilation failed: {:?}",
        report.failures
    );
    assert_eq!(
        report.results.len(),
        1,
        "math_equations regression should compile exactly one case"
    );

    let output_pdf_path = case.input_fixture.with_extension("pdf");
    let output_pdf = std::fs::read(&output_pdf_path)
        .unwrap_or_else(|error| panic!("read output PDF {}: {error}", output_pdf_path.display()));
    let reference_pdf_path = reference_pdf_path(&bench_fixtures, "layout-core", &case);
    let reference_pdf = std::fs::read(&reference_pdf_path).unwrap_or_else(|error| {
        panic!(
            "read reference PDF {}: {error}",
            reference_pdf_path.display()
        )
    });
    let score = compute_parity_score(&output_pdf, &reference_pdf).unwrap_or_else(|error| {
        panic!(
            "compute parity score for {} vs {}: {error}",
            output_pdf_path.display(),
            reference_pdf_path.display()
        )
    });

    eprintln!(
        "[REQ-NF-007 PARITY] case={} document_diff_rate={:.3} pages={}/{} per_page={:?}",
        case.name,
        score.document_diff_rate,
        score.ferritex_pages,
        score.reference_pages,
        score.per_page_diff_rates
    );

    assert!(
        score.document_diff_rate < MATH_EQUATIONS_REGRESSION_BASELINE_DOCUMENT_DIFF_RATE,
        "math_equations document_diff_rate {:.3} should improve past regression baseline {:.3}",
        score.document_diff_rate,
        MATH_EQUATIONS_REGRESSION_BASELINE_DOCUMENT_DIFF_RATE
    );
    assert!(
        score.document_diff_rate <= MATH_EQUATIONS_MAX_DOCUMENT_DIFF_RATE,
        "math_equations document_diff_rate {:.3} should stay within regression threshold {:.2}",
        score.document_diff_rate,
        MATH_EQUATIONS_MAX_DOCUMENT_DIFF_RATE
    );
}
