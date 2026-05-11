mod analysis;
mod cli;
mod git;
mod lsp;
mod output;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use lsp_types::notification::DidOpenTextDocument;
use lsp_types::request::{
    DocumentSymbolRequest, GotoDeclaration, GotoDeclarationParams, GotoDeclarationResponse,
    References,
};
use lsp_types::{
    DidOpenTextDocumentParams, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse,
    Location, PartialResultParams, Position, ReferenceContext, ReferenceParams,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, WorkDoneProgressParams,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use url::Url;

use analysis::bfs::LspBackend;
use cli::{Cli, Command, DebugCmd};
use output::{Reporter, Status, Summary};

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Debug { cmd }) => run_debug(&cli.workspace, cmd),
        None => run_default(cli),
    }
}

fn run_default(cli: Cli) -> Result<()> {
    let format = cli.format;
    let reporter = Reporter::for_format(format);

    let workspace = cli
        .workspace
        .canonicalize()
        .with_context(|| format!("workspace not found: {}", cli.workspace.display()))?;

    let (base, head) = if let Some(pr) = cli.pr {
        let local_ref = format!("dokono-pr-{pr}");
        reporter.phase(format!("fetching PR #{pr} ..."));
        git::fetch_pr(&workspace, pr, &local_ref)?;
        reporter.phase("computing merge-base ...");
        let mb = git::merge_base(&workspace, &cli.base, &local_ref)?;
        reporter.note(format!(
            "pr #{pr}: head={local_ref} base={mb} (merge-base of {})",
            cli.base
        ));
        (mb, local_ref)
    } else {
        let head = cli.head.clone().context("--head or --pr is required")?;
        (cli.base.clone(), head)
    };

    reporter.phase("diffing changes ...");
    let changes = analysis::diff::run(&workspace, &base, &head)?;
    if changes.is_empty() {
        reporter.finish();
        return output::emit(
            format,
            &Summary {
                schema_version: 1,
                pr: cli.pr,
                base,
                head,
                status: Status::NoRsChanges,
                affected: Vec::new(),
            },
        );
    }

    reporter.phase("loading entrypoints ...");
    let entrypoints: HashSet<PathBuf> = analysis::entrypoints::load(&workspace)?
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    if entrypoints.is_empty() {
        reporter.finish();
        anyhow::bail!("no binary entrypoints found in {}", workspace.display());
    }

    reporter.phase("spawning rust-analyzer ...");
    let mut client = lsp::client::Client::spawn(&workspace)?;
    reporter.phase(format!(
        "rust-analyzer started (pid={})",
        client.pid().unwrap_or_default()
    ));
    lsp::lifecycle::initialize(&mut client, &workspace)?;
    reporter.phase("indexing workspace ...");
    lsp::progress::wait_for_index_end(&client)?;

    reporter.phase("locating changed symbols ...");
    let mut backend = ClientBackend::new(&client, workspace.clone());
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
        lsp::lifecycle::shutdown(&mut client)?;
        reporter.finish();
        return output::emit(
            format,
            &Summary {
                schema_version: 1,
                pr: cli.pr,
                base,
                head,
                status: Status::NoSymbolChanges,
                affected: Vec::new(),
            },
        );
    }

    reporter.phase("tracing references (BFS) ...");
    let affected = analysis::bfs::run(&mut backend, starts, &entrypoints)?;

    lsp::lifecycle::shutdown(&mut client)?;
    reporter.finish();

    let affected_rel: Vec<String> = affected
        .iter()
        .map(|p| {
            p.strip_prefix(&workspace)
                .unwrap_or(p)
                .display()
                .to_string()
        })
        .collect();
    output::emit(
        format,
        &Summary {
            schema_version: 1,
            pr: cli.pr,
            base,
            head,
            status: Status::Ok,
            affected: affected_rel,
        },
    )
}

struct ClientBackend<'a> {
    client: &'a lsp::client::Client,
    workspace_root: PathBuf,
    opened: HashSet<PathBuf>,
}

