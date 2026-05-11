mod analysis;
mod cli;
mod git;
mod lsp;
mod output;

use anyhow::{Context, Result};
use clap::Parser;
use std::collections::HashSet;
use std::path::PathBuf;

use analysis::bfs::LspBackend;
use cli::{Cli, Command, DebugCmd};
use lsp::backend::Backend;
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
        let head_sha = git::resolve_sha(&workspace, &local_ref)?;
        reporter.note(format!(
            "pr #{pr}: head={head_sha} base={mb} (merge-base of {})",
            cli.base
        ));
        (mb, head_sha)
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
    let mut backend = Backend::new(&client, workspace.clone());
    let mut starts = Vec::new();
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
        DebugCmd::SpawnOnly => lsp::debug::spawn_only(workspace),
        DebugCmd::Index => lsp::debug::index(workspace),
        DebugCmd::Symbols { file, line } => lsp::debug::symbols(workspace, &file, line),
        DebugCmd::References {
            file,
            line,
            character,
        } => lsp::debug::references(workspace, &file, line, character),
    }
}
