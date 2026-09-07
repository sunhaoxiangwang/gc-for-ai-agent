//! Run context: the loaded config plus the platform handles, assembled once.

use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use anyhow::Result;
use reap_core::config::{self, Config, LoadContext};
use reap_core::guard::{self, GuardContext};
use reap_core::heartbeat::HeartbeatIndex;
use reap_platform::{CachedProcessInspector, GitCli, Volume};

use crate::cli::GlobalArgs;

/// Free-space accounting as the pressure logic sees it.
#[derive(Debug, Clone, Copy)]
pub struct Pressure {
    pub total_bytes: u64,
    /// What the kernel reports.
    pub available_bytes: u64,
    /// What is really available once purgeable snapshot space is allowed for.
    pub effective_bytes: u64,
    pub snapshots: usize,
    pub raw_pct: f64,
    pub effective_pct: f64,
}

/// The nearest existing ancestor of `path`, for asking about a filesystem
/// before the directory itself exists.
pub fn nearest_existing(path: &std::path::Path) -> &std::path::Path {
    let mut probe = path;
    while !probe.exists() {
        match probe.parent() {
            Some(p) => probe = p,
            None => break,
        }
    }
    probe
}

pub struct Context {
    pub config: Config,
    pub volume: Box<dyn Volume>,
    /// One process inspection per run: on macOS a full sweep costs thousands of
    /// syscalls, and the liveness guard consults it once per candidate.
    pub inspector: CachedProcessInspector,
    pub git: GitCli,
    pub heartbeats: HeartbeatIndex,
    /// Declared roots, canonicalized once for the containment check.
    pub canonical_roots: Vec<PathBuf>,
    /// Local snapshots holding purgeable space, read once per run because the
    /// query shells out to `tmutil`.
    snapshots: std::cell::OnceCell<Vec<String>>,
    pub os: &'static str,
    pub arch: &'static str,
    pub host: String,
    pub started_at: SystemTime,
    pub clock: Instant,
    pub args: GlobalArgs,
}

/// Hostname used for machine overlays and reported in JSON.
///
/// Falls back to "unknown" rather than failing: a machine that cannot name
/// itself should still be able to run a dry report.
pub fn hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_owned())
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// The configuration search path, for error messages and `doctor`.
pub fn load_context(args: &GlobalArgs) -> LoadContext {
    LoadContext {
        home: home(),
        hostname: hostname(),
        explicit: args.config.clone(),
        env_config: std::env::var("REAP_CONFIG").ok().filter(|v| !v.is_empty()),
    }
}

impl Context {
    pub fn load(args: &GlobalArgs) -> Result<Self, reap_core::error::ConfigError> {
        let lc = load_context(args);
        let config = config::load(&lc)?;
        Ok(Self::with_config(config, args))
    }

    pub fn with_config(config: Config, args: &GlobalArgs) -> Self {
        let heartbeats = HeartbeatIndex::load(&config.heartbeat_dir);
        let canonical_roots = guard::canonical_roots(&config);
        Self {
            heartbeats,
            canonical_roots,
            snapshots: std::cell::OnceCell::new(),
            inspector: CachedProcessInspector::new(reap_platform::process_inspector()),
            git: GitCli,
            config,
            volume: reap_platform::volume(),
            os: reap_platform::os_name(),
            arch: reap_platform::arch_name(),
            host: hostname(),
            started_at: SystemTime::now(),
            clock: Instant::now(),
            args: args.clone(),
        }
    }

    /// Assembles the context the guard stack runs against.
    pub fn guards(&self) -> GuardContext<'_> {
        GuardContext {
            config: &self.config,
            now: self.started_at,
            inspector: &self.inspector,
            git: &self.git,
            heartbeats: &self.heartbeats,
            pid_alive: &reap_platform::process_exists,
            roots: &self.canonical_roots,
        }
    }

    /// Names of local snapshots holding space the kernel will release on
    /// demand. Always empty on Linux.
    pub fn purgeable_snapshots(&self) -> &[String] {
        self.snapshots.get_or_init(|| {
            self.volume
                .purgeable_snapshots(std::path::Path::new("/"))
                .unwrap_or_default()
        })
    }

    /// Free space as a percentage, and the same number once purgeable snapshot
    /// space is allowed for.
    ///
    /// On APFS the kernel reports snapshot-held space as used even though it
    /// releases it under pressure. Triggering an escalating sweep on the raw
    /// number means escalating for space the machine already has, so the
    /// pressure calculation adds `apfs_snapshot_slack_gb` whenever local
    /// snapshots actually exist. Where there are none, and on Linux always,
    /// the two numbers are the same.
    pub fn pressure(&self) -> Option<Pressure> {
        let probe = nearest_existing(&self.config.quarantine);
        let stats = self.volume.stat(probe).ok()?;
        if stats.total_bytes == 0 {
            return None;
        }
        let snapshots = self.purgeable_snapshots().len();
        let slack = if snapshots > 0 {
            self.config.apfs_snapshot_slack_bytes
        } else {
            0
        };
        let effective = stats.available_bytes.saturating_add(slack);
        Some(Pressure {
            total_bytes: stats.total_bytes,
            available_bytes: stats.available_bytes,
            effective_bytes: effective.min(stats.total_bytes),
            snapshots,
            raw_pct: stats.available_pct(),
            effective_pct: effective.min(stats.total_bytes) as f64 / stats.total_bytes as f64
                * 100.0,
        })
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
    }

    /// Free bytes on the filesystem holding the quarantine directory, or the
    /// nearest existing ancestor of it.
    pub fn free_bytes(&self) -> Option<u64> {
        self.volume
            .stat(nearest_existing(&self.config.quarantine))
            .ok()
            .map(|s| s.available_bytes)
    }
}
