//! `reap report`: what could be reclaimed, sorted by size. Never mutates.

use anyhow::Result;
use reap_core::plan::PlanOptions;
use reap_core::schema::{RunReport, Totals, SCHEMA_VERSION};
use reap_core::time::{human_bytes, human_duration, rfc3339};

use crate::cli::ReportArgs;
use crate::context::{home, Context};
use crate::exit;
use crate::output::{tilde, Style, Table};
use crate::select::select;

pub fn run(ctx: &Context, args: &ReportArgs) -> Result<i32> {
    let opts = PlanOptions {
        now: ctx.started_at,
        max_tier: args.tier,
        ..Default::default()
    };
    let sel = select(ctx, opts);

    let candidates = sel.candidate_reports();
    let selected = candidates.iter().filter(|c| c.selected).count();
    let selected_bytes = sel.selected_bytes();

    let report = RunReport {
        schema_version: SCHEMA_VERSION,
        reap_version: env!("CARGO_PKG_VERSION").to_owned(),
        host: ctx.host.clone(),
        os: ctx.os.to_owned(),
        arch: ctx.arch.to_owned(),
        command: "report".to_owned(),
        config: ctx.config.source.display().to_string(),
        started_at: rfc3339(ctx.started_at),
        duration_ms: ctx.elapsed_ms(),
        free_before_bytes: ctx.free_bytes(),
        free_after_bytes: None,
        applied: false,
        dry_run: ctx.config.dry_run,
        stopped_at_tier: None,
        totals: Totals {
            candidates: candidates.len(),
            selected,
            rejected: candidates.len() - selected,
            selected_bytes,
            reclaimed_bytes: 0,
            commands_run: 0,
            errors: sel.plan.walk_errors.len(),
        },
        candidates,
        commands: sel.command_reports(),
        notes: sel.notes(),
        errors: sel.plan.walk_errors.clone(),
    };

    if ctx.args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(ctx, &report, args.all);
    }

    Ok(
        if report.totals.selected == 0 && report.commands.is_empty() {
            exit::NOTHING_TO_DO
        } else {
            exit::SUCCESS
        },
    )
}

fn print_human(ctx: &Context, report: &RunReport, show_all: bool) {
    let style = Style::detect(ctx.args.no_color, ctx.args.json);
    let home = home();

    if ctx.args.quiet {
        println!(
            "{} across {} directories",
            human_bytes(report.totals.selected_bytes),
            report.totals.selected
        );
        return;
    }

    let mut selected: Vec<_> = report.candidates.iter().filter(|c| c.selected).collect();
    selected.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));

    if selected.is_empty() {
        println!("Nothing is reclaimable right now.");
    } else {
        let mut table = Table::new(&["SIZE", "TIER", "IDLE", "RULE", "PATH"]).right_align(0);
        for c in &selected {
            table.push(vec![
                human_bytes(c.bytes),
                c.tier.to_string(),
                human_duration(std::time::Duration::from_secs(c.idle_seconds)),
                c.rule.clone(),
                tilde(std::path::Path::new(&c.path), home.as_deref()),
            ]);
        }
        print!("{}", table.render(&style));
        println!();
        println!(
            "{} across {} directories",
            style.bold(&human_bytes(report.totals.selected_bytes)),
            report.totals.selected
        );
    }

    if report.totals.rejected > 0 {
        println!();
        if show_all {
            let mut rejected: Vec<_> = report.candidates.iter().filter(|c| !c.selected).collect();
            rejected.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
            let mut table = Table::new(&["SIZE", "RULE", "GUARD", "PATH", "REASON"]).right_align(0);
            for c in &rejected {
                table.push(vec![
                    human_bytes(c.bytes),
                    c.rule.clone(),
                    c.rejected_by.clone().unwrap_or_default(),
                    tilde(std::path::Path::new(&c.path), home.as_deref()),
                    c.reason.clone().unwrap_or_default(),
                ]);
            }
            println!("{}", style.bold("Matched but held back by a guard"));
            print!("{}", table.render(&style));
        } else {
            println!(
                "{}",
                style.dim(&format!(
                    "{} more matched a rule but were held back by a guard. Use --all to list them.",
                    report.totals.rejected
                ))
            );
        }
    }

    if !report.commands.is_empty() {
        println!();
        println!(
            "{}",
            style.bold("Command reclaimers (size not known until run)")
        );
        for c in &report.commands {
            println!("  tier {}  {}  {}", c.tier, c.rule, c.argv.join(" "));
        }
    }

    for note in &report.notes {
        println!("{}", style.dim(&format!("note: {note}")));
    }
    for err in &report.errors {
        eprintln!("{}", style.yellow(&format!("warning: {err}")));
    }

    println!();
    println!(
        "{}",
        style.dim("This command deletes nothing. Run `reap explain <path>` to see the guards for one path.")
    );
}
