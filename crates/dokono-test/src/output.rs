//! Affected-test output formatting.
//!
//! `text` (default) prints a human-readable summary to stdout. `json` emits a
//! single object suitable for `jq`/CI consumers.

use crate::cli::OutputFormat;
use crate::matcher::AffectedTest;
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

pub struct Summary {
    pub pr: Option<u32>,
    pub base: String,
    pub head: String,
    pub affected: BTreeSet<AffectedTest>,
    pub total_tests: usize,
}

pub fn emit(format: OutputFormat, summary: &Summary, workspace: &Path) -> Result<()> {
    match format {
        OutputFormat::Text => {
            print_text(summary, workspace);
            Ok(())
        }
        OutputFormat::Json => {
            print_json(summary, workspace)?;
            Ok(())
        }
    }
}

fn print_text(summary: &Summary, workspace: &Path) {
    if summary.affected.is_empty() {
        println!("Affected tests: none");
        return;
    }
    println!("Affected tests: ({} functions)", summary.affected.len());
    for t in &summary.affected {
        let rel = t.file.strip_prefix(workspace).unwrap_or(&t.file);
        println!(
            "  {} :: {} ({}:{})",
            t.package,
            t.name,
            rel.display(),
            t.line
        );
    }
}

fn print_json(summary: &Summary, workspace: &Path) -> Result<()> {
    let affected: Vec<SerializedAffectedTest> = summary
        .affected
        .iter()
        .map(|t| SerializedAffectedTest::from(t, workspace))
        .collect();
    let payload = JsonSummary {
        pr: summary.pr,
        base: &summary.base,
        head: &summary.head,
        affected_tests: affected,
        stats: Stats::from(summary),
    };
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

#[derive(Serialize)]
struct JsonSummary<'a> {
    pr: Option<u32>,
    base: &'a str,
    head: &'a str,
    affected_tests: Vec<SerializedAffectedTest>,
    stats: Stats,
}

#[derive(Serialize)]
struct SerializedAffectedTest {
    package: String,
    file: String,
    name: String,
    module_path: String,
    line: u32,
}

impl SerializedAffectedTest {
    fn from(t: &AffectedTest, workspace: &Path) -> Self {
        let rel = t.file.strip_prefix(workspace).unwrap_or(&t.file);
        Self {
            package: t.package.clone(),
            file: rel.display().to_string(),
            name: t.name.clone(),
            module_path: t.module_path.clone(),
            line: t.line,
        }
    }
}

#[derive(Serialize)]
struct Stats {
    total_tests: usize,
    affected_tests: usize,
}

impl Stats {
    fn from(summary: &Summary) -> Self {
        Self {
            total_tests: summary.total_tests,
            affected_tests: summary.affected.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn summary_with(affected_count: usize, total: usize) -> Summary {
        let mut affected = BTreeSet::new();
        for i in 0..affected_count {
            affected.insert(AffectedTest {
                package: "p".into(),
                file: PathBuf::from(format!("/ws/p/src/lib.rs:{i}")),
                name: format!("t{i}"),
                module_path: format!("p::t{i}"),
                line: 1,
            });
        }
        Summary {
            pr: None,
            base: "master".into(),
            head: "feat".into(),
            affected,
            total_tests: total,
        }
    }

    #[test]
    fn stats_counts_match() {
        let s = summary_with(3, 1000);
        let st = Stats::from(&s);
        assert_eq!(st.total_tests, 1000);
        assert_eq!(st.affected_tests, 3);
    }

    #[test]
    fn json_roundtrip_parses() {
        let s = summary_with(2, 100);
        let workspace = PathBuf::from("/ws");
        let affected: Vec<_> = s
            .affected
            .iter()
            .map(|t| SerializedAffectedTest::from(t, &workspace))
            .collect();
        let payload = JsonSummary {
            pr: s.pr,
            base: &s.base,
            head: &s.head,
            affected_tests: affected,
            stats: Stats::from(&s),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.get("schema_version").is_none());
        assert!(v.get("status").is_none());
        assert!(v["stats"].get("reduction_pct").is_none());
        assert_eq!(v["stats"]["total_tests"], 100);
        assert_eq!(v["stats"]["affected_tests"], 2);
        assert_eq!(v["affected_tests"].as_array().unwrap().len(), 2);
    }
}
