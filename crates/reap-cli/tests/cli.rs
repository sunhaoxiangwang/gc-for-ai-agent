//! End-to-end tests of the binary: exit codes, JSON schema stability, and the
//! two-opt-in gate.
//!
//! These run the real `reap` binary against a real temporary tree, so they
//! catch wiring mistakes that unit tests on the library cannot.

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use serde_json::Value;

/// Exit codes, mirrored from `src/exit.rs`. They are a public interface, so a
/// test asserts the numbers rather than importing them.
const SUCCESS: i32 = 0;
const CONFIG: i32 = 2;
const NOTHING_TO_DO: i32 = 3;
const PARTIAL: i32 = 4;

struct Tree {
    dir: tempfile::TempDir,
    base: PathBuf,
    config: PathBuf,
}

impl Tree {
    /// A repository with an ignored, aged `target/` directory that every guard
    /// should pass.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();

        let repo = base.join("code/proj");
        std::fs::create_dir_all(repo.join("target/debug")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), b"[package]\nname = \"x\"\n").unwrap();
        std::fs::write(repo.join(".gitignore"), b"/target/\n").unwrap();
        std::fs::write(repo.join("target/debug/blob"), vec![b'x'; 200_000]).unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());

        let config = base.join("reap.toml");
        std::fs::write(
            &config,
            format!(
                r#"
[global]
dry_run = true
quarantine = "{base}/quarantine"
heartbeat_dir = "{base}/heartbeats"

[[root]]
path = "{base}/code"
max_depth = 4

[[rule]]
name = "cargo-target"
kind = "path"
tier = 0
dir_name = "target"
require_sibling = "Cargo.toml"
min_idle = "0s"
"#,
                base = base.display()
            ),
        )
        .unwrap();

        Self { dir, base, config }
    }

    /// Rewrites the config with `dry_run = false`, the second of the two
    /// opt-ins. Tests that mutate call this explicitly, so a test that forgot
    /// to could not remove anything by accident.
    fn arm(&self) {
        let text = std::fs::read_to_string(&self.config).unwrap();
        std::fs::write(
            &self.config,
            text.replace("dry_run = true", "dry_run = false"),
        )
        .unwrap();
    }

    fn quarantine(&self) -> PathBuf {
        self.base.join("quarantine")
    }

    fn reap(&self) -> Command {
        let mut cmd = Command::cargo_bin("reap").unwrap();
        cmd.arg("--config").arg(&self.config);
        // Keep the test independent of whatever the developer has exported.
        cmd.env_remove("REAP_CONFIG");
        cmd.env("NO_COLOR", "1");
        cmd
    }

    fn target(&self) -> PathBuf {
        self.base.join("code/proj/target")
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.base.join(rel)
    }
}

fn json_of(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "output was not JSON: {e}\n--- stdout ---\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

#[test]
fn report_finds_the_build_directory_and_deletes_nothing() {
    let t = Tree::new();
    let out = t.reap().arg("report").output().unwrap();
    assert_eq!(out.status.code(), Some(SUCCESS));

    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("cargo-target"), "{text}");
    assert!(text.contains("across 1 directories"), "{text}");
    assert!(t.target().exists(), "report removed something");
}

#[test]
fn report_exits_3_when_nothing_matches() {
    let t = Tree::new();
    std::fs::remove_dir_all(t.target()).unwrap();
    let out = t.reap().arg("report").output().unwrap();
    assert_eq!(out.status.code(), Some(NOTHING_TO_DO));
}

#[test]
fn a_missing_config_is_exit_2_not_a_crash() {
    let out = Command::cargo_bin("reap")
        .unwrap()
        .args(["--config", "/nonexistent/reap.toml", "report"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(CONFIG));
    assert!(String::from_utf8_lossy(&out.stderr).contains("error:"));
}

#[test]
fn an_invalid_config_is_exit_2_and_names_the_problem() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("reap.toml");
    std::fs::write(&config, "[global]\ndry_runn = false\n").unwrap();

    let out = Command::cargo_bin("reap")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("report")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(CONFIG));
    assert!(String::from_utf8_lossy(&out.stderr).contains("dry_runn"));
}

