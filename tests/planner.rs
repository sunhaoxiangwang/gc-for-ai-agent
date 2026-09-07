//! Planner tests.
//!
//! Every assertion about what the planner produces is an assertion about the
//! exact set. A test that only checks "the path I expected is in there" passes
//! just as happily when the planner also proposed the user's source tree, which
//! is the bug that matters.

#[path = "fixtures.rs"]
mod fixtures;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use fixtures::{Fixture, BASIC_CONFIG};
use reap_core::config::Config;
use reap_core::plan::{plan, PlanOptions};

/// The exact set of candidate paths the planner emitted.
fn candidate_paths(config: &Config, os: &str, opts: PlanOptions) -> BTreeSet<PathBuf> {
    plan(config, os, opts)
        .candidates
        .into_iter()
        .map(|c| c.path)
        .collect()
}

fn expect(paths: &[PathBuf]) -> BTreeSet<PathBuf> {
    paths.iter().cloned().collect()
}

fn opts() -> PlanOptions {
    PlanOptions {
        now: SystemTime::now(),
        max_tier: 2,
        idle_probe_limit: 256,
    }
}

#[test]
fn emits_exactly_the_directories_its_rules_name() {
    let f = Fixture::new();
    f.touch("code/rust-proj/Cargo.toml");
    f.file("code/rust-proj/target/debug/bin", 4096);
    f.file("code/rust-proj/src/main.rs", 128);
    f.touch("code/node-proj/package.json");
    f.file("code/node-proj/node_modules/dep/index.js", 512);
    f.file("code/node-proj/src/app.js", 128);

    let config = f.config(BASIC_CONFIG);
    let got = candidate_paths(&config, "linux", opts());

    assert_eq!(
        got,
        expect(&[
            f.path("code/node-proj/node_modules"),
            f.path("code/rust-proj/target"),
        ]),
        "planner emitted a set other than the two build directories"
    );
}

#[test]
fn a_directory_named_target_without_its_sibling_is_not_a_candidate() {
    let f = Fixture::new();
    // A photographer's target practice folder, not a Rust build directory.
    f.file("code/photos/target/shot.raw", 4096);

    let config = f.config(BASIC_CONFIG);
    assert_eq!(
        candidate_paths(&config, "linux", opts()),
        BTreeSet::new(),
        "require_sibling did not exclude a same-named directory"
    );
}

#[test]
fn nothing_outside_a_declared_root_is_proposed() {
    let f = Fixture::new();
    // Same shape, but under `elsewhere` rather than the declared `code` root.
    f.touch("elsewhere/proj/Cargo.toml");
    f.file("elsewhere/proj/target/debug/bin", 4096);

    let config = f.config(BASIC_CONFIG);
    assert_eq!(
        candidate_paths(&config, "linux", opts()),
        BTreeSet::new(),
        "the planner reached outside its declared roots"
    );
}

#[test]
fn the_depth_cap_is_enforced() {
    let f = Fixture::new();
    f.touch("code/a/b/c/proj/Cargo.toml");
    f.file("code/a/b/c/proj/target/x", 128);

    // code/a=1 b=2 c=3 proj=4 target=5, so a cap of 4 must exclude it and a
    // cap of 5 must include it.
    let shallow = f.config(&BASIC_CONFIG.replace("max_depth = 6", "max_depth = 4"));
    assert_eq!(candidate_paths(&shallow, "linux", opts()), BTreeSet::new());

    let deep = f.config(&BASIC_CONFIG.replace("max_depth = 6", "max_depth = 5"));
    assert_eq!(
        candidate_paths(&deep, "linux", opts()),
        expect(&[f.path("code/a/b/c/proj/target")])
    );
}

#[test]
fn a_matched_directory_is_proposed_whole_and_not_descended_into() {
    let f = Fixture::new();
    f.touch("code/proj/Cargo.toml");
    // A nested Cargo workspace inside the build directory, which would produce
    // a second, redundant candidate if the walker kept descending.
    f.touch("code/proj/target/inner/Cargo.toml");
    f.file("code/proj/target/inner/target/x", 128);

    let config = f.config(BASIC_CONFIG);
    assert_eq!(
        candidate_paths(&config, "linux", opts()),
        expect(&[f.path("code/proj/target")]),
        "the walker descended into a directory it had already proposed"
    );
}

#[test]
fn symlinks_are_never_proposed_and_never_followed() {
    let f = Fixture::new();
    f.touch("code/proj/Cargo.toml");
    f.file("code/proj/target/x", 128);
    // A symlink named like a candidate, and a symlink that would smuggle the
    // walker out of the root entirely.
    f.touch("code/other/Cargo.toml");
    f.symlink(&f.path("code/proj/target"), "code/other/target");
    f.symlink(&f.path("elsewhere"), "code/escape");
    f.touch("elsewhere/proj/Cargo.toml");
    f.file("elsewhere/proj/target/x", 128);

    let config = f.config(BASIC_CONFIG);
    assert_eq!(
        candidate_paths(&config, "linux", opts()),
        expect(&[f.path("code/proj/target")]),
        "a symlink was proposed or followed"
    );
}

#[test]
fn tier_filtering_excludes_higher_tiers() {
    let f = Fixture::new();
    f.touch("code/rust/Cargo.toml");
    f.file("code/rust/target/x", 128);
    f.touch("code/node/package.json");
    f.file("code/node/node_modules/x", 128);

    let config = f.config(BASIC_CONFIG);

    let tier0 = PlanOptions {
        max_tier: 0,
        ..opts()
    };
    assert_eq!(
        candidate_paths(&config, "linux", tier0),
        expect(&[f.path("code/rust/target")]),
        "a tier 1 rule ran during a tier 0 pass"
    );
}

