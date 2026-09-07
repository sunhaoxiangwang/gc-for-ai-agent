//! `reap init`: write a starter configuration for this machine.
//!
//! This is most of the onboarding experience, so it does real work rather than
//! copying a template: it looks for the toolchains that are actually installed
//! and the directories where code actually lives, and emits only the rules that
//! can match something. A config full of rules for tools you do not have is a
//! config nobody reads.
//!
//! What it writes is always `dry_run = true`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::catalog::{self, Stack};
use crate::cli::InitArgs;
use crate::context::{home, load_context};
use crate::exit;
use crate::output::{tilde, Style};

/// Directory names people keep code in, in the order we would rather find them.
const CONVENTIONAL_CODE_DIRS: &[&str] = &[
    "code",
    "src",
    "dev",
    "Developer",
    "projects",
    "Projects",
    "repos",
    "work",
    "git",
    "workspace",
];

/// Parts of a home directory that are never code, and are skipped when looking
/// for one.
///
/// `Library` in particular is enormous and mostly protected by TCC, so walking
/// it to look for a `Cargo.toml` would be slow and would produce a permission
/// error rather than an answer. `Documents` is deliberately *not* here: on
/// macOS it is a common place to keep code.
const NEVER_SCAN: &[&str] = &[
    "Library",
    "Applications",
    "Movies",
    "Music",
    "Pictures",
    "Photos",
    "Public",
    "Desktop",
    "Downloads",
];

/// Files that mark a directory as a project.
const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "build.gradle",
    "build.gradle.kts",
    "Package.swift",
];

/// How far below `$HOME` the search for a code directory goes.
///
/// Three levels covers `~/Documents/Developers/personal` without turning the
/// search into a walk of the whole home directory.
const SEARCH_DEPTH: usize = 3;

/// Finds the directories worth declaring as roots.
///
/// Conventional names first, since a machine with `~/code` has answered the
/// question. Otherwise look for directories that *directly contain* projects
/// and declare those, rather than declaring a broad ancestor: naming
/// `~/Documents/Developers/personal` is precise, and naming `~/Documents`
/// because there is a repository three levels down is not.
pub fn detect_roots(home: &Path) -> Vec<PathBuf> {
    let conventional: Vec<PathBuf> = CONVENTIONAL_CODE_DIRS
        .iter()
        .map(|d| home.join(d))
        .filter(|d| d.is_dir())
        .collect();
    if !conventional.is_empty() {
        return conventional;
    }

    let mut found: Vec<PathBuf> = Vec::new();
    let mut queue: Vec<(PathBuf, usize)> = vec![(home.to_path_buf(), 0)];

    while let Some((dir, depth)) = queue.pop() {
        if depth > SEARCH_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            // A directory we cannot read tells us nothing; on macOS this is
            // usually TCC, which `doctor` reports properly later.
            continue;
        };
        let mut children = Vec::new();
        let mut holds_project = false;
        for entry in entries.flatten().take(500) {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if !path.is_dir() || path.is_symlink() {
                continue;
            }
            if PROJECT_MARKERS.iter().any(|m| path.join(m).exists()) {
                holds_project = true;
            }
            if name.starts_with('.') || (depth == 0 && NEVER_SCAN.contains(&name.as_str())) {
                continue;
            }
            children.push(path);
        }
        if holds_project && dir != home {
            found.push(dir);
            // No need to look inside a directory we have already claimed.
            continue;
        }
        for child in children {
            queue.push((child, depth + 1));
        }
    }

    // Drop anything already covered by a shallower root.
    found.sort();
    let mut roots: Vec<PathBuf> = Vec::new();
    for candidate in found {
        if !roots.iter().any(|r| candidate.starts_with(r)) {
            roots.push(candidate);
        }
    }
    roots
}

