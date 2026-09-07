//! `reap doctor`: validate everything and explain what is wrong.
//!
//! This is the command a user runs when something is not behaving. It is also
//! the command that catches the two macOS problems that otherwise present as
//! "reap does nothing and says nothing": Full Disk Access, which fails silently
//! with `EPERM` rather than prompting, and APFS local snapshots, which make
//! free space look worse than it is.

use std::path::Path;

use anyhow::Result;
use reap_core::plan::PlanOptions;
use reap_core::time::{human_bytes, human_duration};

use crate::context::Context;
use crate::exit;
use crate::output::Style;
use crate::quarantine::Quarantine;
use crate::select::select;

/// Environment variables that collapse per-project caches into shared ones.
///
/// Each one that is unset silently reintroduces N-way duplication, which for
/// most machines is a bigger number than anything reap reclaims.
const PREVENTION_VARS: &[(&str, &str)] = &[
    (
        "CARGO_TARGET_DIR",
        "one shared Rust build directory instead of one per project",
    ),
    (
        "RUSTC_WRAPPER",
        "set to sccache to share compiled crates across projects",
    ),
    (
        "PLAYWRIGHT_BROWSERS_PATH",
        "one browser download instead of one per project",
    ),
    (
        "PUPPETEER_CACHE_DIR",
        "one Chromium download instead of one per project",
    ),
    ("UV_CACHE_DIR", "one shared uv cache"),
    ("PIP_CACHE_DIR", "one shared pip cache"),
    ("GOCACHE", "one shared Go build cache"),
    ("GRADLE_USER_HOME", "one shared Gradle cache"),
];

#[derive(Default)]
struct Findings {
    failures: Vec<String>,
    warnings: Vec<String>,
}

impl Findings {
    fn fail(&mut self, msg: impl Into<String>) {
        self.failures.push(msg.into());
    }
    fn warn(&mut self, msg: impl Into<String>) {
        self.warnings.push(msg.into());
    }
}

