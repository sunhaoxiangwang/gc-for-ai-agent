//! One test per guard, in both directions, including the paths where the guard
//! cannot get an answer.
//!
//! The comment above each test names the invariant from the safety model that
//! it covers.

#[path = "fixtures.rs"]
mod fixtures;

use std::path::{Path, PathBuf};
use std::time::Duration;

use fixtures::{FakeGit, FakeInspector, Fixture, World};
use reap_core::guard::{evaluate, names, Verdict};
use reap_platform::IgnoreStatus;

/// A config whose single rule matches `build/` inside a git repository, with no
/// idle requirement, so each test can isolate the guard it cares about.
const GUARD_CONFIG: &str = r#"
[global]
quarantine = "{base}/quarantine"
heartbeat_dir = "{base}/heartbeats"
pin_marker = ".reap-keep"

[[root]]
path = "{base}/code"
max_depth = 6

[[rule]]
name = "build-dir"
kind = "path"
tier = 0
dir_name = "build"
min_idle = "0s"
"#;

/// Builds a fixture with `code/repo` as a git repository ignoring `build/`, and
/// a `build` directory inside it that every guard should pass.
fn passing_world() -> (Fixture, World) {
    let f = Fixture::new();
    f.git_repo("code/repo", "build/\n");
    f.file("code/repo/build/out.o", 4096);
    let config = f.config(GUARD_CONFIG);
    let world = World::new(config);
    (f, world)
}

fn verdict<'a>(decision: &'a reap_core::guard::Decision, guard: &str) -> &'a Verdict {
    &decision
        .results
        .iter()
        .find(|r| r.guard == guard)
        .unwrap_or_else(|| panic!("guard {guard} did not appear in the decision"))
        .verdict
}

#[test]
fn the_baseline_world_passes_every_guard() {
    // If this fails, every rejection test below is passing for the wrong reason.
    let (f, w) = passing_world();
    let c = f.candidate(&w.config, "code/repo/build");
    let d = evaluate(&c, &w.ctx());
    assert!(
        d.selected,
        "baseline was rejected by {:?}: {:?}",
        d.rejected_by, d.reason
    );
    assert_eq!(d.canonical, Some(f.path("code/repo/build")));
}

// ---------------------------------------------------------------------------
// Guard 1: deny floor. Invariant 8.
// ---------------------------------------------------------------------------

/// Invariant 8: a hardcoded deny floor wins over all configuration.
#[test]
fn invariant_8_the_deny_floor_refuses_shallow_and_critical_paths() {
    let home = PathBuf::from("/home/someone");
    let cases = [
        "/",
        "/usr",
        "/var",
        "/Users",
        "/home/someone",
        "/home/someone/Documents",
        "/home/someone/.ssh",
        // Fewer than three components below the root.
        "/mnt/scratch",
        "/a",
    ];
    for case in cases {
        assert!(
            guard::deny_floor(Path::new(case), Some(&home)).rejected(),
            "{case} was not refused by the deny floor"
        );
    }

    use reap_core::guard;
    // Three components or more, outside the forbidden set, is not refused here.
    // Containment is what decides whether it is actually in scope.
    assert!(!guard::deny_floor(Path::new("/mnt/scratch/build"), Some(&home)).rejected());
}

/// Invariant 8: the floor also refuses anything reached through a directory
/// whose loss is unrecoverable, wherever it appears in the path.
#[test]
fn invariant_8_the_deny_floor_refuses_repository_internals_and_credentials() {
    let home = PathBuf::from("/home/someone");
    for case in [
        "/home/someone/code/proj/.git/objects",
        "/home/someone/code/.ssh/keys",
        "/srv/data/.gnupg/private",
        "/opt/x/.aws/cli",
    ] {
        assert!(
            reap_core::guard::deny_floor(Path::new(case), Some(&home)).rejected(),
            "{case} was not refused"
        );
    }
}

