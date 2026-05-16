//! Integration test that spawns a real rust-analyzer.
//!
//! Disabled by default with `#[ignore]`. To run, set `RUN_INTEGRATION_TESTS=1`
//! and use `cargo test -- --include-ignored`.
//!
//! What it does: copies the minimal workspace under `tests/fixtures/sample-workspace/`
//! into a temporary directory, sets up git history, and edits the body of
//! `app::AppGreeter::greet`. Then it runs `dokono-rs --base HEAD~1 --head HEAD`
//! and verifies that only `bins/src/bin/a.rs` is reported as affected
//! (`b.rs` must not be).
//!
//! This test reproduces the "references via trait dispatch" case end-to-end:
//! - The `Greeter` trait declaration lives in the `domain` crate
//! - `impl Greeter for AppGreeter` lives in the `app` crate
//! - `bin/a.rs` consumes `AppGreeter` through `Arc<dyn Greeter>`
//! - References at the `AppGreeter::greet` impl-method position alone do not
//!   reach `a.rs`
//! - Normalizing to the trait-method declaration via `textDocument/declaration`
//!   makes `a.rs` reachable

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_dokono");

#[test]
#[ignore = "integration: spawns rust-analyzer (slow); set RUN_INTEGRATION_TESTS=1"]
fn change_to_app_greeter_affects_only_bin_a() {
    if std::env::var("RUN_INTEGRATION_TESTS").is_err() {
        eprintln!("RUN_INTEGRATION_TESTS not set — skipping");
        return;
    }

    let temp = setup_temp_workspace();

    // Initial commit
    git(&temp, &["init", "-q", "-b", "main"]);
    git(&temp, &["config", "user.email", "test@example.com"]);
    git(&temp, &["config", "user.name", "Test"]);
    git(&temp, &["add", "."]);
    git(&temp, &["commit", "-q", "-m", "base"]);

    // Modify a single line inside the body of `app::AppGreeter::greet`
    let app_lib = temp.join("app/src/lib.rs");
    let original = std::fs::read_to_string(&app_lib).expect("read app lib");
    let modified = original.replace(
        r#"        "hello from AppGreeter".to_string()"#,
        r#"        // dokono integration test marker
        "hello from AppGreeter".to_string()"#,
    );
    assert_ne!(original, modified, "replacement did not match");
    std::fs::write(&app_lib, modified).expect("write app lib");

    git(&temp, &["commit", "-q", "-am", "modify greet"]);

    // Run dokono-rs
    let output = Command::new(BIN)
        .args([
            "--workspace",
            temp.to_str().expect("temp path utf8"),
            "--base",
            "HEAD~1",
            "--head",
            "HEAD",
        ])
        .output()
        .expect("spawn dokono-rs");

    assert!(
        output.status.success(),
        "dokono-rs exited with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("bins/src/bin/a.rs"),
        "expected a.rs in output, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("bins/src/bin/b.rs"),
        "did not expect b.rs in output, got:\n{stdout}"
    );

    // Cleanup
    let _ = std::fs::remove_dir_all(&temp);
}

fn setup_temp_workspace() -> PathBuf {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-workspace");
    assert!(fixture.is_dir(), "fixture missing at {}", fixture.display());

    // Unique temp directory per process invocation
    let temp = std::env::temp_dir().join(format!(
        "dokono-it-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    if temp.exists() {
        std::fs::remove_dir_all(&temp).expect("clean preexisting temp");
    }
    std::fs::create_dir_all(&temp).expect("mkdir temp");

    // Copy the entire fixture
    let status = Command::new("cp")
        .arg("-R")
        .arg(format!("{}/.", fixture.display()))
        .arg(&temp)
        .status()
        .expect("cp -R");
    assert!(status.success(), "cp -R failed");

    temp
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(status.success(), "git {args:?} failed");
}
