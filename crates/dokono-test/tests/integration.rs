//! Integration test that spawns a real rust-analyzer.
//!
//! Disabled by default with `#[ignore]`. To run, set `RUN_INTEGRATION_TESTS=1`
//! and use `cargo test -- --include-ignored`.
//!
//! What it does: copies the minimal workspace under
//! `tests/fixtures/sample-workspace-with-tests/` into a temporary directory,
//! sets up git history, and edits the body of `app::AppGreeter::greet`. Then
//! it runs `dokono-test --base HEAD~1 --head HEAD --format json` and verifies
//! that the JSON output marks `test_greet_returns_expected_string` as
//! affected and `test_unrelated_arithmetic` as not affected.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_dokono-test");

#[test]
#[ignore = "integration: spawns rust-analyzer (slow); set RUN_INTEGRATION_TESTS=1"]
fn change_to_greet_affects_only_test_greet() {
    if std::env::var("RUN_INTEGRATION_TESTS").is_err() {
        eprintln!("RUN_INTEGRATION_TESTS not set — skipping");
        return;
    }

    let temp = setup_temp_workspace();

    git(&temp, &["init", "-q", "-b", "main"]);
    git(&temp, &["config", "user.email", "test@example.com"]);
    git(&temp, &["config", "user.name", "Test"]);
    git(&temp, &["add", "."]);
    git(&temp, &["commit", "-q", "-m", "base"]);

    // Edit the body of `greet`.
    let app_lib = temp.join("app/src/lib.rs");
    let original = std::fs::read_to_string(&app_lib).expect("read app lib");
    let modified = original.replace(
        r#"        "hello from AppGreeter".to_string()"#,
        r#"        // integration test marker
        "hello from AppGreeter".to_string()"#,
    );
    assert_ne!(original, modified, "replacement did not match");
    std::fs::write(&app_lib, modified).expect("write app lib");

    git(&temp, &["commit", "-q", "-am", "edit greet"]);

    let output = Command::new(BIN)
        .args([
            "--workspace",
            temp.to_str().expect("temp path utf8"),
            "--base",
            "HEAD~1",
            "--head",
            "HEAD",
            "--format",
            "json",
        ])
        .output()
        .expect("spawn dokono-test");

    assert!(
        output.status.success(),
        "dokono-test exited with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{stdout}"));

    let names: Vec<&str> = v["affected_tests"]
        .as_array()
        .expect("affected_tests is array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();

    assert!(
        names.contains(&"test_greet_returns_expected_string"),
        "expected test_greet in affected; got: {names:?}"
    );
    assert!(
        !names.contains(&"test_unrelated_arithmetic"),
        "did not expect test_unrelated in affected; got: {names:?}"
    );

    let _ = std::fs::remove_dir_all(&temp);
}

fn setup_temp_workspace() -> PathBuf {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-workspace-with-tests");
    assert!(fixture.is_dir(), "fixture missing at {}", fixture.display());

    let temp = std::env::temp_dir().join(format!(
        "dokono-test-it-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    copy_dir(&fixture, &temp);
    temp
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("create dst");
    for entry in std::fs::read_dir(src).expect("read src") {
        let entry = entry.expect("entry");
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to);
        } else {
            std::fs::copy(&from, &to).expect("copy file");
        }
    }
}

fn git(workspace: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .status()
        .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
    assert!(status.success(), "git {args:?} exited with {status}");
}