#[test]
fn os_filtering_excludes_rules_for_the_other_platform() {
    let f = Fixture::new();
    f.touch("code/proj/Cargo.toml");
    f.file("code/proj/target/x", 128);

    let config = f.config(&BASIC_CONFIG.replace(
        "name = \"cargo-target\"",
        "name = \"cargo-target\"\nos = [\"macos\"]",
    ));
    assert_eq!(candidate_paths(&config, "linux", opts()), BTreeSet::new());
    assert_eq!(
        candidate_paths(&config, "macos", opts()),
        expect(&[f.path("code/proj/target")])
    );
}

#[test]
fn require_ancestor_looks_up_the_tree_not_just_beside() {
    let f = Fixture::new();
    let config = f.config(
        r#"
[global]
quarantine = "{base}/quarantine"

[[root]]
path = "{base}/code"
max_depth = 8

[[rule]]
name = "venv"
kind = "path"
tier = 1
dir_name = ".venv"
require_ancestor = ".git"
min_idle = "0s"
"#,
    );

    // Inside a repository: the marker is two levels up, not beside it.
    f.dir("code/repo/.git");
    f.file("code/repo/service/.venv/lib/x", 128);
    // Outside any repository.
    f.file("code/loose/.venv/lib/x", 128);

    assert_eq!(
        candidate_paths(&config, "linux", opts()),
        expect(&[f.path("code/repo/service/.venv")]),
        "require_ancestor did not search ancestor directories"
    );
}

#[test]
fn a_missing_root_is_a_note_not_a_failure() {
    let f = Fixture::new();
    // `code` is declared but never created: the same config file on another
    // machine with a different layout must still run.
    let config = f.config(BASIC_CONFIG);
    let p = plan(&config, "linux", opts());

    assert!(p.candidates.is_empty());
    assert_eq!(p.skipped_roots.len(), 1);
    assert!(p.skipped_roots[0].reason.contains("cannot stat"));
}

#[test]
fn a_symlinked_root_is_refused() {
    let f = Fixture::new();
    f.dir("real");
    f.symlink(&f.path("real"), "code");
    f.touch("real/proj/Cargo.toml");
    f.file("real/proj/target/x", 128);

    let config = f.config(BASIC_CONFIG);
    let p = plan(&config, "linux", opts());

    assert!(p.candidates.is_empty(), "walked through a symlinked root");
    assert_eq!(p.skipped_roots.len(), 1);
    assert!(p.skipped_roots[0].reason.contains("symlink"));
}

#[test]
fn rules_that_matched_nothing_are_reported() {
    let f = Fixture::new();
    f.touch("code/proj/Cargo.toml");
    f.file("code/proj/target/x", 128);

    let config = f.config(BASIC_CONFIG);
    let p = plan(&config, "linux", opts());

    assert_eq!(p.unmatched_rules, vec!["node-modules".to_owned()]);
}

#[test]
fn idle_time_is_measured_from_the_newest_immediate_child() {
    let f = Fixture::new();
    f.touch("code/proj/Cargo.toml");
    f.file("code/proj/target/stale", 128);
    f.age_tree("code/proj/target", Duration::from_secs(48 * 3600));

    let config = f.config(BASIC_CONFIG);
    let p = plan(&config, "linux", opts());
    assert_eq!(p.candidates.len(), 1);
    assert!(
        p.candidates[0].idle >= Duration::from_secs(47 * 3600),
        "idle was {:?}",
        p.candidates[0].idle
    );

    // Writing one file at the top level makes the directory look busy again.
    f.file("code/proj/target/fresh", 8);
    let p = plan(&config, "linux", opts());
    assert!(
        p.candidates[0].idle < Duration::from_secs(60),
        "a fresh write did not reset the idle clock: {:?}",
        p.candidates[0].idle
    );
}

#[test]
fn command_rules_are_planned_without_a_path() {
    let f = Fixture::new();
    f.dir("code");
    let config = f.config(
        r#"
[global]
quarantine = "{base}/quarantine"

[[root]]
path = "{base}/code"

[[rule]]
name = "docker-build-cache"
kind = "command"
tier = 2
command = ["docker", "builder", "prune", "-f"]
"#,
    );
    let p = plan(&config, "linux", opts());
    assert!(p.candidates.is_empty());
    assert_eq!(p.commands.len(), 1);
    assert_eq!(p.commands[0].argv, vec!["docker", "builder", "prune", "-f"]);

    // ...and are excluded by a lower tier ceiling, like any other rule.
    let tier0 = PlanOptions {
        max_tier: 0,
        ..opts()
    };
    assert!(plan(&config, "linux", tier0).commands.is_empty());
}

#[test]
fn case_sensitivity_is_declared_not_inherited_from_the_filesystem() {
    let f = Fixture::new();
    f.touch("code/proj/Cargo.toml");
    f.file("code/proj/Target/x", 128);

    let sensitive = f.config(BASIC_CONFIG);
    assert_eq!(
        candidate_paths(&sensitive, "linux", opts()),
        BTreeSet::new(),
        "case-sensitive matching accepted a differently-cased name"
    );

    let insensitive = f.config(&BASIC_CONFIG.replace(
        "dry_run = true",
        "dry_run = true\ncase_sensitive_matching = false",
    ));
    assert_eq!(
        candidate_paths(&insensitive, "linux", opts()),
        expect(&[f.path("code/proj/Target")])
    );
}
