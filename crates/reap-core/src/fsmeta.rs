//! Read-only filesystem measurement: how big a tree is, and how long it has
//! been untouched.
//!
//! Nothing here writes. `reap-core` is compiled without any filesystem
//! mutation, which is what makes "the planner cannot delete" a property of the
//! build rather than a promise in a comment.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime};

use std::os::unix::fs::MetadataExt;
use walkdir::WalkDir;

/// How many immediate children the idle probe will stat before giving up and
/// using what it has.
///
/// The idle check must stay cheap: it runs for every candidate, and a deep walk
/// of a `node_modules` tree to answer "was this touched recently" would cost
/// more than the reclamation saves.
pub const IDLE_PROBE_LIMIT: usize = 256;

/// The measured size of one tree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SizeReport {
    /// Space actually occupied on disk, from the allocated block count. This is
    /// smaller than the apparent size for sparse and transparently compressed
    /// files, and it is the number that predicts how much space a removal
    /// returns.
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
    /// Entries that could not be read, usually a permission error.
    pub errors: u64,
}

/// Measures a tree without following symlinks.
///
/// Hard links are counted once: a `node_modules` tree populated by a linking
/// package manager would otherwise report several times the space its removal
/// would return.
pub fn tree_size(path: &Path) -> SizeReport {
    let mut out = SizeReport::default();
    let mut seen_links: HashSet<(u64, u64)> = HashSet::new();

    for entry in WalkDir::new(path).follow_links(false).into_iter() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                out.errors += 1;
                continue;
            }
        };
        let Ok(md) = entry.metadata() else {
            out.errors += 1;
            continue;
        };
        if md.is_dir() {
            out.dirs += 1;
        } else {
            out.files += 1;
        }
        if md.nlink() > 1 && !seen_links.insert((md.dev(), md.ino())) {
            continue;
        }
        out.bytes = out.bytes.saturating_add(md.blocks().saturating_mul(512));
    }
    out
}

/// How long a directory has been idle.
///
/// The directory's own mtime only changes when an entry is added or removed, so
/// it alone would call an actively-written-into build directory idle. A bounded
/// probe of immediate children catches that without the cost of a deep walk.
/// Deeper writes that touch nothing at the top two levels are not detected here
/// on purpose; the process-liveness guard is what covers an active build.
pub fn idle_for(path: &Path, now: SystemTime, probe_limit: usize) -> std::io::Result<Duration> {
    let md = std::fs::symlink_metadata(path)?;
    let mut newest = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);

    if md.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.take(probe_limit).flatten() {
                if let Ok(child) = entry.metadata() {
                    if let Ok(m) = child.modified() {
                        if m > newest {
                            newest = m;
                        }
                    }
                }
            }
        }
    }

    // A future mtime (clock skew, a restored archive) reads as zero idle time,
    // which is the conservative direction: not idle, so not a candidate.
    Ok(now.duration_since(newest).unwrap_or(Duration::ZERO))
}

/// Whether an entry with this name exists in `dir`, without following symlinks.
pub fn entry_exists(dir: &Path, name: &str) -> bool {
    std::fs::symlink_metadata(dir.join(name)).is_ok()
}