/// Invariant 8: a user's deny list adds to the floor and is honoured.
#[test]
fn invariant_8_a_configured_deny_path_is_honoured() {
    let f = Fixture::new();
    f.git_repo("code/repo", "build/\n");
    f.file("code/repo/build/out.o", 4096);

    let config = f.config(&GUARD_CONFIG.replace(
        "pin_marker = \".reap-keep\"",
        "pin_marker = \".reap-keep\"\ndeny = [\"{base}/code/repo\"]",
    ));
    let w = World::new(config);
    let c = f.candidate(&w.config, "code/repo/build");
    let d = evaluate(&c, &w.ctx());

    assert_eq!(d.rejected_by, Some(names::DENY_FLOOR));
    assert!(d
        .reason
        .as_deref()
        .unwrap()
        .contains("configured deny path"));
}

// ---------------------------------------------------------------------------
// Guard 2: containment. Invariant 2.
// ---------------------------------------------------------------------------

/// Invariant 2: the canonical result must remain a strict descendant of a
/// declared root.
#[test]
fn invariant_2_a_symlink_below_the_root_is_refused() {
    let f = Fixture::new();
    f.git_repo("code/repo", "build\n");
    // `build` is a symlink pointing at a tree outside the root entirely.
    f.file("elsewhere/real/out.o", 4096);
    f.symlink(&f.path("elsewhere/real"), "code/repo/build");

    let config = f.config(GUARD_CONFIG);
    let w = World::new(config);

    // The planner refuses to propose a symlink at all, which is the first line
    // of defence.
    let plan = reap_core::plan::plan(
        &w.config,
        reap_platform::os_name(),
        reap_core::plan::PlanOptions::default(),
    );
    assert!(plan.candidates.is_empty(), "the planner proposed a symlink");

    // And if one reached the guard anyway, containment refuses it.
    let c = reap_core::plan::Candidate {
        path: f.path("code/repo/build"),
        root: f.path("code"),
        rule: "build-dir".to_owned(),
        tier: reap_core::config::Tier::new(0).unwrap(),
        depth: 2,
        depth_cap: 6,
        bytes: 0,
        idle: Duration::from_secs(9999),
        min_idle: Duration::ZERO,
        orphaned_only: false,
    };
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::CONTAINMENT));
    assert!(d.reason.as_deref().unwrap().contains("symlink"));
    assert_eq!(
        d.canonical, None,
        "no canonical path is offered on rejection"
    );
}

/// Invariant 2: a candidate that resolves outside every root is refused.
#[test]
fn invariant_2_a_path_outside_every_root_is_refused() {
    let f = Fixture::new();
    f.dir("code");
    f.git_repo("elsewhere/repo", "build/\n");
    f.file("elsewhere/repo/build/out.o", 4096);

    let config = f.config(GUARD_CONFIG);
    let w = World::new(config);
    let c = reap_core::plan::Candidate {
        path: f.path("elsewhere/repo/build"),
        root: f.path("code"),
        rule: "build-dir".to_owned(),
        tier: reap_core::config::Tier::new(0).unwrap(),
        depth: 2,
        depth_cap: 6,
        bytes: 0,
        idle: Duration::from_secs(9999),
        min_idle: Duration::ZERO,
        orphaned_only: false,
    };
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::CONTAINMENT));
}

/// Invariant 2: a root can never remove itself.
#[test]
fn invariant_2_a_root_is_not_a_strict_descendant_of_itself() {
    let f = Fixture::new();
    f.git_repo("code", "*\n");
    let config = f.config(GUARD_CONFIG);
    let w = World::new(config);

    let c = reap_core::plan::Candidate {
        path: f.path("code"),
        root: f.path("code"),
        rule: "build-dir".to_owned(),
        tier: reap_core::config::Tier::new(0).unwrap(),
        depth: 0,
        depth_cap: 6,
        bytes: 0,
        idle: Duration::from_secs(9999),
        min_idle: Duration::ZERO,
        orphaned_only: false,
    };
    let d = evaluate(&c, &w.ctx());
    assert!(!d.selected);
}

