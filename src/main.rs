mod analysis;
mod cli;
mod git;
mod lsp;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use lsp_types::{DocumentSymbol, DocumentSymbolResponse, Location, Position};
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use url::Url;

use analysis::bfs::LspBackend;
use cli::{Cli, Command, DebugCmd};

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Debug { cmd }) => run_debug(&cli.workspace, cmd),
        None => run_default(cli),
    }
}

fn run_default(cli: Cli) -> Result<()> {
    let workspace = cli
        .workspace
        .canonicalize()
        .with_context(|| format!("workspace not found: {}", cli.workspace.display()))?;

    let (base, head) = if let Some(pr) = cli.pr {
        let local_ref = format!("dokono-pr-{pr}");
        git::fetch_pr(&workspace, pr, &local_ref)?;
        let mb = git::merge_base(&workspace, &cli.base, &local_ref)?;
        eprintln!(
            "pr #{pr}: head={local_ref} base={mb} (merge-base of {})",
            cli.base
        );
        (mb, local_ref)
    } else {
        let head = cli.head.clone().context("--head or --pr is required")?;
        (cli.base.clone(), head)
    };

    let changes = analysis::diff::run(&workspace, &base, &head)?;
    if changes.is_empty() {
        println!("(no .rs file changes between {base} and {head})");
        return Ok(());
    }

    let entrypoints: HashSet<PathBuf> = analysis::entrypoints::load(&workspace)?
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    if entrypoints.is_empty() {
        anyhow::bail!("no binary entrypoints found in {}", workspace.display());
    }

    let mut client = lsp::client::Client::spawn(&workspace)?;
    eprintln!(
        "rust-analyzer spawned: pid={}",
        client.pid().unwrap_or_default()
    );
    lsp::lifecycle::initialize(&mut client, &workspace)?;
    eprintln!("waiting for index end ...");
    lsp::progress::wait_for_index_end(&client)?;
    eprintln!("index ended");

    let mut backend = ClientBackend::new(&mut client, workspace.clone());
    let mut starts: Vec<(PathBuf, Position)> = Vec::new();
    for change in &changes {
        let Ok(abs) = workspace.join(&change.path).canonicalize() else {
            continue;
        };
        backend.open(&abs)?;
        let symbols = backend.document_symbols(&abs)?;
        for hit in analysis::symbols::pick_at_lines(&symbols, &change.lines) {
            starts.push((abs.clone(), hit.position));
        }
    }
    if starts.is_empty() {
        println!("(no symbol-level changes found)");
        lsp::lifecycle::shutdown(&mut client)?;
        return Ok(());
    }

    let affected = analysis::bfs::run(&mut backend, starts, &entrypoints)?;

    if affected.is_empty() {
        println!("Affected entrypoints: none");
    } else {
        println!("Affected entrypoints:");
        for p in &affected {
            let display = p.strip_prefix(&workspace).unwrap_or(p);
            println!("  {}", display.display());
        }
    }

    lsp::lifecycle::shutdown(&mut client)?;
    Ok(())
}

struct ClientBackend<'a> {
    client: &'a mut lsp::client::Client,
    workspace_root: PathBuf,
    opened: HashSet<PathBuf>,
}

impl<'a> ClientBackend<'a> {
    fn new(client: &'a mut lsp::client::Client, workspace_root: PathBuf) -> Self {
        Self {
            client,
            workspace_root,
            opened: HashSet::new(),
        }
    }
}

/// rust-analyzer occasionally panics on complex generic/lifetime code, surfacing as
/// `code=-32603` ("Internal error"). We log + skip rather than killing the whole BFS.
fn is_lsp_internal_error(e: &anyhow::Error) -> bool {
    e.to_string().contains("code=-32603")
}

fn warn_skip(method: &str, where_at: impl std::fmt::Display, e: &anyhow::Error) {
    let summary = e.to_string().lines().next().unwrap_or("").to_string();
    eprintln!("[warn] rust-analyzer error on {method}({where_at}); skipping: {summary}");
}

