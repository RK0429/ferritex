use std::{path::PathBuf, process::Command};

fn ferritex_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ferritex"))
}

#[test]
fn perf_evidence_command_is_visible_in_help() {
    let output = Command::new(ferritex_bin())
        .arg("--help")
        .output()
        .expect("run ferritex --help");

    assert!(
        output.status.success(),
        "help command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("perf-evidence"));
    assert!(stdout.contains("Run a bounded release-binary performance evidence workflow"));
}

#[test]
fn perf_evidence_command_writes_parseable_bounded_artifacts() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let output_dir = temp_dir.path().join("perf-evidence");

    let output = Command::new(ferritex_bin())
        .current_dir(temp_dir.path())
        .arg("perf-evidence")
        .arg("--output-dir")
        .arg(&output_dir)
        .arg("--measured-runs")
        .arg("1")
        .output()
        .expect("run ferritex perf-evidence");

    assert!(
        output.status.success(),
        "perf-evidence command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let json_path = output_dir.join("ferritex-perf-evidence.json");
    let report_path = output_dir.join("ferritex-perf-evidence.txt");
    let json = std::fs::read_to_string(&json_path).expect("read JSON artifact");
    let parsed: serde_json::Value =
        serde_json::from_str(&json).expect("JSON artifact should parse");
    let expected_run_dir = output_dir.join("run").join("measured-0");
    let expected_fixture_path = output_dir.join("perf-evidence-smoke.tex");
    assert_eq!(parsed["fixture"]["source"].as_str(), Some("embedded"));
    assert_eq!(parsed["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(parsed["failures"].as_array().map(Vec::len), Some(0));
    assert_eq!(parsed["config"]["measured_runs"].as_u64(), Some(1));
    assert_eq!(parsed["results"][0]["success"].as_bool(), Some(true));
    assert_eq!(
        parsed["results"][0]["compile_result"]["schemaVersion"].as_str(),
        Some("ferritex.compileResult.v1")
    );
    assert_eq!(
        parsed["results"][0]["compile_result"]["command"].as_str(),
        Some("compile")
    );
    assert_eq!(
        parsed["results"][0]["compile_result"]["success"].as_bool(),
        Some(true)
    );
    assert_eq!(
        parsed["results"][0]["compile_result"]["output"]["pdfPath"].as_str(),
        Some(
            expected_run_dir
                .join("perf-evidence-smoke.pdf")
                .to_str()
                .expect("UTF-8 pdf path")
        )
    );
    assert_eq!(parsed["command"]["args_kind"].as_str(), Some("template"));
    assert_eq!(
        parsed["command"]["actual_invocation"].as_str(),
        Some("results[].command and failures[].command")
    );
    assert_eq!(
        parsed["results"][0]["command"]["binary"].as_str(),
        Some(ferritex_bin().to_str().expect("UTF-8 ferritex binary path"))
    );
    let actual_args = parsed["results"][0]["command"]["args"]
        .as_array()
        .expect("measured result should record actual child args")
        .iter()
        .map(|value| value.as_str().expect("command args should be strings"))
        .collect::<Vec<_>>();
    assert_eq!(
        actual_args,
        vec![
            "compile",
            "--format",
            "json",
            "--no-cache",
            "--reproducible",
            "--output-dir",
            expected_run_dir.to_str().expect("UTF-8 run dir"),
            expected_fixture_path.to_str().expect("UTF-8 fixture path"),
        ]
    );
    assert!(!actual_args.contains(&"<run-dir>"));
    assert!(!actual_args.contains(&"perf-evidence-smoke.tex"));
    assert!(output_dir.join("perf-evidence-smoke.tex").exists());
    assert!(output_dir
        .join("run")
        .join("measured-0")
        .join("perf-evidence-smoke.pdf")
        .exists());
    assert!(report_path.exists(), "missing {}", report_path.display());
    let text_report = std::fs::read_to_string(&report_path).expect("read text report artifact");
    assert!(text_report.contains("fixture: embedded"));
    assert!(text_report.contains("measured runs: 1"));
    assert!(!json.contains("ferritex-bench"));
    assert!(!text_report.contains("ferritex-bench"));
}
