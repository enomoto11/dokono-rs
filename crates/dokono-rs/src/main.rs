mod cli;
mod debug;
mod output;

use anyhow::{Context, Result};
use clap::Parser;
use std::collections::HashSet;
use std::path::PathBuf;

use cli::{Cli, Command, DebugCmd};
use dokono_core::bfs::{self, BfsDirection, BfsResult, LspBackend, ParentMap};
use dokono_core::git;
use dokono_core::lsp::backend::Backend;
use dokono_core::lsp::{client, lifecycle, progress};
use dokono_core::types::Position;
use output::{Reporter, Status, Summary};
use std::collections::HashMap;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

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
    let changes = git::changes_between(&workspace, &base, &head)?;
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
    let entrypoints: HashSet<PathBuf> = dokono_core::entrypoints::load_bin_entrypoints(&workspace)?
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    if entrypoints.is_empty() {
        reporter.finish();
        anyhow::bail!("no binary entrypoints found in {}", workspace.display());
    }

    reporter.phase("spawning rust-analyzer ...");
    let mut client = client::Client::spawn(&workspace)?;
    reporter.phase(format!(
        "rust-analyzer started (pid={})",
        client.pid().unwrap_or_default()
    ));
    lifecycle::initialize(&mut client, &workspace)?;
    reporter.phase("indexing workspace ...");
    progress::wait_for_index_end(&client)?;

    reporter.phase("locating changed symbols ...");
    let mut backend = Backend::new(&client, workspace.clone());
    let mut starts = Vec::new();
    for change in &changes {
        let Ok(abs) = workspace.join(&change.path).canonicalize() else {
            continue;
        };
        backend.open(&abs)?;
        let symbols = backend.document_symbols(&abs)?;
        for hit in dokono_core::symbols::pick_at_lines(&symbols, &change.lines) {
            starts.push((abs.clone(), hit.position));
        }
    }
    if starts.is_empty() {
        lifecycle::shutdown(&mut client)?;
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
    let BfsResult {
        affected,
        parents,
        entry_hits,
    } = bfs::run_with_parents(&mut backend, starts, &entrypoints, BfsDirection::Upward)?;

    lifecycle::shutdown(&mut client)?;
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
    )?;

    if cli.explain {
        print_traces(&entry_hits, &parents, &workspace);
    }

    Ok(())
}

fn print_traces(
    entry_hits: &HashMap<PathBuf, Vec<Position>>,
    parents: &ParentMap,
    workspace: &PathBuf,
) {
    eprintln!();
    eprintln!("BFS trace per affected entrypoint:");
    let mut sorted: Vec<_> = entry_hits.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    for (entry, positions) in sorted {
        let rel = entry.strip_prefix(workspace).unwrap_or(entry);
        eprintln!("  {}", rel.display());
        let mut seen_paths: std::collections::HashSet<Vec<(PathBuf, Position)>> =
            std::collections::HashSet::new();
        for pos in positions {
            let chain = walk_parents(parents, (entry.clone(), *pos));
            if !seen_paths.insert(chain.clone()) {
                continue;
            }
            eprintln!("    ref at line {} col {}", pos.line + 1, pos.character + 1);
            for (i, (f, p)) in chain.iter().enumerate() {
                let r = f.strip_prefix(workspace).unwrap_or(f);
                let arrow = if i == chain.len() - 1 {
                    "       └─ start"
                } else {
                    "       ←"
                };
                eprintln!(
                    "{} {}:{}:{}",
                    arrow,
                    r.display(),
                    p.line + 1,
                    p.character + 1
                );
            }
        }
    }
}

fn walk_parents(parents: &ParentMap, start: (PathBuf, Position)) -> Vec<(PathBuf, Position)> {
    let mut chain = Vec::new();
    let mut current = parents.get(&start).cloned().flatten();
    let mut guard = 0;
    while let Some(p) = current {
        chain.push(p.clone());
        guard += 1;
        if guard > 100 {
            break;
        }
        current = parents.get(&p).cloned().flatten();
    }
    chain
}

fn run_debug(workspace: &std::path::Path, cmd: DebugCmd) -> Result<()> {
    match cmd {
        DebugCmd::PrintDiff { base, head } => {
            let changes = git::changes_between(workspace, &base, &head)?;
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
            let bins = dokono_core::entrypoints::load_bin_entrypoints(workspace)?;
            if bins.is_empty() {
                println!("(no binary entrypoints in {})", workspace.display());
            } else {
                for p in &bins {
                    println!("{}", p.display());
                }
            }
            Ok(())
        }
        DebugCmd::SpawnOnly => debug::spawn_only(workspace),
        DebugCmd::Index => debug::index(workspace),
        DebugCmd::Symbols { file, line } => debug::symbols(workspace, &file, line),
        DebugCmd::References {
            file,
            line,
            character,
        } => debug::references(workspace, &file, line, character),
        DebugCmd::Declaration {
            file,
            line,
            character,
        } => debug::declaration(workspace, &file, line, character),
    }
}
