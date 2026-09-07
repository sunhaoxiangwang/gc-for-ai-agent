//! `reap explain <path>`: the trust-building command.
//!
//! It reports on rules that did not match as well as the one that did, because
//! "why was this not selected" is asked at least as often as the opposite.

use anyhow::Result;
use reap_core::plan::{explain, PathKind, PlanOptions};
use reap_core::time::{human_bytes, human_duration};

use crate::cli::ExplainArgs;
use crate::context::Context;
use crate::exit;
use crate::output::Style;

pub fn run(ctx: &Context, args: &ExplainArgs) -> Result<i32> {
    // Report on the path the user typed, resolved against the working
    // directory but not canonicalized: canonicalizing here would hide exactly
    // the symlink situation they may be asking about.
    let path = if args.path.is_absolute() {
        args.path.clone()
    } else {
        std::env::current_dir()?.join(&args.path)
    };

    let opts = PlanOptions {
        now: ctx.started_at,
        ..Default::default()
    };
    let ex = explain(&ctx.config, ctx.os, &path, opts);
    let style = Style::detect(ctx.args.no_color, ctx.args.json);

    if ctx.args.json {
        println!("{}", serde_json::to_string_pretty(&to_json(ctx, &ex))?);
        return Ok(outcome_code(&ex));
    }

    println!("{} {}", style.bold("path:"), ex.path.display());
    match &ex.kind {
        PathKind::Directory => {}
        PathKind::Symlink => println!(
            "  {} it is a symlink; reap never proposes or follows one",
            style.yellow("note:")
        ),
        PathKind::NotADirectory => println!(
            "  {} it is not a directory; rules match directories",
            style.yellow("note:")
        ),
        PathKind::Missing(e) => println!("  {} {e}", style.yellow("note:")),
    }

    match (&ex.root, ex.depth) {
        (Some(root), Some(depth)) => {
            println!("{} {} (depth {depth})", style.bold("root:"), root.display())
        }
        _ => println!(
            "{} {}",
            style.bold("root:"),
            style.red("none - this path is under no declared root, so it can never be selected")
        ),
    }

    println!();
    println!("{}", style.bold("Rules"));
    for trace in &ex.traces {
        let marker = if trace.outcome.matched() {
            style.green("match")
        } else {
            style.dim("  no ")
        };
        println!(
            "  {marker}  tier {}  {:<28} {}",
            trace.tier,
            trace.rule,
            style.dim(&trace.outcome.describe())
        );
    }

    println!();
    match &ex.candidate {
        None => {
            println!(
                "{}",
                style.green("Result: no rule selects this path. reap would not touch it.")
            );
        }
        Some(c) => {
            println!("{}", style.bold("Candidate"));
            println!("  rule      {}", c.rule);
            println!("  tier      {} ({})", c.tier, c.tier.description());
            println!("  size      {}", human_bytes(c.bytes));
            println!(
                "  idle      {} (rule requires {})",
                human_duration(c.idle),
                human_duration(c.min_idle)
            );
            println!();
            println!(
                "{}",
                style.yellow(
                    "Guards have not run: this build reports rule matching only. \
                     A matched path is still subject to every guard before removal."
                )
            );
        }
    }

    Ok(outcome_code(&ex))
}

fn outcome_code(ex: &reap_core::plan::PathExplanation) -> i32 {
    if ex.candidate.is_some() {
        exit::SUCCESS
    } else {
        exit::NOTHING_TO_DO
    }
}

fn to_json(ctx: &Context, ex: &reap_core::plan::PathExplanation) -> serde_json::Value {
    serde_json::json!({
        "schema_version": reap_core::schema::SCHEMA_VERSION,
        "command": "explain",
        "host": ctx.host,
        "os": ctx.os,
        "config": ctx.config.source.display().to_string(),
        "path": ex.path.display().to_string(),
        "root": ex.root.as_ref().map(|r| r.display().to_string()),
        "depth": ex.depth,
        "kind": match &ex.kind {
            PathKind::Directory => "directory".to_owned(),
            PathKind::Symlink => "symlink".to_owned(),
            PathKind::NotADirectory => "not_a_directory".to_owned(),
            PathKind::Missing(e) => format!("missing: {e}"),
        },
        "rules": ex.traces.iter().map(|t| serde_json::json!({
            "rule": t.rule,
            "tier": t.tier.get(),
            "matched": t.outcome.matched(),
            "reason": t.outcome.describe(),
        })).collect::<Vec<_>>(),
        "selected": ex.candidate.is_some(),
        "candidate": ex.candidate.as_ref().map(|c| serde_json::json!({
            "rule": c.rule,
            "tier": c.tier.get(),
            "bytes": c.bytes,
            "idle_seconds": c.idle.as_secs(),
            "min_idle_seconds": c.min_idle.as_secs(),
        })),
    })
}