/// Renders the configuration file.
///
/// Pure, so the generator is testable and `--dry-run` can show exactly what
/// would be written.
pub fn render(found: &[catalog::Found], roots: &[PathBuf], home: &Path, hostname: &str) -> String {
    let stacks: BTreeSet<&str> = found.iter().map(|f| f.stack.label()).collect();
    let present: Vec<Stack> = found.iter().map(|f| f.stack).collect();
    let tilde = |p: &Path| -> String {
        match p.strip_prefix(home) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => p.display().to_string(),
        }
    };

    let mut out = String::new();
    out.push_str(&format!(
        "# reap configuration, generated by `reap init --detect` on {hostname}.\n\
         #\n\
         # Detected: {}.\n\
         #\n\
         # Nothing here deletes anything yet: dry_run is true, and removal also\n\
         # needs --apply on the command line. Read this file, run `reap report`\n\
         # for a few days, and set dry_run = false when the list stops surprising\n\
         # you.\n\
         #\n\
         # Full reference: docs/configuration.md\n\n",
        stacks.into_iter().collect::<Vec<_>>().join(", ")
    ));

    out.push_str(
        "[global]\n\
         # Both of these must change for anything to be removed: this, and\n\
         # --apply on the command line.\n\
         dry_run = true\n\n\
         # Staging area for removal. Must be on the same filesystem as every\n\
         # root below; `reap doctor` proves it with a real test rename.\n",
    );
    out.push_str("quarantine = \"~/.cache/reap/quarantine\"\n\n");
    out.push_str(
        "# Free-space thresholds, used by `sweep --until-free`.\n\
         low_water_pct = 15\n\
         target_free_pct = 25\n\n",
    );
    if cfg!(target_os = "macos") {
        out.push_str(
            "# APFS reports space held by Time Machine local snapshots as used,\n\
             # even though the kernel releases it on demand. This allowance stops\n\
             # reap escalating to tier 2 over space the machine already has.\n\
             apfs_snapshot_slack_gb = 20\n\n",
        );
    }
    out.push_str(
        "# Stop starting new work after this long. A reclaimer that pegs a\n\
         # laptop's disk for ten minutes is a bug.\n\
         max_run_seconds = 300\n\n\
         # Create a file with this name in any directory to protect it and\n\
         # everything below it, permanently, with no config change.\n\
         pin_marker = \".reap-keep\"\n\n\
         # Where a supervisor writes one file per running job. reap refuses to\n\
         # touch anything a live heartbeat claims. See docs/configuration.md.\n\
         heartbeat_dir = \"~/.cache/reap/heartbeats\"\n\n\
         # Paths reap must never touch, on top of the hardcoded deny floor.\n\
         deny = []\n\n",
    );

    out.push_str(&format!(
        "# Per-machine overrides. This file can live in a dotfiles repository and\n\
         # be shared across hosts; a [machine.\"...\"] table overrides [global] on\n\
         # that host only. A root that does not exist on a given machine is\n\
         # skipped with a warning rather than being an error.\n\
         # \n\
         # [machine.\"{hostname}\"]\n\
         # dry_run = false\n\n"
    ));

    out.push_str(
        "# ---------------------------------------------------------------------\n\
         # Roots. Nothing outside these is ever looked at, let alone removed.\n\
         # ---------------------------------------------------------------------\n\n",
    );
    if roots.is_empty() {
        out.push_str(
            "# No code directory was found automatically. Point this at yours;\n\
             # until you do, reap will report that the root does not exist.\n\
             [[root]]\n\
             path = \"~/code\"\n\
             max_depth = 6\n\n",
        );
    }
    for root in roots {
        out.push_str(&format!(
            "[[root]]\npath = \"{}\"\nmax_depth = 6\n\n",
            tilde(root)
        ));
    }

    // Roots that particular rules need in order to match anything.
    let mut extra: Vec<(&str, usize)> = Vec::new();
    for entry in catalog::CATALOG {
        if !present.contains(&entry.stack) {
            continue;
        }
        if let Some(r) = entry.extra_root {
            if !extra.iter().any(|(p, _)| *p == r) {
                extra.push((r, entry.extra_root_depth));
            }
        }
    }
    if !extra.is_empty() {
        out.push_str("# Tool cache directories the rules below need to reach.\n");
        for (path, depth) in extra {
            out.push_str(&format!(
                "[[root]]\npath = \"{path}\"\nmax_depth = {depth}\n\n"
            ));
        }
    }

    out.push_str(
        "# ---------------------------------------------------------------------\n\
         # Rules. Nothing is removable unless a rule here names it.\n\
         #\n\
         # Tiers are rebuild cost, not confidence. Every tier gets the same\n\
         # guards. A plain `reap sweep` touches tier 0 only.\n\
         #   0  near-free to regenerate\n\
         #   1  regenerable, costs real time\n\
         #   2  shared caches whose loss slows every later build\n\
         # ---------------------------------------------------------------------\n",
    );

    let mut last_tier = None;
    for entry in catalog::CATALOG {
        if !present.contains(&entry.stack) {
            continue;
        }
        if !entry.os.is_empty() && !entry.os.contains(&reap_platform::os_name()) {
            continue;
        }
        if last_tier != Some(entry.tier) {
            out.push_str(&format!("\n# --- tier {} ---\n", entry.tier));
            last_tier = Some(entry.tier);
        }
        out.push_str(&format!(
            "\n# {} ({})\n[[rule]]\nname = \"{}\"\n{}",
            entry.comment,
            entry.stack.label(),
            entry.name,
            entry.body
        ));
    }

    out
}

