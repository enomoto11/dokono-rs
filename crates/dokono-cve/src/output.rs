//! Text and JSON formatters for the final result.
//!
//! Per the MVP scope, output covers per-bin reachability labels
//! (Production / TestsOnly / NotReachable), the resolved seed location,
//! and the lists of unresolved and unsupported advisories.

use serde::Serialize;
use std::path::Path;

use crate::input::{InputResult, UnsupportedReason};
use crate::probe::{ResolveResult, UnresolvedReason};
use crate::runner::{Reachability, RunResult};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
}

pub fn render(
    workspace: &Path,
    input: &InputResult,
    resolve: &ResolveResult,
    run: &RunResult,
    format: Format,
) -> String {
    match format {
        Format::Text => render_text(workspace, input, resolve, run),
        Format::Json => render_json(workspace, input, resolve, run),
    }
}

fn render_text(
    workspace: &Path,
    input: &InputResult,
    resolve: &ResolveResult,
    run: &RunResult,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Workspace: {} ({} bins)\n",
        workspace.display(),
        run.all_bins.len()
    ));

    for sr in &run.per_seed {
        out.push('\n');
        let id = sr.seed.symbol.advisory_id.as_deref().unwrap_or("(direct)");
        let crate_name = &sr.seed.symbol.crate_name;
        out.push_str(&format!("{id}  {crate_name}  {}\n", sr.seed.symbol.path));
        out.push_str(&format!(
            "  seed: {}:{}:{}\n",
            sr.seed.file.display(),
            sr.seed.position.line + 1,
            sr.seed.position.character + 1
        ));
        let mut prod = Vec::new();
        let mut tests = Vec::new();
        let mut none = Vec::new();
        for bin in &run.all_bins {
            let rel = relative_str(bin, workspace);
            match sr.bins.get(bin) {
                Some(Reachability::Production) => prod.push(rel),
                Some(Reachability::TestsOnly) => tests.push(rel),
                Some(Reachability::NotReachable) | None => none.push(rel),
            }
        }
        emit_section(&mut out, "REACHABLE", &prod);
        emit_section(&mut out, "REACHABLE FROM TESTS ONLY", &tests);
        emit_section(&mut out, "NOT REACHABLE", &none);
    }

    if !resolve.unresolved.is_empty() {
        out.push_str("\nUnresolved seeds:\n");
        for u in &resolve.unresolved {
            let id = u.symbol.advisory_id.as_deref().unwrap_or("(direct)");
            out.push_str(&format!(
                "  {id}  {}  ({})\n",
                u.symbol.path,
                describe_unresolved(&u.reason)
            ));
        }
    }

    if !input.unsupported.is_empty() {
        out.push_str("\nUnsupported advisories (no affected.functions in DB):\n");
        for u in &input.unsupported {
            out.push_str(&format!("  {}  {}\n", u.advisory_id, u.crate_name));
        }
    }

    out
}

fn emit_section(out: &mut String, label: &str, bins: &[String]) {
    out.push_str(&format!("  {label}\n"));
    if bins.is_empty() {
        out.push_str("    (none)\n");
        return;
    }
    for b in bins {
        out.push_str(&format!("    {b}\n"));
    }
}

fn relative_str(p: &Path, workspace: &Path) -> String {
    p.strip_prefix(workspace).unwrap_or(p).display().to_string()
}

fn describe_unresolved(reason: &UnresolvedReason) -> String {
    match reason {
        UnresolvedReason::NoDependency => "no_dependency".into(),
        UnresolvedReason::NotAffectedVersion(v) => format!("not_affected_version (in use: {v})"),
        UnresolvedReason::ProbeFailed(msg) => format!("probe_failed: {msg}"),
    }
}

fn render_json(
    workspace: &Path,
    input: &InputResult,
    resolve: &ResolveResult,
    run: &RunResult,
) -> String {
    let report = JsonReport {
        schema_version: SCHEMA_VERSION,
        workspace: workspace.display().to_string(),
        all_bins: run
            .all_bins
            .iter()
            .map(|b| relative_str(b, workspace))
            .collect(),
        advisories: run
            .per_seed
            .iter()
            .map(|sr| {
                let mut bins_json = Vec::with_capacity(run.all_bins.len());
                for bin in &run.all_bins {
                    let r = sr
                        .bins
                        .get(bin)
                        .copied()
                        .unwrap_or(Reachability::NotReachable);
                    bins_json.push(JsonBin {
                        path: relative_str(bin, workspace),
                        reachability: reachability_str(r),
                    });
                }
                JsonAdvisory {
                    advisory_id: sr.seed.symbol.advisory_id.clone(),
                    crate_name: sr.seed.symbol.crate_name.clone(),
                    vulnerable_symbol: sr.seed.symbol.path.clone(),
                    seed_file: sr.seed.file.display().to_string(),
                    seed_line: sr.seed.position.line + 1,
                    seed_character: sr.seed.position.character + 1,
                    bins: bins_json,
                }
            })
            .collect(),
        unresolved: resolve
            .unresolved
            .iter()
            .map(|u| JsonUnresolved {
                advisory_id: u.symbol.advisory_id.clone(),
                crate_name: u.symbol.crate_name.clone(),
                vulnerable_symbol: u.symbol.path.clone(),
                reason: describe_unresolved(&u.reason),
            })
            .collect(),
        unsupported: input
            .unsupported
            .iter()
            .map(|u| JsonUnsupported {
                advisory_id: u.advisory_id.clone(),
                crate_name: u.crate_name.clone(),
                reason: match u.reason {
                    UnsupportedReason::NoAffectedFunctions => "no_affected_functions".into(),
                },
            })
            .collect(),
    };
    serde_json::to_string_pretty(&report).expect("JSON serialization is infallible for our types")
}

