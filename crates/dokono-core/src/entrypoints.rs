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

    fn dokono_rs_crate() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/")
            .join("dokono-rs")
    }

    #[test]
    fn discovers_main_rs() {
        let bins = load_bin_entrypoints(&dokono_rs_crate()).unwrap();
        assert!(
            bins.iter().any(|p| p.ends_with("src/main.rs")),
            "expected to find src/main.rs in {bins:?}"
        );
    }

    #[test]
    fn does_not_include_lib_or_examples() {
        let bins = load_bin_entrypoints(&dokono_rs_crate()).unwrap();
        for p in &bins {
            assert!(
                !p.to_string_lossy().contains("/examples/"),
                "unexpected example path: {}",
                p.display()
            );
        }
    }
}
