//! `reap sweep`: plan, guard, and — given both opt-ins — reclaim.
//!
//! Mutation requires two separate decisions: `--apply` on the command line and
//! `dry_run = false` in the configuration file. One is a thing you type in a
//! moment; the other is a thing you edited on purpose. Requiring both means no
//! single mistake, in either place, removes anything.

use anyhow::Result;
use reap_core::plan::PlanOptions;
use reap_core::schema::{CommandReport, RunReport, Totals, SCHEMA_VERSION};
use reap_core::time::{human_bytes, human_duration, rfc3339};

use crate::cli::SweepArgs;
use crate::commands::commands_runner::run_command_rules;
use crate::context::{home, Context};
use crate::executor::{Executor, TimeBudget};
use crate::exit;
use crate::output::{tilde, Style, Table};
use crate::select::{select, Selection};

/// Whether this invocation is allowed to mutate, and why not when it is not.
pub enum Gate {
    /// Both opt-ins are present.
    Armed,
    /// `--apply` was not passed. This is the ordinary case.
    NotRequested,
    /// `--apply` was passed but the configuration still says `dry_run = true`.
    ConfigStillDry,
}

pub fn gate(ctx: &Context, apply: bool) -> Gate {
    match (apply, ctx.config.dry_run) {
        (false, _) => Gate::NotRequested,
        (true, true) => Gate::ConfigStillDry,
        (true, false) => Gate::Armed,
    }
}

pub fn run(ctx: &Context, args: &SweepArgs) -> Result<i32> {
    let opts = PlanOptions {
        now: ctx.started_at,
        max_tier: args.tier,
        ..Default::default()
    };
    let sel = select(ctx, opts);
    execute(ctx, sel, args.apply, args.tier, "sweep", None)
}