/// The `--json` document is a public interface. This asserts the fields the
/// schema promises are present and typed as documented.
#[test]
fn the_json_schema_is_stable() {
    let t = Tree::new();
    let out = t.reap().args(["report", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(SUCCESS));
    let v = json_of(&out);

    assert_eq!(v["schema_version"], 1);
    for key in [
        "reap_version",
        "host",
        "os",
        "arch",
        "command",
        "config",
        "started_at",
    ] {
        assert!(v[key].is_string(), "{key} should be a string: {v}");
    }
    assert!(v["duration_ms"].is_u64());
    assert!(v["applied"].is_boolean());
    assert!(v["dry_run"].is_boolean());
    assert!(v["candidates"].is_array());
    assert!(v["commands"].is_array());
    assert!(v["notes"].is_array());
    assert!(v["errors"].is_array());

    let c = &v["candidates"][0];
    assert!(c["path"].is_string());
    assert!(c["rule"].is_string());
    assert!(c["tier"].is_u64());
    assert!(c["bytes"].is_u64());
    assert!(c["idle_seconds"].is_u64());
    assert!(c["selected"].is_boolean());
    assert!(c["rejected_by"].is_null());

    let totals = &v["totals"];
    for key in [
        "candidates",
        "selected",
        "rejected",
        "selected_bytes",
        "reclaimed_bytes",
        "commands_run",
        "errors",
    ] {
        assert!(totals[key].is_u64(), "totals.{key} missing: {totals}");
    }
    assert_eq!(
        v["started_at"].as_str().unwrap().len(),
        20,
        "RFC 3339, seconds, UTC"
    );
}

#[test]
fn explain_reports_every_guard_in_order() {
    let t = Tree::new();
    let out = t
        .reap()
        .arg("explain")
        .arg(t.target())
        .args(["--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(SUCCESS));

    let v = json_of(&out);
    assert_eq!(v["selected"], true);
    let guards: Vec<&str> = v["guards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["guard"].as_str().unwrap())
        .collect();
    assert_eq!(
        guards,
        vec![
            "deny-floor",
            "containment",
            "depth",
            "pin-marker",
            "heartbeat",
            "git-ignore",
            "idle-age",
            "process-liveness",
        ]
    );
}

#[test]
fn explain_exits_3_and_names_the_guard_that_held_a_path_back() {
    let t = Tree::new();
    std::fs::write(t.path("code/proj/target/.reap-keep"), b"").unwrap();

    let out = t
        .reap()
        .arg("explain")
        .arg(t.target())
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(NOTHING_TO_DO));

    let v = json_of(&out);
    assert_eq!(v["selected"], false);
    assert_eq!(v["rejected_by"], "pin-marker");
}

#[test]
fn explain_on_an_unrelated_path_says_no_rule_names_it() {
    let t = Tree::new();
    let out = t.reap().args(["explain", "/etc"]).output().unwrap();
    assert_eq!(out.status.code(), Some(NOTHING_TO_DO));
    assert!(String::from_utf8_lossy(&out.stdout).contains("no rule names this path"));
}

#[test]
fn doctor_passes_on_a_healthy_configuration() {
    let t = Tree::new();
    let out = t.reap().arg("doctor").output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(SUCCESS),
        "doctor failed:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("rename from"), "{text}");
    assert!(text.contains("Everything checks out"), "{text}");
}

#[test]
fn doctor_leaves_no_scratch_directory_behind() {
    let t = Tree::new();
    t.reap().arg("doctor").output().unwrap();

    let leftovers: Vec<PathBuf> = std::fs::read_dir(t.path("code"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".reap-rename-test-"))
        })
        .collect();
    assert!(leftovers.is_empty(), "left behind {leftovers:?}");
    assert!(
        std::fs::read_dir(t.path("quarantine")).unwrap().count() == 0,
        "the rename test left the quarantine directory dirty"
    );
}

#[test]
fn the_config_search_path_prefers_the_environment_over_the_default() {
    let t = Tree::new();
    let mut cmd = Command::cargo_bin("reap").unwrap();
    cmd.env("REAP_CONFIG", &t.config)
        .env("NO_COLOR", "1")
        .args(["report", "--json"]);
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(SUCCESS));
    assert_eq!(json_of(&out)["config"], t.config.display().to_string());
}