/// Diagnostic for external (std / cargo registry) paths surfaced by the LSP server.
/// Production behavior is to silently drop them so BFS stays in the workspace; this
/// log is gated on `DOKONO_VERBOSE` so debug runs can still see what was filtered.
fn log_external(source: &str, file: &Path, pos: Position) {
    if std::env::var_os("DOKONO_VERBOSE").is_none() {
        return;
    }
    eprintln!(
        "[external] from {source}: {}:{}:{}",
        file.display(),
        pos.line,
        pos.character
    );
}

impl analysis::bfs::LspBackend for ClientBackend<'_> {
    fn open(&mut self, file: &Path) -> Result<()> {
        if !self.opened.insert(file.to_path_buf()) {
            return Ok(());
        }
        let text =
            std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
        let uri = Url::from_file_path(file)
            .map_err(|_| anyhow!("not absolute path: {}", file.display()))?;
        self.client.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "rust",
                    "version": 1,
                    "text": text,
                }
            }),
        )?;
        Ok(())
    }

    fn references(&mut self, file: &Path, pos: Position) -> Result<Vec<Location>> {
        let uri = Url::from_file_path(file)
            .map_err(|_| anyhow!("not absolute path: {}", file.display()))?;
        let result: Result<Option<Vec<Location>>> = self.client.request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": pos.line, "character": pos.character },
                "context": { "includeDeclaration": false }
            }),
        );
        match result {
            Ok(opt) => {
                // Drop external paths. rust-analyzer's `references` can return locations
                // outside the workspace (observed >7000 on large workspaces), which would
                // explode the BFS queue and trigger downstream `-32603` panics on querying
                // them. Entrypoints live in the workspace, so external locations cannot
                // reach a bin anyway.
                let workspace_root = self.workspace_root.clone();
                let locations: Vec<Location> = opt
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|loc| {
                        let path = match loc.uri.to_file_path() {
                            Ok(p) => p,
                            Err(_) => return false,
                        };
                        if path.starts_with(&workspace_root) {
                            true
                        } else {
                            log_external("references", &path, loc.range.start);
                            false
                        }
                    })
                    .collect();
                Ok(locations)
            }
            Err(e) if is_lsp_internal_error(&e) => {
                warn_skip(
                    "references",
                    format_args!("{}:{},{}", file.display(), pos.line, pos.character),
                    &e,
                );
                Ok(Vec::new())
            }
            Err(e) => Err(e),
        }
    }

    fn declaration(&mut self, file: &Path, pos: Position) -> Result<(PathBuf, Position)> {
        let uri = Url::from_file_path(file)
            .map_err(|_| anyhow!("not absolute path: {}", file.display()))?;
        // Response is Location | Location[] | LocationLink[] | null per LSP spec; tolerate all.
        let result: Result<serde_json::Value> = self.client.request(
            "textDocument/declaration",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": pos.line, "character": pos.character }
            }),
        );
        let response = match result {
            Ok(v) => v,
            Err(e) if is_lsp_internal_error(&e) => {
                warn_skip(
                    "declaration",
                    format_args!("{}:{},{}", file.display(), pos.line, pos.character),
                    &e,
                );
                return Ok((file.to_path_buf(), pos));
            }
            Err(e) => return Err(e),
        };
        let first_loc = response.as_array().and_then(|arr| arr.first()).cloned();
        let loc_value = first_loc.unwrap_or(response);
        if loc_value.is_null() {
            return Ok((file.to_path_buf(), pos));
        }
        let target_uri = loc_value
            .get("uri")
            .or_else(|| loc_value.get("targetUri"))
            .and_then(|v| v.as_str());
        let range = loc_value
            .get("range")
            .or_else(|| loc_value.get("targetSelectionRange"));
        let (Some(uri_str), Some(range)) = (target_uri, range) else {
            return Ok((file.to_path_buf(), pos));
        };
        let target_path = Url::parse(uri_str)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .unwrap_or_else(|| file.to_path_buf());
        let target_pos = Position {
            line: range
                .get("start")
                .and_then(|s| s.get("line"))
                .and_then(|v| v.as_u64())
                .unwrap_or(pos.line as u64) as u32,
            character: range
                .get("start")
                .and_then(|s| s.get("character"))
                .and_then(|v| v.as_u64())
                .unwrap_or(pos.character as u64) as u32,
        };
        if !target_path.starts_with(&self.workspace_root) {
            // Declaration jumped outside the workspace (e.g., impl method → std trait method).
            // Don't follow it; fall back to the original position so BFS stays bounded.
            log_external("declaration", &target_path, target_pos);
            return Ok((file.to_path_buf(), pos));
        }
        Ok((target_path, target_pos))
    }

    fn document_symbols(&mut self, file: &Path) -> Result<Vec<DocumentSymbol>> {
        let uri = Url::from_file_path(file)
            .map_err(|_| anyhow!("not absolute path: {}", file.display()))?;
        let result: Result<Option<DocumentSymbolResponse>> = self.client.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
        );
        let response = match result {
            Ok(r) => r,
            Err(e) if is_lsp_internal_error(&e) => {
                warn_skip("documentSymbol", file.display(), &e);
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        match response {
            Some(DocumentSymbolResponse::Nested(s)) => Ok(s),
            // `DocumentSymbolResponse` is `#[serde(untagged)]` and lists `Flat` first, so an
            // empty `[]` deserializes as `Flat(vec![])` even when we declared
            // hierarchicalDocumentSymbolSupport. Treat it as "no symbols".
            Some(DocumentSymbolResponse::Flat(s)) if s.is_empty() => Ok(Vec::new()),
            Some(DocumentSymbolResponse::Flat(_)) => Err(anyhow!(
                "server returned non-empty flat document symbols for {} (unsupported)",
                file.display()
            )),
            None => Ok(Vec::new()),
        }
    }
}

