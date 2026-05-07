use serde_json::Value;
use std::process::Command;
use tempfile::tempdir;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_bioconda2rpm"))
        .args(args)
        .output()
        .expect("run bioconda2rpm command")
}

#[test]
fn help_lists_primary_commands() {
    let output = run(&["--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for command in [
        "build",
        "server",
        "remove",
        "regression",
        "generate-priority-specs",
        "recipes",
        "lookup",
        "list",
        "failures",
        "catalog",
    ] {
        assert!(
            stdout.contains(command),
            "expected --help to list `{command}`"
        );
    }
}

#[test]
fn catalog_migrate_help_is_exposed() {
    let output = run(&["catalog", "migrate", "--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--topdir <TOPDIR>"));
    assert!(stdout.contains("--json"));
}

#[test]
fn failures_json_emits_catalogue_failures() {
    let topdir = tempdir().expect("tempdir");
    let topdir_arg = topdir.path().to_string_lossy().to_string();
    let catalog = serde_json::json!({
        "entries": [],
        "failures": [
            {
                "software": "tbl2asn-forever",
                "version": "25.7.2f",
                "arch": "aarch64",
                "target_id": "target-aarch64",
                "status": "quarantined",
                "reason": "payload spec build failed in container",
                "report_path": "/tmp/build_tbl2asn-forever.json",
                "failed_at": "2026-05-06T10:00:00Z"
            }
        ]
    });
    std::fs::write(
        topdir.path().join(".catalog.json"),
        serde_json::to_string(&catalog).expect("catalog json"),
    )
    .expect("write catalog");

    let output = run(&["failures", "--json", "--topdir", &topdir_arg]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("failures json");
    assert_eq!(parsed.as_array().expect("array").len(), 1);
    assert_eq!(parsed[0]["software"], "tbl2asn-forever");
}

#[test]
fn build_help_exposes_public_package_selection_flags() {
    let output = run(&["build", "--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[PACKAGE]..."));
    assert!(stdout.contains("--packages-file <PACKAGES_FILE>"));
    assert!(stdout.contains("--recipe-root <RECIPE_ROOT>"));
    assert!(stdout.contains("--refresh-files"));
}

#[test]
fn server_help_exposes_lifecycle_controls() {
    let output = run(&["server", "--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--close"));
    assert!(stdout.contains("--kill"));
    assert!(stdout.contains("--status"));
}

#[test]
fn server_status_compact_emits_runtime_snapshot() {
    let topdir = tempdir().expect("tempdir");
    let topdir_arg = topdir.path().to_string_lossy().to_string();

    let output = run(&["server", "--status", "--compact", "--topdir", &topdir_arg]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("server status json");
    assert!(parsed.get("topdir").is_some());
    assert!(parsed.get("server_status").is_some());
}

#[test]
fn lookup_compact_emits_machine_readable_json() {
    let topdir = tempdir().expect("tempdir");
    let topdir_arg = topdir.path().to_string_lossy().to_string();

    let output = run(&["lookup", "--compact", "--topdir", &topdir_arg]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("lookup json");
    assert!(parsed.get("topdir").is_some());
    assert!(parsed.get("lock_held").is_some());
    assert!(parsed.get("updated_at_utc").is_some());
}

#[test]
fn build_without_package_or_packages_file_is_rejected() {
    let topdir = tempdir().expect("tempdir");
    let topdir_arg = topdir.path().to_string_lossy().to_string();

    let output = run(&["build", "--ui", "plain", "--topdir", &topdir_arg]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("failed to determine requested packages"));
}