#[test]
fn no_command_writes_outside_the_quarantine_directory() {
    // A blunt check that the read-only commands are read-only: snapshot the
    // tree, run each one, and compare.
    let t = Tree::new();
    let before = snapshot(&t.base);

    for args in [
        vec!["report"],
        vec!["report", "--all"],
        vec!["explain", "."],
    ] {
        let mut cmd = t.reap();
        cmd.args(&args);
        if args[0] == "explain" {
            cmd.arg(t.target());
        }
        cmd.output().unwrap();
    }

    assert_eq!(
        before,
        snapshot(&t.base),
        "a read-only command changed the tree"
    );
    let _ = &t.dir;
}

/// Every path under `dir`, excluding the quarantine directory, which reap owns.
fn snapshot(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n == "quarantine") {
                continue;
            }
            if p.is_dir() && !p.is_symlink() {
                stack.push(p.clone());
            }
            out.push(p);
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Invariant 4: dry run is the default, and mutation needs both opt-ins.
// ---------------------------------------------------------------------------

#[test]
fn invariant_4_sweep_without_apply_removes_nothing() {
    let t = Tree::new();
    t.arm(); // the config says removal is allowed...
    let out = t.reap().arg("sweep").output().unwrap();

    // ...but --apply was not given, so nothing happens.
    assert_eq!(out.status.code(), Some(SUCCESS));
    assert!(
        t.target().exists(),
        "sweep removed something without --apply"
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Nothing was removed"), "{text}");
}

#[test]
fn invariant_4_apply_against_a_dry_run_config_removes_nothing_and_exits_2() {
    let t = Tree::new();
    // --apply is given, but the config was never armed.
    let out = t.reap().args(["sweep", "--apply"]).output().unwrap();

    assert_eq!(
        out.status.code(),
        Some(CONFIG),
        "a config still set to dry_run must be visible in the exit code"
    );
    assert!(
        t.target().exists(),
        "one opt-in was enough to remove something"
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("still has dry_run = true"), "{text}");
}

#[test]
fn invariant_4_both_opt_ins_together_reclaim_and_leave_the_source_alone() {
    let t = Tree::new();
    t.arm();
    let out = t.reap().args(["sweep", "--apply"]).output().unwrap();

    assert_eq!(out.status.code(), Some(SUCCESS));
    assert!(!t.target().exists(), "the build directory should be gone");
    assert!(
        t.path("code/proj/Cargo.toml").exists(),
        "source was removed"
    );
    assert!(
        t.path("code/proj/.git").exists(),
        "the repository was removed"
    );

    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Reclaimed"), "{text}");
    assert!(text.contains("across 1 directories"), "{text}");
}

/// Invariant 9: every mutation is logged with path, bytes, rule and tier,
/// before it happens.
#[test]
fn invariant_9_each_removal_is_logged_with_its_path_bytes_rule_and_tier() {
    let t = Tree::new();
    t.arm();
    let out = t.reap().args(["sweep", "--apply"]).output().unwrap();

    let log = String::from_utf8_lossy(&out.stderr);
    assert!(
        log.contains("reclaiming"),
        "no intent line was logged: {log}"
    );
    assert!(log.contains("rule=cargo-target"), "{log}");
    assert!(log.contains("tier=0"), "{log}");
    assert!(log.contains("bytes="), "{log}");
    assert!(log.contains("target"), "{log}");
}

#[test]
fn a_sweep_defaults_to_tier_0_only() {
    let t = Tree::new();
    let text = std::fs::read_to_string(&t.config).unwrap();
    std::fs::write(
        &t.config,
        text + "\n[[rule]]\nname = \"node-modules\"\nkind = \"path\"\ntier = 1\ndir_name = \"node_modules\"\nmin_idle = \"0s\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(t.path("code/proj/node_modules")).unwrap();
    std::fs::write(t.path("code/proj/node_modules/x"), vec![b'x'; 1000]).unwrap();
    // The repository must ignore it, or the git guard would be what stops it
    // and this test would pass for the wrong reason.
    std::fs::write(
        t.path("code/proj/.gitignore"),
        b"/target/\n/node_modules/\n",
    )
    .unwrap();
    t.arm();

    t.reap().args(["sweep", "--apply"]).output().unwrap();
    assert!(!t.target().exists(), "tier 0 should have been reclaimed");
    assert!(
        t.path("code/proj/node_modules").exists(),
        "a plain sweep reached tier 1"
    );

    t.reap()
        .args(["sweep", "--tier", "1", "--apply"])
        .output()
        .unwrap();
    assert!(!t.path("code/proj/node_modules").exists());
}

/// Invariant 7: an interrupted run leaves orphaned quarantine entries that the
/// next run clears, exercised through the real binary.
#[test]
fn invariant_7_the_next_run_clears_what_an_interrupted_one_left_staged() {
    let t = Tree::new();
    t.arm();

    // The state a kill between rename and removal leaves behind.
    let orphan = t.quarantine().join("staged-orphan");
    std::fs::create_dir_all(orphan.join("deep")).unwrap();
    std::fs::write(orphan.join("deep/blob"), vec![b'x'; 5000]).unwrap();
    std::fs::write(
        t.quarantine().join("manifest.jsonl"),
        "{\"staged_at\":\"2026-01-01T00:00:00Z\",\"entry\":\"staged-orphan\",\"original_path\":\"/gone/target\",\"rule\":\"cargo-target\",\"tier\":0,\"bytes\":5000}\n",
    )
    .unwrap();

    let out = t.reap().args(["sweep", "--apply"]).output().unwrap();
    assert_eq!(out.status.code(), Some(SUCCESS));
    assert!(!orphan.exists(), "the orphaned entry survived");

    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("interrupted run"), "{text}");
    assert!(
        text.contains("/gone/target"),
        "the note should name where the orphan came from: {text}"
    );

    // Quarantine holds nothing but an empty manifest afterwards.
    let left: Vec<String> = std::fs::read_dir(t.quarantine())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, vec!["manifest.jsonl".to_owned()]);
    assert_eq!(
        std::fs::read_to_string(t.quarantine().join("manifest.jsonl")).unwrap(),
        ""
    );
}

