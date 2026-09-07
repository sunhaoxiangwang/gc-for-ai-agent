//! macOS implementations.
//!
//! Process inspection goes through `libproc`. The crate exposes `proc_listpids`
//! but not `PROC_PIDVNODEPATHINFO` or `PROC_PIDFDVNODEPATHINFO`, so the two
//! structs and the two entry points are declared here. This is the only module
//! in the workspace containing `unsafe`.

use std::ffi::CStr;
use std::fs;
use std::os::raw::{c_int, c_void};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use libproc::libproc::file_info::{ListFDs, ProcFDInfo, ProcFDType};
use libproc::libproc::proc_pid::listpidinfo;
use libproc::processes::{pids_by_type, ProcFilter};

use crate::{widen, ProcessInspector, Volume, VolumeStats};

const MAXPATHLEN: usize = 1024;
const PROC_PIDVNODEPATHINFO: c_int = 9;
const PROC_PIDFDVNODEPATHINFO: c_int = 2;
/// Upper bound on descriptors queried per process. A process with more open
/// files than this is not the kind of thing we are trying to protect a
/// workspace from, and an unbounded query is a denial-of-service on ourselves.
const MAX_FDS_PER_PROCESS: usize = 4096;

// Layouts mirror <sys/proc_info.h>. Field names match the header so a reader
// can diff them against it.
#[repr(C)]
#[derive(Clone, Copy)]
struct VinfoStat {
    vst_dev: u32,
    vst_mode: u16,
    vst_nlink: u16,
    vst_ino: u64,
    vst_uid: u32,
    vst_gid: u32,
    vst_atime: i64,
    vst_atimensec: i64,
    vst_mtime: i64,
    vst_mtimensec: i64,
    vst_ctime: i64,
    vst_ctimensec: i64,
    vst_birthtime: i64,
    vst_birthtimensec: i64,
    vst_size: i64,
    vst_blocks: i64,
    vst_blksize: i32,
    vst_flags: u32,
    vst_gen: u32,
    vst_rdev: u32,
    vst_qspare: [i64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Fsid {
    val: [i32; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VnodeInfo {
    vi_stat: VinfoStat,
    vi_type: i32,
    vi_pad: i32,
    vi_fsid: Fsid,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VnodeInfoPath {
    vip_vi: VnodeInfo,
    vip_path: [u8; MAXPATHLEN],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcVnodePathInfo {
    pvi_cdir: VnodeInfoPath,
    pvi_rdir: VnodeInfoPath,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcFileInfo {
    fi_openflags: u32,
    fi_status: u32,
    fi_offset: i64,
    fi_type: i32,
    fi_guardflags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VnodeFdInfoWithPath {
    pfi: ProcFileInfo,
    pvip: VnodeInfoPath,
}

// libproc lives inside libSystem on macOS, so no explicit link attribute is
// needed; the symbols resolve against the platform's default libraries.
extern "C" {
    fn proc_pidinfo(
        pid: c_int,
        flavor: c_int,
        arg: u64,
        buffer: *mut c_void,
        buffersize: c_int,
    ) -> c_int;

    fn proc_pidfdinfo(
        pid: c_int,
        fd: c_int,
        flavor: c_int,
        buffer: *mut c_void,
        buffersize: c_int,
    ) -> c_int;
}

fn path_from_buf(buf: &[u8]) -> Option<PathBuf> {
    let cstr = CStr::from_bytes_until_nul(buf).ok()?;
    let s = cstr.to_str().ok()?;
    if s.is_empty() {
        return None;
    }
    Some(PathBuf::from(s))
}

/// Current working directory of one process, or `None` when we are not allowed
/// to ask. `EPERM` for processes owned by another user is expected and common.
fn pid_cwd(pid: i32) -> Option<PathBuf> {
    // SAFETY: `info` is a correctly sized, correctly aligned buffer of exactly
    // the type the PROC_PIDVNODEPATHINFO flavor writes. The kernel writes at
    // most `size` bytes and returns how many it wrote.
    let mut info = std::mem::MaybeUninit::<ProcVnodePathInfo>::zeroed();
    let size = std::mem::size_of::<ProcVnodePathInfo>() as c_int;
    let written = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast::<c_void>(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: the call above wrote a full struct.
    let info = unsafe { info.assume_init() };
    path_from_buf(&info.pvi_cdir.vip_path)
}

/// Paths of the vnode file descriptors held open by one process.
///
/// Errors are swallowed per descriptor: a process can close an fd between the
/// listing and the query, and other users' processes return `EPERM`.
fn pid_open_paths(pid: i32, out: &mut Vec<PathBuf>) {
    let Ok(fds) = listpidinfo::<ListFDs>(pid, MAX_FDS_PER_PROCESS) else {
        return;
    };
    for ProcFDInfo {
        proc_fd,
        proc_fdtype,
    } in fds
    {
        if !matches!(ProcFDType::from(proc_fdtype), ProcFDType::VNode) {
            continue;
        }
        // SAFETY: `info` is a correctly sized and aligned buffer of exactly the
        // type the PROC_PIDFDVNODEPATHINFO flavor writes.
        let mut info = std::mem::MaybeUninit::<VnodeFdInfoWithPath>::zeroed();
        let size = std::mem::size_of::<VnodeFdInfoWithPath>() as c_int;
        let written = unsafe {
            proc_pidfdinfo(
                pid,
                proc_fd,
                PROC_PIDFDVNODEPATHINFO,
                info.as_mut_ptr().cast::<c_void>(),
                size,
            )
        };
        if written != size {
            continue;
        }
        // SAFETY: the call above wrote a full struct.
        let info = unsafe { info.assume_init() };
        if let Some(p) = path_from_buf(&info.pvip.vip_path) {
            out.push(p);
        }
    }
}

/// Every pid we can see. `proc_listpids` returns the whole table; processes we
/// may not inspect fail later, one at a time.
fn all_pids() -> Result<Vec<u32>> {
    pids_by_type(ProcFilter::All).map_err(|e| anyhow::anyhow!("proc_listpids failed: {e}"))
}

pub struct MacProcessInspector;

impl ProcessInspector for MacProcessInspector {
    fn live_cwds(&self) -> Result<Vec<PathBuf>> {
        let pids = all_pids()?;
        let mut out = Vec::with_capacity(pids.len());
        for pid in pids {
            if pid == 0 {
                continue;
            }
            if let Some(p) = pid_cwd(pid as i32) {
                out.push(p);
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    fn open_paths(&self) -> Result<Vec<PathBuf>> {
        let pids = all_pids()?;
        let mut out = Vec::new();
        for pid in pids {
            if pid == 0 {
                continue;
            }
            pid_open_paths(pid as i32, &mut out);
        }
        out.sort();
        out.dedup();
        Ok(out)
    }
}

pub struct MacVolume;

impl Volume for MacVolume {
    fn stat(&self, path: &Path) -> Result<VolumeStats> {
        let st = nix::sys::statvfs::statvfs(path)
            .with_context(|| format!("statvfs failed for {}", path.display()))?;
        // statvfs field widths differ between the two platforms; From<T> for T
        // makes this compile on both without a lossy cast.
        let frsize = widen(st.fragment_size());
        Ok(VolumeStats {
            total_bytes: widen(st.blocks()).saturating_mul(frsize),
            // On APFS this undercounts: space held by Time Machine local
            // snapshots is reported as used even though the kernel will free it
            // on demand. Callers add `apfs_snapshot_slack_gb` before deciding
            // there is disk pressure.
            available_bytes: widen(st.blocks_available()).saturating_mul(frsize),
            fsid: fs::metadata(path)
                .map(|m| m.dev())
                .with_context(|| format!("stat failed for {}", path.display()))?,
        })
    }

    fn same_fs(&self, a: &Path, b: &Path) -> Result<bool> {
        // Advisory only. Two APFS volumes in one container share free space but
        // have different st_dev, and rename(2) between them fails with EXDEV.
        // The executor confirms with a real test rename regardless of this.
        let da = fs::metadata(a)
            .with_context(|| format!("stat failed for {}", a.display()))?
            .dev();
        let db = fs::metadata(b)
            .with_context(|| format!("stat failed for {}", b.display()))?
            .dev();
        Ok(da == db)
    }

    fn purgeable_snapshots(&self, path: &Path) -> Result<Vec<String>> {
        let out = Command::new("tmutil")
            .arg("listlocalsnapshots")
            .arg(path)
            .output()
            .context("failed to run tmutil listlocalsnapshots")?;
        if !out.status.success() {
            // No snapshots, or Time Machine has never been configured. Not an
            // error worth failing a run over.
            return Ok(Vec::new());
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("com.apple.TimeMachine."))
            .map(str::to_owned)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The FFI struct layouts in this module are hand-transcribed from
    /// <sys/proc_info.h>. If any field width drifts, `vip_path` lands at the
    /// wrong offset and this test fails: our own process must always appear.
    #[test]
    fn own_cwd_is_visible() {
        let cwds = MacProcessInspector.live_cwds().expect("live_cwds");
        let me = std::env::current_dir().expect("cwd");
        let me = me.canonicalize().unwrap_or(me);
        assert!(
            cwds.contains(&me),
            "own cwd {} not found among {} inspected cwds",
            me.display(),
            cwds.len()
        );
    }

    /// Same check for the descriptor path flavor: a file we hold open must show
    /// up in the open-path listing.
    #[test]
    fn own_open_file_is_visible() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("held-open");
        let _f = std::fs::File::create(&path).expect("create");
        let want = path.canonicalize().expect("canonicalize");
        let open = MacProcessInspector.open_paths().expect("open_paths");
        assert!(
            open.contains(&want),
            "held-open file {} not found among {} inspected paths",
            want.display(),
            open.len()
        );
    }

    #[test]
    fn volume_stat_reports_a_nonzero_total() {
        let st = MacVolume.stat(Path::new("/")).expect("statvfs /");
        assert!(st.total_bytes > 0);
        assert!(st.available_pct() >= 0.0 && st.available_pct() <= 100.0);
    }
}
