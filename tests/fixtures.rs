//! Synthetic filesystem trees for the rest of the test suite.
//!
//! Every test that touches the planner or a guard builds its world here rather
//! than depending on whatever happens to be on the machine running the tests.
//! Other test files pull this in with `#[path = "fixtures.rs"] mod fixtures;`.

#![allow(dead_code)]

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use reap_core::config::{Config, LoadContext};

pub struct Fixture {
    pub dir: tempfile::TempDir,
    /// The tempdir path with symlinks resolved. On macOS `/tmp` is itself a
    /// symlink to `/private/tmp`, so a fixture that skipped this would fail
    /// containment checks for reasons that have nothing to do with the test.
    pub base: PathBuf,
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().canonicalize().expect("canonicalize tempdir");
        Self { dir, base }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.base.join(rel)
    }

    /// Creates a directory and all of its parents.
    pub fn dir(&self, rel: &str) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(&p).expect("create_dir_all");
        p
    }

    /// Creates a file of `bytes` length, creating parent directories.
    pub fn file(&self, rel: &str, bytes: usize) -> PathBuf {
        let p = self.path(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create_dir_all");
        }
        let mut f = File::create(&p).expect("create file");
        f.write_all(&vec![b'x'; bytes]).expect("write");
        p
    }

    pub fn touch(&self, rel: &str) -> PathBuf {
        self.file(rel, 0)
    }

    /// Creates a symlink at `rel` pointing at `target`.
    pub fn symlink(&self, target: &Path, rel: &str) -> PathBuf {
        let p = self.path(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create_dir_all");
        }
        std::os::unix::fs::symlink(target, &p).expect("symlink");
        p
    }

    /// Initialises a real git repository at `rel` with the given `.gitignore`.
    ///
    /// The git-ignore guard shells out to the real `git`, so the tests do too;
    /// a hand-rolled ignore parser in the test suite would be testing the wrong
    /// thing.
    pub fn git_repo(&self, rel: &str, gitignore: &str) -> PathBuf {
        let p = self.dir(rel);
        let out = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&p)
            .output()
            .expect("git init (is git installed?)");
        assert!(out.status.success(), "git init failed: {out:?}");
        fs::write(p.join(".gitignore"), gitignore).expect("write .gitignore");
        p
    }

    /// Drops a pin marker into `rel`, protecting it and everything below it.
    pub fn pin(&self, rel: &str, marker: &str) -> PathBuf {
        let p = self.dir(rel);
        fs::write(p.join(marker), b"").expect("write pin marker");
        p
    }

    /// Sets the modification time of `rel` to `age` in the past.
    pub fn age(&self, rel: &str, age: Duration) {
        let p = self.path(rel);
        set_mtime(&p, SystemTime::now() - age);
    }

    /// Ages a directory and every entry directly inside it, which is what the
    /// idle probe looks at.
    pub fn age_tree(&self, rel: &str, age: Duration) {
        let p = self.path(rel);
        let when = SystemTime::now() - age;
        if let Ok(entries) = fs::read_dir(&p) {
            for e in entries.flatten() {
                set_mtime(&e.path(), when);
            }
        }
        set_mtime(&p, when);
    }

    /// Writes a heartbeat file for `session`, `age` old, naming `workspace`.
    pub fn heartbeat(&self, dir: &str, session: &str, workspace: &Path, age: Duration, pid: u32) {
        let d = self.dir(dir);
        let updated = SystemTime::now() - age;
        let body = format!(
            r#"{{"session_id":"{session}","workspace":"{}","pid":{pid},"interval_seconds":60,"updated_at":"{}"}}"#,
            workspace.display(),
            reap_core::time::rfc3339(updated),
        );
        let p = d.join(format!("{session}.json"));
        fs::write(&p, body).expect("write heartbeat");
        set_mtime(&p, updated);
    }

    /// Parses a config in the context of this fixture. `{base}` in the text is
    /// replaced with the fixture root, so configs read naturally.
    pub fn config(&self, toml: &str) -> Config {
        let text = toml.replace("{base}", &self.base.display().to_string());
        let ctx = LoadContext {
            home: Some(self.base.clone()),
            hostname: "test-host".to_owned(),
            explicit: None,
            env_config: None,
        };
        reap_core::config::parse(&text, &self.base.join("reap.toml"), &ctx)
            .unwrap_or_else(|e| panic!("fixture config did not parse: {e}"))
    }

    /// Same, but returns the error for tests that expect rejection.
    pub fn try_config(&self, toml: &str) -> Result<Config, reap_core::error::ConfigError> {
        let text = toml.replace("{base}", &self.base.display().to_string());
        let ctx = LoadContext {
            home: Some(self.base.clone()),
            hostname: "test-host".to_owned(),
            explicit: None,
            env_config: None,
        };
        reap_core::config::parse(&text, &self.base.join("reap.toml"), &ctx)
    }
}

/// Sets an mtime without pulling in a date-time crate.
pub fn set_mtime(path: &Path, when: SystemTime) {
    let times = fs::FileTimes::new().set_modified(when).set_accessed(when);
    // Directories cannot be opened for writing, but futimens only needs the
    // caller to own the file, which in a tempdir we always do.
    let f = File::options()
        .read(true)
        .open(path)
        .unwrap_or_else(|e| panic!("open {} for set_times: {e}", path.display()));
    f.set_times(times)
        .unwrap_or_else(|e| panic!("set_times {}: {e}", path.display()));
}

/// A minimal config covering the common Rust and Node rules, for tests that
/// care about the planner rather than about configuration.
pub const BASIC_CONFIG: &str = r#"
[global]
dry_run = true
quarantine = "{base}/quarantine"
heartbeat_dir = "{base}/heartbeats"

[[root]]
path = "{base}/code"
max_depth = 6

[[rule]]
name = "cargo-target"
kind = "path"
tier = 0
dir_name = "target"
require_sibling = "Cargo.toml"
min_idle = "0s"

[[rule]]
name = "node-modules"
kind = "path"
tier = 1
dir_name = "node_modules"
require_sibling = "package.json"
min_idle = "0s"
"#;

#[test]
fn fixture_can_age_a_directory() {
    let f = Fixture::new();
    f.dir("code/proj");
    f.age("code/proj", Duration::from_secs(7200));
    let idle = reap_core::fsmeta::idle_for(&f.path("code/proj"), SystemTime::now(), 256).unwrap();
    assert!(idle >= Duration::from_secs(7100), "idle was {idle:?}");
}

#[test]
fn fixture_git_repo_reports_ignored_paths() {
    let f = Fixture::new();
    let repo = f.git_repo("code/proj", "target/\n");
    f.dir("code/proj/target");
    let status = Command::new("git")
        .args(["check-ignore", "--quiet", "target"])
        .current_dir(&repo)
        .status()
        .expect("git check-ignore");
    assert!(
        status.success(),
        "target should be ignored in the fixture repo"
    );
}
