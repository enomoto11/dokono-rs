//! User-facing output: progress reporting (stderr) and final result (stdout).
//!
//! Two output modes are supported:
//! - **text** (default): a `indicatif` spinner on stderr when stderr is a TTY,
//!   plain log lines otherwise (for CI logs). Final result goes to stdout in
//!   a human-readable form.
//! - **json**: silent on stderr; final result is a single JSON object on stdout,
//!   suitable for CI consumers (`| jq`).

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Serialize;
use std::io::IsTerminal;
use std::time::Duration;

use crate::cli::OutputFormat;

pub enum Reporter {
    Spinner(ProgressBar),
    Plain,
    Silent,
}

impl Reporter {
    pub fn for_format(format: OutputFormat) -> Self {
        match format {
            OutputFormat::Json => Reporter::Silent,
            OutputFormat::Text => {
                if std::io::stderr().is_terminal() {
                    let pb = ProgressBar::new_spinner();
                    pb.set_style(
                        ProgressStyle::default_spinner()
                            .template("{spinner:.cyan} {msg}")
                            .expect("static template is valid"),
                    );
                    pb.enable_steady_tick(Duration::from_millis(100));
                    Reporter::Spinner(pb)
                } else {
                    Reporter::Plain
                }
            }
        }
    }

    /// Set the current phase. In spinner mode this updates the live message;
    /// in plain mode it prints a log line; in silent mode it is a no-op.
    pub fn phase(&self, msg: impl Into<String>) {
        let msg = msg.into();
        match self {
            Reporter::Spinner(pb) => pb.set_message(msg),
            Reporter::Plain => eprintln!("{msg}"),
            Reporter::Silent => {}
        }
    }

    /// Print a one-off informational line that should remain visible above the
    /// spinner (e.g., the resolved PR head/base).
    pub fn note(&self, msg: impl AsRef<str>) {
        let msg = msg.as_ref();
        match self {
            Reporter::Spinner(pb) => pb.println(msg),
            Reporter::Plain => eprintln!("{msg}"),
            Reporter::Silent => {}
        }
    }

    /// Tear down the spinner before final stdout output so the cleared line
    /// does not collide with the result.
    pub fn finish(self) {
        if let Reporter::Spinner(pb) = self {
            pb.finish_and_clear();
        }
    }
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    NoRsChanges,
    NoSymbolChanges,
}

#[derive(Serialize, Debug)]
pub struct Summary {
    pub schema_version: u32,
    pub pr: Option<u32>,
    pub base: String,
    pub head: String,
    pub status: Status,
    pub affected: Vec<String>,
}

pub fn emit(format: OutputFormat, summary: &Summary) -> Result<()> {
    match format {
        OutputFormat::Text => {
            print_text(summary);
            Ok(())
        }
        OutputFormat::Json => {
            let s = serde_json::to_string_pretty(summary)?;
            println!("{s}");
            Ok(())
        }
    }
}

fn print_text(summary: &Summary) {
    match summary.status {
        Status::NoRsChanges => {
            println!(
                "(no .rs file changes between {} and {})",
                summary.base, summary.head
            );
        }
        Status::NoSymbolChanges => {
            println!("(no symbol-level changes found)");
        }
        Status::Ok => {
            if summary.affected.is_empty() {
                println!("Affected entrypoints: none");
            } else {
                println!("Affected entrypoints:");
                for p in &summary.affected {
                    println!("  {p}");
                }
            }
        }
    }
}