/// Runs a prepared selection, applying it when both opt-ins allow.
///
/// Shared with `gc-session`, which differs only in how it narrows the
/// selection.
pub fn execute(
    ctx: &Context,
    sel: Selection,
    apply: bool,
    tier: u8,
    command_name: &str,
    stopped_at_tier: Option<u8>,
) -> Result<i32> {
    let style = Style::detect(ctx.args.no_color, ctx.args.json);
    let gate = gate(ctx, apply);
    let free_before = ctx.free_bytes();

    let mut candidates = sel.candidate_reports();
    let mut commands: Vec<CommandReport> = sel.command_reports();
    let mut notes = sel.notes();
    let mut errors = sel.plan.walk_errors.clone();
    let mut reclaimed_bytes = 0u64;
    let mut commands_run = 0usize;
    let mut applied = false;
    let mut partial = false;

    if matches!(gate, Gate::Armed) {
        let mut ex = Executor::open(&ctx.config.quarantine).map_err(|e| {
            e.context(
                "refusing to sweep: the quarantine directory is not usable, and reap will not                  fall back to removing in place",
            )
        })?;

        // Anything a previous run left staged goes first, without re-planning.
        let recovered = ex.recover();
        if recovered.entries > 0 {
            notes.push(format!(
                "cleared {} entr{} left behind by an interrupted run{}",
                recovered.entries,
                if recovered.entries == 1 { "y" } else { "ies" },
                // The manifest says where those entries came from, which is the
                // difference between a reassuring note and a mysterious one.
                if recovered.from_manifest.is_empty() {
                    String::new()
                } else {
                    format!(
                        " (originally {})",
                        recovered
                            .from_manifest
                            .iter()
                            .map(|e| e.original_path.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            ));
            errors.extend(recovered.stats.errors);
        }

        // The rename has to work before anything is moved, every time.
        for root in &ctx.canonical_roots {
            if let Err(e) = ex.verify_rename(root) {
                return Err(e.context(
                    "refusing to sweep: the quarantine directory is not usable, and reap will \
                     not fall back to removing in place",
                ));
            }
        }

        let budget = TimeBudget::new(ctx.config.max_run);
        let mut timed_out = false;

        // `candidates` is built from `sel.judged` in order, so the index is
        // what ties a staged tree back to its report row. Matching on the path
        // string would not: a candidate is walked as `/tmp/x` and staged from
        // its canonical `/private/tmp/x`.
        for (index, judged) in sel.judged.iter().enumerate() {
            if !judged.selected() {
                continue;
            }
            if budget.exhausted() {
                timed_out = true;
                break;
            }
            let Some(canonical) = judged.decision.canonical.as_deref() else {
                // Unreachable: a selected candidate always has one. Skipping is
                // the safe reading of a state that should not exist.
                errors.push(format!(
                    "{} was selected without a canonical path; skipped",
                    judged.candidate.path.display()
                ));
                continue;
            };
            match ex.stage(&judged.candidate, canonical) {
                Ok(()) => {
                    candidates[index].reclaimed = true;
                    reclaimed_bytes += judged.candidate.bytes;
                }
                Err(e) => errors.push(format!("{e:#}")),
            }
        }

        applied = candidates.iter().any(|c| c.reclaimed);

        // Remove the staged trees. Errors here are collected, not fatal: the
        // trees are already out of the user's way.
        let drain = ex.drain();
        errors.extend(drain.errors);

        if timed_out {
            notes.push(format!(
                "stopped starting new work after {}; run again to continue",
                human_duration(ctx.config.max_run)
            ));
        }

        let (ran, cmd_errors) = run_command_rules(ctx, &mut commands, &budget);
        commands_run = ran;
        errors.extend(cmd_errors);
        applied = applied || commands_run > 0;
        partial = !errors.is_empty();
    } else {
        // In a dry run, command rules print what they would execute rather than
        // running it.
        for c in &commands {
            notes.push(format!("would run: {}", c.argv.join(" ")));
        }
    }

    let selected = candidates.iter().filter(|c| c.selected).count();
    let report = RunReport {
        schema_version: SCHEMA_VERSION,
        reap_version: env!("CARGO_PKG_VERSION").to_owned(),
        host: ctx.host.clone(),
        os: ctx.os.to_owned(),
        arch: ctx.arch.to_owned(),
        command: command_name.to_owned(),
        config: ctx.config.source.display().to_string(),
        started_at: rfc3339(ctx.started_at),
        duration_ms: ctx.elapsed_ms(),
        free_before_bytes: free_before,
        free_after_bytes: if applied { ctx.free_bytes() } else { None },
        applied,
        dry_run: ctx.config.dry_run,
        stopped_at_tier,
        totals: Totals {
            candidates: candidates.len(),
            selected,
            rejected: candidates.len() - selected,
            selected_bytes: sel.selected_bytes(),
            reclaimed_bytes,
            commands_run,
            errors: errors.len(),
        },
        candidates,
        commands,
        notes,
        errors,
    };

    if ctx.args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(ctx, &style, &report, &gate, tier);
    }

    Ok(match gate {
        // `--apply` against a config that still says dry_run is a configuration
        // state standing between the user and what they asked for, and it needs
        // to be visible in `systemctl status`, not swallowed as success.
        Gate::ConfigStillDry => exit::CONFIG,
        _ if partial => exit::PARTIAL,
        _ if report.totals.selected == 0 && report.commands.is_empty() => exit::NOTHING_TO_DO,
        _ => exit::SUCCESS,
    })
}

fn print_human(ctx: &Context, style: &Style, report: &RunReport, gate: &Gate, tier: u8) {
    let home = home();
    let mut rows: Vec<_> = report.candidates.iter().filter(|c| c.selected).collect();
    rows.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));

    if rows.is_empty() && report.commands.is_empty() {
        println!("Nothing to reclaim at tier {tier} or below.");
    } else if !rows.is_empty() {
        let verb = if report.applied {
            "RECLAIMED"
        } else {
            "WOULD FREE"
        };
        let mut table = Table::new(&[verb, "TIER", "RULE", "PATH"]).right_align(0);
        for c in &rows {
            table.push(vec![
                human_bytes(c.bytes),
                c.tier.to_string(),
                c.rule.clone(),
                tilde(std::path::Path::new(&c.path), home.as_deref()),
            ]);
        }
        print!("{}", table.render(style));
        println!();
    }

    for c in &report.commands {
        if c.executed {
            println!(
                "  ran  {}  ({})",
                c.argv.join(" "),
                match c.reclaimed_bytes {
                    Some(b) => human_bytes(b),
                    None => "size not reported".to_owned(),
                }
            );
        }
    }

    match gate {
        Gate::Armed => {
            println!(
                "{}",
                style.bold(&format!(
                    "Reclaimed {} across {} directories.",
                    human_bytes(report.totals.reclaimed_bytes),
                    report.candidates.iter().filter(|c| c.reclaimed).count()
                ))
            );
            if let (Some(before), Some(after)) = (report.free_before_bytes, report.free_after_bytes)
            {
                println!(
                    "Free space {} -> {}.",
                    human_bytes(before),
                    human_bytes(after)
                );
            }
        }
        Gate::NotRequested => {
            println!(
                "{} across {} directories.",
                style.bold(&human_bytes(report.totals.selected_bytes)),
                report.totals.selected
            );
            println!(
                "{}",
                style
                    .dim("Nothing was removed. Add --apply once you are satisfied with this list.")
            );
        }
        Gate::ConfigStillDry => {
            println!(
                "{} across {} directories.",
                style.bold(&human_bytes(report.totals.selected_bytes)),
                report.totals.selected
            );
            println!();
            println!(
                "{}",
                style.yellow(&format!(
                    "Nothing was removed: --apply was given, but {} still has dry_run = true.",
                    ctx.config.source.display()
                ))
            );
            println!(
                "{}",
                style.dim(
                    "Removal needs both opt-ins. Set dry_run = false in the config, then run \
                     with --apply again."
                )
            );
        }
    }

    if report.totals.rejected > 0 {
        println!(
            "{}",
            style.dim(&format!(
                "{} matched a rule but were held back by a guard; `reap report --all` lists them.",
                report.totals.rejected
            ))
        );
    }
    for note in &report.notes {
        println!("{}", style.dim(&format!("note: {note}")));
    }
    for err in &report.errors {
        eprintln!("{}", style.yellow(&format!("warning: {err}")));
    }
}
