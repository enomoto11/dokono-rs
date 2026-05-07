//! BFS over `references` + `documentSymbol` + `declaration` to find which binary
//! entrypoints are reachable from the changed symbols.
//!
//! Two non-obvious LSP details drive the design:
//!
//! 1. **`documentSymbol` per step.** `references(file, pos)` returns references *to
//!    the symbol at `pos`*; feeding `r.range.start` straight back in just re-asks
//!    for the same symbol. To traverse to the calling function we have to look up
//!    the innermost symbol enclosing the reference site and use its
//!    `selectionRange.start`.
//!
//! 2. **`declaration` to normalize impl → trait.** For methods inside `impl Trait
//!    for Type`, `references` only returns concrete-typed call sites; calls
//!    through `dyn Trait` are missed. Sending `textDocument/declaration` first
//!    redirects to the trait method declaration, where `references` does pick up
//!    trait-dispatched callers. For non-trait symbols the call returns the same
//!    position, so it is safe to do unconditionally.

use anyhow::{Result, anyhow};
use lsp_types::{DocumentSymbol, Location, Position};
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use super::symbols;

/// LSP surface used by the BFS; trait-shaped so tests can inject a fake.
pub trait LspBackend {
    /// Open `file` for analysis. Idempotent.
    fn open(&mut self, file: &Path) -> Result<()>;
    /// Resolve the declaration of the symbol at `file:pos`. For trait-impl methods
    /// this jumps to the trait method declaration; otherwise it returns the input.
    fn declaration(&mut self, file: &Path, pos: Position) -> Result<(PathBuf, Position)>;
    /// References to the symbol at `file:pos` (`includeDeclaration: false`).
    fn references(&mut self, file: &Path, pos: Position) -> Result<Vec<Location>>;
    fn document_symbols(&mut self, file: &Path) -> Result<Vec<DocumentSymbol>>;
}

