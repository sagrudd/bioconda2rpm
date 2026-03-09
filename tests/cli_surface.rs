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
        "regression",
        "generate-priority-specs",
        "recipes",
        "lookup",
    ] {
        assert!(
            stdout.contains(command),
            "expected --help to list `{command}`"
        );
    }
}

#[test]
fn build_help_exposes_public_package_selection_flags() {
    let output = run(&["build", "--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[PACKAGE]..."));
    assert!(stdout.contains("--packages-file <PACKAGES_FILE>"));
    assert!(stdout.contains("--recipe-root <RECIPE_ROOT>"));
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
