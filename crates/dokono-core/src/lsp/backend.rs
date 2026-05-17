//! [`LspBackend`] implementation backed by [`Client`].
//!
//! All `lsp_types` request/notification construction and response parsing
//! lives here so the rest of the crate stays free of LSP protocol details.

use anyhow::{Context, Result, anyhow};
use lsp_types::notification::DidOpenTextDocument;
use lsp_types::request::{
    DocumentSymbolRequest, GotoDeclaration, GotoDeclarationParams, GotoDeclarationResponse,
    References,
};
use lsp_types::{
    DidOpenTextDocumentParams, DocumentSymbolParams, DocumentSymbolResponse, PartialResultParams,
    ReferenceContext, ReferenceParams, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, WorkDoneProgressParams,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use url::Url;

use crate::bfs::LspBackend;
use crate::lsp::client::Client;
use crate::types::{self as at};

impl From<lsp_types::Position> for at::Position {
    fn from(p: lsp_types::Position) -> Self {
        Self {
            line: p.line,
            character: p.character,
        }
    }
}

impl From<lsp_types::Range> for at::Range {
    fn from(r: lsp_types::Range) -> Self {
        Self {
            start: r.start.into(),
            end: r.end.into(),
        }
    }
}

impl From<lsp_types::Location> for at::Location {
    fn from(loc: lsp_types::Location) -> Self {
        let path = loc
            .uri
            .to_file_path()
            .unwrap_or_else(|_| PathBuf::from(loc.uri.path()));
        Self {
            path,
            range: loc.range.into(),
        }
    }
}

fn convert_symbols(symbols: Vec<lsp_types::DocumentSymbol>) -> Vec<at::DocumentSymbol> {
    symbols
        .into_iter()
        .map(|s| at::DocumentSymbol {
            name: s.name,
            range: s.range.into(),
            selection_range: s.selection_range.into(),
            children: s.children.map(convert_symbols).unwrap_or_default(),
        })
        .collect()
}

fn lsp_pos(pos: at::Position) -> lsp_types::Position {
    lsp_types::Position {
        line: pos.line,
        character: pos.character,
    }
}

pub struct Backend<'a> {
    client: &'a Client,
    workspace_root: PathBuf,
    opened: HashSet<PathBuf>,
}

impl<'a> Backend<'a> {
    pub fn new(client: &'a Client, workspace_root: PathBuf) -> Self {
        Self {
            client,
            workspace_root,
            opened: HashSet::new(),
        }
    }
}

