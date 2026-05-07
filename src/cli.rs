use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Detect which binary entrypoints are affected by code changes"
)]
pub struct Cli {
    /// Path to the Rust workspace root
    #[arg(long, global = true, default_value = ".")]
    pub workspace: PathBuf,

    /// Base git ref. In plain mode it is the diff base; in `--pr` mode it is the
    /// branch we compute the PR's merge-base against.
    #[arg(long, default_value = "master")]
    pub base: String,

    /// Head git ref. Required unless `--pr` is given.
    #[arg(long, conflicts_with = "pr")]
    pub head: Option<String>,

    /// GitHub PR number. Fetches `pull/<N>/head` from `origin` and computes the
    /// merge-base against `--base` automatically.
    #[arg(long, conflicts_with = "head")]
    pub pr: Option<u32>,

    #[command(subcommand)]
    pub command: Option<Command>,
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
pub enum DebugCmd {
    /// Print parsed git diff (file, changed line numbers)
    PrintDiff {
        #[arg(long)]
        base: String,
        #[arg(long)]
        head: String,
    },
    /// Print binary entrypoints discovered via cargo metadata
    PrintEntrypoints,
    /// Spawn rust-analyzer and exit
    SpawnOnly,
    /// Spawn + initialize + wait for index, then shutdown
    Index,
    /// Print documentSymbols intersecting a given line
    Symbols {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        line: Option<u32>,
    },
    /// Print references for a given (file, line, character). `--line` and `--char` are 0-based.
    References {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        line: u32,
        #[arg(long = "char")]
        character: u32,
    },
}