fn print_symbol_tree(symbols: &[lsp_types::DocumentSymbol], depth: usize) {
    let indent = "  ".repeat(depth);
    for s in symbols {
        println!(
            "{indent}- {} ({:?}) range=({},{})..({},{}) sel=({},{})",
            s.name,
            s.kind,
            s.range.start.line,
            s.range.start.character,
            s.range.end.line,
            s.range.end.character,
            s.selection_range.start.line,
            s.selection_range.start.character,
        );
        if let Some(children) = &s.children {
            print_symbol_tree(children, depth + 1);
        }
    }
}

fn run_debug(workspace: &std::path::Path, cmd: DebugCmd) -> Result<()> {
    match cmd {
        DebugCmd::PrintDiff { base, head } => {
            let changes = analysis::diff::run(workspace, &base, &head)?;
            if changes.is_empty() {
                println!("(no .rs file changes between {base} and {head})");
            } else {
                for file in &changes {
                    println!("{}: {:?}", file.path.display(), file.lines);
                }
            }
            Ok(())
        }
        DebugCmd::PrintEntrypoints => {
            let bins = analysis::entrypoints::load(workspace)?;
            if bins.is_empty() {
                println!("(no binary entrypoints in {})", workspace.display());
            } else {
                for p in &bins {
                    println!("{}", p.display());
                }
            }
            Ok(())
        }
        DebugCmd::SpawnOnly => {
            let client = lsp::client::Client::spawn(workspace)?;
            match client.pid() {
                Some(pid) => println!("rust-analyzer spawned: pid={pid}"),
                None => println!("rust-analyzer spawned (pid unavailable)"),
            }
            println!("dropping client (will kill the process)...");
            drop(client);
            println!("done");
            Ok(())
        }
        DebugCmd::Index => {
            use std::time::Instant;
            // Use stderr — stdout becomes block-buffered when piped on macOS.
            let mut client = lsp::client::Client::spawn(workspace)?;
            eprintln!(
                "rust-analyzer spawned: pid={}",
                client.pid().unwrap_or_default()
            );

            let init_start = Instant::now();
            lsp::lifecycle::initialize(&mut client, workspace)?;
            eprintln!("initialize ok ({:.2?})", init_start.elapsed());

            eprintln!("waiting for index end ...");
            let idx_start = Instant::now();
            lsp::progress::wait_for_index_end(&client)?;
            eprintln!("index ended ({:.2?})", idx_start.elapsed());

            lsp::lifecycle::shutdown(&mut client)?;
            eprintln!("shutdown ok");
            Ok(())
        }
        DebugCmd::Symbols { file, line } => {
            use lsp_types::DocumentSymbolResponse;
            use serde_json::json;
            use url::Url;

            let file_abs = workspace
                .join(&file)
                .canonicalize()
                .with_context(|| format!("file not found: {}", file.display()))?;
            let uri = Url::from_file_path(&file_abs)
                .map_err(|_| anyhow::anyhow!("not absolute: {}", file_abs.display()))?;

            let mut client = lsp::client::Client::spawn(workspace)?;
            eprintln!(
                "rust-analyzer spawned: pid={}",
                client.pid().unwrap_or_default()
            );
            lsp::lifecycle::initialize(&mut client, workspace)?;
            eprintln!("waiting for index end ...");
            lsp::progress::wait_for_index_end(&client)?;
            eprintln!("index ended");

            // didOpen is required before any documentSymbol/references query.
            let text = std::fs::read_to_string(&file_abs)
                .with_context(|| format!("read {}", file_abs.display()))?;
            client.notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": "rust",
                        "version": 1,
                        "text": text
                    }
                }),
            )?;

            let response: Option<DocumentSymbolResponse> = client.request(
                "textDocument/documentSymbol",
                json!({ "textDocument": { "uri": uri } }),
            )?;
            let symbols = match response {
                Some(DocumentSymbolResponse::Nested(s)) => s,
                Some(DocumentSymbolResponse::Flat(_)) => {
                    lsp::lifecycle::shutdown(&mut client)?;
                    anyhow::bail!("server returned flat document symbols (unsupported)");
                }
                None => Vec::new(),
            };

            match line {
                Some(line1) => {
                    let hits = analysis::symbols::pick_at_lines(&symbols, &[line1]);
                    if hits.is_empty() {
                        println!("no symbol hits line {line1}");
                    } else {
                        for h in &hits {
                            println!(
                                "hit: {} @ ({}, {})",
                                h.name, h.position.line, h.position.character
                            );
                        }
                    }
                }
                None => {
                    println!("documentSymbol tree:");
                    print_symbol_tree(&symbols, 0);
                }
            }

            lsp::lifecycle::shutdown(&mut client)?;
            Ok(())
        }
        DebugCmd::References {
            file,
            line,
            character,
        } => {
            use serde_json::json;
            use url::Url;

            let file_abs = workspace
                .join(&file)
                .canonicalize()
                .with_context(|| format!("file not found: {}", file.display()))?;
            let uri = Url::from_file_path(&file_abs)
                .map_err(|_| anyhow::anyhow!("not absolute: {}", file_abs.display()))?;

            let mut client = lsp::client::Client::spawn(workspace)?;
            eprintln!(
                "rust-analyzer spawned: pid={}",
                client.pid().unwrap_or_default()
            );
            lsp::lifecycle::initialize(&mut client, workspace)?;
            eprintln!("waiting for index end ...");
            lsp::progress::wait_for_index_end(&client)?;
            eprintln!("index ended");

            let text = std::fs::read_to_string(&file_abs)
                .with_context(|| format!("read {}", file_abs.display()))?;
            client.notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": "rust",
                        "version": 1,
                        "text": text,
                    }
                }),
            )?;

            let response: Option<Vec<lsp_types::Location>> = client.request(
                "textDocument/references",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                    "context": { "includeDeclaration": false }
                }),
            )?;

            let refs = response.unwrap_or_default();
            if refs.is_empty() {
                println!(
                    "(no references found at {}:{}:{})",
                    file.display(),
                    line,
                    character
                );
            } else {
                println!("references ({} hits):", refs.len());
                for r in &refs {
                    let display = r
                        .uri
                        .to_file_path()
                        .ok()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| r.uri.to_string());
                    println!(
                        "  {}:{}:{}",
                        display, r.range.start.line, r.range.start.character
                    );
                }
            }

            lsp::lifecycle::shutdown(&mut client)?;
            Ok(())
        }
    }
}
