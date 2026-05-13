//! Domain types used by the analysis layer.

use std::path::PathBuf;

/// 0-based line/character position (mirrors `lsp_types::Position`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// Half-open range [start, end) expressed as two positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// A reference location: file path + range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub range: Range,
}

/// Hierarchical symbol information (mirrors the subset of
/// `lsp_types::DocumentSymbol` used by BFS/symbols).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSymbol {
    pub name: String,
    pub range: Range,
    pub selection_range: Range,
    pub children: Vec<DocumentSymbol>,
}
