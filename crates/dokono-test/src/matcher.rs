//! Function-level affected-test resolution.
//!
//! BFS reaches files. This module narrows file-level hits down to the test
//! functions whose body range encloses a BFS-visited position.

use dokono_core::bfs::BfsResult;
use dokono_core::types::Position;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::test_goals::TestFn;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AffectedTest {
    pub package: String,
    pub file: PathBuf,
    pub name: String,
    /// `<crate>::<file::components>::<mod>::<name>` — for display / JSON output.
    /// Built from file path relative to the crate root (dropping `src/` and a
    /// terminal `lib`/`main` filename) plus the chain of inline `mod` names
    /// enclosing the function. Not a fully resolved module path — external
    /// `mod foo;` declarations are not followed.
    pub module_path: String,
    /// 1-based line of the function body opening brace.
    pub line: u32,
}

pub fn resolve(
    starts: &[(PathBuf, Position)],
    bfs: &BfsResult,
    tests_by_file: &HashMap<PathBuf, Vec<TestFn>>,
    packages: &PackageMap,
    workspace: &Path,
) -> BTreeSet<AffectedTest> {
    let mut out = BTreeSet::new();
    let visited = starts
        .iter()
        .map(|(p, pos)| (p, *pos))
        .chain(bfs.parents.keys().map(|(p, pos)| (p, *pos)))
        .chain(
            bfs.entry_hits
                .iter()
                .flat_map(|(p, ps)| ps.iter().map(move |pos| (p, *pos))),
        );
    for (path, pos) in visited {
        let Some(tests) = tests_by_file.get(path) else {
            continue;
        };
        for t in tests {
            if pos_in_body(pos, t) {
                out.insert(make_affected(t, packages, workspace));
            }
        }
    }
    out
}

/// BFS positions are LSP 0-based; `body_range` is 1-based.
fn pos_in_body(pos: Position, t: &TestFn) -> bool {
    let line_1based = pos.line + 1;
    t.body_range.0 <= line_1based && line_1based <= t.body_range.1
}

fn make_affected(t: &TestFn, packages: &PackageMap, workspace: &Path) -> AffectedTest {
    let package = packages
        .package_for(&t.file)
        .unwrap_or("<unknown>")
        .to_string();
    let crate_root = packages.root_for(&t.file).unwrap_or(workspace);
    let module_path = module_path_for(t, &package, crate_root);
    AffectedTest {
        module_path,
        package,
        file: t.file.clone(),
        name: t.name.clone(),
        line: t.body_range.0,
    }
}

/// Compose a display module path: `<crate>::<file_segs>::<mod_segs>::<name>`.
/// File-derived segments come from the source file's path relative to the
/// crate root with the conventional `src/` prefix and `lib`/`main` filename
/// stripped. Inline `mod` names from the function's enclosing modules are
/// appended. Hyphens in the package name become underscores so the result
/// reads as a valid Rust path.
fn module_path_for(t: &TestFn, package: &str, crate_root: &Path) -> String {
    let rel = t.file.strip_prefix(crate_root).unwrap_or(&t.file);
    let mut segs: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str().map(str::to_string),
            _ => None,
        })
        .collect();
    if segs.first().map(String::as_str) == Some("src") {
        segs.remove(0);
    }
    if let Some(last) = segs.last_mut()
        && let Some(stripped) = last.strip_suffix(".rs")
    {
        *last = stripped.to_string();
    }
    if matches!(segs.last().map(String::as_str), Some("lib") | Some("main")) {
        segs.pop();
    }
    segs.extend(t.mod_path.iter().cloned());
    segs.push(t.name.clone());
    let crate_ident = package.replace('-', "_");
    format!("{crate_ident}::{}", segs.join("::"))
}

#[derive(Debug, Clone, Default)]
pub struct PackageMap {
    /// (crate-root, package-name); sorted by descending root depth so the most
    /// specific match wins when packages nest.
    entries: Vec<(PathBuf, String)>,
}

impl PackageMap {
    pub fn load(workspace: &Path) -> anyhow::Result<Self> {
        use cargo_metadata::MetadataCommand;
        let manifest = workspace.join("Cargo.toml");
        let metadata = MetadataCommand::new()
            .manifest_path(&manifest)
            .no_deps()
            .exec()?;
        let mut entries = Vec::new();
        for pkg in &metadata.packages {
            let Some(root) = pkg.manifest_path.parent() else {
                continue;
            };
            entries.push((root.as_std_path().to_path_buf(), pkg.name.clone()));
        }
        entries.sort_by_key(|b| std::cmp::Reverse(b.0.components().count()));
        Ok(Self { entries })
    }

    pub fn package_for(&self, file: &Path) -> Option<&str> {
        self.lookup(file).map(|(_, n)| n)
    }

    pub fn root_for(&self, file: &Path) -> Option<&Path> {
        self.lookup(file).map(|(p, _)| p)
    }

