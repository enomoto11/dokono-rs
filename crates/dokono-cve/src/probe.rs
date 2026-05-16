//! Resolve a vulnerable symbol path into a `(registry_file, position)` seed for the BFS.
//!
//! For each (crate, version) group, we synthesize a tiny standalone cargo crate in
//! `~/.cache/dokono-cve/probe-<crate>-<version>/`, spawn a one-shot rust-analyzer there,
//! and ask `textDocument/definition` at the position we wrote. The returned location
//! points inside `~/.cargo/registry/...` and is shared with the user's workspace
//! (both rust-analyzer instances see the same cargo registry).
//!
//! This deliberately never touches the user's workspace.

use anyhow::{Context, Result};
use dokono_core::lsp::backend::{file_uri, open_document};
use dokono_core::lsp::{client::Client, lifecycle, progress};
use dokono_core::types::Position;
use lsp_types::request::GotoDefinition;
use lsp_types::{
    GotoDefinitionParams, GotoDefinitionResponse, PartialResultParams, TextDocumentIdentifier,
    TextDocumentPositionParams, WorkDoneProgressParams,
};
use semver::{Version, VersionReq};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::input::VulnSymbol;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSeed {
    pub symbol: VulnSymbol,
    pub file: PathBuf,
    pub position: Position,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedSeed {
    pub symbol: VulnSymbol,
    pub reason: UnresolvedReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnresolvedReason {
    NoDependency,
    NotAffectedVersion(Version),
    ProbeFailed(String),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ResolveResult {
    pub resolved: Vec<ResolvedSeed>,
    pub unresolved: Vec<UnresolvedSeed>,
}

pub fn resolve_seeds(workspace: &Path, symbols: &[VulnSymbol]) -> Result<ResolveResult> {
    let versions = workspace_dep_versions(workspace)?;
    let mut unresolved = Vec::new();
    let mut groups: HashMap<(String, Version), Vec<VulnSymbol>> = HashMap::new();

    for sym in symbols {
        let Some(version) = versions.get(&sym.crate_name) else {
            unresolved.push(UnresolvedSeed {
                symbol: sym.clone(),
                reason: UnresolvedReason::NoDependency,
            });
            continue;
        };
        if !version_matches(&sym.version_reqs, version) {
            unresolved.push(UnresolvedSeed {
                symbol: sym.clone(),
                reason: UnresolvedReason::NotAffectedVersion(version.clone()),
            });
            continue;
        }
        groups
            .entry((sym.crate_name.clone(), version.clone()))
            .or_default()
            .push(sym.clone());
    }

    let mut resolved = Vec::new();
    for ((crate_name, version), syms) in groups {
        match probe_group(&crate_name, &version, &syms) {
            Ok(locations) => {
                for (sym, (file, position)) in syms.into_iter().zip(locations) {
                    resolved.push(ResolvedSeed {
                        symbol: sym,
                        file,
                        position,
                    });
                }
            }
            Err(e) => {
                let msg = e.to_string();
                for sym in syms {
                    unresolved.push(UnresolvedSeed {
                        symbol: sym,
                        reason: UnresolvedReason::ProbeFailed(msg.clone()),
                    });
                }
            }
        }
    }

    resolved.sort_by(|a, b| a.symbol.path.cmp(&b.symbol.path));
    unresolved.sort_by(|a, b| a.symbol.path.cmp(&b.symbol.path));
    Ok(ResolveResult {
        resolved,
        unresolved,
    })
}

pub fn version_matches(reqs: &[VersionReq], version: &Version) -> bool {
    reqs.is_empty() || reqs.iter().any(|r| r.matches(version))
}

fn workspace_dep_versions(workspace: &Path) -> Result<HashMap<String, Version>> {
    let manifest = workspace.join("Cargo.toml");
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(&manifest)
        .exec()
        .with_context(|| format!("cargo metadata at {}", manifest.display()))?;
    let mut out: HashMap<String, Version> = HashMap::new();
    for pkg in &metadata.packages {
        let v: Version = pkg.version.to_string().parse()?;
        out.entry(pkg.name.to_string())
            .and_modify(|cur| {
                if v < *cur {
                    *cur = v.clone();
                }
            })
            .or_insert(v);
    }
    Ok(out)
}

fn probe_group(
    crate_name: &str,
    version: &Version,
    symbols: &[VulnSymbol],
) -> Result<Vec<(PathBuf, Position)>> {
    let probe_dir = ensure_probe_crate(crate_name, version, symbols)?;
    let main_rs = probe_dir.join("src").join("main.rs");

    tracing::info!(
        "probe: spawning rust-analyzer at {} for {}@{}",
        probe_dir.display(),
        crate_name,
        version
    );
    let mut client = Client::spawn(&probe_dir)?;
    lifecycle::initialize(&mut client, &probe_dir)?;
    progress::wait_for_index_end(&client)?;
    open_document(&client, &main_rs)?;

    let mut out = Vec::with_capacity(symbols.len());
    for (i, sym) in symbols.iter().enumerate() {
        let pos = symbol_position_in_probe(i, &sym.path);
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: file_uri(&main_rs)?,
                },
                position: lsp_types::Position {
                    line: pos.line,
                    character: pos.character,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        let response = client.request::<GotoDefinition>(params)?;
        let loc = extract_definition_location(response)
            .with_context(|| format!("no definition returned for {}", sym.path))?;
        out.push(loc);
    }

    lifecycle::shutdown(&mut client)?;
    Ok(out)
}

fn ensure_probe_crate(
    crate_name: &str,
    version: &Version,
    symbols: &[VulnSymbol],
) -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let dir = PathBuf::from(home)
        .join(".cache")
        .join("dokono-cve")
        .join(format!("probe-{crate_name}-{version}"));
    std::fs::create_dir_all(dir.join("src"))
        .with_context(|| format!("create_dir_all {}", dir.display()))?;

    let cargo_toml = format!(
        "[package]\n\
         name = \"dokono-cve-probe\"\n\
         version = \"0.0.1\"\n\
         edition = \"2021\"\n\
         \n\
         [dependencies]\n\
         {crate_name} = \"={version}\"\n\
         \n\
         [workspace]\n"
    );
    std::fs::write(dir.join("Cargo.toml"), cargo_toml)?;
    std::fs::write(dir.join("src").join("main.rs"), build_probe_main(symbols))?;
    Ok(dir)
}

pub(crate) fn build_probe_main(symbols: &[VulnSymbol]) -> String {
    let mut out = String::new();
    out.push_str("#![allow(unused, dead_code, clippy::all)]\n");
    out.push_str("fn __dokono_cve_probe() {\n");
    for sym in symbols {
        out.push_str("    let _ = ");
        out.push_str(&sym.path);
        out.push_str(";\n");
    }
    out.push_str("}\n");
    out
}

pub(crate) fn symbol_position_in_probe(index: usize, path: &str) -> Position {
    // Layout (0-indexed):
    //   line 0: #![allow(...)]
    //   line 1: fn __dokono_cve_probe() {
    //   line 2..2+N: "    let _ = <path>;"
    let line = 2 + index as u32;
    let leaf_start = path.rfind("::").map(|i| i + 2).unwrap_or(0);
    let character = (b"    let _ = ".len() + leaf_start) as u32;
    Position { line, character }
}

fn extract_definition_location(
    response: Option<GotoDefinitionResponse>,
) -> Option<(PathBuf, Position)> {
    match response? {
        GotoDefinitionResponse::Scalar(loc) => Some((
            loc.uri.to_file_path().ok()?,
            Position {
                line: loc.range.start.line,
                character: loc.range.start.character,
            },
        )),
        GotoDefinitionResponse::Array(locs) => {
            let loc = locs.into_iter().next()?;
            Some((
                loc.uri.to_file_path().ok()?,
                Position {
                    line: loc.range.start.line,
                    character: loc.range.start.character,
                },
            ))
        }
        GotoDefinitionResponse::Link(links) => {
            let link = links.into_iter().next()?;
            Some((
                link.target_uri.to_file_path().ok()?,
                Position {
                    line: link.target_selection_range.start.line,
                    character: link.target_selection_range.start.character,
                },
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(path: &str) -> VulnSymbol {
        VulnSymbol {
            advisory_id: None,
            crate_name: path.split("::").next().unwrap().to_string(),
            path: path.to_string(),
            version_reqs: Vec::new(),
        }
    }

    #[test]
    fn version_matches_empty_reqs() {
        let v: Version = "1.2.3".parse().unwrap();
        assert!(version_matches(&[], &v));
    }

    #[test]
    fn version_matches_within_range() {
        let v: Version = "1.2.3".parse().unwrap();
        let r: VersionReq = ">=1.0, <2.0".parse().unwrap();
        assert!(version_matches(&[r], &v));
    }

    #[test]
    fn version_matches_outside_range() {
        let v: Version = "2.5.0".parse().unwrap();
        let r: VersionReq = ">=1.0, <2.0".parse().unwrap();
        assert!(!version_matches(&[r], &v));
    }

    #[test]
    fn position_points_at_leaf() {
        let cases = [
            ("openssl::ssl::SslContext::new", "new"),
            ("openssl::md::Md::fetch", "fetch"),
            ("openssl::md::fetch_md_by_name", "fetch_md_by_name"),
            ("foo::bar", "bar"),
        ];
        let symbols: Vec<VulnSymbol> = cases.iter().map(|(p, _)| sym(p)).collect();
        let main_rs = build_probe_main(&symbols);
        let lines: Vec<&str> = main_rs.lines().collect();
        for (i, (path, leaf)) in cases.iter().enumerate() {
            let pos = symbol_position_in_probe(i, path);
            let line = lines[pos.line as usize];
            let from = &line[pos.character as usize..];
            assert!(
                from.starts_with(leaf),
                "case {i} ({path}): expected leaf {leaf:?}, line {line:?}, slice {from:?}"
            );
        }
    }

    #[test]
    fn probe_main_includes_all_paths() {
        let symbols = vec![
            sym("openssl::ssl::SslContext::new"),
            sym("openssl::md::Md::fetch"),
        ];
        let main_rs = build_probe_main(&symbols);
        assert!(main_rs.contains("openssl::ssl::SslContext::new"));
        assert!(main_rs.contains("openssl::md::Md::fetch"));
        assert!(main_rs.starts_with("#!"));
    }
}
