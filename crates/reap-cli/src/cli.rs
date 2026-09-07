//! Command-line surface.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Reclaim disk space taken by build artifacts, dependency trees and tool
/// caches. Deletes nothing unless a rule in your config names it, and nothing
/// at all without two separate opt-ins.
#[derive(Debug, Parser)]
#[command(name = "reap", version, about, long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args, Clone)]
pub struct GlobalArgs {
    /// Configuration file to use, overriding the search path.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Print per-path detail while working.
    #[arg(long, short, global = true, conflicts_with = "quiet")]
    pub verbose: bool,

    /// Print only errors.
    #[arg(long, short, global = true)]
    pub quiet: bool,

    /// Emit a single JSON document instead of human-readable output.
    #[arg(long, global = true)]
    pub json: bool,

    /// Disable colour even when the output is a terminal.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Answer yes to any confirmation prompt.
    #[arg(long, short = 'y', global = true)]
    pub yes: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show what could be reclaimed, largest first. Deletes nothing.
    Report(ReportArgs),

    /// Explain why one path would or would not be reclaimed, guard by guard.
    Explain(ExplainArgs),
}

#[derive(Debug, Args)]
pub struct ReportArgs {
    /// Highest tier to include. 0 is near-free to regenerate, 2 is shared
    /// caches whose loss slows every later build.
    #[arg(long, value_name = "N", default_value_t = 2)]
    pub tier: u8,

    /// Include candidates that were rejected by a guard.
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct ExplainArgs {
    /// The path to explain.
    pub path: PathBuf,
}