/// Invariant 2: a broken symlink cannot be canonicalized, and that is a
/// rejection rather than an error.
#[test]
fn invariant_2_a_path_that_cannot_be_canonicalized_is_refused() {
    let f = Fixture::new();
    f.dir("code");
    let config = f.config(GUARD_CONFIG);
    let w = World::new(config);

    let c = reap_core::plan::Candidate {
        path: f.path("code/gone/build"),
        root: f.path("code"),
        rule: "build-dir".to_owned(),
        tier: reap_core::config::Tier::new(0).unwrap(),
        depth: 2,
        depth_cap: 6,
        bytes: 0,
        idle: Duration::from_secs(9999),
        min_idle: Duration::ZERO,
        orphaned_only: false,
    };
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::CONTAINMENT));
    assert!(d.reason.as_deref().unwrap().contains("canonicalize"));
}

// ---------------------------------------------------------------------------
// Guard 3: depth
// ---------------------------------------------------------------------------

#[test]
fn the_depth_guard_rechecks_a_candidate_it_did_not_build() {
    let (f, w) = passing_world();
    let mut c = f.candidate(&w.config, "code/repo/build");
    c.depth_cap = 1;
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::DEPTH));
}

// ---------------------------------------------------------------------------
// Guard 4: the pin marker
// ---------------------------------------------------------------------------

/// The pin marker protects the directory it sits in and everything below it.
#[test]
fn a_pin_marker_in_the_candidate_protects_it() {
    let (f, w) = passing_world();
    f.pin("code/repo/build", ".reap-keep");
    let c = f.candidate(&w.config, "code/repo/build");
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::PIN_MARKER));
}

#[test]
fn a_pin_marker_in_an_ancestor_protects_everything_below_it() {
    let (f, w) = passing_world();
    f.pin("code/repo", ".reap-keep");
    let c = f.candidate(&w.config, "code/repo/build");
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::PIN_MARKER));
    assert!(d.reason.as_deref().unwrap().contains(".reap-keep"));
}

#[test]
fn a_pin_marker_outside_the_root_does_not_reach_inside_it() {
    let (f, w) = passing_world();
    // A marker above the root protects nothing: the search stops at the root.
    std::fs::write(f.path(".reap-keep"), b"").unwrap();
    let c = f.candidate(&w.config, "code/repo/build");
    let d = evaluate(&c, &w.ctx());
    assert!(d.selected, "the pin search walked past the root");
}

// ---------------------------------------------------------------------------
// Guard 5: heartbeats. Invariant 6.
// ---------------------------------------------------------------------------

/// Invariant 6: never delete an active workspace.
#[test]
fn invariant_6_a_live_heartbeat_protects_the_workspace_it_names() {
    let (f, w) = passing_world();
    f.heartbeat(
        "heartbeats",
        "job-1",
        &f.path("code/repo"),
        Duration::from_secs(5),
        0,
    );
    let w = World::new(w.config);
    let c = f.candidate(&w.config, "code/repo/build");
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::HEARTBEAT));
    assert!(d.reason.as_deref().unwrap().contains("job-1"));
}

/// Invariant 6: a heartbeat that has stopped being written no longer protects.
#[test]
fn invariant_6_a_stale_heartbeat_stops_protecting() {
    let (f, w) = passing_world();
    // Interval 60s, multiplier 3, so anything past 180s is stale.
    f.heartbeat(
        "heartbeats",
        "job-1",
        &f.path("code/repo"),
        Duration::from_secs(600),
        0,
    );
    let w = World::new(w.config);
    let c = f.candidate(&w.config, "code/repo/build");
    assert!(evaluate(&c, &w.ctx()).selected);
}

/// Invariant 6: liveness is not mtime guessing. A stale heartbeat whose process
/// is still running still protects.
#[test]
fn invariant_6_a_running_process_outvotes_a_stale_heartbeat() {
    let (f, w) = passing_world();
    f.heartbeat(
        "heartbeats",
        "job-1",
        &f.path("code/repo"),
        Duration::from_secs(600),
        4242,
    );
    let w = World::new(w.config).with_live_pids();
    let c = f.candidate(&w.config, "code/repo/build");
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::HEARTBEAT));
}

