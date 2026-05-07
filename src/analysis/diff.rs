//! Run `git diff --unified=0` and extract changed `.rs` files plus their new-side line numbers.
//! Line numbers are **1-based** (git convention); LSP is 0-based, so callers convert.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// New-side path as emitted by git (relative to the workspace root).
    pub path: PathBuf,
    /// 1-based line numbers, sorted and deduplicated.
    pub lines: Vec<u32>,
}

pub fn run(workspace: &Path, base: &str, head: &str) -> Result<Vec<ChangedFile>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .arg("diff")
        .arg(base)
        .arg(head)
        .arg("--unified=0")
        .arg("--")
        .arg("*.rs")
        .output()
        .with_context(|| format!("failed to spawn git diff {base}..{head}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "git diff exited with status {}: {}",
            output.status,
            stderr.trim()
        );
    }

    let text =
        std::str::from_utf8(&output.stdout).context("git diff output was not valid UTF-8")?;
    parse_unified_diff(text)
}

pub fn parse_unified_diff(text: &str) -> Result<Vec<ChangedFile>> {
    let mut files: Vec<ChangedFile> = Vec::new();
    let mut current: Option<ChangedFile> = None;
    let mut deleted = false;

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            if let Some(file) = current.take() {
                push_if_lines(&mut files, file);
            }
            deleted = false;

            if rest == "/dev/null" {
                deleted = true;
                continue;
            }

            let path = rest.strip_prefix("b/").unwrap_or(rest);
            current = Some(ChangedFile {
                path: PathBuf::from(path),
                lines: Vec::new(),
            });
        } else if let Some(rest) = line.strip_prefix("@@ ") {
            if deleted {
                continue;
            }
            let Some(file) = current.as_mut() else {
                continue;
            };
            let Some((header, _)) = rest.split_once(" @@") else {
                bail!("malformed hunk header: {}", line);
            };
            let token = header
                .split_whitespace()
                .find(|t| t.starts_with('+'))
                .with_context(|| format!("hunk header missing new-file range: {line}"))?;
            let token = &token[1..];

            let (start, count) = match token.split_once(',') {
                Some((a, b)) => (
                    a.parse::<u32>()
                        .with_context(|| format!("bad hunk start: {line}"))?,
                    b.parse::<u32>()
                        .with_context(|| format!("bad hunk count: {line}"))?,
                ),
                None => (
                    token
                        .parse::<u32>()
                        .with_context(|| format!("bad hunk start: {line}"))?,
                    1,
                ),
            };

            if count == 0 {
                continue;
            }

            for i in 0..count {
                file.lines.push(start + i);
            }
        }
    }
    if let Some(file) = current.take() {
        push_if_lines(&mut files, file);
    }

    for f in files.iter_mut() {
        f.lines.sort_unstable();
        f.lines.dedup();
    }

    Ok(files)
}

fn push_if_lines(files: &mut Vec<ChangedFile>, file: ChangedFile) {
    if !file.lines.is_empty() {
        files.push(file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_file_single_hunk() {
        let diff = "\
diff --git a/src/foo.rs b/src/foo.rs
index abc..def 100644
--- a/src/foo.rs
+++ b/src/foo.rs
@@ -10,2 +12,3 @@
+new1
+new2
+new3
";
        let result = parse_unified_diff(diff).unwrap();
        assert_eq!(
            result,
            vec![ChangedFile {
                path: PathBuf::from("src/foo.rs"),
                lines: vec![12, 13, 14],
            }]
        );
    }

    #[test]
    fn single_line_hunk_no_comma() {
        let diff = "+++ b/x.rs\n@@ -5 +7 @@\n+y\n";
        let result = parse_unified_diff(diff).unwrap();
        assert_eq!(
            result,
            vec![ChangedFile {
                path: PathBuf::from("x.rs"),
                lines: vec![7],
            }]
        );
    }

    #[test]
    fn pure_deletion_no_added_lines() {
        let diff = "+++ b/x.rs\n@@ -5,3 +5,0 @@\n";
        let result = parse_unified_diff(diff).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn file_deletion_skipped() {
        let diff = "--- a/x.rs\n+++ /dev/null\n@@ -1,5 +0,0 @@\n-line\n";
        let result = parse_unified_diff(diff).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn file_creation() {
        let diff = "--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1,3 @@\n+a\n+b\n+c\n";
        let result = parse_unified_diff(diff).unwrap();
        assert_eq!(
            result,
            vec![ChangedFile {
                path: PathBuf::from("new.rs"),
                lines: vec![1, 2, 3],
            }]
        );
    }

    #[test]
    fn multiple_hunks_in_same_file_are_merged_and_sorted() {
        let diff = "\
+++ b/x.rs
@@ -20 +25,2 @@
+b
+c
@@ -10 +10 @@
+a
";
        let result = parse_unified_diff(diff).unwrap();
        assert_eq!(
            result,
            vec![ChangedFile {
                path: PathBuf::from("x.rs"),
                lines: vec![10, 25, 26],
            }]
        );
    }

    #[test]
    fn multiple_files() {
        let diff = "\
+++ b/a.rs
@@ -1 +1 @@
+a
+++ b/b.rs
@@ -2 +5 @@
+b
";
        let result = parse_unified_diff(diff).unwrap();
        assert_eq!(
            result,
            vec![
                ChangedFile {
                    path: PathBuf::from("a.rs"),
                    lines: vec![1],
                },
                ChangedFile {
                    path: PathBuf::from("b.rs"),
                    lines: vec![5],
                }
            ]
        );
    }

    #[test]
    fn hunk_with_trailing_context_in_header() {
        let diff = "+++ b/x.rs\n@@ -10,1 +12,1 @@ fn foo()\n+y\n";
        let result = parse_unified_diff(diff).unwrap();
        assert_eq!(
            result,
            vec![ChangedFile {
                path: PathBuf::from("x.rs"),
                lines: vec![12],
            }]
        );
    }

    #[test]
    fn empty_diff() {
        let result = parse_unified_diff("").unwrap();
        assert!(result.is_empty());
    }
}
