use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Detect which tests in a Rust workspace are affected by code changes"
)]
pub struct Cli {
    /// Path to the Rust workspace root
    #[arg(long, global = true, default_value = ".")]
    pub workspace: PathBuf,

    /// Base git ref. In plain mode it is the diff base; in `--pr` mode it is the
    /// branch the PR's merge-base is computed against.
    #[arg(long, default_value = "master")]
    pub base: String,

    /// Head git ref. Required unless `--pr` is given.
    #[arg(long, conflicts_with = "pr")]
    pub head: Option<String>,

    /// GitHub PR number. Fetches `pull/<N>/head` from `origin`.
    #[arg(long, conflicts_with = "head")]
    pub pr: Option<u32>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,

    /// Run the affected tests via `cargo test` after detection.
    #[arg(long)]
    pub exec: bool,

    /// Restrict scan/exec to these packages (repeatable). Empty means all.
    #[arg(long = "package")]
    pub packages: Vec<String>,

    /// Path to a `dokono-test.toml` config file.
    #[arg(long)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum OutputFormat {
    Text,
    Json,
    Cargo,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Inspect individual pipeline stages
    Debug {
        #[command(subcommand)]
        cmd: DebugCmd,
    },
}

#[derive(Subcommand, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum DebugCmd {
    /// Print every test function discovered by static `syn` parsing.
    PrintTests,
    /// Print parsed git diff (file, changed line numbers).
    PrintDiff {
        #[arg(long)]
        base: String,
        #[arg(long)]
        head: String,
    },
    /// Print BFS starting symbols derived from the git diff via LSP.
    PrintStarts {
        #[arg(long)]
        base: String,
        #[arg(long)]
        head: String,
    },
}