pub fn run(args: &InitArgs, global: &crate::cli::GlobalArgs) -> Result<i32> {
    let style = Style::detect(global.no_color, global.json);
    let home = home().context("$HOME is unset, so there is no default place to write a config")?;
    let lc = load_context(global);

    // Honour the same resolution order the rest of the program uses, so `init`
    // writes where the next command will look.
    let target = global.config.clone().unwrap_or_else(|| {
        reap_core::config::search_path(None, lc.env_config.as_deref(), Some(&home))
            .into_iter()
            .next()
            .unwrap_or_else(|| home.join(".config/reap/reap.toml"))
    });

    let found = if args.detect {
        catalog::detect(&home)
    } else {
        // Without --detect, write the full catalogue so the user can delete
        // what does not apply rather than go looking for what does.
        let mut all: Vec<catalog::Found> = Vec::new();
        for stack in [
            Stack::Generic,
            Stack::Rust,
            Stack::Node,
            Stack::Pnpm,
            Stack::Python,
            Stack::Uv,
            Stack::Xcode,
            Stack::Go,
            Stack::Gradle,
            Stack::Docker,
            Stack::Browsers,
        ] {
            all.push(catalog::Found {
                stack,
                evidence: "included without detection".to_owned(),
            });
        }
        all
    };
    let roots = detect_roots(&home);
    let text = render(&found, &roots, &home, &lc.hostname);

    if args.print {
        print!("{text}");
        return Ok(exit::SUCCESS);
    }

    if target.exists() && !args.force {
        anyhow::bail!(
            "{} already exists. Pass --force to overwrite it, or --print to see \
             what would be written.",
            target.display()
        );
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    std::fs::write(&target, &text)
        .with_context(|| format!("could not write {}", target.display()))?;

    println!("wrote {}", tilde(&target, Some(&home)));
    println!();
    if args.detect {
        println!("{}", style.bold("Detected on this machine"));
        for f in &found {
            println!("  {:<18} {}", f.stack.label(), style.dim(&f.evidence));
        }
        println!();
    }
    println!("{}", style.bold("Roots"));
    if roots.is_empty() {
        println!(
            "  {}",
            style.yellow("none found automatically; edit [[root]] in the file above")
        );
    }
    for root in &roots {
        println!("  {}", tilde(root, Some(&home)));
    }

    println!();
    println!("{}", style.bold("Next"));
    println!("  reap doctor      check that everything resolves");
    println!("  reap report      see what could be reclaimed. Deletes nothing.");
    println!();
    println!(
        "{}",
        style.dim("dry_run is true, so nothing will be removed until you change it.")
    );

    Ok(exit::SUCCESS)
}
