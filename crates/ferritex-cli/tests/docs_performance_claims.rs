use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("ferritex-cli crate should live under crates/")
        .to_path_buf()
}

#[test]
fn readme_qualifies_warm_incremental_proxy_claim_against_public_cli_latency() {
    let readme = std::fs::read_to_string(workspace_root().join("README.md")).expect("read README");

    assert!(readme.contains("66ms"));
    assert!(readme.contains("70ms"));
    assert!(readme.contains("not a public CLI latency guarantee"));
    assert!(
        readme.contains("current release-build public CLI warm incremental median of **8.13s**")
    );
    assert!(readme.contains("5 of 11 non-functional requirements are fully verified"));
    assert!(readme.contains("canonical `FTX-BENCH-001` verification remains open"));
    assert!(!readme.contains("clears its 100ms threshold"));
    assert!(!readme.contains("All Must requirements in `docs/requirements.md` are satisfied"));
    assert!(!readme.contains("6 of 11 non-functional requirements are fully verified"));
}

#[test]
fn requirements_marks_req_nf_002_canonical_verification_as_open() {
    let requirements = std::fs::read_to_string(workspace_root().join("docs/requirements.md"))
        .expect("read requirements");

    assert!(requirements.contains("#### REQ-NF-002: 差分コンパイル速度"));
    assert!(requirements.contains("Must 要件として未完了"));
    assert!(requirements.contains("internal proxy evidence"));
    assert!(requirements.contains("public CLI latency guarantee ではない"));
    assert!(requirements.contains("canonical `FTX-BENCH-001`"));
    assert!(requirements.contains("formal verification は open item"));
}

#[test]
fn optimization_design_marks_66ms_70ms_as_proxy_not_public_cli_guarantee() {
    let design = std::fs::read_to_string(
        workspace_root().join("docs/design-incremental-100ms-optimization.md"),
    )
    .expect("read incremental optimization design");

    assert!(design.contains("Proxy evidence only"));
    assert!(design.contains("66ms no-ref / 70ms with-ref"));
    assert!(design.contains("current release-build public CLI warm incremental median **8.13s**"));
    assert!(design.contains("not a public CLI latency guarantee"));

    for stale_claim in [
        "REQ-NF-002 を満たした",
        "REQ-NF-002 達成",
        "REQ-NF-002 達成済み",
        "`REQ-NF-002` の 100ms 達成",
        "総合で 100ms 未満を達成",
        "を達成した",
    ] {
        assert!(
            !design.contains(stale_claim),
            "design doc must not present proxy measurements as public REQ-NF-002 completion: {stale_claim}"
        );
    }
}