impl<'a> ClientBackend<'a> {
    fn new(client: &'a lsp::client::Client, workspace_root: PathBuf) -> Self {
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

fn file_uri(file: &Path) -> Result<Url> {
    Url::from_file_path(file).map_err(|_| anyhow!("not absolute path: {}", file.display()))
}

fn references_params(file: &Path, pos: Position) -> Result<ReferenceParams> {
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

fn declaration_params(file: &Path, pos: Position) -> Result<GotoDeclarationParams> {
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

fn document_symbol_params(file: &Path) -> Result<DocumentSymbolParams> {
    Ok(DocumentSymbolParams {
        text_document: TextDocumentIdentifier {
            uri: file_uri(file)?,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    })
}

impl analysis::bfs::LspBackend for ClientBackend<'_> {
    fn open(&mut self, file: &Path) -> Result<()> {
        if !self.opened.insert(file.to_path_buf()) {
            return Ok(());
        }
        let text =
            std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
        self.client
            .notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: file_uri(file)?,
                    language_id: "rust".into(),
                    version: 1,
                    text,
                },
            })?;
        Ok(())
    }

    fn references(&mut self, file: &Path, pos: Position) -> Result<Vec<Location>> {
        let mut v = self.references_batch(&[(file.to_path_buf(), pos)])?;
        Ok(v.pop().expect("one in, one out"))
    }

    fn declaration(&mut self, file: &Path, pos: Position) -> Result<(PathBuf, Position)> {
        let mut v = self.declarations_batch(&[(file.to_path_buf(), pos)])?;
        Ok(v.pop().expect("one in, one out"))
    }

    fn document_symbols(&mut self, file: &Path) -> Result<Vec<DocumentSymbol>> {
        let mut v = self.document_symbols_batch(&[file.to_path_buf()])?;
        Ok(v.pop().expect("one in, one out"))
    }

    fn references_batch(&mut self, items: &[(PathBuf, Position)]) -> Result<Vec<Vec<Location>>> {
        let mut pendings = Vec::with_capacity(items.len());
        for (file, pos) in items {
            let params = references_params(file, *pos)?;
            pendings.push(self.client.request_async::<References>(params));
        }
        let results = self.client.wait_all(pendings);
        let mut out = Vec::with_capacity(items.len());
        for ((file, pos), res) in items.iter().zip(results) {
            match res {
                Ok(opt) => out.push(filter_workspace_locations(
                    opt.unwrap_or_default(),
                    &self.workspace_root,
                )),
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
        items: &[(PathBuf, Position)],
    ) -> Result<Vec<(PathBuf, Position)>> {
        let mut pendings = Vec::with_capacity(items.len());
        for (file, pos) in items {
            let params = declaration_params(file, *pos)?;
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

    fn document_symbols_batch(&mut self, files: &[PathBuf]) -> Result<Vec<Vec<DocumentSymbol>>> {
        let mut pendings = Vec::with_capacity(files.len());
        for file in files {
            let params = document_symbol_params(file)?;
            pendings.push(self.client.request_async::<DocumentSymbolRequest>(params));
        }
        let results = self.client.wait_all(pendings);
        let mut out = Vec::with_capacity(files.len());
        for (file, res) in files.iter().zip(results) {
            match res {
                Ok(response) => out.push(parse_document_symbols(response, file)?),
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

/// rust-analyzer's `references` can return locations outside the workspace
/// (observed >7000 on large workspaces), which explode the BFS queue and
/// trigger downstream `-32603` panics. Entrypoints live in the workspace, so
/// external locations cannot reach a bin anyway.
fn filter_workspace_locations(locs: Vec<Location>, workspace_root: &Path) -> Vec<Location> {
    locs.into_iter()
        .filter(|loc| {
            let path = match loc.uri.to_file_path() {
                Ok(p) => p,
                Err(_) => return false,
            };
            if path.starts_with(workspace_root) {
                true
            } else {
                log_external("references", &path, loc.range.start);
                false
            }
        })
        .collect()
}

/// Returns the original `(file, pos)` if anything is missing or external.
fn parse_declaration(
    response: Option<GotoDeclarationResponse>,
    file: &Path,
    pos: Position,
    workspace_root: &Path,
) -> (PathBuf, Position) {
    let (target_path, target_pos) = match response {
        Some(GotoDeclarationResponse::Scalar(loc)) => {
            let p = loc
                .uri
                .to_file_path()
                .unwrap_or_else(|_| file.to_path_buf());
            (p, loc.range.start)
        }
        Some(GotoDeclarationResponse::Array(locs)) => {
            let Some(loc) = locs.into_iter().next() else {
                return (file.to_path_buf(), pos);
            };
            let p = loc
                .uri
                .to_file_path()
                .unwrap_or_else(|_| file.to_path_buf());
            (p, loc.range.start)
        }
        Some(GotoDeclarationResponse::Link(links)) => {
            let Some(link) = links.into_iter().next() else {
                return (file.to_path_buf(), pos);
            };
            let p = link
                .target_uri
                .to_file_path()
                .unwrap_or_else(|_| file.to_path_buf());
            (p, link.target_selection_range.start)
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
fn parse_document_symbols(
    response: Option<DocumentSymbolResponse>,
    file: &Path,
) -> Result<Vec<DocumentSymbol>> {
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
            let file_abs = workspace
                .join(&file)
                .canonicalize()
                .with_context(|| format!("file not found: {}", file.display()))?;

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
            client.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: file_uri(&file_abs)?,
                    language_id: "rust".into(),
                    version: 1,
                    text,
                },
            })?;

            let response: Option<DocumentSymbolResponse> =
                client.request::<DocumentSymbolRequest>(document_symbol_params(&file_abs)?)?;
            let symbols = parse_document_symbols(response, &file_abs)?;

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
            let file_abs = workspace
                .join(&file)
                .canonicalize()
                .with_context(|| format!("file not found: {}", file.display()))?;

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
            client.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: file_uri(&file_abs)?,
                    language_id: "rust".into(),
                    version: 1,
                    text,
                },
            })?;

            let pos = Position { line, character };
            let response: Option<Vec<Location>> =
                client.request::<References>(references_params(&file_abs, pos)?)?;

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
