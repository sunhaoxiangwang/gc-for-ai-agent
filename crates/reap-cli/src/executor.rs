//! The only code in the workspace that removes anything.
//!
//! Removal is two steps, and they are separated on purpose.
//!
//! 1. `rename` the tree into the quarantine directory. This is O(1) and atomic:
//!    it either happened or it did not. The moment it returns, the user's
//!    workspace is clean.
//! 2. Remove the quarantine contents recursively. This is slow, and nothing
//!    depends on it finishing. An interruption here leaves orphaned entries in
//!    quarantine, which the next run clears without re-planning; it can never
//!    leave a half-removed tree where a build directory used to be.
//!
//! Every mutation is logged with its path, byte count, rule and tier *before*
//! it happens, so an interrupted run is reconstructable from the log alone.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use anyhow::{Context, Result};
use reap_core::plan::Candidate;
use reap_core::time::{human_bytes, rfc3339};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::quarantine::{unique_name, Quarantine};

/// Name of the run manifest inside the quarantine directory.
pub const MANIFEST: &str = "manifest.jsonl";

/// One staged tree, recorded the moment it lands in quarantine.
///
/// The manifest describes what is *currently* staged. Once quarantine is empty
/// it is truncated, so its length is the amount of work outstanding rather than
/// a history that grows without bound.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub staged_at: String,
    /// Name of the directory inside quarantine holding the tree.
    pub entry: String,
    /// Where it came from. Recorded so an interrupted run is diagnosable.
    pub original_path: String,
    pub rule: String,
    pub tier: u8,
    pub bytes: u64,
}

#[derive(Debug, Default)]
pub struct RemoveStats {
    pub files: u64,
    pub dirs: u64,
    pub errors: Vec<String>,
}

impl RemoveStats {
    fn merge(&mut self, other: RemoveStats) {
        self.files += other.files;
        self.dirs += other.dirs;
        self.errors.extend(other.errors);
    }
}

/// Removes a tree without ever following a symlink.
///
/// A symlinked directory is unlinked, never descended into. Following one would
/// let a link planted inside a build directory redirect the removal at
/// something outside every root, which is the single worst thing this program
/// could do.
///
/// Errors are collected rather than fatal: one unreadable subdirectory should
/// not abandon the rest of the work. The caller reports them and exits 4.
pub fn remove_tree(path: &Path) -> RemoveStats {
    let mut stats = RemoveStats::default();
    // (path, children already queued). An explicit stack rather than recursion,
    // because tree depth is attacker-influenced in the general case.
    let mut stack: Vec<(PathBuf, bool)> = vec![(path.to_path_buf(), false)];

    while let Some((p, children_queued)) = stack.pop() {
        if children_queued {
            match std::fs::remove_dir(&p) {
                Ok(()) => stats.dirs += 1,
                Err(e) => stats.errors.push(format!("rmdir {}: {e}", p.display())),
            }
            continue;
        }

        let md = match std::fs::symlink_metadata(&p) {
            Ok(md) => md,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                stats.errors.push(format!("stat {}: {e}", p.display()));
                continue;
            }
        };

        if md.file_type().is_symlink() || !md.is_dir() {
            // Unlink the link itself. Never look at what is on the other side.
            match std::fs::remove_file(&p) {
                Ok(()) => stats.files += 1,
                Err(e) => stats.errors.push(format!("unlink {}: {e}", p.display())),
            }
            continue;
        }

        match std::fs::read_dir(&p) {
            Ok(entries) => {
                stack.push((p.clone(), true));
                for e in entries.flatten() {
                    stack.push((e.path(), false));
                }
            }
            Err(e) => {
                stats.errors.push(format!("read {}: {e}", p.display()));
            }
        }
    }
    stats
}

/// What a recovery pass found and cleared.
#[derive(Debug, Default)]
pub struct Recovery {
    pub entries: usize,
    pub stats: RemoveStats,
    pub from_manifest: Vec<ManifestEntry>,
}

pub struct Executor {
    q: Quarantine,
    manifest: File,
    /// Destinations inside quarantine, in staging order, for `drain`.
    staged: Vec<PathBuf>,
}

impl Executor {
    pub fn open(dir: &Path) -> Result<Self> {
        let q = Quarantine::ensure(dir)?;
        let manifest = OpenOptions::new()
            .create(true)
            .append(true)
            .open(q.dir.join(MANIFEST))
            .with_context(|| format!("could not open the run manifest in {}", q.dir.display()))?;
        Ok(Self {
            q,
            manifest,
            staged: Vec::new(),
        })
    }

    /// Confirms a tree can be renamed from `root` into quarantine.
    ///
    /// Run at the top of every sweep, not only in `doctor`. Failing loudly here
    /// is the point: falling back to removing in place would trade the whole
    /// interruption guarantee for the convenience of not stopping.
    pub fn verify_rename(&self, root: &Path) -> Result<()> {
        self.q.test_rename(root)
    }

