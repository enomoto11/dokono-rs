//! Affected-test output formatting.
//!
//! `text` (default) prints a human-readable summary to stdout. `json` emits a
//! single object suitable for `jq`/CI consumers; the schema is stable through
//! `schema_version`. `cargo` is added in Step 9.

use crate::cli::OutputFormat;
use crate::matcher::AffectedTest;
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    NoRsChanges,
    NoSymbolChanges,
}

pub struct Summary {
    pub schema_version: u32,
    pub pr: Option<u32>,
    pub base: String,
    pub head: String,
    pub status: Status,
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
        OutputFormat::Cargo => write_cargo(summary, &mut std::io::stdout()),
    }
}

/// Emit one `cargo test -p PKG ...` line per affected package, joined with
/// `&& \\\n` so the entire script can be piped to a shell and fails fast on
/// the first failing package.
///
/// We feed `test_path` (file + inline mod + fn) as a positional filter, not
/// `--exact`, because `#[test_case]`-generated subcases appear under the
/// path `<test_path>::_<case-id>` and substring matching catches them
/// alongside the base function. With `--exact` only the bare function (no
/// subcases) would run, defeating the point of the cargo format.
fn write_cargo<W: std::io::Write>(summary: &Summary, mut out: W) -> Result<()> {
    if summary.affected.is_empty() {
        writeln!(out, "# dokono-test: no affected tests")?;
        return Ok(());
    }
    let mut by_pkg: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for t in &summary.affected {
        by_pkg
            .entry(t.package.as_str())
            .or_default()
            .push(&t.test_path);
    }
    let lines: Vec<String> = by_pkg
        .into_iter()
        .map(|(pkg, paths)| {
            let escaped: Vec<String> = paths.iter().map(|n| shell_quote(n)).collect();
            // cargo's positional TESTNAME accepts at most one filter; multiple
            // filters must be passed to libtest after `--`.
            format!("cargo test -p {pkg} -- {}", escaped.join(" "))
        })
        .collect();
    writeln!(out, "{}", lines.join(" && \\\n"))?;
    Ok(())
}

