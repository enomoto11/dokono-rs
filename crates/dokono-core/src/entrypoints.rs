//! Collect absolute paths of `kind == "bin"` targets via `cargo metadata`.
//! Custom `[[bin]]` paths declared in `Cargo.toml` are picked up automatically;
//! never hardcode `src/bin/`.

use anyhow::{Context, Result};
use cargo_metadata::MetadataCommand;
use std::path::{Path, PathBuf};

pub fn load_bin_entrypoints(workspace: &Path) -> Result<Vec<PathBuf>> {
    let manifest = workspace.join("Cargo.toml");
    let metadata = MetadataCommand::new()
        .manifest_path(&manifest)
        .no_deps()
        .exec()
        .with_context(|| format!("cargo metadata failed for {}", manifest.display()))?;

    let mut bins: Vec<PathBuf> = Vec::new();
    for pkg in &metadata.packages {
        for tgt in &pkg.targets {
            if tgt.kind.iter().any(|k| k == "bin") {
                bins.push(tgt.src_path.as_std_path().to_path_buf());
            }
        }
    }
    bins.sort();
    bins.dedup();
    Ok(bins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_fixture_crate() -> TempDir {
        let dir = tempfile::tempdir().expect("create tempdir");
        let root = dir.path();
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "fixture"
version = "0.0.0"
edition = "2021"
"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::create_dir_all(root.join("examples")).unwrap();
        fs::write(root.join("examples/ex.rs"), "fn main() {}\n").unwrap();
        dir
    }

    #[test]
    fn discovers_main_rs() {
        let dir = make_fixture_crate();
        let bins = load_bin_entrypoints(dir.path()).unwrap();
        assert!(
            bins.iter().any(|p| p.ends_with("src/main.rs")),
            "expected to find src/main.rs in {bins:?}"
        );
    }

    #[test]
    fn does_not_include_lib_or_examples() {
        let dir = make_fixture_crate();
        let bins = load_bin_entrypoints(dir.path()).unwrap();
        for p in &bins {
            assert!(
                !p.to_string_lossy().contains("/examples/"),
                "unexpected example path: {}",
                p.display()
            );
            assert!(
                !p.ends_with("src/lib.rs"),
                "unexpected lib path: {}",
                p.display()
            );
        }
    }
}