pub fn run(
    backend: &mut dyn LspBackend,
    starts: Vec<(PathBuf, Position)>,
    entrypoints: &HashSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>> {
    let verbose = std::env::var_os("DOKONO_VERBOSE").is_some();

    let mut queue: VecDeque<(PathBuf, Position)> = starts.into();
    let mut visited: HashSet<(PathBuf, Position)> = HashSet::new();
    let mut affected: BTreeSet<PathBuf> = BTreeSet::new();

    while let Some((file, pos)) = queue.pop_front() {
        if !visited.insert((file.clone(), pos)) {
            continue;
        }
        if verbose {
            eprintln!(
                "[bfs] visit {} @ ({},{})",
                file.display(),
                pos.line,
                pos.character
            );
        }
        backend.open(&file)?;

        // Normalize impl-method positions to their trait-method declaration so that
        // `references` picks up `dyn Trait` callers; for non-trait symbols this is a no-op.
        let (canon_file, canon_pos) = backend.declaration(&file, pos)?;
        let (file, pos) = if (canon_file.as_path(), canon_pos) == (file.as_path(), pos) {
            (file, pos)
        } else {
            if !visited.insert((canon_file.clone(), canon_pos)) {
                continue;
            }
            backend.open(&canon_file)?;
            if verbose {
                eprintln!(
                    "[bfs]   canonicalized → {} @ ({},{}) (trait method)",
                    canon_file.display(),
                    canon_pos.line,
                    canon_pos.character
                );
            }
            (canon_file, canon_pos)
        };

        let refs = backend.references(&file, pos)?;

        for r in refs {
            let ref_file = url_to_path(&r.uri)?;
            if entrypoints.contains(&ref_file) {
                if verbose {
                    eprintln!("[bfs]   ref → entrypoint {}", ref_file.display());
                }
                affected.insert(ref_file);
                continue;
            }
            // Walk to the enclosing function so the next BFS step asks "who calls
            // this caller", not "who else uses the same callee".
            backend.open(&ref_file)?;
            let symbols = backend.document_symbols(&ref_file)?;
            // pick_at_lines takes 1-based git line numbers; r.range.start.line is 0-based.
            let hits = symbols::pick_at_lines(&symbols, &[r.range.start.line + 1]);
            if hits.is_empty() {
                if verbose {
                    eprintln!(
                        "[bfs]   ref {}:{} has no enclosing symbol — skipped",
                        ref_file.display(),
                        r.range.start.line
                    );
                }
                continue;
            }
            for hit in hits {
                if verbose {
                    eprintln!(
                        "[bfs]   ref → {} :: {} @ ({},{})",
                        ref_file.display(),
                        hit.name,
                        hit.position.line,
                        hit.position.character
                    );
                }
                queue.push_back((ref_file.clone(), hit.position));
            }
        }
    }
    Ok(affected)
}

fn url_to_path(url: &lsp_types::Url) -> Result<PathBuf> {
    url.to_file_path()
        .map_err(|_| anyhow!("URL is not a file path: {url}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Range, SymbolKind, Url};
    use std::collections::HashMap;

    /// Stubbed `LspBackend` for unit tests. `declaration` defaults to no-jump unless
    /// a mapping is registered with `add_declaration`.
    struct FakeBackend {
        refs: HashMap<(PathBuf, u32, u32), Vec<Location>>,
        symbols: HashMap<PathBuf, Vec<DocumentSymbol>>,
        declarations: HashMap<(PathBuf, u32, u32), (PathBuf, Position)>,
        ref_calls: usize,
    }

    impl FakeBackend {
        fn new() -> Self {
            Self {
                refs: HashMap::new(),
                symbols: HashMap::new(),
                declarations: HashMap::new(),
                ref_calls: 0,
            }
        }
        fn add_refs(&mut self, file: &Path, pos: Position, refs: Vec<Location>) {
            self.refs
                .insert((file.to_path_buf(), pos.line, pos.character), refs);
        }
        fn add_symbols(&mut self, file: &Path, symbols: Vec<DocumentSymbol>) {
            self.symbols.insert(file.to_path_buf(), symbols);
        }
        fn add_declaration(
            &mut self,
            from_file: &Path,
            from_pos: Position,
            to: (PathBuf, Position),
        ) {
            self.declarations.insert(
                (from_file.to_path_buf(), from_pos.line, from_pos.character),
                to,
            );
        }
    }

    impl LspBackend for FakeBackend {
        fn open(&mut self, _file: &Path) -> Result<()> {
            Ok(())
        }
        fn declaration(&mut self, file: &Path, pos: Position) -> Result<(PathBuf, Position)> {
            Ok(self
                .declarations
                .get(&(file.to_path_buf(), pos.line, pos.character))
                .cloned()
                .unwrap_or_else(|| (file.to_path_buf(), pos)))
        }
        fn references(&mut self, file: &Path, pos: Position) -> Result<Vec<Location>> {
            self.ref_calls += 1;
            Ok(self
                .refs
                .get(&(file.to_path_buf(), pos.line, pos.character))
                .cloned()
                .unwrap_or_default())
        }
        fn document_symbols(&mut self, file: &Path) -> Result<Vec<DocumentSymbol>> {
            Ok(self.symbols.get(file).cloned().unwrap_or_default())
        }
    }

    #[allow(deprecated)] // lsp_types::DocumentSymbol::deprecated field is required for construction
    fn fn_symbol(name: &str, body: (u32, u32), sel: (u32, u32)) -> DocumentSymbol {
        DocumentSymbol {
            name: name.into(),
            detail: None,
            kind: SymbolKind::FUNCTION,
            tags: None,
            deprecated: None,
            range: Range {
                start: Position {
                    line: body.0,
                    character: 0,
                },
                end: Position {
                    line: body.1,
                    character: 0,
                },
            },
            selection_range: Range {
                start: Position {
                    line: sel.0,
                    character: sel.1,
                },
                end: Position {
                    line: sel.0,
                    character: sel.1 + 1,
                },
            },
            children: None,
        }
    }

    fn pos(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    fn loc(file: &Path, line: u32, character: u32) -> Location {
        Location {
            uri: Url::from_file_path(file).unwrap(),
            range: Range {
                start: pos(line, character),
                end: pos(line, character + 1),
            },
        }
    }

    /// `domain::Foo` (changed) ← `usecase::use_foo` ← `bin/main` — should reach `bin`.
    #[test]
    fn finds_entrypoint_via_chain() {
        let domain = PathBuf::from("/test/domain.rs");
        let usecase = PathBuf::from("/test/usecase.rs");
        let bin = PathBuf::from("/test/bin/main.rs");
        let entrypoints: HashSet<PathBuf> = [bin.clone()].into_iter().collect();

        let mut be = FakeBackend::new();

        be.add_refs(&domain, pos(0, 11), vec![loc(&usecase, 4, 13)]);
        be.add_symbols(&usecase, vec![fn_symbol("use_foo", (3, 5), (3, 7))]);
        be.add_refs(&usecase, pos(3, 7), vec![loc(&bin, 1, 14)]);

        let starts = vec![(domain.clone(), pos(0, 11))];
        let affected = run(&mut be, starts, &entrypoints).unwrap();
        assert_eq!(affected, [bin].into_iter().collect());
    }

    /// Cycle A→B→A must terminate via `visited` and not flag any entrypoint.
    #[test]
    fn cycle_terminates() {
        let a = PathBuf::from("/test/a.rs");
        let b = PathBuf::from("/test/b.rs");
        let entrypoints: HashSet<PathBuf> = HashSet::new();

        let mut be = FakeBackend::new();
        be.add_symbols(&a, vec![fn_symbol("a_fn", (0, 5), (0, 3))]);
        be.add_symbols(&b, vec![fn_symbol("b_fn", (0, 5), (0, 3))]);
        be.add_refs(&a, pos(0, 3), vec![loc(&b, 2, 4)]);
        be.add_refs(&b, pos(0, 3), vec![loc(&a, 2, 4)]);

        let starts = vec![(a.clone(), pos(0, 3))];
        let affected = run(&mut be, starts, &entrypoints).unwrap();
        assert!(affected.is_empty());
        // Each node visited exactly once.
        assert_eq!(be.ref_calls, 2);
    }

    #[test]
    fn direct_reference_to_entrypoint() {
        let domain = PathBuf::from("/test/domain.rs");
        let bin = PathBuf::from("/test/bin/main.rs");
        let entrypoints: HashSet<PathBuf> = [bin.clone()].into_iter().collect();

        let mut be = FakeBackend::new();
        be.add_refs(&domain, pos(0, 11), vec![loc(&bin, 1, 14)]);

        let starts = vec![(domain, pos(0, 11))];
        let affected = run(&mut be, starts, &entrypoints).unwrap();
        assert_eq!(affected, [bin].into_iter().collect());
    }

    #[test]
    fn multiple_paths_to_same_entrypoint_dedup() {
        let domain = PathBuf::from("/test/domain.rs");
        let bin = PathBuf::from("/test/bin/main.rs");
        let entrypoints: HashSet<PathBuf> = [bin.clone()].into_iter().collect();

        let mut be = FakeBackend::new();
        be.add_refs(
            &domain,
            pos(0, 11),
            vec![loc(&bin, 1, 14), loc(&bin, 5, 20)],
        );

        let starts = vec![(domain, pos(0, 11))];
        let affected = run(&mut be, starts, &entrypoints).unwrap();
        assert_eq!(affected.len(), 1);
        assert!(affected.contains(&bin));
    }

    /// Without `declaration` normalizing impl→trait, `dyn Trait` callers are missed.
    #[test]
    fn impl_method_normalized_to_trait_method() {
        let domain = PathBuf::from("/test/domain.rs");
        let usecase = PathBuf::from("/test/usecase.rs");
        let bin = PathBuf::from("/test/bin/main.rs");
        let entrypoints: HashSet<PathBuf> = [bin.clone()].into_iter().collect();

        let mut be = FakeBackend::new();

        // Foo's reference lives inside the impl method's body in usecase.rs.
        be.add_refs(&domain, pos(0, 11), vec![loc(&usecase, 4, 13)]);
        be.add_symbols(&usecase, vec![fn_symbol("use_foo_impl", (3, 5), (3, 7))]);
        // declaration: impl method (3,7) → trait method (1,7).
        let trait_pos = Position {
            line: 1,
            character: 7,
        };
        be.add_declaration(&usecase, pos(3, 7), (usecase.clone(), trait_pos));
        // The trait method's references reach bin/main.rs.
        be.add_refs(&usecase, trait_pos, vec![loc(&bin, 5, 4)]);

        let starts = vec![(domain, pos(0, 11))];
        let affected = run(&mut be, starts, &entrypoints).unwrap();
        assert_eq!(affected, [bin].into_iter().collect());
    }

    /// References landing on module-level lines (e.g. `use` statements) have no
    /// enclosing function and must be silently dropped from the queue.
    #[test]
    fn reference_with_no_enclosing_symbol_skipped() {
        let domain = PathBuf::from("/test/domain.rs");
        let other = PathBuf::from("/test/other.rs");
        let bin = PathBuf::from("/test/bin/main.rs");
        let entrypoints: HashSet<PathBuf> = [bin].into_iter().collect();

        let mut be = FakeBackend::new();
        be.add_refs(&domain, pos(0, 11), vec![loc(&other, 0, 5)]);
        be.add_symbols(&other, vec![fn_symbol("some_fn", (10, 20), (10, 7))]);

        let starts = vec![(domain, pos(0, 11))];
        let affected = run(&mut be, starts, &entrypoints).unwrap();
        assert!(affected.is_empty());
    }
}
