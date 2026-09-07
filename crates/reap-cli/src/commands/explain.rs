//! `reap explain <path>`: the trust-building command.
//!
//! It reports on rules that did not match as well as the one that did, because
//! "why was this not selected" is asked at least as often as the opposite.

use anyhow::Result;
use reap_core::guard::{evaluate, Decision, Verdict};
use reap_core::plan::{explain, Candidate, PathExplanation, PathKind, PlanOptions};
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
    // Guards run against the candidate the rules produced. When no rule
    // matched there is nothing to guard, and saying so is the whole answer.
    let decision = ex.candidate.as_ref().map(|c| evaluate(c, &ctx.guards()));
    let style = Style::detect(ctx.args.no_color, ctx.args.json);

    if ctx.args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&to_json(ctx, &ex, decision.as_ref()))?
        );
        return Ok(outcome_code(&ex, decision.as_ref()));
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

    let Some(c) = &ex.candidate else {
        println!();
        println!(
            "{}",
            style.green("Result: no rule names this path, so reap would never touch it.")
        );
        return Ok(outcome_code(&ex, None));
    };
    let decision = decision.expect("a candidate always gets a decision");

    println!();
    print_candidate(&style, c);

    println!();
    println!("{}", style.bold("Guards"));
    for result in &decision.results {
        let (marker, detail) = match &result.verdict {
            Verdict::Pass(d) => (style.green(" pass   "), d.clone()),
            Verdict::Reject(d) => (style.red(" FAIL   "), d.clone()),
            Verdict::Skipped(d) => (style.dim(" skipped"), d.clone()),
            Verdict::NotReached => (
                style.dim(" -      "),
                "not reached; an earlier guard already rejected".to_owned(),
            ),
        };
        println!("  {marker}  {:<18} {}", result.guard, style.dim(&detail));
    }

    println!();
    if decision.selected {
        println!(
            "{}",
            style.yellow("Result: every guard passed. This path would be reclaimed by a sweep.")
        );
    } else {
        println!(
            "{}",
            style.green(&format!(
                "Result: held back by the {} guard. reap would not touch it.",
                decision.rejected_by.unwrap_or("unknown")
            ))
        );
    }

    Ok(outcome_code(&ex, Some(&decision)))
}

fn print_candidate(style: &Style, c: &Candidate) {
    println!("{}", style.bold("Candidate"));
    println!("  rule      {}", c.rule);
    println!("  tier      {} ({})", c.tier, c.tier.description());
    println!("  size      {}", human_bytes(c.bytes));
    println!(
        "  idle      {} (rule requires {})",
        human_duration(c.idle),
        human_duration(c.min_idle)
    );
}

/// Exit 0 when the path would be reclaimed, 3 when it would not.
///
/// A scheduler or a script can therefore ask "is this going away?" without
/// parsing anything.
fn outcome_code(ex: &PathExplanation, decision: Option<&Decision>) -> i32 {
    match (ex.candidate.is_some(), decision) {
        (true, Some(d)) if d.selected => exit::SUCCESS,
        _ => exit::NOTHING_TO_DO,
    }
}

fn to_json(ctx: &Context, ex: &PathExplanation, decision: Option<&Decision>) -> serde_json::Value {
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
        "selected": decision.is_some_and(|d| d.selected),
        "rejected_by": decision.and_then(|d| d.rejected_by),
        "reason": decision.and_then(|d| d.reason.clone()),
        "guards": decision.map(|d| d.results.iter().map(|r| serde_json::json!({
            "guard": r.guard,
            "verdict": r.verdict.label(),
            "detail": r.verdict.detail(),
        })).collect::<Vec<_>>()),
        "candidate": ex.candidate.as_ref().map(|c| serde_json::json!({
            "rule": c.rule,
            "tier": c.tier.get(),
            "bytes": c.bytes,
            "idle_seconds": c.idle.as_secs(),
            "min_idle_seconds": c.min_idle.as_secs(),
        })),
    })
}
