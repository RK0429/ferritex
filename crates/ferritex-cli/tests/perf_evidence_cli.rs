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
    assert_eq!(parsed["fixture"]["source"].as_str(), Some("embedded"));
    assert_eq!(parsed["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(parsed["failures"].as_array().map(Vec::len), Some(0));
    assert_eq!(parsed["config"]["measured_runs"].as_u64(), Some(1));
    assert_eq!(parsed["results"][0]["success"].as_bool(), Some(true));
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
