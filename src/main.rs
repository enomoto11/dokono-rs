mod analysis;
mod cli;
mod git;
mod lsp;
mod output;

use anyhow::{Context, Result};
use clap::Parser;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::SdkTracerProvider;
use std::collections::HashSet;
use std::path::PathBuf;

use analysis::bfs::LspBackend;
use cli::{Cli, Command, DebugCmd};
use lsp::backend::Backend;
use output::{Reporter, Status, Summary};

fn init_tracing() -> Option<SdkTracerProvider> {
    use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));

    let fmt_layer = tracing_subscriber::fmt::layer();

    if std::env::var("DOKONO_OTEL").is_ok() {
        // OTLP batch exporter (tonic) needs a Tokio runtime for its background
        // worker.  We create a dedicated runtime here and leak it so the
        // exporter's tasks keep running until process exit.
        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime for OTLP");
        let _guard = rt.enter();

        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .build()
            .expect("failed to create OTLP span exporter");
        let provider = SdkTracerProvider::builder()
            .with_resource(
                opentelemetry_sdk::Resource::builder()
                    .with_service_name("dokono")
                    .build(),
            )
            .with_batch_exporter(exporter)
            .build();
        let tracer = provider.tracer("dokono");
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .with(otel_layer)
            .init();

        std::mem::forget(rt);
        Some(provider)
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .init();
        None
    }
}

fn main() -> Result<()> {
    let provider = init_tracing();

    let cli = Cli::parse();

    let result = match cli.command {
        Some(Command::Debug { cmd }) => run_debug(&cli.workspace, cmd),
        None => run_default(cli),
    };

    // Flush pending spans before exit so the batch exporter sends them
    // to the OTLP backend.
    if let Some(p) = provider {
        let _ = p.shutdown();
    }

    result
}

fn run_default(cli: Cli) -> Result<()> {
    let span = tracing::info_span!("dokono.run");
    let _enter = span.enter();

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
    tracing::info!(count = changes.len(), "diff complete");
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

    let init_span = tracing::info_span!("lsp.initialize");
    lsp::lifecycle::initialize(&mut client, &workspace)?;
    drop(init_span);

    let index_span = tracing::info_span!("lsp.index");
    reporter.phase("indexing workspace ...");
    lsp::progress::wait_for_index_end(&client)?;
    drop(index_span);

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
    let bfs_span = tracing::info_span!("bfs", starts = starts.len());
    let affected = analysis::bfs::run(&mut backend, starts, &entrypoints)?;
    drop(bfs_span);
    tracing::info!(affected = affected.len(), "bfs complete");

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