impl LspBackend for Backend<'_> {
    fn open(&mut self, file: &Path) -> Result<()> {
        if !self.opened.insert(file.to_path_buf()) {
            return Ok(());
        }
        open_document(self.client, file)
    }

    fn references(&mut self, file: &Path, pos: at::Position) -> Result<Vec<at::Location>> {
        let mut v = self.references_batch(&[(file.to_path_buf(), pos)])?;
        Ok(v.pop().expect("one in, one out"))
    }

    fn declaration(&mut self, file: &Path, pos: at::Position) -> Result<(PathBuf, at::Position)> {
        let mut v = self.declarations_batch(&[(file.to_path_buf(), pos)])?;
        Ok(v.pop().expect("one in, one out"))
    }

    fn document_symbols(&mut self, file: &Path) -> Result<Vec<at::DocumentSymbol>> {
        let mut v = self.document_symbols_batch(&[file.to_path_buf()])?;
        Ok(v.pop().expect("one in, one out"))
    }

    fn references_batch(
        &mut self,
        items: &[(PathBuf, at::Position)],
    ) -> Result<Vec<Vec<at::Location>>> {
        let mut pendings = Vec::with_capacity(items.len());
        for (file, pos) in items {
            let params = references_params(file, lsp_pos(*pos))?;
            pendings.push(self.client.request_async::<References>(params));
        }
        let results = self.client.wait_all(pendings);
        let mut out = Vec::with_capacity(items.len());
        for ((file, pos), res) in items.iter().zip(results) {
            match res {
                Ok(opt) => out.push(
                    filter_workspace_locations(opt.unwrap_or_default(), &self.workspace_root)
                        .into_iter()
                        .map(at::Location::from)
                        .collect(),
                ),
                Err(e) if is_lsp_internal_error(&e) => {
                    warn_skip(
                        "references",
                        format_args!("{}:{},{}", file.display(), pos.line, pos.character),
                        &e,
                    );
                    out.push(Vec::new());
                }
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    fn declarations_batch(
        &mut self,
        items: &[(PathBuf, at::Position)],
    ) -> Result<Vec<(PathBuf, at::Position)>> {
        let mut pendings = Vec::with_capacity(items.len());
        for (file, pos) in items {
            let params = declaration_params(file, lsp_pos(*pos))?;
            pendings.push(self.client.request_async::<GotoDeclaration>(params));
        }
        let results = self.client.wait_all(pendings);
        let mut out = Vec::with_capacity(items.len());
        for ((file, pos), res) in items.iter().zip(results) {
            match res {
                Ok(response) => out.push(parse_declaration(
                    response,
                    file,
                    *pos,
                    &self.workspace_root,
                )),
                Err(e) if is_lsp_internal_error(&e) => {
                    warn_skip(
                        "declaration",
                        format_args!("{}:{},{}", file.display(), pos.line, pos.character),
                        &e,
                    );
                    out.push((file.clone(), *pos));
                }
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    fn document_symbols_batch(
        &mut self,
        files: &[PathBuf],
    ) -> Result<Vec<Vec<at::DocumentSymbol>>> {
        let mut pendings = Vec::with_capacity(files.len());
        for file in files {
            let params = document_symbol_params(file)?;
            pendings.push(self.client.request_async::<DocumentSymbolRequest>(params));
        }
        let results = self.client.wait_all(pendings);
        let mut out = Vec::with_capacity(files.len());
        for (file, res) in files.iter().zip(results) {
            match res {
                Ok(response) => {
                    let raw = parse_document_symbols(response, file)?;
                    out.push(convert_symbols(raw));
                }
                Err(e) if is_lsp_internal_error(&e) => {
                    warn_skip("documentSymbol", file.display(), &e);
                    out.push(Vec::new());
                }
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }
}

pub fn open_document(client: &Client, file: &Path) -> Result<()> {
    let text = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    client.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: file_uri(file)?,
            language_id: "rust".into(),
            version: 1,
            text,
        },
    })
}

pub fn file_uri(file: &Path) -> Result<Url> {
    Url::from_file_path(file).map_err(|_| anyhow!("not absolute path: {}", file.display()))
}

pub fn document_symbol_params(file: &Path) -> Result<DocumentSymbolParams> {
    Ok(DocumentSymbolParams {
        text_document: TextDocumentIdentifier {
            uri: file_uri(file)?,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    })
}

pub fn references_params(file: &Path, pos: lsp_types::Position) -> Result<ReferenceParams> {
    Ok(ReferenceParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: file_uri(file)?,
            },
            position: pos,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: ReferenceContext {
            include_declaration: false,
        },
    })
}

fn declaration_params(file: &Path, pos: lsp_types::Position) -> Result<GotoDeclarationParams> {
    Ok(GotoDeclarationParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: file_uri(file)?,
            },
            position: pos,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    })
}

/// rust-analyzer's `references` can return locations outside the workspace
/// (observed >7000 on large workspaces), which explode the BFS queue and
/// trigger downstream `-32603` panics. Entrypoints live in the workspace, so
/// external locations cannot reach a bin anyway.
fn filter_workspace_locations(
    locs: Vec<lsp_types::Location>,
    workspace_root: &Path,
) -> Vec<lsp_types::Location> {
    locs.into_iter()
        .filter(|loc| {
            let path = match loc.uri.to_file_path() {
                Ok(p) => p,
                Err(_) => return false,
            };
            if path.starts_with(workspace_root) {
                true
            } else {
                log_external("references", &path, loc.range.start.into());
                false
            }
        })
        .collect()
}

/// Returns the original `(file, pos)` if anything is missing or external.
fn parse_declaration(
    response: Option<GotoDeclarationResponse>,
    file: &Path,
    pos: at::Position,
    workspace_root: &Path,
) -> (PathBuf, at::Position) {
    let (target_path, target_pos): (PathBuf, at::Position) = match response {
        Some(GotoDeclarationResponse::Scalar(loc)) => {
            let p = loc
                .uri
                .to_file_path()
                .unwrap_or_else(|_| file.to_path_buf());
            (p, loc.range.start.into())
        }
        Some(GotoDeclarationResponse::Array(locs)) => {
            let Some(loc) = locs.into_iter().next() else {
                return (file.to_path_buf(), pos);
            };
            let p = loc
                .uri
                .to_file_path()
                .unwrap_or_else(|_| file.to_path_buf());
            (p, loc.range.start.into())
        }
        Some(GotoDeclarationResponse::Link(links)) => {
            let Some(link) = links.into_iter().next() else {
                return (file.to_path_buf(), pos);
            };
            let p = link
                .target_uri
                .to_file_path()
                .unwrap_or_else(|_| file.to_path_buf());
            (p, link.target_selection_range.start.into())
        }
        None => return (file.to_path_buf(), pos),
    };
    if !target_path.starts_with(workspace_root) {
        log_external("declaration", &target_path, target_pos);
        return (file.to_path_buf(), pos);
    }
    (target_path, target_pos)
}

/// `DocumentSymbolResponse` is `#[serde(untagged)]` and lists `Flat` first, so
/// an empty `[]` deserializes as `Flat(vec![])` even when
/// hierarchicalDocumentSymbolSupport is declared. Treat it as no symbols.
pub fn parse_document_symbols(
    response: Option<DocumentSymbolResponse>,
    file: &Path,
) -> Result<Vec<lsp_types::DocumentSymbol>> {
    match response {
        Some(DocumentSymbolResponse::Nested(s)) => Ok(s),
        Some(DocumentSymbolResponse::Flat(s)) if s.is_empty() => Ok(Vec::new()),
        Some(DocumentSymbolResponse::Flat(_)) => Err(anyhow!(
            "server returned non-empty flat document symbols for {} (unsupported)",
            file.display()
        )),
        None => Ok(Vec::new()),
    }
}

/// rust-analyzer occasionally panics on complex generic/lifetime code, surfacing as
/// `code=-32603` ("Internal error"). We log + skip rather than killing the whole BFS.
fn is_lsp_internal_error(e: &anyhow::Error) -> bool {
    e.to_string().contains("code=-32603")
}

fn warn_skip(method: &str, where_at: impl std::fmt::Display, e: &anyhow::Error) {
    let summary = e.to_string().lines().next().unwrap_or("").to_string();
    tracing::warn!("rust-analyzer error on {method}({where_at}); skipping: {summary}");
}

/// Diagnostic for external (std / cargo registry) paths surfaced by the LSP server.
/// Production behavior is to silently drop them so BFS stays in the workspace; this
/// log uses `debug` level so it can be enabled via `RUST_LOG=dokono_core=debug`.
fn log_external(source: &str, file: &Path, pos: at::Position) {
    tracing::debug!(
        "from {source}: {}:{}:{}",
        file.display(),
        pos.line,
        pos.character
    );
}
