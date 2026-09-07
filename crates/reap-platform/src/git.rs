//! The git-ignore oracle.
//!
//! This lives in `reap-platform` rather than `reap-core` for the same reason
//! process inspection does: `reap-core` is checked for the absence of
//! filesystem writes and process spawning, so anything that shells out belongs
//! behind a trait.

use std::path::Path;
use std::process::Command;

/// What `git check-ignore` said about a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnoreStatus {
    /// git considers the path ignored. This is the only answer that permits
    /// reclamation.
    Ignored,
    /// git tracks the path, or would.
    NotIgnored,
    /// git could not answer: it is not installed, the repository is broken,
    /// or it refused (for example "dubious ownership"). Treated as a rejection.
    Unknown(String),
}

/// Asks git whether a path is ignored.
pub trait GitOracle: Send + Sync {
    /// `worktree` is the directory containing the `.git` entry; `path` is the
    /// absolute path being asked about.
    fn check_ignore(&self, worktree: &Path, path: &Path) -> IgnoreStatus;
}

/// The real oracle: the `git` binary on `PATH`.
pub struct GitCli;

impl GitOracle for GitCli {
    fn check_ignore(&self, worktree: &Path, path: &Path) -> IgnoreStatus {
        let out = Command::new("git")
            .arg("-C")
            .arg(worktree)
            // --no-index would consult the index; we want the ignore rules
            // exactly as git applies them to an untracked path.
            .args(["check-ignore", "--quiet", "--"])
            .arg(path)
            .output();

        match out {
            Err(e) => IgnoreStatus::Unknown(format!("could not run git: {e}")),
            Ok(out) => match out.status.code() {
                Some(0) => IgnoreStatus::Ignored,
                Some(1) => IgnoreStatus::NotIgnored,
                Some(code) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let detail = stderr.trim();
                    IgnoreStatus::Unknown(if detail.is_empty() {
                        format!("git exited {code}")
                    } else {
                        format!("git exited {code}: {detail}")
                    })
                }
                None => IgnoreStatus::Unknown("git was killed by a signal".to_owned()),
            },
        }
    }
}
