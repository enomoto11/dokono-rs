//! Static collection of test-goal functions via `syn`.
//!
//! Walks the workspace, parses every `.rs` file standalone, and lists every
//! free function whose attribute set matches a known test attribute. No build,
//! no macro expansion — so proc-macro-generated tests (`sqlx::test`, etc.) are
//! intentionally missed; see design.md §2.2③.
//!
//! Files that fail to read or parse are skipped silently; the walk should be
//! resilient against in-flight edits or incomplete code.
//!
//! Line numbers are 1-based to match the rest of the dokono pipeline.

use anyhow::Result;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::Visit;
use walkdir::{DirEntry, WalkDir};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestAttr {
    /// `#[test]`
    Test,
    /// `#[tokio::test]`
    TokioTest,
    /// `#[async_std::test]`
    AsyncStdTest,
    /// `#[rstest]`
    Rstest,
    /// `#[test_case(...)]` — one syn-level function may expand to N runtime cases.
    TestCase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestFn {
    pub file: PathBuf,
    pub name: String,
    /// Inclusive 1-based line range covering the **whole** test item: from the
    /// first attribute (`#[test]` / `#[tokio::test]` / ...) down to the closing
    /// `}` of the body. We use the outer range, not just the body, because
    /// rust-analyzer's `documentSymbol` reports the function's
    /// `selectionRange.start` (the identifier) which sits between the
    /// attributes and the body — that point is outside the body braces yet
    /// clearly inside the same test item.
    pub body_range: (u32, u32),
    pub attr: TestAttr,
    /// Inline `mod` names that wrap this function, in nesting order. e.g.
    /// `["tests"]` for `mod tests { #[test] fn foo() {} }`. External `mod foo;`
    /// declarations are not followed — those live in another file and contribute
    /// to the path via `file` instead.
    pub mod_path: Vec<String>,
}

pub fn collect(workspace: &Path) -> Result<Vec<TestFn>> {
    let mut out = Vec::new();
    let walker = WalkDir::new(workspace)
        .into_iter()
        .filter_entry(|e| !is_excluded_dir(e));
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::debug!("walkdir error: {err}");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.path().extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        collect_in_file(entry.path(), &mut out);
    }
    Ok(out)
}

fn is_excluded_dir(entry: &DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return false;
    }
    let name = entry.file_name().to_str().unwrap_or("");
    name == "target" || name == ".git"
}

fn collect_in_file(path: &Path, out: &mut Vec<TestFn>) {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(err) => {
            tracing::debug!("read {} failed: {err}", path.display());
            return;
        }
    };
    let file = match syn::parse_file(&src) {
        Ok(f) => f,
        Err(err) => {
            tracing::debug!("syn parse {} failed: {err}", path.display());
            return;
        }
    };
    let mut visitor = TestVisitor {
        file_path: path,
        out,
        mod_stack: Vec::new(),
    };
    visitor.visit_file(&file);
}

struct TestVisitor<'a> {
    file_path: &'a Path,
    out: &'a mut Vec<TestFn>,
    mod_stack: Vec<String>,
}

impl<'ast> Visit<'ast> for TestVisitor<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        // External `mod foo;` has no inline body; skip — the contents live in
        // another file and will be picked up when the walker reaches it.
        if node.content.is_some() {
            self.mod_stack.push(node.ident.to_string());
            syn::visit::visit_item_mod(self, node);
            self.mod_stack.pop();
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if let Some(attr) = classify_attrs(&node.attrs) {
            let start_line = node
                .attrs
                .first()
                .map(|a| a.span().start().line as u32)
                .unwrap_or_else(|| node.sig.fn_token.span().start().line as u32);
            let end_line = node.block.span().end().line as u32;
            self.out.push(TestFn {
                file: self.file_path.to_path_buf(),
                name: node.sig.ident.to_string(),
                body_range: (start_line, end_line),
                attr,
                mod_path: self.mod_stack.clone(),
            });
        }
        syn::visit::visit_item_fn(self, node);
    }
}