pub fn run(ctx: &Context) -> Result<i32> {
    let style = Style::detect(ctx.args.no_color, ctx.args.json);
    let mut f = Findings::default();
    let mut lines: Vec<String> = Vec::new();

    macro_rules! say {
        ($($arg:tt)*) => { lines.push(format!($($arg)*)) };
    }

    // --- configuration -----------------------------------------------------
    say!("{}", style.bold("Configuration"));
    say!("  file            {}", ctx.config.source.display());
    say!("  host            {}", ctx.host);
    match &ctx.config.applied_overlay {
        Some(name) => say!("  machine overlay [machine.\"{name}\"] applied"),
        None => say!(
            "  machine overlay none matched this host{}",
            if ctx.config.hostname == ctx.host {
                ""
            } else {
                " (!)"
            }
        ),
    }
    say!(
        "  dry_run         {}{}",
        ctx.config.dry_run,
        if ctx.config.dry_run {
            "  (nothing will be removed until this is false and --apply is passed)"
        } else {
            "  (removal is armed; --apply is still required on the command line)"
        }
    );
    say!("  pin marker      {}", ctx.config.pin_marker);
    say!("  max run         {}", human_duration(ctx.config.max_run));
    say!(
        "  case matching   {}",
        if ctx.config.case_sensitive_matching {
            "case-sensitive"
        } else {
            "case-insensitive"
        }
    );

    // --- roots -------------------------------------------------------------
    say!("");
    say!("{}", style.bold("Roots"));
    let mut usable_roots = Vec::new();
    for root in &ctx.config.roots {
        match std::fs::symlink_metadata(&root.path) {
            Err(e) => {
                say!("  {} {}  ({e})", style.yellow("skip"), root.path.display());
                f.warn(format!(
                    "root {} does not exist; it will be skipped on this machine",
                    root.path.display()
                ));
            }
            Ok(md) if md.file_type().is_symlink() => {
                say!("  {} {}", style.red("FAIL"), root.path.display());
                f.fail(format!(
                    "root {} is a symlink; declare the path it resolves to instead",
                    root.path.display()
                ));
            }
            Ok(md) if !md.is_dir() => {
                say!("  {} {}", style.red("FAIL"), root.path.display());
                f.fail(format!("root {} is not a directory", root.path.display()));
            }
            Ok(_) => {
                // A test read is what detects a TCC denial: on macOS it fails
                // with EPERM rather than prompting.
                match std::fs::read_dir(&root.path) {
                    Ok(entries) => {
                        let n = entries.count();
                        say!(
                            "  {} {}  (depth {}, {n} entries)",
                            style.green("ok  "),
                            root.path.display(),
                            root.max_depth
                        );
                        usable_roots.push(root.path.clone());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                        say!(
                            "  {} {}  (permission denied)",
                            style.red("FAIL"),
                            root.path.display()
                        );
                        f.fail(tcc_message(&root.path));
                    }
                    Err(e) => {
                        say!("  {} {}  ({e})", style.red("FAIL"), root.path.display());
                        f.fail(format!("cannot read {}: {e}", root.path.display()));
                    }
                }
            }
        }
    }

    // --- quarantine --------------------------------------------------------
    say!("");
    say!("{}", style.bold("Quarantine"));
    say!("  directory       {}", ctx.config.quarantine.display());
    match Quarantine::ensure(&ctx.config.quarantine) {
        Err(e) => {
            say!("  {} {e:#}", style.red("FAIL"));
            f.fail(format!("{e:#}"));
        }
        Ok(q) => {
            for root in &usable_roots {
                match q.test_rename(root) {
                    Ok(()) => say!(
                        "  {} rename from {} succeeds",
                        style.green("ok  "),
                        root.display()
                    ),
                    Err(e) => {
                        say!("  {} {e:#}", style.red("FAIL"));
                        f.fail(format!("{e:#}"));
                    }
                }
            }
        }
    }

    // --- free space --------------------------------------------------------
    say!("");
    say!("{}", style.bold("Disk"));
    match ctx.volume.stat(nearest_existing(&ctx.config.quarantine)) {
        Err(e) => {
            say!(
                "  {} could not measure free space: {e}",
                style.yellow("warn")
            );
            f.warn(format!("could not measure free space: {e}"));
        }
        Ok(stats) => {
            say!(
                "  free            {} of {} ({:.1}%)",
                human_bytes(stats.available_bytes),
                human_bytes(stats.total_bytes),
                stats.available_pct()
            );
            say!(
                "  low water       {}%   target {}%",
                ctx.config.low_water_pct,
                ctx.config.target_free_pct
            );
            let snapshots = ctx
                .volume
                .purgeable_snapshots(Path::new("/"))
                .unwrap_or_default();
            if !snapshots.is_empty() || ctx.os == "macos" {
                let with_slack = stats
                    .available_bytes
                    .saturating_add(ctx.config.apfs_snapshot_slack_bytes);
                let pct = if stats.total_bytes > 0 {
                    with_slack as f64 / stats.total_bytes as f64 * 100.0
                } else {
                    0.0
                };
                say!("  local snapshots {}", snapshots.len());
                say!(
                    "  free + slack    {} ({pct:.1}%)  using apfs_snapshot_slack_gb = {}",
                    human_bytes(with_slack),
                    ctx.config.apfs_snapshot_slack_bytes / (1024 * 1024 * 1024)
                );
                if !snapshots.is_empty() {
                    say!(
                        "  {}",
                        style.dim(
                            "Space held by local snapshots is reported as used but is released \
                             on demand, so free space can look worse than it is."
                        )
                    );
                }
            }
        }
    }

    // --- heartbeats --------------------------------------------------------
    say!("");
    say!("{}", style.bold("Heartbeats"));
    say!("  directory       {}", ctx.config.heartbeat_dir.display());
    match &ctx.heartbeats.unreadable {
        Some(why) => {
            say!("  {} {why}", style.dim("none"));
            say!(
                "  {}",
                style.dim(
                    "Nothing is claiming a workspace. Rules with orphaned_only will reject \
                     every candidate until this directory exists."
                )
            );
        }
        None => {
            let live = ctx
                .heartbeats
                .live(
                    ctx.started_at,
                    ctx.config.heartbeat_staleness_multiplier,
                    &reap_platform::process_exists,
                )
                .count();
            say!(
                "  {} {} entries, {live} live",
                style.green("ok  "),
                ctx.heartbeats.entries.len()
            );
        }
    }

    // --- rules -------------------------------------------------------------
    say!("");
    say!("{}", style.bold("Rules"));
    let sel = select(
        ctx,
        PlanOptions {
            now: ctx.started_at,
            ..Default::default()
        },
    );
    let inactive: Vec<&reap_core::config::Rule> = ctx.config.inactive_rules(ctx.os).collect();
    for rule in ctx.config.active_rules(ctx.os) {
        let matched = sel
            .judged
            .iter()
            .filter(|j| j.candidate.rule == rule.name)
            .count();
        let selected = sel
            .judged
            .iter()
            .filter(|j| j.candidate.rule == rule.name && j.selected())
            .count();
        if rule.kind == reap_core::config::RuleKind::Command {
            say!(
                "  {} tier {} {:<28} command",
                style.green("ok  "),
                rule.tier,
                rule.name
            );
        } else if matched == 0 {
            say!(
                "  {} tier {} {:<28} matched nothing",
                style.yellow("warn"),
                rule.tier,
                rule.name
            );
            f.warn(format!(
                "rule {:?} matched no directory; check its dir_name, glob and roots",
                rule.name
            ));
        } else {
            say!(
                "  {} tier {} {:<28} {matched} matched, {selected} would be reclaimed",
                style.green("ok  "),
                rule.tier,
                rule.name
            );
        }
    }
    for rule in &inactive {
        say!(
            "  {} tier {} {:<28} excluded: os = {:?}",
            style.dim("skip"),
            rule.tier,
            rule.name,
            rule.os
        );
    }

    // --- git ---------------------------------------------------------------
    say!("");
    say!("{}", style.bold("Tools"));
    match std::process::Command::new("git").arg("--version").output() {
        Ok(out) if out.status.success() => say!(
            "  {} {}",
            style.green("ok  "),
            String::from_utf8_lossy(&out.stdout).trim()
        ),
        _ => {
            say!("  {} git is not available", style.red("FAIL"));
            f.fail(
                "git is not on PATH. The git-ignore guard cannot get an answer without it, so \
                 every candidate inside a repository will be refused."
                    .to_owned(),
            );
        }
    }

    // --- prevention --------------------------------------------------------
    say!("");
    say!("{}", style.bold("Prevention"));
    let mut unset = Vec::new();
    for (var, why) in PREVENTION_VARS {
        match std::env::var(var) {
            Ok(v) if !v.is_empty() => say!("  {} {var}={v}", style.green("set ")),
            _ => {
                unset.push(*var);
                say!("  {} {var:<26} {}", style.dim("unset"), style.dim(why));
            }
        }
    }
    if !unset.is_empty() {
        say!(
            "  {}",
            style.dim(
                "Each unset variable above means one copy of a cache per project. Setting them \
                 usually saves more space than reclaiming does."
            )
        );
    }

    // --- verdict -----------------------------------------------------------
    if ctx.args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": reap_core::schema::SCHEMA_VERSION,
                "command": "doctor",
                "host": ctx.host,
                "os": ctx.os,
                "arch": ctx.arch,
                "config": ctx.config.source.display().to_string(),
                "ok": f.failures.is_empty(),
                "failures": f.failures,
                "warnings": f.warnings,
                "unset_prevention_vars": unset,
            }))?
        );
    } else {
        for line in &lines {
            println!("{line}");
        }
        println!();
        if f.failures.is_empty() && f.warnings.is_empty() {
            println!("{}", style.green("Everything checks out."));
        } else {
            for w in &f.warnings {
                println!("{} {w}", style.yellow("warning:"));
            }
            for e in &f.failures {
                println!("{} {e}", style.red("failure:"));
            }
        }
    }

    Ok(if f.failures.is_empty() {
        exit::SUCCESS
    } else {
        exit::PARTIAL
    })
}

/// The nearest existing ancestor of `path`, for asking about free space before
/// the directory itself exists.
fn nearest_existing(path: &Path) -> &Path {
    let mut probe = path;
    while !probe.exists() {
        match probe.parent() {
            Some(p) => probe = p,
            None => break,
        }
    }
    probe
}

/// The macOS Full Disk Access message, with the exact place to fix it.
fn tcc_message(root: &Path) -> String {
    if cfg!(target_os = "macos") {
        format!(
            "cannot read {}: permission denied.\n  \
             On macOS this is usually TCC: a scheduled job cannot read ~/Library/Caches, \
             ~/Documents, ~/Desktop or ~/Downloads without Full Disk Access, and the denial is \
             silent rather than a prompt.\n  \
             Grant it in System Settings > Privacy & Security > Full Disk Access, adding the \
             reap binary (and your terminal, if you are running it by hand).",
            root.display()
        )
    } else {
        format!("cannot read {}: permission denied", root.display())
    }
}
