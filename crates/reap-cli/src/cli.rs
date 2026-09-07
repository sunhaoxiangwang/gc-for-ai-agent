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
    /// Validate the configuration and the environment, and explain what is wrong.
    Doctor,

    /// Show what could be reclaimed, largest first. Deletes nothing.
    Report(ReportArgs),

    /// Explain why one path would or would not be reclaimed, guard by guard.
    Explain(ExplainArgs),

    /// Reclaim what the rules select and the guards permit.
    Sweep(SweepArgs),

    /// Reclaim one supervised session's leftovers. Safe on an unknown id and
    /// safe to call twice.
    GcSession(GcSessionArgs),

    /// Install the scheduler unit for this platform.
    Install(InstallArgs),

    /// Remove the scheduler unit, leaving configuration and data alone.
    Uninstall(UninstallArgs),
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
pub struct SweepArgs {
    /// Highest tier to reclaim. A plain sweep touches tier 0 only.
    #[arg(
        long,
        value_name = "N",
        default_value_t = 0,
        conflicts_with = "until_free"
    )]
    pub tier: u8,

    /// Escalate through the tiers until this percentage of the filesystem is
    /// free, stopping as soon as the target is met.
    #[arg(long, value_name = "PCT")]
    pub until_free: Option<u8>,

    /// Actually remove things. Requires dry_run = false in the config as well;
    /// neither opt-in is sufficient on its own.
    #[arg(long)]
    pub apply: bool,
}

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Install a systemd user timer. The default on Linux.
    #[arg(long, conflicts_with = "launchd")]
    pub systemd: bool,

    /// Install a launchd agent. The default on macOS.
    #[arg(long)]
    pub launchd: bool,

    /// Print the unit files and the activation commands without writing or
    /// running anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// Remove the systemd user timer. The default on Linux.
    #[arg(long, conflicts_with = "launchd")]
    pub systemd: bool,

    /// Remove the launchd agent. The default on macOS.
    #[arg(long)]
    pub launchd: bool,
}

#[derive(Debug, Args)]
pub struct GcSessionArgs {
    /// The session id, as used in the heartbeat file name.
    pub id: String,

    /// Actually remove things. Requires dry_run = false in the config as well.
    #[arg(long)]
    pub apply: bool,
}

#[derive(Debug, Args)]
pub struct ExplainArgs {
    /// The path to explain.
    pub path: PathBuf,
}