/// Match by the path's last segment so we accept both `#[tokio::test]` and the
/// fully-qualified `#[::tokio::test]` form, and likewise `#[test_case::test_case(...)]`
/// in addition to `#[test_case(...)]`. The bare `#[test]` is special-cased on
/// path length so we do not mistakenly catch every `*::test` ident.
fn classify_attrs(attrs: &[syn::Attribute]) -> Option<TestAttr> {
    for attr in attrs {
        let segs = &attr.path().segments;
        let Some(last) = segs.last() else {
            continue;
        };
        if last.ident == "test" {
            if segs.len() == 1 {
                return Some(TestAttr::Test);
            }
            if segs.iter().any(|s| s.ident == "tokio") {
                return Some(TestAttr::TokioTest);
            }
            if segs.iter().any(|s| s.ident == "async_std") {
                return Some(TestAttr::AsyncStdTest);
            }
            continue;
        }
        if last.ident == "rstest" {
            return Some(TestAttr::Rstest);
        }
        if last.ident == "test_case" {
            return Some(TestAttr::TestCase);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(name: &str, contents: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "dokono-test-fixture-{}-{nanos}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.rs"));
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn cleanup(path: &Path) {
        if let Some(p) = path.parent() {
            let _ = std::fs::remove_dir_all(p);
        }
    }

    #[test]
    fn detects_simple_test_fn() {
        let path = write_tmp(
            "simple",
            "#[test]\nfn foo() {\n    assert_eq!(1, 1);\n}\n\nfn not_a_test() {}\n",
        );
        let mut out = Vec::new();
        collect_in_file(&path, &mut out);
        assert_eq!(out.len(), 1, "expected one test, got {out:?}");
        assert_eq!(out[0].name, "foo");
        assert_eq!(out[0].attr, TestAttr::Test);
        // body_range covers the whole item: from `#[test]` on line 1 down to
        // the closing brace on line 4 of the 6-line fixture.
        assert_eq!(out[0].body_range, (1, 4));
        cleanup(&path);
    }

    #[test]
    fn detects_test_fn_in_nested_module() {
        let path = write_tmp(
            "nested",
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn nested_test() {}\n\n    fn helper() {}\n}\n",
        );
        let mut out = Vec::new();
        collect_in_file(&path, &mut out);
        assert_eq!(out.len(), 1, "expected one test, got {out:?}");
        assert_eq!(out[0].name, "nested_test");
        cleanup(&path);
    }

    #[test]
    fn ignores_non_test_attributes() {
        let path = write_tmp(
            "non-test",
            "#[derive(Debug)]\nstruct S;\n\n#[inline]\nfn inlined() {}\n\n#[allow(dead_code)]\nfn allowed() {}\n",
        );
        let mut out = Vec::new();
        collect_in_file(&path, &mut out);
        assert!(out.is_empty(), "got: {out:?}");
        cleanup(&path);
    }

    #[test]
    fn detects_tokio_test() {
        let path = write_tmp(
            "tokio",
            "#[tokio::test]\nasync fn tk() {}\n\n#[tokio::test(flavor = \"current_thread\")]\nasync fn tk_flav() {}\n",
        );
        let mut out = Vec::new();
        collect_in_file(&path, &mut out);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|t| t.attr == TestAttr::TokioTest));
        cleanup(&path);
    }

    #[test]
    fn detects_async_std_test() {
        let path = write_tmp("async-std", "#[async_std::test]\nasync fn asyncstd() {}\n");
        let mut out = Vec::new();
        collect_in_file(&path, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].attr, TestAttr::AsyncStdTest);
        cleanup(&path);
    }

    #[test]
    fn detects_rstest() {
        let path = write_tmp("rstest", "#[rstest]\nfn rs(#[case] x: i32) {}\n");
        let mut out = Vec::new();
        collect_in_file(&path, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].attr, TestAttr::Rstest);
        cleanup(&path);
    }

    #[test]
    fn detects_test_case() {
        let path = write_tmp(
            "test-case",
            "#[test_case(\"a\")]\n#[test_case(\"b\")]\nfn tc(x: &str) {}\n",
        );
        let mut out = Vec::new();
        collect_in_file(&path, &mut out);
        assert_eq!(out.len(), 1, "got: {out:?}");
        assert_eq!(out[0].attr, TestAttr::TestCase);
        cleanup(&path);
    }

    /// when written as `#[test_case::test_case(...)]` and `#[tokio::test]` with full paths.
    #[test]
    fn detects_fully_qualified_test_case_and_tokio() {
        let path = write_tmp(
            "fq",
            "#[test_case::test_case(1)]\nfn a(x: i32) {}\n\n#[::tokio::test]\nasync fn b() {}\n",
        );
        let mut out = Vec::new();
        collect_in_file(&path, &mut out);
        assert_eq!(out.len(), 2, "got: {out:?}");
        assert!(
            out.iter()
                .any(|t| t.name == "a" && t.attr == TestAttr::TestCase)
        );
        assert!(
            out.iter()
                .any(|t| t.name == "b" && t.attr == TestAttr::TokioTest)
        );
        cleanup(&path);
    }

    #[test]
    fn collect_walks_subdirs_and_skips_target() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("dokono-test-walk-{}-{nanos}", std::process::id()));
        let sub = root.join("crate-a/src");
        let target = root.join("target/debug");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(sub.join("lib.rs"), "#[test]\nfn inside() {}\n").unwrap();
        std::fs::write(
            target.join("garbage.rs"),
            "#[test]\nfn should_not_be_seen() {}\n",
        )
        .unwrap();

        let tests = collect(&root).unwrap();
        let names: Vec<_> = tests.iter().map(|t| t.name.clone()).collect();
        assert!(names.contains(&"inside".to_string()), "got: {names:?}");
        assert!(
            !names.contains(&"should_not_be_seen".to_string()),
            "target was scanned: {names:?}"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
