use anyhow::{Context, Result};
use clap::{ArgGroup, Parser, ValueEnum};
use dokono_cve::exit::{exit_code, verdict};
use dokono_cve::input::{InputMode, InputResult, UnsupportedReason, collect};
use dokono_cve::output::{Format, render};
use dokono_cve::probe::{ResolveResult, UnresolvedReason, resolve_seeds};
use dokono_cve::runner::run;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(
    name = "dokono-cve",
    about = "Decide whether vulnerable functions in dependencies are reachable from your binaries",
    long_about = "dokono-cve takes a vulnerable symbol (from cargo audit, a RUSTSEC advisory, or a direct path), \
                  walks the workspace's call graph upward through rust-analyzer, and reports which `bin` \
                  entrypoints can actually reach the symbol. Production paths and `#[cfg(test)]`-only paths \
                  are reported separately so CI can fail only on real reachability.",
    group(ArgGroup::new("input").required(true).multiple(false))
)]
struct Cli {
    /// Path to the target Rust workspace.
    #[arg(long, value_name = "PATH")]
    workspace: PathBuf,

    /// Path to a `cargo audit --json` output file.
    #[arg(long, value_name = "PATH", group = "input")]
    audit_json: Option<PathBuf>,

    /// RUSTSEC advisory id (e.g. RUSTSEC-2025-0022).
    #[arg(long, value_name = "ID", group = "input")]
    advisory: Option<String>,

    /// Fully-qualified path of one vulnerable symbol (e.g. openssl::ssl::SslContext::new).
    #[arg(long, value_name = "PATH", group = "input")]
    symbol: Option<String>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = FormatArg::Text)]
    format: FormatArg,

    /// Print parsed input and exit (skip reachability analysis).
    #[arg(long)]
    dry_run: bool,

    /// Resolve seed positions and exit (skip BFS).
    #[arg(long)]
    resolve_only: bool,

    /// Treat tests-only reachable bins as a failure (exit 1).
    #[arg(long)]
    strict: bool,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum FormatArg {
    Text,
    Json,
}

impl From<FormatArg> for Format {
    fn from(f: FormatArg) -> Self {
        match f {
            FormatArg::Text => Format::Text,
            FormatArg::Json => Format::Json,
        }
    }
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    match run_app() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:?}");
            ExitCode::from(2)
        }
    }
}

fn run_app() -> Result<ExitCode> {
    let cli = Cli::parse();
    let mode = if let Some(path) = cli.audit_json {
        InputMode::AuditJson(path)
    } else if let Some(id) = cli.advisory {
        InputMode::AdvisoryId(id)
    } else if let Some(sym) = cli.symbol {
        InputMode::DirectSymbol(sym)
    } else {
        unreachable!("clap ArgGroup guarantees one of audit-json / advisory / symbol");
    };

    let input_result = collect(mode)?;

    if cli.dry_run {
        print_dry_run(&input_result);
        return Ok(ExitCode::SUCCESS);
    }

    let workspace = cli
        .workspace
        .canonicalize()
        .with_context(|| format!("workspace not found: {}", cli.workspace.display()))?;
    let resolve_result = resolve_seeds(&workspace, &input_result.vuln_symbols)?;

    if cli.resolve_only {
        print_resolve_only(&input_result, &resolve_result);
        return Ok(ExitCode::SUCCESS);
    }

    let run_result = run(&workspace, resolve_result.resolved.clone())?;
    let formatted = render(
        &workspace,
        &input_result,
        &resolve_result,
        &run_result,
        cli.format.into(),
    );
    print!("{formatted}");
    Ok(ExitCode::from(exit_code(verdict(&run_result), cli.strict)))
}

fn print_dry_run(r: &InputResult) {
    println!(
        "Parsed input: {} symbol(s), {} unsupported advisory(ies)",
        r.vuln_symbols.len(),
        r.unsupported.len()
    );
    for v in &r.vuln_symbols {
        let id = v.advisory_id.as_deref().unwrap_or("(direct)");
        let reqs: Vec<String> = v.version_reqs.iter().map(|r| r.to_string()).collect();
        let reqs_str = if reqs.is_empty() {
            "*".to_string()
        } else {
            reqs.join(", ")
        };
        println!(
            "  symbol  {id}  {}  {}  [{}]",
            v.crate_name, v.path, reqs_str
        );
    }
    for u in &r.unsupported {
        let reason = match u.reason {
            UnsupportedReason::NoAffectedFunctions => "no affected.functions in DB",
        };
        println!("  unsupp  {}  {}  ({reason})", u.advisory_id, u.crate_name);
    }
}

fn print_resolve_only(input: &InputResult, r: &ResolveResult) {
    println!(
        "Resolved: {} seed(s), {} unresolved",
        r.resolved.len(),
        r.unresolved.len()
    );
    for seed in &r.resolved {
        let id = seed.symbol.advisory_id.as_deref().unwrap_or("(direct)");
        println!(
            "  resolved  {id}  {}  -> {}:{}:{}",
            seed.symbol.path,
            seed.file.display(),
            seed.position.line + 1,
            seed.position.character + 1
        );
    }
    for u in &r.unresolved {
        let id = u.symbol.advisory_id.as_deref().unwrap_or("(direct)");
        let reason = match &u.reason {
            UnresolvedReason::NoDependency => "no_dependency".to_string(),
            UnresolvedReason::NotAffectedVersion(v) => {
                format!("not_affected_version (in use: {v})")
            }
            UnresolvedReason::ProbeFailed(msg) => format!("probe_failed: {msg}"),
        };
        println!("  unresolved  {id}  {}  ({reason})", u.symbol.path);
    }
    for u in &input.unsupported {
        let reason = match u.reason {
            UnsupportedReason::NoAffectedFunctions => "no affected.functions in DB",
        };
        println!(
            "  unsupported {}  {}  ({reason})",
            u.advisory_id, u.crate_name
        );
    }
}
