//! Linux implementations: `/proc` for process inspection, `statvfs` for space.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{widen, ProcessInspector, Volume, VolumeStats};

pub struct LinuxProcessInspector;

fn numeric_pid_dirs() -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect()
}

impl ProcessInspector for LinuxProcessInspector {
    fn live_cwds(&self) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for pid_dir in numeric_pid_dirs() {
            // EACCES for another user's process is the normal case, not an error.
            if let Ok(target) = fs::read_link(pid_dir.join("cwd")) {
                out.push(target);
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    fn open_paths(&self) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for pid_dir in numeric_pid_dirs() {
            let Ok(fds) = fs::read_dir(pid_dir.join("fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                if let Ok(target) = fs::read_link(fd.path()) {
                    // Skip sockets, pipes, anon inodes and other non-paths.
                    if target.is_absolute() {
                        out.push(target);
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }
}

pub struct LinuxVolume;

impl Volume for LinuxVolume {
    fn stat(&self, path: &Path) -> Result<VolumeStats> {
        let st = nix::sys::statvfs::statvfs(path)
            .with_context(|| format!("statvfs failed for {}", path.display()))?;
        // statvfs field widths differ between the two platforms; From<T> for T
        // makes this compile on both without a lossy cast.
        let frsize = widen(st.fragment_size());
        Ok(VolumeStats {
            total_bytes: widen(st.blocks()).saturating_mul(frsize),
            // f_bavail: blocks available to an unprivileged process. On Linux
            // this number is trustworthy; there is no purgeable-space concept.
            available_bytes: widen(st.blocks_available()).saturating_mul(frsize),
            fsid: fs::metadata(path)
                .map(|m| m.dev())
                .with_context(|| format!("stat failed for {}", path.display()))?,
        })
    }

    fn same_fs(&self, a: &Path, b: &Path) -> Result<bool> {
        let da = fs::metadata(a)
            .with_context(|| format!("stat failed for {}", a.display()))?
            .dev();
        let db = fs::metadata(b)
            .with_context(|| format!("stat failed for {}", b.display()))?
            .dev();
        Ok(da == db)
    }

    fn purgeable_snapshots(&self, _path: &Path) -> Result<Vec<String>> {
        // Linux has no equivalent of APFS local snapshots holding space that
        // statvfs reports as used. Free space is free space.
        Ok(Vec::new())
    }
}