    fn lookup(&self, file: &Path) -> Option<(&Path, &str)> {
        self.entries
            .iter()
            .find(|(root, _)| file.starts_with(root))
            .map(|(root, n)| (root.as_path(), n.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_goals::TestAttr;
    use dokono_core::bfs::ParentMap;
    use dokono_core::types::Position;
    use std::collections::BTreeSet;

    fn pos(line: u32) -> Position {
        Position { line, character: 0 }
    }

    fn test_fn(file: &Path, name: &str, body_range: (u32, u32)) -> TestFn {
        TestFn {
            file: file.to_path_buf(),
            name: name.into(),
            body_range,
            attr: TestAttr::Test,
            mod_path: Vec::new(),
        }
    }

    fn empty_packages() -> PackageMap {
        PackageMap::default()
    }

    fn bfs(parents: ParentMap, entry_hits: HashMap<PathBuf, Vec<Position>>) -> BfsResult {
        BfsResult {
            affected: BTreeSet::new(),
            parents,
            entry_hits,
        }
    }

    #[test]
    fn pos_inside_body_hits() {
        let f = PathBuf::from("/ws/crate-a/src/lib.rs");
        let t = test_fn(&f, "foo", (10, 20));
        // pos.line 14 (0-based) → 15 (1-based) ∈ [10, 20]
        assert!(pos_in_body(pos(14), &t));
    }

    #[test]
    fn pos_outside_body_misses() {
        let f = PathBuf::from("/ws/crate-a/src/lib.rs");
        let t = test_fn(&f, "foo", (10, 20));
        assert!(!pos_in_body(pos(5), &t));
        assert!(!pos_in_body(pos(25), &t));
    }

    #[test]
    fn pos_on_boundary_inclusive() {
        let f = PathBuf::from("/ws/crate-a/src/lib.rs");
        let t = test_fn(&f, "foo", (10, 20));
        // body_range is inclusive on both ends; LSP line is pos.line+1
        assert!(pos_in_body(pos(9), &t)); // → 10
        assert!(pos_in_body(pos(19), &t)); // → 20
    }

    #[test]
    fn resolve_picks_test_via_parents() {
        let workspace = PathBuf::from("/ws");
        let file = PathBuf::from("/ws/crate-a/src/lib.rs");
        let tests_by_file: HashMap<PathBuf, Vec<TestFn>> = [(
            file.clone(),
            vec![test_fn(&file, "a", (10, 20)), test_fn(&file, "b", (30, 40))],
        )]
        .into_iter()
        .collect();

        let parents: ParentMap = [
            ((file.clone(), pos(14)), None),
            ((file.clone(), pos(50)), None),
        ]
        .into_iter()
        .collect();

        let out = resolve(
            &[],
            &bfs(parents, HashMap::new()),
            &tests_by_file,
            &empty_packages(),
            &workspace,
        );
        let names: Vec<_> = out.iter().map(|a| a.name.clone()).collect();
        assert_eq!(names, vec!["a".to_string()]);
    }

    #[test]
    fn resolve_picks_test_via_entry_hits() {
        let workspace = PathBuf::from("/ws");
        let file = PathBuf::from("/ws/crate-a/src/lib.rs");
        let tests_by_file: HashMap<PathBuf, Vec<TestFn>> = [(
            file.clone(),
            vec![test_fn(&file, "a", (10, 20)), test_fn(&file, "b", (30, 40))],
        )]
        .into_iter()
        .collect();

        // Only `entry_hits` carries the reach (BFS landed inside an entrypoint file).
        let entry_hits = [(file, vec![pos(34)])].into_iter().collect();

        let out = resolve(
            &[],
            &bfs(ParentMap::new(), entry_hits),
            &tests_by_file,
            &empty_packages(),
            &workspace,
        );
        let names: Vec<_> = out.iter().map(|a| a.name.clone()).collect();
        assert_eq!(names, vec!["b".to_string()]);
    }

    #[test]
    fn resolve_picks_test_via_direct_start_in_test_body() {
        let workspace = PathBuf::from("/ws");
        let file = PathBuf::from("/ws/crate-a/src/lib.rs");
        let tests_by_file: HashMap<PathBuf, Vec<TestFn>> = [(
            file.clone(),
            vec![test_fn(&file, "edited_test", (100, 120))],
        )]
        .into_iter()
        .collect();

        let starts = vec![(file, pos(104))];
        let out = resolve(
            &starts,
            &bfs(ParentMap::new(), HashMap::new()),
            &tests_by_file,
            &empty_packages(),
            &workspace,
        );
        let names: Vec<_> = out.iter().map(|a| a.name.clone()).collect();
        assert_eq!(names, vec!["edited_test".to_string()]);
    }

    #[test]
    fn resolve_skips_files_not_in_tests_by_file() {
        let workspace = PathBuf::from("/ws");
        let test_file = PathBuf::from("/ws/crate-a/src/lib.rs");
        let other = PathBuf::from("/ws/crate-a/src/util.rs");
        let tests_by_file: HashMap<PathBuf, Vec<TestFn>> = [(
            test_file,
            vec![test_fn(&PathBuf::from("ignored"), "a", (10, 20))],
        )]
        .into_iter()
        .collect();

        let parents: ParentMap = [((other, pos(14)), None)].into_iter().collect();
        let out = resolve(
            &[],
            &bfs(parents, HashMap::new()),
            &tests_by_file,
            &empty_packages(),
            &workspace,
        );
        assert!(out.is_empty());
    }
}
