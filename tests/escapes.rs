//! Deliberate attempts to make the planner or the guard stack step outside the
//! region it is allowed to touch.
//!
//! Each case is something that has broken a real cleanup script somewhere.

#[path = "fixtures.rs"]
mod fixtures;

use std::time::Duration;

use fixtures::{FakeInspector, Fixture, World};
use reap_core::guard::{evaluate, names};
use reap_core::plan::{plan, PlanOptions};

const ESCAPE_CONFIG: &str = r#"
[global]
quarantine = "{base}/quarantine"
heartbeat_dir = "{base}/heartbeats"

[[root]]
path = "{base}/code"
max_depth = 8

[[rule]]
name = "build-dir"
kind = "path"
tier = 0
dir_name = "build"
min_idle = "0s"
"#;

/// A symlink inside a workspace pointing at the home directory.
///
/// This is the classic way a naive cleaner destroys somebody's documents: it
/// finds a directory whose name matches, follows the link, and removes what is
/// on the other side.
#[test]
fn a_symlink_pointing_at_home_is_never_followed() {
    let f = Fixture::new();
    // `home` stands in for $HOME; the fixture's own base is the home directory
    // as far as the config is concerned.
    f.file("home/Documents/thesis.tex", 4096);
    f.dir("code/workspace");
    f.symlink(&f.path("home"), "code/workspace/build");

    let config = f.config(ESCAPE_CONFIG);
    let p = plan(&config, reap_platform::os_name(), PlanOptions::default());

    assert!(
        p.candidates.is_empty(),
        "a symlink to the home directory was proposed: {:?}",
        p.candidates.iter().map(|c| &c.path).collect::<Vec<_>>()
    );
    assert!(f.path("home/Documents/thesis.tex").exists());
}

/// A glob that tries to climb out of its root with `..`.
#[test]
fn a_glob_containing_a_parent_component_is_refused_at_load_time() {
    let f = Fixture::new();
    let err = f
        .try_config(
            r#"
[[root]]
path = "{base}/code"

[[rule]]
name = "climb"
kind = "path"
tier = 0
glob = "{base}/code/../home/*"
min_idle = "0s"
"#,
        )
        .expect_err("a glob with `..` was accepted");
    assert!(err.to_string().contains("`..`"), "{err}");
}

/// A root that is itself a symlink.
///
/// Allowing this would mean the path the walker descends and the path the
/// containment check compares against are two different places.
#[test]
fn a_symlinked_root_is_refused_rather_than_resolved() {
    let f = Fixture::new();
    f.dir("real/proj/build");
    f.symlink(&f.path("real"), "code");

    let config = f.config(ESCAPE_CONFIG);
    let p = plan(&config, reap_platform::os_name(), PlanOptions::default());

    assert!(p.candidates.is_empty());
    assert_eq!(p.skipped_roots.len(), 1);
    assert!(p.skipped_roots[0].reason.contains("symlink"));
}

/// A candidate that is the current working directory of a running process,
/// including this one.
#[test]
fn the_process_own_working_directory_is_protected() {
    let f = Fixture::new();
    f.file("code/proj/build/out.o", 4096);
    let config = f.config(ESCAPE_CONFIG);

    let candidate_path = f.path("code/proj/build");
    let w = World::new(config).with_inspector(FakeInspector::with_cwd(&candidate_path));
    let c = f.candidate(&w.config, "code/proj/build");

    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::PROCESS_LIVENESS));
}

/// A directory named to look like a rule's target but sitting one level above
/// the root.
#[test]
fn a_sibling_of_the_root_is_not_reachable() {
    let f = Fixture::new();
    f.dir("code");
    f.file("codex/proj/build/out.o", 4096);

    let config = f.config(ESCAPE_CONFIG);
    let p = plan(&config, reap_platform::os_name(), PlanOptions::default());
    assert!(
        p.candidates.is_empty(),
        "a path sharing a prefix with the root was reached"
    );
}

/// A rule whose `dir_name` would match the root itself.
#[test]
fn a_rule_cannot_select_its_own_root() {
    let f = Fixture::new();
    f.dir("code/build");
    let config =
        f.config(&ESCAPE_CONFIG.replace("path = \"{base}/code\"", "path = \"{base}/code/build\""));
    let p = plan(&config, reap_platform::os_name(), PlanOptions::default());
    assert!(p.candidates.is_empty(), "the root proposed itself");
}

/// A heartbeat naming a path that contains the candidate, rather than one
/// inside it.
#[test]
fn a_heartbeat_naming_an_ancestor_protects_the_candidate() {
    let f = Fixture::new();
    f.file("code/session-7/build/out.o", 4096);
    f.heartbeat(
        "heartbeats",
        "session-7",
        &f.path("code/session-7"),
        Duration::from_secs(1),
        0,
    );

    let config = f.config(ESCAPE_CONFIG);
    let w = World::new(config);
    let c = f.candidate(&w.config, "code/session-7/build");
    assert_eq!(evaluate(&c, &w.ctx()).rejected_by, Some(names::HEARTBEAT));
}

/// A `.git` directory that a rule happens to name.
///
/// The deny floor refuses anything with a `.git` component, wherever it appears,
/// because no build regenerates history.
#[test]
fn a_rule_naming_a_git_internal_is_refused_by_the_floor() {
    let f = Fixture::new();
    f.file("code/repo/.git/build/x", 128);

    let config = f.config(ESCAPE_CONFIG);
    let w = World::new(config);
    let c = f.candidate(&w.config, "code/repo/.git/build");
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::DENY_FLOOR));
    assert!(d.reason.as_deref().unwrap().contains(".git"));
}
