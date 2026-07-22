#[test]
fn claude_hook_reports_pr_on_session_start() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/integration/assets/claude/herdr-agent-state.sh"
    );
    let asset = std::fs::read_to_string(path).expect("read hook asset");
    assert!(
        asset.contains("tab report-pr"),
        "hook must call tab report-pr"
    );
    assert!(
        asset.contains("HERDR_INTEGRATION_VERSION=8"),
        "integration version must be bumped to 8"
    );
    assert!(
        asset.contains("--merged"),
        "hook must pass --merged when PR state is MERGED"
    );
}