    /// Clears anything a previous run left staged.
    ///
    /// This deliberately does not re-plan and does not consult the guards. The
    /// entries are already outside every root; they were judged before they got
    /// here, and the only thing left to do is finish removing them.
    pub fn recover(&mut self) -> Recovery {
        let mut rec = Recovery {
            from_manifest: self.read_manifest(),
            ..Default::default()
        };

        let Ok(entries) = std::fs::read_dir(&self.q.dir) else {
            return rec;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == MANIFEST) {
                continue;
            }
            info!(
                path = %path.display(),
                "clearing an entry left behind by an interrupted run"
            );
            rec.entries += 1;
            rec.stats.merge(remove_tree(&path));
        }

        if rec.entries > 0 && rec.stats.errors.is_empty() {
            self.truncate_manifest();
        }
        rec
    }

    /// Moves one candidate into quarantine.
    ///
    /// The intent line goes out before the rename, so the log describes the
    /// mutation even if the process dies during it.
    pub fn stage(&mut self, candidate: &Candidate, canonical: &Path) -> Result<()> {
        info!(
            path = %canonical.display(),
            bytes = candidate.bytes,
            human = %human_bytes(candidate.bytes),
            rule = %candidate.rule,
            tier = candidate.tier.get(),
            "reclaiming"
        );

        let name = unique_name("staged-");
        let dest = self.q.dir.join(&name);
        std::fs::rename(canonical, &dest).with_context(|| {
            format!(
                "could not move {} into quarantine at {}",
                canonical.display(),
                dest.display()
            )
        })?;

        let entry = ManifestEntry {
            staged_at: rfc3339(SystemTime::now()),
            entry: name,
            original_path: canonical.display().to_string(),
            rule: candidate.rule.clone(),
            tier: candidate.tier.get(),
            bytes: candidate.bytes,
        };
        // A manifest write that fails is not worth undoing the rename over: the
        // tree is already out of the user's way, and the next run will clear it
        // whether or not it is described here.
        if let Err(e) = self.append(&entry) {
            warn!(error = %e, "could not append to the run manifest");
        }
        self.staged.push(dest);
        Ok(())
    }

    /// Removes everything this run staged.
    pub fn drain(&mut self) -> RemoveStats {
        let mut stats = RemoveStats::default();
        for dest in &self.staged {
            stats.merge(remove_tree(dest));
        }
        if stats.errors.is_empty() {
            self.truncate_manifest();
        }
        stats
    }

    fn append(&mut self, entry: &ManifestEntry) -> Result<()> {
        let line = serde_json::to_string(entry)?;
        writeln!(self.manifest, "{line}")?;
        self.manifest.flush()?;
        Ok(())
    }

    fn read_manifest(&self) -> Vec<ManifestEntry> {
        let Ok(text) = std::fs::read_to_string(self.q.dir.join(MANIFEST)) else {
            return Vec::new();
        };
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// Empties the manifest once quarantine holds nothing.
    ///
    /// The manifest is a record of outstanding work, not an audit log, so its
    /// length should track what is actually staged.
    fn truncate_manifest(&mut self) {
        if let Err(e) = File::create(self.q.dir.join(MANIFEST)) {
            warn!(error = %e, "could not truncate the run manifest");
        }
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.q.dir.join(MANIFEST))
        {
            Ok(f) => self.manifest = f,
            Err(e) => warn!(error = %e, "could not reopen the run manifest"),
        }
    }
}

/// A budget for how long a run may keep starting new work.
///
/// A reclaimer that pegs a laptop's disk for ten minutes is a bug, so the
/// budget is checked before each new item. Work already in flight finishes.
pub struct TimeBudget {
    started: Instant,
    limit: std::time::Duration,
}

impl TimeBudget {
    pub fn new(limit: std::time::Duration) -> Self {
        Self {
            started: Instant::now(),
            limit,
        }
    }

