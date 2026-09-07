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