/// A pin marker stops a sweep that is otherwise fully armed. This is the
/// property the README tells users to rely on.
#[test]
fn a_pin_marker_stops_an_armed_sweep() {
    let t = Tree::new();
    t.arm();
    std::fs::write(t.path("code/proj/.reap-keep"), b"").unwrap();

    let out = t.reap().args(["sweep", "--apply"]).output().unwrap();
    assert_eq!(out.status.code(), Some(NOTHING_TO_DO));
    assert!(t.target().exists(), "a pinned directory was removed");
}

/// Invariant 5, end to end: a build directory the repository does not ignore
/// survives an armed sweep.
#[test]
fn invariant_5_a_directory_git_does_not_ignore_survives_an_armed_sweep() {
    let t = Tree::new();
    std::fs::write(t.path("code/proj/.gitignore"), b"# nothing ignored\n").unwrap();
    t.arm();

    let out = t.reap().args(["sweep", "--apply"]).output().unwrap();
    assert_eq!(out.status.code(), Some(NOTHING_TO_DO));
    assert!(t.target().exists(), "tracked work was removed");
}

// ---------------------------------------------------------------------------
// gc-session
// ---------------------------------------------------------------------------

#[test]
fn gc_session_is_safe_on_an_id_that_never_existed() {
    let t = Tree::new();
    t.arm();
    let out = t
        .reap()
        .args(["gc-session", "no-such-session", "--apply"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(NOTHING_TO_DO));
    assert!(
        t.target().exists(),
        "gc-session touched an unrelated project"
    );
}

#[test]
fn gc_session_reclaims_only_its_own_session_and_is_safe_to_call_twice() {
    let t = Tree::new();
    let sess = t.path("code/job-42");
    std::fs::create_dir_all(sess.join("target")).unwrap();
    std::fs::write(sess.join("Cargo.toml"), b"[package]\nname=\"y\"\n").unwrap();
    std::fs::write(sess.join(".gitignore"), b"/target/\n").unwrap();
    std::fs::write(sess.join("target/blob"), vec![b'x'; 3000]).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(&sess)
        .status()
        .unwrap()
        .success());
    t.arm();

    let out = t
        .reap()
        .args(["gc-session", "job-42", "--apply"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(SUCCESS));
    assert!(
        !sess.join("target").exists(),
        "the session's build dir survived"
    );
    assert!(
        t.target().exists(),
        "gc-session reached outside its session"
    );

    // Calling it again is a no-op, which is what makes it safe on a crash path
    // where nobody knows what already ran.
    let again = t
        .reap()
        .args(["gc-session", "job-42", "--apply"])
        .output()
        .unwrap();
    assert_eq!(again.status.code(), Some(NOTHING_TO_DO));
}

#[test]
fn gc_session_refuses_an_id_that_could_be_read_as_a_path() {
    let t = Tree::new();
    t.arm();
    for id in ["../../etc", "a/b", "."] {
        let out = t
            .reap()
            .args(["gc-session", id, "--apply"])
            .output()
            .unwrap();
        assert_ne!(out.status.code(), Some(SUCCESS), "id {id:?} was accepted");
        assert!(t.target().exists());
    }
}

#[test]
fn gc_session_does_not_run_machine_wide_command_reclaimers() {
    let t = Tree::new();
    // A command rule that leaves a trace if it runs.
    let marker = t.path("command-ran");
    let text = std::fs::read_to_string(&t.config).unwrap();
    std::fs::write(
        &t.config,
        format!(
            "{text}\n[[rule]]\nname = \"touch-marker\"\nkind = \"command\"\ntier = 0\ncommand = [\"/usr/bin/touch\", \"{}\"]\n",
            marker.display()
        ),
    )
    .unwrap();
    t.arm();

    let sess = t.path("code/job-9");
    std::fs::create_dir_all(sess.join("target")).unwrap();
    std::fs::write(sess.join("Cargo.toml"), b"[package]\nname=\"y\"\n").unwrap();
    std::fs::write(sess.join("target/blob"), vec![b'x'; 100]).unwrap();

    t.reap()
        .args(["gc-session", "job-9", "--apply"])
        .output()
        .unwrap();
    assert!(
        !marker.exists(),
        "gc-session ran a machine-wide command reclaimer"
    );

    // A full sweep does run it.
    t.reap().args(["sweep", "--apply"]).output().unwrap();
    assert!(marker.exists(), "sweep did not run the command rule");
}

/// A quarantine directory that cannot receive a rename aborts the sweep.
///
/// The point of the check is that there is no fallback: removing in place when
/// the rename fails would trade the entire interruption guarantee for the
/// convenience of not stopping. On a real machine the usual cause is a
/// quarantine directory on a different filesystem, which fails with `EXDEV`;
/// here an unwritable directory drives the same path.
#[test]
fn a_quarantine_that_cannot_receive_a_rename_aborts_rather_than_falling_back() {
    use std::os::unix::fs::PermissionsExt;

    let t = Tree::new();
    t.arm();
    std::fs::create_dir_all(t.quarantine()).unwrap();
    let mut perms = std::fs::metadata(t.quarantine()).unwrap().permissions();
    perms.set_mode(0o500); // readable and traversable, but not writable
    std::fs::set_permissions(t.quarantine(), perms).unwrap();

    let out = t.reap().args(["sweep", "--apply"]).output().unwrap();

    let mut perms = std::fs::metadata(t.quarantine()).unwrap().permissions();
    perms.set_mode(0o755);
    let _ = std::fs::set_permissions(t.quarantine(), perms);

    assert_ne!(
        out.status.code(),
        Some(SUCCESS),
        "the sweep reported success"
    );
    assert!(
        t.target().exists(),
        "the sweep fell back to removing in place"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("refusing to sweep"),
        "the failure should say why: {err}"
    );
    assert!(
        err.contains("quarantine"),
        "the failure should name the directory at fault: {err}"
    );
}

/// The same fault, seen by `doctor`, which is where a user is told about it
/// before a sweep ever runs.
#[test]
fn doctor_reports_a_quarantine_that_cannot_receive_a_rename() {
    use std::os::unix::fs::PermissionsExt;

    let t = Tree::new();
    std::fs::create_dir_all(t.quarantine()).unwrap();
    let mut perms = std::fs::metadata(t.quarantine()).unwrap().permissions();
    perms.set_mode(0o500);
    std::fs::set_permissions(t.quarantine(), perms).unwrap();

    let out = t.reap().arg("doctor").output().unwrap();

    let mut perms = std::fs::metadata(t.quarantine()).unwrap().permissions();
    perms.set_mode(0o755);
    let _ = std::fs::set_permissions(t.quarantine(), perms);

    assert_eq!(out.status.code(), Some(PARTIAL));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("failure:"), "{text}");
}