/// A heartbeat we cannot parse is still a claim of ownership.
#[test]
fn an_unreadable_heartbeat_protects_the_directory_it_is_named_for() {
    let f = Fixture::new();
    f.git_repo("code/repo", "build/\n");
    f.file("code/repo/build/out.o", 4096);
    f.dir("heartbeats");
    std::fs::write(f.path("heartbeats/build.json"), b"{ this is not json").unwrap();

    let config = f.config(GUARD_CONFIG);
    let w = World::new(config);
    let c = f.candidate(&w.config, "code/repo/build");
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::HEARTBEAT));
    assert!(d.reason.as_deref().unwrap().contains("could not be read"));
}

/// `orphaned_only` demands positive evidence: a heartbeat directory it cannot
/// read is a rejection, not a pass.
#[test]
fn orphaned_only_refuses_when_the_heartbeat_directory_is_unreadable() {
    let f = Fixture::new();
    f.git_repo("code/repo", "build/\n");
    f.file("code/repo/build/out.o", 4096);
    // Note: no heartbeats directory is created at all.

    let config = f.config(&GUARD_CONFIG.replace(
        "min_idle = \"0s\"",
        "min_idle = \"0s\"\norphaned_only = true",
    ));
    let w = World::new(config);
    let c = f.candidate(&w.config, "code/repo/build");
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::HEARTBEAT));
    assert!(d.reason.as_deref().unwrap().contains("positive evidence"));

    // With a readable directory and nothing claiming the path, it passes.
    f.dir("heartbeats");
    let w = World::new(w.config);
    assert!(evaluate(&c, &w.ctx()).selected);
}

// ---------------------------------------------------------------------------
// Guard 6: git ignore. Invariant 5.
// ---------------------------------------------------------------------------

/// Invariant 5: never delete anything git does not consider ignored.
#[test]
fn invariant_5_a_tracked_directory_is_refused() {
    let f = Fixture::new();
    // The repository ignores nothing, so `build` is work git would track.
    f.git_repo("code/repo", "# nothing ignored\n");
    f.file("code/repo/build/out.o", 4096);

    let config = f.config(GUARD_CONFIG);
    let w = World::new(config);
    let c = f.candidate(&w.config, "code/repo/build");
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::GIT_IGNORE));
    assert!(d.reason.as_deref().unwrap().contains("does not ignore"));
}

/// Invariant 5: if git is missing or errors, reject.
#[test]
fn invariant_5_an_inconclusive_answer_from_git_is_a_rejection() {
    let (f, w) = passing_world();
    let w = World::new(w.config).with_git(FakeGit(IgnoreStatus::Unknown(
        "git: command not found".to_owned(),
    )));
    let c = f.candidate(&w.config, "code/repo/build");
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::GIT_IGNORE));
    assert!(d.reason.as_deref().unwrap().contains("command not found"));
}

/// Outside a work tree the guard has nothing to say, and says so rather than
/// silently passing.
#[test]
fn outside_a_work_tree_the_git_guard_is_skipped() {
    let f = Fixture::new();
    f.file("code/loose/build/out.o", 4096);
    let config = f.config(GUARD_CONFIG);
    let w = World::new(config);
    let c = f.candidate(&w.config, "code/loose/build");
    let d = evaluate(&c, &w.ctx());

    assert!(matches!(
        verdict(&d, names::GIT_IGNORE),
        Verdict::Skipped(_)
    ));
    assert!(d.selected);
}

/// A `.git` file rather than a directory (a linked worktree or a submodule) is
/// still a work tree.
#[test]
fn a_git_file_marks_a_work_tree_too() {
    let f = Fixture::new();
    f.dir("code/linked/build");
    std::fs::write(
        f.path("code/linked/.git"),
        b"gitdir: /elsewhere/.git/worktrees/x",
    )
    .unwrap();
    assert_eq!(
        reap_core::guard::find_worktree(&f.path("code/linked/build")),
        Some(f.path("code/linked"))
    );
}

