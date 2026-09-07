//! Platform abstraction for `reap`.
//!
//! Everything that has to ask the operating system a question lives behind one
//! of the two traits in this crate. `reap-core` depends only on the traits, so
//! the planner and the guard stack stay free of `#[cfg(target_os)]`.

use std::path::{Path, PathBuf};

use anyhow::Result;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

mod cache;
mod git;
pub use cache::CachedProcessInspector;
pub use git::{GitCli, GitOracle, IgnoreStatus};

/// Free-space accounting for one mounted filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeStats {
    /// Total size of the filesystem in bytes.
    pub total_bytes: u64,
    /// Bytes free as reported by the kernel. On APFS this number is a lie in
    /// the direction of "less free than you really have", because Time Machine
    /// local snapshots hold purgeable space. See [`Volume::purgeable_snapshots`].
    pub available_bytes: u64,
    /// Opaque filesystem identity. Equal values mean "same filesystem" only as
    /// a hint; use [`Volume::same_fs`] for a decision.
    pub fsid: u64,
}

impl VolumeStats {
    /// Available space as a percentage of the total, saturating at 100.
    pub fn available_pct(&self) -> f64 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.available_bytes as f64 / self.total_bytes as f64) * 100.0
    }
}

/// Reads the working directories and open files of live processes.
///
/// Implementations are expected to skip processes owned by other users rather
/// than failing: on both platforms an unprivileged caller cannot inspect every
/// process, and a partial answer is the normal case.
pub trait ProcessInspector: Send + Sync {
    /// Current working directories of every process we are allowed to inspect.
    fn live_cwds(&self) -> Result<Vec<PathBuf>>;
    /// Paths of regular files and directories held open by those processes.
    fn open_paths(&self) -> Result<Vec<PathBuf>>;
}

/// Filesystem-level questions: free space, filesystem identity, snapshots.
pub trait Volume: Send + Sync {
    /// Free-space accounting for the filesystem containing `path`.
    fn stat(&self, path: &Path) -> Result<VolumeStats>;
    /// Whether `a` and `b` live on the same filesystem.
    ///
    /// This is advisory. `rename(2)` across two APFS volumes in the same
    /// container fails with `EXDEV` even though the volumes share free space,
    /// so the executor always confirms with a real test rename.
    fn same_fs(&self, a: &Path, b: &Path) -> Result<bool>;
    /// Names of local snapshots holding purgeable space on the volume
    /// containing `path`. Always empty on Linux.
    fn purgeable_snapshots(&self, path: &Path) -> Result<Vec<String>>;
}

/// The process inspector for the current platform.
pub fn process_inspector() -> Box<dyn ProcessInspector> {
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxProcessInspector)
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacProcessInspector)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        compile_error!("reap supports Linux and macOS only")
    }
}

/// The volume helper for the current platform.
pub fn volume() -> Box<dyn Volume> {
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxVolume)
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacVolume)
    }
}

/// Whether a process with this id currently exists.
///
/// Used as a backstop behind the heartbeat contract: a supervisor that died
/// without removing its heartbeat leaves a stale file, but if the process it
/// described is still running the workspace is still live.
pub fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // Signal 0 performs the permission and existence checks without sending
    // anything. EPERM means the process exists but belongs to someone else,
    // which for our purposes is still "exists".
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
        Ok(()) => true,
        Err(nix::errno::Errno::EPERM) => true,
        Err(_) => false,
    }
}

/// Widens a `statvfs` field to `u64`.
///
/// The field widths differ between Linux and macOS, so the same expression is
/// a no-op conversion on one platform and a real one on the other. Going
/// through a generic keeps both sides honest without a cast or a lint waiver.
pub(crate) fn widen(v: impl Into<u64>) -> u64 {
    v.into()
}

/// Short platform name used in configuration (`os = [...]`) and JSON output.
pub const fn os_name() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "linux"
    }
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
}

/// Machine architecture, reported in JSON output.
pub const fn arch_name() -> &'static str {
    #[cfg(target_arch = "aarch64")]
    {
        "aarch64"
    }
    #[cfg(target_arch = "x86_64")]
    {
        "x86_64"
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        "unknown"
    }
}