fn reachability_str(r: Reachability) -> &'static str {
    match r {
        Reachability::Production => "production",
        Reachability::TestsOnly => "tests_only",
        Reachability::NotReachable => "not_reachable",
    }
}

#[derive(Serialize)]
struct JsonReport {
    schema_version: u32,
    workspace: String,
    all_bins: Vec<String>,
    advisories: Vec<JsonAdvisory>,
    unresolved: Vec<JsonUnresolved>,
    unsupported: Vec<JsonUnsupported>,
}

#[derive(Serialize)]
struct JsonAdvisory {
    advisory_id: Option<String>,
    #[serde(rename = "crate")]
    crate_name: String,
    vulnerable_symbol: String,
    seed_file: String,
    seed_line: u32,
    seed_character: u32,
    bins: Vec<JsonBin>,
}

#[derive(Serialize)]
struct JsonBin {
    path: String,
    reachability: &'static str,
}

#[derive(Serialize)]
struct JsonUnresolved {
    advisory_id: Option<String>,
    #[serde(rename = "crate")]
    crate_name: String,
    vulnerable_symbol: String,
    reason: String,
}

#[derive(Serialize)]
struct JsonUnsupported {
    advisory_id: String,
    #[serde(rename = "crate")]
    crate_name: String,
    reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{UnsupportedAdvisory, VulnSymbol};
    use crate::probe::{ResolvedSeed, UnresolvedSeed};
    use crate::runner::SeedReachability;
    use dokono_core::types::Position;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    fn workspace() -> PathBuf {
        PathBuf::from("/work")
    }

    fn sample_input() -> InputResult {
        InputResult {
            vuln_symbols: vec![],
            unsupported: vec![UnsupportedAdvisory {
                advisory_id: "RUSTSEC-2099-0001".into(),
                crate_name: "hyper".into(),
                reason: UnsupportedReason::NoAffectedFunctions,
            }],
        }
    }

    fn sample_resolve() -> ResolveResult {
        ResolveResult {
            resolved: vec![],
            unresolved: vec![UnresolvedSeed {
                symbol: VulnSymbol {
                    advisory_id: None,
                    crate_name: "ghost".into(),
                    path: "ghost::foo".into(),
                    version_reqs: vec![],
                },
                reason: UnresolvedReason::NoDependency,
            }],
        }
    }

    fn sample_run() -> RunResult {
        let bin_a = PathBuf::from("/work/bin/a.rs");
        let bin_b = PathBuf::from("/work/bin/b.rs");
        let bin_c = PathBuf::from("/work/bin/c.rs");
        let all_bins: BTreeSet<PathBuf> = [&bin_a, &bin_b, &bin_c].into_iter().cloned().collect();

        let mut bins = BTreeMap::new();
        bins.insert(bin_a.clone(), Reachability::Production);
        bins.insert(bin_b.clone(), Reachability::TestsOnly);
        bins.insert(bin_c.clone(), Reachability::NotReachable);

        RunResult {
            all_bins,
            per_seed: vec![SeedReachability {
                seed: ResolvedSeed {
                    symbol: VulnSymbol {
                        advisory_id: Some("RUSTSEC-2025-0022".into()),
                        crate_name: "openssl".into(),
                        path: "openssl::md::Md::fetch".into(),
                        version_reqs: vec![],
                    },
                    file: PathBuf::from(
                        "/cargo/registry/src/index.crates.io/openssl-0.10.68/src/md.rs",
                    ),
                    position: Position {
                        line: 98,
                        character: 11,
                    },
                },
                bins,
            }],
        }
    }

    #[test]
    fn text_snapshot() {
        let out = render(
            &workspace(),
            &sample_input(),
            &sample_resolve(),
            &sample_run(),
            Format::Text,
        );
        insta::assert_snapshot!(out);
    }

    #[test]
    fn json_snapshot() {
        let out = render(
            &workspace(),
            &sample_input(),
            &sample_resolve(),
            &sample_run(),
            Format::Json,
        );
        insta::assert_snapshot!(out);
    }

    #[test]
    fn json_parses_as_object() {
        let out = render(
            &workspace(),
            &sample_input(),
            &sample_resolve(),
            &sample_run(),
            Format::Json,
        );
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["advisories"][0]["advisory_id"], "RUSTSEC-2025-0022");
        assert_eq!(v["advisories"][0]["bins"][0]["reachability"], "production");
        assert_eq!(v["advisories"][0]["bins"][1]["reachability"], "tests_only");
        assert_eq!(
            v["advisories"][0]["bins"][2]["reachability"],
            "not_reachable"
        );
    }
}