/// Single-quote the argument and escape any embedded single quotes. Safe for
/// bash/zsh; we keep it minimal because test idents are mostly ASCII or kanji
/// without shell metacharacters.
fn shell_quote(s: &str) -> String {
    if !s
        .chars()
        .any(|c| c.is_whitespace() || "'\"\\$`!#&;<>()|*?{}[]~".contains(c))
    {
        return s.to_string();
    }
    let escaped = s.replace('\'', r#"'\''"#);
    format!("'{escaped}'")
}

fn print_text(summary: &Summary, workspace: &Path) {
    match summary.status {
        Status::NoRsChanges => {
            println!(
                "(no .rs file changes between {} and {})",
                summary.base, summary.head
            );
        }
        Status::NoSymbolChanges => {
            println!("(no symbol-level changes found)");
        }
        Status::Ok => {
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
    }
}

fn print_json(summary: &Summary, workspace: &Path) -> Result<()> {
    let affected: Vec<SerializedAffectedTest> = summary
        .affected
        .iter()
        .map(|t| SerializedAffectedTest::from(t, workspace))
        .collect();
    let stats = Stats::from(summary);
    let payload = JsonSummary {
        schema_version: summary.schema_version,
        pr: summary.pr,
        base: &summary.base,
        head: &summary.head,
        status: summary.status,
        affected_tests: affected,
        stats,
    };
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

#[derive(Serialize)]
struct JsonSummary<'a> {
    schema_version: u32,
    pr: Option<u32>,
    base: &'a str,
    head: &'a str,
    status: Status,
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
    /// Reduction relative to total tests, rounded to 1 decimal. 0.0 when
    /// total_tests is 0 to avoid division-by-zero on empty workspaces.
    reduction_pct: f64,
}

impl Stats {
    fn from(summary: &Summary) -> Self {
        let affected = summary.affected.len();
        let total = summary.total_tests;
        let raw = if total == 0 {
            0.0
        } else {
            (1.0 - affected as f64 / total as f64) * 100.0
        };
        Self {
            total_tests: total,
            affected_tests: affected,
            reduction_pct: (raw * 10.0).round() / 10.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn summary_with(affected_count: usize, total: usize, status: Status) -> Summary {
        let mut affected = BTreeSet::new();
        for i in 0..affected_count {
            affected.insert(AffectedTest {
                package: "p".into(),
                file: PathBuf::from(format!("/ws/p/src/lib.rs:{i}")),
                name: format!("t{i}"),
                module_path: format!("p::t{i}"),
                test_path: format!("t{i}"),
                line: 1,
            });
        }
        Summary {
            schema_version: 1,
            pr: None,
            base: "master".into(),
            head: "feat".into(),
            status,
            affected,
            total_tests: total,
        }
    }

    fn affected_in(package: &str, name: &str) -> AffectedTest {
        AffectedTest {
            package: package.into(),
            file: PathBuf::from(format!("/ws/{package}/src/lib.rs")),
            name: name.into(),
            module_path: format!("{}::{}", package.replace('-', "_"), name),
            test_path: name.to_string(),
            line: 10,
        }
    }

    fn summary_with_affected(items: Vec<AffectedTest>) -> Summary {
        Summary {
            schema_version: 1,
            pr: None,
            base: "master".into(),
            head: "feat".into(),
            status: Status::Ok,
            affected: items.into_iter().collect(),
            total_tests: 100,
        }
    }

    #[test]
    fn cargo_groups_tests_by_package() {
        let s = summary_with_affected(vec![
            affected_in("a", "t1"),
            affected_in("a", "t2"),
            affected_in("b", "t3"),
        ]);
        let mut buf = Vec::new();
        write_cargo(&s, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("cargo test -p a -- t1 t2"), "got: {out}");
        assert!(out.contains("cargo test -p b -- t3"), "got: {out}");
        assert!(out.contains(" && \\\n"));
    }

    #[test]
    fn cargo_empty_writes_comment() {
        let s = summary_with_affected(vec![]);
        let mut buf = Vec::new();
        write_cargo(&s, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.starts_with("# dokono-test: no affected tests"));
    }

    #[test]
    fn cargo_quotes_special_chars_in_name() {
        let s = summary_with_affected(vec![affected_in("a", "has space and ;semicolon")]);
        let mut buf = Vec::new();
        write_cargo(&s, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("'has space and ;semicolon'"), "got: {out}");
    }

    #[test]
    fn reduction_pct_basic() {
        let s = summary_with(3, 1000, Status::Ok);
        let st = Stats::from(&s);
        assert_eq!(st.total_tests, 1000);
        assert_eq!(st.affected_tests, 3);
        assert_eq!(st.reduction_pct, 99.7);
    }

    #[test]
    fn reduction_pct_zero_total() {
        let s = summary_with(0, 0, Status::Ok);
        let st = Stats::from(&s);
        assert_eq!(st.reduction_pct, 0.0);
    }

    #[test]
    fn json_roundtrip_parses() {
        let s = summary_with(2, 100, Status::Ok);
        let workspace = PathBuf::from("/ws");
        let affected: Vec<_> = s
            .affected
            .iter()
            .map(|t| SerializedAffectedTest::from(t, &workspace))
            .collect();
        let payload = JsonSummary {
            schema_version: s.schema_version,
            pr: s.pr,
            base: &s.base,
            head: &s.head,
            status: s.status,
            affected_tests: affected,
            stats: Stats::from(&s),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["status"], "ok");
        assert_eq!(v["stats"]["total_tests"], 100);
        assert_eq!(v["stats"]["affected_tests"], 2);
        assert_eq!(v["affected_tests"].as_array().unwrap().len(), 2);
    }
}
