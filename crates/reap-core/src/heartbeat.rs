//! The heartbeat contract.
//!
//! Liveness of a workspace comes from whatever supervises the work saying so,
//! not from reap guessing. A supervisor writes one file per active job and
//! rewrites it periodically; reap refuses to touch anything a live file names.
//!
//! The contract, which is also what `docs/configuration.md` documents:
//!
//! * One file per session in `heartbeat_dir`, named `<session-id>.json`.
//! * `session_id` matches `[A-Za-z0-9._-]{1,128}`.
//! * The body is a JSON object:
//!   `{"session_id": "...", "workspace": "/abs/path", "tmp": ["/abs/path"],
//!     "pid": 1234, "interval_seconds": 60, "updated_at": "RFC3339"}`.
//!   Only `session_id` is required.
//! * The supervisor rewrites or touches the file at least every
//!   `interval_seconds`.
//! * reap treats a heartbeat as live while
//!   `now - updated_at < interval_seconds * heartbeat_staleness_multiplier`,
//!   or while `pid` names a running process.
//!
//! Absence of a heartbeat means unprotected, not protected. A rule that needs
//! the opposite sets `orphaned_only`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Deserialize;

/// Default rewrite interval assumed when a heartbeat does not declare one.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Deserialize)]
struct RawHeartbeat {
    session_id: Option<String>,
    workspace: Option<String>,
    #[serde(default)]
    tmp: Vec<String>,
    pid: Option<u32>,
    interval_seconds: Option<u64>,
    updated_at: Option<String>,
}

/// One session's heartbeat, as read from disk.
#[derive(Debug, Clone)]
pub struct Heartbeat {
    pub session_id: String,
    pub file: PathBuf,
    /// Paths this session owns. Reap refuses to touch these, anything inside
    /// them, and anything that contains them.
    pub paths: Vec<PathBuf>,
    pub pid: Option<u32>,
    pub interval: Duration,
    /// The newer of the declared `updated_at` and the file's own mtime. Using
    /// the file's mtime as a floor means a supervisor that only touches the
    /// file still works.
    pub updated_at: SystemTime,
    /// Set when the file existed but could not be understood. Such a heartbeat
    /// is treated as live: an unreadable claim of ownership is still a claim.
    pub unreadable: Option<String>,
}

impl Heartbeat {
    /// Whether this heartbeat still vouches for its session.
    pub fn is_live(
        &self,
        now: SystemTime,
        multiplier: f64,
        pid_alive: &dyn Fn(u32) -> bool,
    ) -> bool {
        if self.unreadable.is_some() {
            return true;
        }
        // A process that is still running outvotes any timestamp: the
        // supervisor may have died without cleaning up while the work it
        // started carries on.
        if let Some(pid) = self.pid {
            if pid_alive(pid) {
                return true;
            }
        }
        let age = now
            .duration_since(self.updated_at)
            .unwrap_or(Duration::ZERO);
        age.as_secs_f64() < self.interval.as_secs_f64() * multiplier
    }
}

/// Every heartbeat found in the heartbeat directory.
#[derive(Debug, Clone, Default)]
pub struct HeartbeatIndex {
    pub dir: PathBuf,
    pub entries: Vec<Heartbeat>,
    /// Set when the directory could not be listed. Rules that demand positive
    /// evidence of orphanhood reject everything in that case.
    pub unreadable: Option<String>,
}

impl HeartbeatIndex {
    /// Reads the heartbeat directory. A missing directory is normal: it just
    /// means nothing is currently claiming a workspace.
    pub fn load(dir: &Path) -> Self {
        let mut out = Self {
            dir: dir.to_path_buf(),
            ..Default::default()
        };
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                out.unreadable = Some(format!("{} does not exist", dir.display()));
                return out;
            }
            Err(e) => {
                out.unreadable = Some(format!("cannot read {}: {e}", dir.display()));
                return out;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            out.entries.push(read_one(&path));
        }
        out.entries.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        out
    }

    /// The live heartbeats among them.
    pub fn live<'a>(
        &'a self,
        now: SystemTime,
        multiplier: f64,
        pid_alive: &'a dyn Fn(u32) -> bool,
    ) -> impl Iterator<Item = &'a Heartbeat> {
        self.entries
            .iter()
            .filter(move |h| h.is_live(now, multiplier, pid_alive))
    }
}

fn read_one(path: &Path) -> Heartbeat {
    let session_from_name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mtime = std::fs::symlink_metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let mut hb = Heartbeat {
        session_id: session_from_name.clone(),
        file: path.to_path_buf(),
        paths: Vec::new(),
        pid: None,
        interval: DEFAULT_INTERVAL,
        updated_at: mtime,
        unreadable: None,
    };

    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            hb.unreadable = Some(format!("cannot read: {e}"));
            return hb;
        }
    };
    let raw: RawHeartbeat = match serde_json::from_str(&text) {
        Ok(r) => r,
        Err(e) => {
            hb.unreadable = Some(format!("not valid heartbeat JSON: {e}"));
            return hb;
        }
    };

    if let Some(id) = raw.session_id {
        hb.session_id = id;
    }
    hb.pid = raw.pid;
    if let Some(secs) = raw.interval_seconds {
        // A zero interval would make every heartbeat instantly stale.
        hb.interval = Duration::from_secs(secs.max(1));
    }
    if let Some(declared) = raw
        .updated_at
        .as_deref()
        .and_then(crate::time::parse_rfc3339)
    {
        if declared > hb.updated_at {
            hb.updated_at = declared;
        }
    }
    for p in std::iter::once(raw.workspace).flatten().chain(raw.tmp) {
        let p = PathBuf::from(p);
        if p.is_absolute() {
            hb.paths.push(p);
        }
    }
    hb
}
