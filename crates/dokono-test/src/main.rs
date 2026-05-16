mod cli;
mod matcher;
mod output;
mod test_goals;

use anyhow::{Context, Result};
use clap::Parser;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use cli::{Cli, Command, DebugCmd};
use dokono_core::bfs::{self, BfsDirection, LspBackend};
use dokono_core::git;
use dokono_core::lsp::backend::Backend;
use dokono_core::lsp::{client, lifecycle, progress};
use matcher::PackageMap;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let mut cli = Cli::parse();
    match cli.command.take() {
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
        let local_ref = format!("dokono-test-pr-{pr}");
        eprintln!("fetching PR #{pr} ...");
        git::fetch_pr(&workspace, pr, &local_ref)?;
        eprintln!("computing merge-base ...");
        let mb = git::merge_base(&workspace, &cli.base, &local_ref)?;
        let head_sha = git::resolve_sha(&workspace, &local_ref)?;
        eprintln!(
            "pr #{pr}: head={head_sha} base={mb} (merge-base of {})",
            cli.base
        );
        (mb, head_sha)
    } else {
        let head = cli.head.context("--head or --pr is required")?;
        (cli.base, head)
    };

    eprintln!("collecting test goals (syn) ...");
    let tests = test_goals::collect(&workspace)?;
    let mut tests_by_file: HashMap<PathBuf, Vec<test_goals::TestFn>> = HashMap::new();
    for t in tests {
        let canon = t.file.canonicalize().unwrap_or_else(|_| t.file.clone());
        let mut t = t;
        t.file = canon.clone();
        tests_by_file.entry(canon).or_default().push(t);
    }
    let test_files: HashSet<PathBuf> = tests_by_file.keys().cloned().collect();
    let total_tests: usize = tests_by_file.values().map(|v| v.len()).sum();
    eprintln!("{} tests across {} files", total_tests, test_files.len());

    let mut summary = output::Summary {
        schema_version: 1,
        pr: cli.pr,
        base: base.clone(),
        head: head.clone(),
        status: output::Status::Ok,
        affected: BTreeSet::new(),
        total_tests,
    };

    eprintln!("diffing changes ...");
    let changes = git::changes_between(&workspace, &base, &head)?;
    if changes.is_empty() {
        summary.status = output::Status::NoRsChanges;
        return output::emit(cli.format, &summary, &workspace);
    }

    if test_files.is_empty() {
        anyhow::bail!("no test functions found in {}", workspace.display());
    }
    let packages = PackageMap::load(&workspace)
        .with_context(|| format!("cargo metadata failed at {}", workspace.display()))?;

    eprintln!("spawning rust-analyzer ...");
    let mut client = client::Client::spawn(&workspace)?;
    eprintln!(
        "rust-analyzer started (pid={})",
        client.pid().unwrap_or_default()
    );
    lifecycle::initialize(&mut client, &workspace)?;
    eprintln!("indexing workspace ...");
    progress::wait_for_index_end(&client)?;

    eprintln!("locating changed symbols ...");
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
        summary.status = output::Status::NoSymbolChanges;
        return output::emit(cli.format, &summary, &workspace);
    }
    eprintln!("starts: {} symbol(s)", starts.len());

    let starts_snapshot = starts.clone();
    eprintln!("tracing references (BFS) ...");
    let result = bfs::run_with_parents(&mut backend, starts, &test_files, BfsDirection::Upward)?;

    lifecycle::shutdown(&mut client)?;

    summary.affected = matcher::resolve(
        &starts_snapshot,
        &result,
        &tests_by_file,
        &packages,
        &workspace,
    );
    output::emit(cli.format, &summary, &workspace)
}

fn run_debug(workspace: &Path, cmd: DebugCmd) -> Result<()> {
    match cmd {
        DebugCmd::PrintTests => print_tests(workspace),
        DebugCmd::PrintDiff { base, head } => print_diff(workspace, &base, &head),
        DebugCmd::PrintStarts { base, head } => print_starts(workspace, &base, &head),
    }
}

fn print_tests(workspace: &Path) -> Result<()> {
    let workspace = workspace
        .canonicalize()
        .with_context(|| format!("workspace not found: {}", workspace.display()))?;
    let tests = test_goals::collect(&workspace)?;
    for t in &tests {
        let rel = t.file.strip_prefix(&workspace).unwrap_or(&t.file);
        println!(
            "{}\t{}\t{:?}\t{}:{}",
            rel.display(),
            t.name,
            t.attr,
            t.body_range.0,
            t.body_range.1
        );
    }
    eprintln!("total: {}", tests.len());
    Ok(())
}

fn print_diff(workspace: &Path, base: &str, head: &str) -> Result<()> {
    let changes = git::changes_between(workspace, base, head)?;
    if changes.is_empty() {
        println!("(no .rs file changes between {base} and {head})");
    } else {
        for file in &changes {
            println!("{}: {:?}", file.path.display(), file.lines);
        }
    }
    Ok(())
}

fn print_starts(workspace: &Path, base: &str, head: &str) -> Result<()> {
    let workspace = workspace
        .canonicalize()
        .with_context(|| format!("workspace not found: {}", workspace.display()))?;
    let changes = git::changes_between(&workspace, base, head)?;
    if changes.is_empty() {
        eprintln!("(no .rs file changes between {base} and {head})");
        return Ok(());
    }

    eprintln!("spawning rust-analyzer ...");
    let mut client = client::Client::spawn(&workspace)?;
    eprintln!(
        "rust-analyzer started (pid={})",
        client.pid().unwrap_or_default()
    );
    lifecycle::initialize(&mut client, &workspace)?;
    eprintln!("indexing workspace ...");
    progress::wait_for_index_end(&client)?;

    let mut backend = Backend::new(&client, workspace.clone());
    for change in &changes {
        let Ok(abs) = workspace.join(&change.path).canonicalize() else {
            eprintln!("skip: cannot canonicalize {}", change.path.display());
            continue;
        };
        backend.open(&abs)?;
        let symbols = backend.document_symbols(&abs)?;
        let rel = abs.strip_prefix(&workspace).unwrap_or(&abs);
        let hits = dokono_core::symbols::pick_at_lines(&symbols, &change.lines);
        if hits.is_empty() {
            println!(
                "{}\t(no enclosing symbol for lines {:?})",
                rel.display(),
                change.lines
            );
            continue;
        }
        for hit in hits {
            println!(
                "{}\t{}\t({},{})",
                rel.display(),
                hit.name,
                hit.position.line,
                hit.position.character
            );
        }
    }

    lifecycle::shutdown(&mut client)?;
    Ok(())
}