    pub fn exhausted(&self) -> bool {
        !self.limit.is_zero() && self.started.elapsed() >= self.limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Invariant 7 support: removal never follows a symlink.
    ///
    /// A link planted inside a build directory must be unlinked, not walked.
    /// Following one would let a candidate inside a declared root redirect the
    /// removal at anything on the machine.
    #[test]
    fn remove_tree_unlinks_symlinks_without_descending() {
        let outside = tempfile::tempdir().unwrap();
        let treasure = outside.path().join("keep-me");
        std::fs::create_dir(&treasure).unwrap();
        std::fs::write(treasure.join("important"), b"payload").unwrap();

        let doomed = tempfile::tempdir().unwrap();
        let root = doomed.path().join("target");
        std::fs::create_dir_all(root.join("debug")).unwrap();
        std::fs::write(root.join("debug/blob"), b"x").unwrap();
        std::os::unix::fs::symlink(&treasure, root.join("link-to-dir")).unwrap();
        std::os::unix::fs::symlink(treasure.join("important"), root.join("link-to-file")).unwrap();

        let stats = remove_tree(&root);

        assert!(stats.errors.is_empty(), "{:?}", stats.errors);
        assert!(!root.exists(), "the tree was not removed");
        assert!(
            treasure.join("important").exists(),
            "removal followed a symlink out of the tree"
        );
        // blob, and the two symlinks themselves.
        assert_eq!(stats.files, 3);
        assert_eq!(stats.dirs, 2);
    }

    #[test]
    fn remove_tree_is_silent_about_a_path_that_is_already_gone() {
        let dir = tempfile::tempdir().unwrap();
        let stats = remove_tree(&dir.path().join("never-existed"));
        assert!(stats.errors.is_empty());
        assert_eq!(stats.files, 0);
    }

    #[test]
    fn remove_tree_collects_errors_rather_than_stopping() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        std::fs::create_dir_all(root.join("locked")).unwrap();
        std::fs::write(root.join("locked/file"), b"x").unwrap();
        std::fs::write(root.join("readable"), b"x").unwrap();
        // A directory we cannot list.
        let mut perms = std::fs::metadata(root.join("locked"))
            .unwrap()
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o000);
        std::fs::set_permissions(root.join("locked"), perms).unwrap();

        let stats = remove_tree(&root);
        assert!(!stats.errors.is_empty(), "expected an error to be recorded");
        // The readable sibling still went away.
        assert_eq!(stats.files, 1);

        let mut perms = std::fs::metadata(root.join("locked"))
            .unwrap()
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        let _ = std::fs::set_permissions(root.join("locked"), perms);
    }

    /// Invariant 7: an interrupted run leaves nothing half-removed, and the
    /// next run finishes the job without re-planning.
    #[test]
    fn invariant_7_a_run_interrupted_between_rename_and_removal_is_recovered() {
        let q = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let doomed = work.path().join("target");
        std::fs::create_dir_all(doomed.join("debug")).unwrap();
        std::fs::write(doomed.join("debug/blob"), vec![b'x'; 4096]).unwrap();

        let candidate = reap_core::plan::Candidate {
            path: doomed.clone(),
            root: work.path().to_path_buf(),
            rule: "cargo-target".to_owned(),
            tier: reap_core::config::Tier::new(0).unwrap(),
            depth: 1,
            depth_cap: 4,
            bytes: 4096,
            idle: std::time::Duration::from_secs(9999),
            min_idle: std::time::Duration::ZERO,
            orphaned_only: false,
        };

        // First run: stage, then die before draining.
        {
            let mut ex = Executor::open(q.path()).unwrap();
            ex.stage(&candidate, &doomed).unwrap();
            assert!(
                !doomed.exists(),
                "the workspace is clean the moment rename returns"
            );
            std::mem::forget(ex);
        }

        let staged: Vec<_> = std::fs::read_dir(q.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| n != MANIFEST)
            .collect();
        assert_eq!(staged.len(), 1, "the tree should be sitting in quarantine");

        // Second run: recovers without being told anything about the plan.
        let mut ex = Executor::open(q.path()).unwrap();
        let rec = ex.recover();
        assert_eq!(rec.entries, 1);
        assert!(rec.stats.errors.is_empty(), "{:?}", rec.stats.errors);
        assert_eq!(
            rec.from_manifest.first().map(|e| e.original_path.as_str()),
            Some(doomed.display().to_string().as_str()),
            "the manifest should say where the orphan came from"
        );

        let left: Vec<_> = std::fs::read_dir(q.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| n != MANIFEST)
            .collect();
        assert!(left.is_empty(), "quarantine still holds {left:?}");
        assert_eq!(
            std::fs::read_to_string(q.path().join(MANIFEST)).unwrap(),
            "",
            "the manifest should be empty once nothing is outstanding"
        );
    }

    #[test]
    fn recovery_on_an_empty_quarantine_does_nothing_and_says_so() {
        let q = tempfile::tempdir().unwrap();
        let mut ex = Executor::open(q.path()).unwrap();
        let rec = ex.recover();
        assert_eq!(rec.entries, 0);
        assert!(rec.stats.errors.is_empty());
    }

    #[test]
    fn the_time_budget_expires_and_a_zero_budget_never_does() {
        let budget = TimeBudget::new(std::time::Duration::ZERO);
        assert!(!budget.exhausted(), "a zero budget means no limit");

        let budget = TimeBudget::new(std::time::Duration::from_nanos(1));
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(budget.exhausted());
    }
}
