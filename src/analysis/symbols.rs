//! Map git-diff line numbers to the **innermost** enclosing `DocumentSymbol`.
//! When a parent symbol and one of its children both contain the line we prefer
//! the child, so that BFS starts from the function body that was edited rather
//! than from the surrounding `impl` block.

use super::types::{DocumentSymbol, Position, Range};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolHit {
    pub name: String,
    pub position: Position,
}

/// `git_lines_1based` is converted to LSP 0-based internally. Hits with the
/// same `selection_range.start` are deduplicated.
pub fn pick_at_lines(symbols: &[DocumentSymbol], git_lines_1based: &[u32]) -> Vec<SymbolHit> {
    let mut hits: Vec<SymbolHit> = Vec::new();
    for &line1 in git_lines_1based {
        let line0 = line1.saturating_sub(1);
        for sym in innermost_at(symbols, line0) {
            let hit = SymbolHit {
                name: sym.name.clone(),
                position: sym.selection_range.start,
            };
            if !hits.iter().any(|h| h.position == hit.position) {
                hits.push(hit);
            }
        }
    }
    hits
}

fn innermost_at(symbols: &[DocumentSymbol], line0: u32) -> Vec<&DocumentSymbol> {
    let mut result = Vec::new();
    for sym in symbols {
        if !range_contains_line(&sym.range, line0) {
            continue;
        }
        let nested = innermost_at(&sym.children, line0);
        if nested.is_empty() {
            result.push(sym);
        } else {
            result.extend(nested);
        }
    }
    result
}

fn range_contains_line(range: &Range, line0: u32) -> bool {
    range.start.line <= line0 && line0 <= range.end.line
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests assert against `selection_range.start` of named symbols rather than literal
    /// line numbers, so re-numbering the fixture doesn't silently break them.
    fn sym(
        name: &str,
        range: (u32, u32),
        sel_line: u32,
        children: Vec<DocumentSymbol>,
    ) -> DocumentSymbol {
        DocumentSymbol {
            name: name.into(),
            range: Range {
                start: Position { line: range.0, character: 0 },
                end: Position { line: range.1, character: 0 },
            },
            selection_range: Range {
                start: Position { line: sel_line, character: 4 },
                end: Position { line: sel_line, character: 7 },
            },
            children,
        }
    }

    #[test]
    fn picks_innermost_function_inside_impl() {
        // impl Bar { fn foo(); } — editing foo's body should return foo.
        let foo = sym("foo", (10, 20), 10, vec![]);
        let bar = sym("Bar", (5, 25), 5, vec![foo.clone()]);
        let symbols = vec![bar];

        let hits = pick_at_lines(&symbols, &[16]);
        assert_eq!(
            hits,
            vec![SymbolHit {
                name: "foo".into(),
                position: foo.selection_range.start,
            }]
        );
    }

    #[test]
    fn falls_back_to_outer_when_no_child_matches() {
        let foo = sym("foo", (10, 20), 10, vec![]);
        let bar = sym("Bar", (5, 25), 5, vec![foo]);
        let symbols = vec![bar.clone()];

        // Line 7 is inside Bar but outside foo's range.
        let hits = pick_at_lines(&symbols, &[7]);
        assert_eq!(
            hits,
            vec![SymbolHit {
                name: "Bar".into(),
                position: bar.selection_range.start,
            }]
        );
    }

    #[test]
    fn line_outside_all_returns_empty() {
        let foo = sym("foo", (10, 20), 10, vec![]);
        let symbols = vec![foo];
        assert!(pick_at_lines(&symbols, &[1]).is_empty());
    }

    #[test]
    fn dedupes_when_multiple_lines_hit_same_symbol() {
        let foo = sym("foo", (10, 20), 10, vec![]);
        let symbols = vec![foo.clone()];
        let hits = pick_at_lines(&symbols, &[12, 15, 18]);
        assert_eq!(
            hits,
            vec![SymbolHit {
                name: "foo".into(),
                position: foo.selection_range.start,
            }]
        );
    }

    #[test]
    fn multiple_top_level_symbols_each_hit() {
        let a = sym("a", (5, 10), 5, vec![]);
        let b = sym("b", (20, 30), 20, vec![]);
        let symbols = vec![a.clone(), b.clone()];
        let hits = pick_at_lines(&symbols, &[7, 25]);
        assert_eq!(
            hits,
            vec![
                SymbolHit {
                    name: "a".into(),
                    position: a.selection_range.start
                },
                SymbolHit {
                    name: "b".into(),
                    position: b.selection_range.start
                },
            ]
        );
    }

    #[test]
    fn deeply_nested_innermost() {
        // mod m { impl S { fn f() { ... } } } — innermost match is f.
        let f = sym("f", (10, 14), 10, vec![]);
        let s = sym("S", (8, 16), 8, vec![f.clone()]);
        let m = sym("m", (5, 20), 5, vec![s]);
        let symbols = vec![m];

        let hits = pick_at_lines(&symbols, &[13]);
        assert_eq!(
            hits,
            vec![SymbolHit {
                name: "f".into(),
                position: f.selection_range.start
            }]
        );
    }
}