// ---------------------------------------------------------------------------
// Guard 7: idle age
// ---------------------------------------------------------------------------

#[test]
fn a_directory_younger_than_the_rule_requires_is_refused() {
    let f = Fixture::new();
    f.git_repo("code/repo", "build/\n");
    f.file("code/repo/build/out.o", 4096);

    let config = f.config(&GUARD_CONFIG.replace("min_idle = \"0s\"", "min_idle = \"48h\""));
    let w = World::new(config);
    let c = f.candidate(&w.config, "code/repo/build");
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::IDLE_AGE));

    // Age it past the threshold and the same candidate passes.
    f.age_tree("code/repo/build", Duration::from_secs(72 * 3600));
    let c = f.candidate(&w.config, "code/repo/build");
    assert!(evaluate(&c, &w.ctx()).selected);
}

// ---------------------------------------------------------------------------
// Guard 8: process liveness
// ---------------------------------------------------------------------------

#[test]
fn a_process_working_inside_the_candidate_protects_it() {
    let (f, w) = passing_world();
    let inside = f.path("code/repo/build/deep");
    let w = World::new(w.config).with_inspector(FakeInspector::with_cwd(&inside));
    let c = f.candidate(&w.config, "code/repo/build");
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::PROCESS_LIVENESS));
    assert!(d.reason.as_deref().unwrap().contains("working directory"));
}

#[test]
fn an_open_file_inside_the_candidate_protects_it() {
    let (f, w) = passing_world();
    let inside = f.path("code/repo/build/out.o");
    let w = World::new(w.config).with_inspector(FakeInspector::with_open(&inside));
    let c = f.candidate(&w.config, "code/repo/build");
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::PROCESS_LIVENESS));
    assert!(d.reason.as_deref().unwrap().contains("holds"));
}

/// Not being able to inspect is not the same as nothing running.
#[test]
fn a_failed_process_inspection_is_a_rejection() {
    let (f, w) = passing_world();
    let w = World::new(w.config).with_inspector(FakeInspector::failing("permission denied"));
    let c = f.candidate(&w.config, "code/repo/build");
    let d = evaluate(&c, &w.ctx());
    assert_eq!(d.rejected_by, Some(names::PROCESS_LIVENESS));
}

/// A sibling directory whose name shares a prefix must not be mistaken for
/// something inside the candidate.
#[test]
fn a_process_in_a_prefix_sharing_sibling_does_not_protect() {
    let (f, w) = passing_world();
    let sibling = f.path("code/repo/build-tools");
    let w = World::new(w.config).with_inspector(FakeInspector::with_cwd(&sibling));
    let c = f.candidate(&w.config, "code/repo/build");
    assert!(evaluate(&c, &w.ctx()).selected);
}

// ---------------------------------------------------------------------------
// The stack as a whole
// ---------------------------------------------------------------------------

/// Evaluation stops at the first rejection, and the rest of the stack is
/// reported as not reached rather than quietly omitted.
#[test]
fn every_guard_appears_in_the_decision_in_order() {
    let (f, w) = passing_world();
    f.pin("code/repo/build", ".reap-keep");
    let c = f.candidate(&w.config, "code/repo/build");
    let d = evaluate(&c, &w.ctx());

    let order: Vec<&str> = d.results.iter().map(|r| r.guard).collect();
    assert_eq!(order, names::ORDER.to_vec());

    assert!(matches!(verdict(&d, names::PIN_MARKER), Verdict::Reject(_)));
    for later in [
        names::HEARTBEAT,
        names::GIT_IGNORE,
        names::IDLE_AGE,
        names::PROCESS_LIVENESS,
    ] {
        assert!(
            matches!(verdict(&d, later), Verdict::NotReached),
            "{later} ran after a rejection"
        );
    }
}
