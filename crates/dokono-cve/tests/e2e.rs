//! End-to-end tests that exercise the compiled `dokono-cve` binary.
//!
//! Most tests are `#[ignore]`-d because they spawn rust-analyzer twice (probe + BFS) and
//! take tens of seconds. Run them locally with `cargo test --test e2e -- --ignored`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_dokono-cve")
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cve-basic")
}

fn fixture_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

#[test]
fn help_smoke() {
    let out = Command::new(bin_path()).arg("--help").output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("dokono-cve"));
    assert!(stdout.contains("--workspace"));
}

#[test]
fn dry_run_audit_json() {
    // Phase 1's tier-A fixture has 1 VulnSymbol, 0 unsupported.
    let audit = fixture_path("tests/fixtures/audit-tier-a-only.json");
    let out = Command::new(bin_path())
        .args([
            "--workspace",
            "/tmp",
            "--audit-json",
            audit.to_str().unwrap(),
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("1 symbol"), "{stdout}");
    assert!(stdout.contains("openssl::md::Md::fetch"), "{stdout}");
}

#[test]
fn dry_run_audit_mixed_classifies_unsupported() {
    let audit = fixture_path("tests/fixtures/audit-mixed.json");
    let out = Command::new(bin_path())
        .args([
            "--workspace",
            "/tmp",
            "--audit-json",
            audit.to_str().unwrap(),
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("1 symbol(s), 1 unsupported"));
    assert!(stdout.contains("RUSTSEC-2099-0002")); // unsupported (tokio, no functions)
}

#[test]
fn missing_workspace_exits_2() {
    let out = Command::new(bin_path())
        .args([
            "--workspace",
            "/path/that/does/not/exist/anywhere",
            "--symbol",
            "x::y",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn broken_audit_json_exits_2() {
    let tmp = std::env::temp_dir().join("dokono_cve_e2e_broken.json");
    std::fs::write(&tmp, "this is not json").unwrap();
    let out = Command::new(bin_path())
        .args([
            "--workspace",
            "/tmp",
            "--audit-json",
            tmp.to_str().unwrap(),
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
#[ignore = "slow: spawns rust-analyzer twice and fetches serde_json from the registry"]
fn fixture_reachability_against_cve_basic() {
    let fixture = fixture_dir();
    assert!(
        fixture.join("Cargo.toml").exists(),
        "fixture missing at {}",
        fixture.display()
    );

    let out = Command::new(bin_path())
        .args([
            "--workspace",
            fixture.to_str().unwrap(),
            "--symbol",
            "serde_json::from_str",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    let status = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        status, 1,
        "expected exit 1 (production reachable). stderr: {stderr}\nstdout: {stdout}"
    );

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("parse JSON: {e}\nstdout: {stdout}\nstderr: {stderr}"));

    let bins = json["advisories"][0]["bins"].as_array().unwrap();
    let by_path: HashMap<&str, &str> = bins
        .iter()
        .map(|b| {
            (
                b["path"].as_str().unwrap(),
                b["reachability"].as_str().unwrap(),
            )
        })
        .collect();

    let direct = pick(&by_path, "direct.rs");
    let via_lib = pick(&by_path, "via_lib.rs");
    let unrelated = pick(&by_path, "unrelated.rs");
    assert_eq!(
        direct.1, "production",
        "direct.rs: {direct:?}\nstderr: {stderr}"
    );
    assert_eq!(
        via_lib.1, "production",
        "via_lib.rs: {via_lib:?}\nstderr: {stderr}"
    );
    assert_eq!(
        unrelated.1, "not_reachable",
        "unrelated.rs: {unrelated:?}\nstderr: {stderr}"
    );
}

fn pick<'a>(map: &'a HashMap<&'a str, &'a str>, suffix: &str) -> (&'a str, &'a str) {
    map.iter()
        .find(|(p, _)| Path::new(p).ends_with(suffix))
        .map(|(p, r)| (*p, *r))
        .unwrap_or_else(|| panic!("no bin ends with {suffix}; map: {map:?}"))
}
