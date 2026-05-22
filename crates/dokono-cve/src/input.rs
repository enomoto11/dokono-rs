//! Parse one of three input modes (`cargo audit --json`, a RUSTSEC advisory id, or a direct
//! symbol path) into a uniform list of vulnerable symbols plus a list of advisories that the
//! RUSTSEC DB doesn't pin down to a function (Tier B — we can't narrow reachability for these).

use anyhow::{Context, Result, anyhow};
use semver::VersionReq;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VulnSymbol {
    pub advisory_id: Option<String>,
    pub crate_name: String,
    pub path: String,
    pub version_reqs: Vec<VersionReq>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedAdvisory {
    pub advisory_id: String,
    pub crate_name: String,
    pub reason: UnsupportedReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsupportedReason {
    NoAffectedFunctions,
}

#[derive(Debug)]
pub enum InputMode {
    AuditJson(PathBuf),
    AdvisoryId(String),
    DirectSymbol(String),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InputResult {
    pub vuln_symbols: Vec<VulnSymbol>,
    pub unsupported: Vec<UnsupportedAdvisory>,
}

pub fn collect(mode: InputMode) -> Result<InputResult> {
    match mode {
        InputMode::AuditJson(path) => collect_from_audit_json(&path),
        InputMode::AdvisoryId(id) => collect_from_advisory_id(&id),
        InputMode::DirectSymbol(path) => collect_from_symbol(&path),
    }
}

fn collect_from_audit_json(path: &Path) -> Result<InputResult> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading audit JSON: {}", path.display()))?;
    let report: rustsec::Report = serde_json::from_str(&content)
        .with_context(|| format!("parsing audit JSON: {}", path.display()))?;

    let mut out = InputResult::default();
    for vuln in &report.vulnerabilities.list {
        push_vuln(vuln, &mut out);
    }
    Ok(out)
}

fn collect_from_advisory_id(id_str: &str) -> Result<InputResult> {
    let id: rustsec::advisory::Id = id_str
        .parse()
        .with_context(|| format!("not a valid RUSTSEC advisory id: {id_str}"))?;

    let repo_path = rustsec::Repository::default_path();
    let repo = rustsec::Repository::open(&repo_path).with_context(|| {
        format!(
            "opening advisory DB at {} — run `cargo audit fetch` or update cargo-audit",
            repo_path.display()
        )
    })?;
    let db = rustsec::Database::load_from_repo(&repo).context("loading advisory DB")?;
    let advisory = db
        .get(&id)
        .ok_or_else(|| anyhow!("advisory not found in DB: {id_str}"))?;

    let crate_name = advisory.metadata.package.to_string();
    let mut out = InputResult::default();
    push_from_advisory(
        &advisory.metadata,
        advisory.affected.as_ref(),
        &crate_name,
        &mut out,
    );
    Ok(out)
}

fn collect_from_symbol(path: &str) -> Result<InputResult> {
    let segments: Vec<&str> = path.split("::").collect();
    if segments.len() < 2 || segments.iter().any(|s| s.is_empty()) {
        return Err(anyhow!(
            "symbol must have at least 2 segments (crate::item): {path:?}"
        ));
    }
    Ok(InputResult {
        vuln_symbols: vec![VulnSymbol {
            advisory_id: None,
            crate_name: segments[0].to_string(),
            path: path.to_string(),
            version_reqs: Vec::new(),
        }],
        unsupported: Vec::new(),
    })
}

fn push_vuln(vuln: &rustsec::Vulnerability, out: &mut InputResult) {
    let crate_name = vuln.package.name.to_string();
    push_from_advisory(&vuln.advisory, vuln.affected.as_ref(), &crate_name, out);
}

fn push_from_advisory(
    metadata: &rustsec::advisory::Metadata,
    affected: Option<&rustsec::advisory::Affected>,
    crate_name: &str,
    out: &mut InputResult,
) {
    let id = metadata.id.to_string();
    let affected = match affected {
        Some(a) if !a.functions.is_empty() => a,
        _ => {
            out.unsupported.push(UnsupportedAdvisory {
                advisory_id: id,
                crate_name: crate_name.to_string(),
                reason: UnsupportedReason::NoAffectedFunctions,
            });
            return;
        }
    };

    for (fn_path, reqs) in &affected.functions {
        out.vuln_symbols.push(VulnSymbol {
            advisory_id: Some(id.clone()),
            crate_name: crate_name.to_string(),
            path: fn_path.to_string(),
            version_reqs: reqs.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_symbol_extracts_crate_name() {
        let r = collect_from_symbol("openssl::ssl::SslContext::new").unwrap();
        assert_eq!(r.vuln_symbols.len(), 1);
        assert_eq!(r.vuln_symbols[0].crate_name, "openssl");
        assert_eq!(r.vuln_symbols[0].path, "openssl::ssl::SslContext::new");
        assert!(r.vuln_symbols[0].advisory_id.is_none());
        assert!(r.unsupported.is_empty());
    }

    #[test]
    fn direct_symbol_rejects_empty() {
        assert!(collect_from_symbol("").is_err());
    }

    #[test]
    fn direct_symbol_rejects_single_segment() {
        // RUSTSEC requires `crate::item` at minimum; mirror the constraint.
        assert!(collect_from_symbol("bare_ident").is_err());
    }

    #[test]
    fn direct_symbol_rejects_trailing_colons() {
        assert!(collect_from_symbol("openssl::").is_err());
        assert!(collect_from_symbol("::openssl::foo").is_err());
    }

    #[test]
    fn audit_json_tier_a_only() {
        let path = fixture_path("audit-tier-a-only.json");
        let r = collect_from_audit_json(&path).unwrap();
        assert_eq!(r.vuln_symbols.len(), 1, "{:?}", r);
        assert!(r.unsupported.is_empty());
        let v = &r.vuln_symbols[0];
        assert_eq!(v.crate_name, "openssl");
        assert_eq!(v.path, "openssl::md::Md::fetch");
        assert_eq!(v.advisory_id.as_deref(), Some("RUSTSEC-2025-0022"));
    }

    #[test]
    fn audit_json_tier_b_only() {
        let path = fixture_path("audit-tier-b-only.json");
        let r = collect_from_audit_json(&path).unwrap();
        assert!(r.vuln_symbols.is_empty(), "{:?}", r);
        assert_eq!(r.unsupported.len(), 1);
        assert_eq!(r.unsupported[0].advisory_id, "RUSTSEC-2099-0001");
        assert_eq!(
            r.unsupported[0].reason,
            UnsupportedReason::NoAffectedFunctions
        );
    }

    #[test]
    fn audit_json_mixed() {
        let path = fixture_path("audit-mixed.json");
        let r = collect_from_audit_json(&path).unwrap();
        assert_eq!(r.vuln_symbols.len(), 1);
        assert_eq!(r.unsupported.len(), 1);
    }

    #[test]
    fn audit_json_empty_list() {
        let path = fixture_path("audit-empty.json");
        let r = collect_from_audit_json(&path).unwrap();
        assert!(r.vuln_symbols.is_empty());
        assert!(r.unsupported.is_empty());
    }

    fn fixture_path(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }
}
