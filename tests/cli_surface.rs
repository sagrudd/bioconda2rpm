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
        "todo",
        "blacklist",
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
fn todo_json_emits_manual_source_file_task() {
    let topdir = tempdir().expect("tempdir");
    let topdir_arg = topdir.path().to_string_lossy().to_string();
    let report_path = topdir.path().join("targets/test-target/reports/build_cap3.json");
    let spec_path = topdir.path().join("SPECS/phoreus-cap3.spec");
    std::fs::create_dir_all(report_path.parent().expect("report parent")).expect("reports dir");
    std::fs::create_dir_all(spec_path.parent().expect("spec parent")).expect("spec dir");
    std::fs::create_dir_all(topdir.path().join("SOURCES")).expect("sources dir");
    std::fs::write(
        &spec_path,
        "Name: phoreus-cap3\nSource0:        https://example.invalid/downloads/cap3.tar.gz\n",
    )
    .expect("write spec");
    std::fs::write(
        topdir
            .path()
            .join("SOURCES/.bioconda2rpm-source-checksum-cap3.env"),
        "BIOCONDA2RPM_SOURCE_SHA256='abc123'\n",
    )
    .expect("write checksum policy");
    let report = serde_json::json!([
        {
            "software": "cap3",
            "priority": 0,
            "status": "quarantined",
            "reason": "source download failed after retries",
            "overlap_recipe": "cap3",
            "overlap_reason": "exact",
            "variant_dir": "/recipes/cap3",
            "package_name": "cap3",
            "version": "10.2011",
            "payload_spec_path": spec_path.display().to_string(),
            "meta_spec_path": "",
            "staged_build_sh": ""
        }
    ]);
    std::fs::write(
        &report_path,
        serde_json::to_string(&report).expect("report json"),
    )
    .expect("write report");
    let catalog = serde_json::json!({
        "entries": [],
        "failures": [
            {
                "software": "cap3",
                "version": "10.2011",
                "arch": "aarch64",
                "target_id": "test-target",
                "status": "quarantined",
                "reason": "source download failed after retries",
                "report_path": report_path.display().to_string(),
                "failed_at": "2026-05-07T10:00:00Z"
            }
        ]
    });
    std::fs::write(
        topdir.path().join(".catalog.json"),
        serde_json::to_string(&catalog).expect("catalog json"),
    )
    .expect("write catalog");

    let output = run(&["todo", "--json", "--topdir", &topdir_arg]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("todo json");
    let items = parsed.as_array().expect("array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["software"], "cap3");
    assert_eq!(items[0]["expected_file"], "cap3.tar.gz");
    assert_eq!(items[0]["source_url"], "https://example.invalid/downloads/cap3.tar.gz");
    assert_eq!(items[0]["sha256"], "abc123");
    assert_eq!(
        items[0]["command"],
        "bioconda2rpm build cap3 --files cap3.tar.gz"
    );
}

#[test]
fn blacklist_updates_csv_and_clears_catalogue_failures() {
    let topdir = tempdir().expect("tempdir");
    let topdir_arg = topdir.path().to_string_lossy().to_string();
    let blacklist_path = topdir.path().join("blacklist.txt");
    let blacklist_arg = blacklist_path.to_string_lossy().to_string();
    let report_path = topdir.path().join("targets/test-target/reports/build_bam2fasta.json");
    std::fs::create_dir_all(report_path.parent().expect("report parent")).expect("reports dir");
    let report = serde_json::json!([
        {
            "software": "bam2fasta",
            "priority": 0,
            "status": "quarantined",
            "reason": "source download failed after retries",
            "overlap_recipe": "bam2fasta",
            "overlap_reason": "exact",
            "variant_dir": "/recipes/bam2fasta",
            "package_name": "bam2fasta",
            "version": "1.0.8",
            "payload_spec_path": "",
            "meta_spec_path": "",
            "staged_build_sh": ""
        }
    ]);
    std::fs::write(
        &report_path,
        serde_json::to_string(&report).expect("report json"),
    )
    .expect("write report");
    let catalog = serde_json::json!({
        "entries": [],
        "failures": [
            {
                "software": "bam2fasta",
                "version": "1.0.8",
                "arch": "x86_64",
                "target_id": "test-target",
                "status": "quarantined",
                "reason": "source download failed after retries",
                "report_path": report_path.display().to_string(),
                "failed_at": "2026-05-07T10:00:00Z"
            }
        ]
    });
    std::fs::write(
        topdir.path().join(".catalog.json"),
        serde_json::to_string(&catalog).expect("catalog json"),
    )
    .expect("write catalog");

    let output = run(&[
        "blacklist",
        "bam2fasta",
        "--reason",
        "withdrawn upstream",
        "--url",
        "https://example.invalid/bam2fasta-1.0.8.tar.gz",
        "--topdir",
        &topdir_arg,
        "--blacklist",
        &blacklist_arg,
        "--json",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("blacklist json");
    assert_eq!(parsed["action"], "added");
    assert_eq!(parsed["removed_failures"], 1);
    let blacklist = std::fs::read_to_string(&blacklist_path).expect("read blacklist");
    assert!(blacklist.contains("bam2fasta"));
    assert!(blacklist.contains("withdrawn upstream"));

    let failures = run(&["failures", "--json", "--topdir", &topdir_arg]);
    assert!(failures.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&failures.stdout).trim()).expect("failures json");
    assert_eq!(parsed.as_array().expect("array").len(), 0);

    let refreshed = Command::new(env!("CARGO_BIN_EXE_bioconda2rpm"))
        .env("BIOCONDA2RPM_BLACKLIST", &blacklist_path)
        .args(["failures", "--refresh", "--json", "--topdir", &topdir_arg])
        .output()
        .expect("run failures refresh");
    assert!(refreshed.status.success());
    let parsed: Value = serde_json::from_str(String::from_utf8_lossy(&refreshed.stdout).trim())
        .expect("refreshed failures json");
    assert_eq!(parsed.as_array().expect("array").len(), 0);
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
