//! Run context: the loaded config plus the platform handles, assembled once.

use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use anyhow::Result;
use reap_core::config::{self, Config, LoadContext};
use reap_platform::Volume;

use crate::cli::GlobalArgs;

pub struct Context {
    pub config: Config,
    pub volume: Box<dyn Volume>,
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
        Self {
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

    pub fn elapsed_ms(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
    }

    /// Free bytes on the filesystem holding the quarantine directory, or the
    /// nearest existing ancestor of it.
    pub fn free_bytes(&self) -> Option<u64> {
        let mut probe = self.config.quarantine.as_path();
        loop {
            if probe.exists() {
                return self.volume.stat(probe).ok().map(|s| s.available_bytes);
            }
            probe = probe.parent()?;
        }
    }
}
