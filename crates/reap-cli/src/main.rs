//! `reap`: a guarded, declarative reclaimer for build artifacts and tool
//! caches.
//!
//! This is the only crate in the workspace that mutates the filesystem.

mod cli;
mod commands;
mod context;
mod exit;
mod output;
mod quarantine;
mod select;

use clap::Parser;
use cli::{Cli, Command};
use context::Context;
use reap_core::error::ConfigError;

fn main() {
    let cli = Cli::parse();
    init_tracing(&cli);

    let code = match run(&cli) {
        Ok(code) => code,
        Err(e) => {
            // A configuration problem is the user's to fix and gets its own
            // exit code, so a scheduler can tell it apart from a crash.
            if e.downcast_ref::<ConfigError>().is_some() {
                eprintln!("error: {e}");
                exit::CONFIG
            } else {
                eprintln!("error: {e:#}");
                exit::INTERNAL
            }
        }
    };
    std::process::exit(code);
}

fn init_tracing(cli: &Cli) {
    let level = if cli.global.quiet {
        "error"
    } else if cli.global.verbose {
        "debug"
    } else {
        "info"
    };
    let filter = std::env::var("REAP_LOG").unwrap_or_else(|_| level.to_owned());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
        .try_init();
}

fn run(cli: &Cli) -> anyhow::Result<i32> {
    let ctx = Context::load(&cli.global)?;
    match &cli.command {
        Command::Doctor => commands::doctor::run(&ctx),
        Command::Report(args) => commands::report::run(&ctx, args),
        Command::Explain(args) => commands::explain::run(&ctx, args),
    }
}
