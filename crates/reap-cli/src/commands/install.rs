//! `reap install` and `reap uninstall`.
//!
//! Installing writes the scheduler unit and activates it. Uninstalling removes
//! the unit and tells you exactly what it left alone, because a tool that
//! silently deletes your configuration when you uninstall it has failed at the
//! one thing this whole program is about.

use anyhow::{Context as _, Result};

use crate::cli::{InstallArgs, UninstallArgs};
use crate::context::{home, Context};
use crate::exit;
use crate::output::Style;
use crate::scheduler::{self, Kind, Plan};

fn resolve_kind(systemd: bool, launchd: bool) -> Result<Kind> {
    match (systemd, launchd) {
        (true, false) => Ok(Kind::Systemd),
        (false, true) => Ok(Kind::Launchd),
        (false, false) => Ok(Kind::native()),
        (true, true) => anyhow::bail!("--systemd and --launchd are mutually exclusive"),
    }
}

/// Where the running binary lives, which is what the unit will invoke.
///
/// A unit pointing at a path that will not exist tomorrow is worse than no unit
/// at all, so a binary under a build directory is called out rather than
/// quietly written into a scheduler file.
fn binary_path() -> Result<std::path::PathBuf> {
    let exe = std::env::current_exe().context("could not determine the path of this binary")?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

fn build_plan(ctx: &Context, kind: Kind) -> Result<Plan> {
    let home = home().context("$HOME is unset, so there is nowhere to install a user unit")?;
    let binary = binary_path()?;
    Ok(scheduler::plan(
        kind,
        &ctx.config,
        &binary,
        &ctx.config.source,
        &home,
    ))
}

pub fn install(ctx: &Context, args: &InstallArgs) -> Result<i32> {
    let style = Style::detect(ctx.args.no_color, ctx.args.json);
    let kind = resolve_kind(args.systemd, args.launchd)?;
    let plan = build_plan(ctx, kind)?;
    let binary = binary_path()?;

    if args.dry_run {
        println!(
            "{}",
            style.bold(&format!("Would install a {} unit:", plan.kind.as_str()))
        );
        for unit in &plan.units {
            println!();
            println!(
                "{}",
                style.bold(&format!("--- {} ---", unit.path.display()))
            );
            print!("{}", unit.contents);
        }
        println!();
        println!("{}", style.bold("Then run:"));
        for cmd in &plan.activate {
            println!("  {}", cmd.join(" "));
        }
        return Ok(exit::SUCCESS);
    }

    // A unit that invokes a binary out of a build directory will break the
    // first time someone runs `cargo clean`, which is a particularly poor
    // outcome for this program.
    if binary
        .components()
        .any(|c| c.as_os_str() == "target" || c.as_os_str() == "debug")
    {
        eprintln!(
            "{}",
            style.yellow(&format!(
                "warning: the unit will invoke {}, which looks like a build directory. \
                 Install the binary somewhere stable first.",
                binary.display()
            ))
        );
    }

    if ctx.config.dry_run {
        eprintln!(
            "{}",
            style.yellow(&format!(
                "warning: {} still has dry_run = true, so the scheduled unit will report and \
                 remove nothing. That is a reasonable way to start; set dry_run = false when \
                 you are ready.",
                ctx.config.source.display()
            ))
        );
    }

    for unit in &plan.units {
        if let Some(parent) = unit.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        std::fs::write(&unit.path, &unit.contents)
            .with_context(|| format!("could not write {}", unit.path.display()))?;
        println!("wrote {}", unit.path.display());
    }

    // launchd writes its log where the plist says, and will not create the
    // directory itself.
    if plan.kind == Kind::Launchd {
        if let Some(h) = home() {
            let _ = std::fs::create_dir_all(h.join("Library/Logs/reap"));
        }
    }

    let mut failures = 0;
    for cmd in &plan.activate {
        let (program, rest) = cmd.split_first().expect("a command has a program");
        match std::process::Command::new(program).args(rest).output() {
            Ok(out) if out.status.success() => println!("ran {}", cmd.join(" ")),
            Ok(out) => {
                failures += 1;
                eprintln!(
                    "{} {} exited {}: {}",
                    style.yellow("warning:"),
                    cmd.join(" "),
                    out.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Err(e) => {
                failures += 1;
                eprintln!(
                    "{} could not run {}: {e}",
                    style.yellow("warning:"),
                    cmd.join(" ")
                );
            }
        }
    }

    println!();
    if failures == 0 {
        println!("{}", style.green("Scheduler installed."));
    } else {
        println!(
            "{}",
            style.yellow("The unit files are in place, but activation did not complete.")
        );
        println!("Activate it by hand with:");
        for cmd in &plan.activate {
            println!("  {}", cmd.join(" "));
        }
    }
    println!(
        "{}",
        style.dim("Check that it actually fires before trusting it; docs/scheduling.md says how.")
    );

    Ok(if failures == 0 {
        exit::SUCCESS
    } else {
        exit::PARTIAL
    })
}

pub fn uninstall(ctx: &Context, args: &UninstallArgs) -> Result<i32> {
    let style = Style::detect(ctx.args.no_color, ctx.args.json);
    let kind = resolve_kind(args.systemd, args.launchd)?;
    let plan = build_plan(ctx, kind)?;

    // Deactivate before removing the file, so the scheduler is not left holding
    // a reference to something that no longer exists.
    for cmd in &plan.deactivate {
        let (program, rest) = cmd.split_first().expect("a command has a program");
        match std::process::Command::new(program).args(rest).output() {
            Ok(out) if out.status.success() => println!("ran {}", cmd.join(" ")),
            // A unit that was never loaded is not an error worth reporting as
            // one; uninstall has to be safe to run twice.
            Ok(_) | Err(_) => {}
        }
    }

    let mut removed = 0;
    for unit in &plan.units {
        match std::fs::remove_file(&unit.path) {
            Ok(()) => {
                removed += 1;
                println!("removed {}", unit.path.display());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!(
                "{} could not remove {}: {e}",
                style.yellow("warning:"),
                unit.path.display()
            ),
        }
    }

    println!();
    if removed == 0 {
        println!("No {} unit was installed.", plan.kind.as_str());
    } else {
        println!("{}", style.green("Scheduler removed."));
    }

    // Say what survived, by name. Someone uninstalling wants to know what is
    // still on their disk, and nothing here should be a surprise later.
    println!();
    println!("{}", style.bold("Left in place:"));
    println!("  {}  (your configuration)", ctx.config.source.display());
    println!(
        "  {}  (quarantine; safe to delete)",
        ctx.config.quarantine.display()
    );
    println!(
        "  {}  (heartbeats, written by whatever supervises your jobs)",
        ctx.config.heartbeat_dir.display()
    );
    if plan.kind == Kind::Launchd {
        if let Some(h) = home() {
            println!("  {}  (logs)", h.join("Library/Logs/reap").display());
        }
    }
    println!("  the reap binary itself");
    println!();
    println!(
        "{}",
        style.dim("Nothing you configured or reclaimed was touched.")
    );

    Ok(exit::SUCCESS)
}
