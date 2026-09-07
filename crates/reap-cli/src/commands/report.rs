//! `reap report`: what could be reclaimed, sorted by size. Never mutates.

use anyhow::Result;
use reap_core::plan::{plan, PlanOptions};
use reap_core::schema::{CandidateReport, CommandReport, RunReport, Totals, SCHEMA_VERSION};
use reap_core::time::{human_bytes, human_duration, rfc3339};

use crate::cli::ReportArgs;
use crate::context::{home, Context};
use crate::exit;
use crate::output::{tilde, Style, Table};

pub fn run(ctx: &Context, args: &ReportArgs) -> Result<i32> {
    let opts = PlanOptions {
        now: ctx.started_at,
        max_tier: args.tier,
        ..Default::default()
    };
    let plan = plan(&ctx.config, ctx.os, opts);

    let mut notes: Vec<String> = Vec::new();
    for skipped in &plan.skipped_roots {
        notes.push(format!(
            "root {} skipped: {}",
            skipped.path.display(),
            skipped.reason
        ));
    }
    if !plan.unmatched_rules.is_empty() {
        notes.push(format!(
            "rules that matched nothing: {}",
            plan.unmatched_rules.join(", ")
        ));
    }

    let candidates: Vec<CandidateReport> = plan
        .candidates
        .iter()
        .map(|c| CandidateReport {
            path: c.path.display().to_string(),
            root: c.root.display().to_string(),
            rule: c.rule.clone(),
            tier: c.tier.get(),
            depth: c.depth,
            bytes: c.bytes,
            idle_seconds: c.idle.as_secs(),
            selected: true,
            rejected_by: None,
            reason: None,
            reclaimed: false,
        })
        .collect();

    let commands: Vec<CommandReport> = plan
        .commands
        .iter()
        .map(|c| CommandReport {
            rule: c.rule.clone(),
            tier: c.tier.get(),
            argv: c.argv.clone(),
            executed: false,
            reclaimed_bytes: None,
            exit_code: None,
            error: None,
        })
        .collect();

    let selected_bytes: u64 = candidates.iter().map(|c| c.bytes).sum();
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
            selected: candidates.len(),
            rejected: 0,
            selected_bytes,
            reclaimed_bytes: 0,
            commands_run: 0,
            errors: plan.walk_errors.len(),
        },
        candidates,
        commands,
        notes,
        errors: plan.walk_errors.clone(),
    };

    if ctx.args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(ctx, &report);
    }

    Ok(
        if report.totals.selected == 0 && report.commands.is_empty() {
            exit::NOTHING_TO_DO
        } else {
            exit::SUCCESS
        },
    )
}

fn print_human(ctx: &Context, report: &RunReport) {
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

    let mut rows: Vec<&reap_core::schema::CandidateReport> = report.candidates.iter().collect();
    rows.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));

    if rows.is_empty() {
        println!("No path candidates matched.");
    } else {
        let mut table = Table::new(&["SIZE", "TIER", "IDLE", "RULE", "PATH"]).right_align(0);
        for c in &rows {
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
        style.dim(
            "This command deletes nothing. See `reap explain <path>` for why a path was chosen."
        )
    );
}
