//! The quarantine directory: staging ground between "decided" and "gone".
//!
//! Removal happens in two steps. First an atomic `rename` moves the tree into
//! quarantine, which is O(1) and either happens or does not. Then the contents
//! are removed recursively, which is slow and interruptible. Nothing depends on
//! the second step finishing: an interrupted run leaves orphaned entries that
//! the next run clears, and never a half-deleted tree where the user's build
//! directory used to be.
//!
//! `rename` only works within one filesystem, so the directory has to sit on
//! the same one as every root. `st_dev` is not a reliable test for that on
//! macOS, where two APFS volumes in a container share free space but fail
//! `rename` with `EXDEV`, so this module confirms with a real rename instead.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

/// Prefix for the scratch directory the rename test creates inside a root.
///
/// Deliberately recognisable: if a crash ever leaves one behind, it should be
/// obvious what made it and safe to delete.
pub const TEST_PREFIX: &str = ".reap-rename-test-";

pub struct Quarantine {
    pub dir: PathBuf,
}

/// A short name unique within this process and unlikely to collide across
/// concurrent runs, without pulling in a UUID dependency for the purpose.
pub fn unique_name(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}{}-{nanos:x}-{n:x}", std::process::id())
}

impl Quarantine {
    /// Opens the quarantine directory, creating it if it does not exist.
    pub fn ensure(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| {
            format!(
                "could not create the quarantine directory {}",
                dir.display()
            )
        })?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    /// Confirms that a directory created inside `root` can be renamed into
    /// quarantine.
    ///
    /// This is the check that catches a quarantine directory on the wrong
    /// filesystem, and it runs in `doctor` and again at the top of every sweep.
    /// Falling back to in-place deletion when it fails would trade the entire
    /// interruption guarantee for the convenience of not stopping.
    pub fn test_rename(&self, root: &Path) -> Result<()> {
        let scratch = root.join(unique_name(TEST_PREFIX));
        std::fs::create_dir(&scratch).with_context(|| {
            format!(
                "could not create a scratch directory in {}; reap needs write access to the \
                 roots it reclaims from",
                root.display()
            )
        })?;

        let staged = self.dir.join(unique_name("rename-test-"));
        let result = std::fs::rename(&scratch, &staged);

        // Clean up whichever side survived, on every path out of here.
        let cleanup = |p: &Path| {
            let _ = std::fs::remove_dir_all(p);
        };

        match result {
            Ok(()) => {
                cleanup(&staged);
                Ok(())
            }
            Err(e) => {
                cleanup(&scratch);
                let hint = if e.raw_os_error() == Some(EXDEV) {
                    format!(
                        "\n  {} and {} are on different filesystems. Move `quarantine` in your \
                         config onto the same filesystem as this root.",
                        root.display(),
                        self.dir.display()
                    )
                } else {
                    String::new()
                };
                Err(anyhow::anyhow!(
                    "a test rename from {} into {} failed: {e}{hint}",
                    root.display(),
                    self.dir.display()
                ))
            }
        }
    }
}

/// `EXDEV`, "cross-device link", is 18 on both Linux and macOS.
///
/// `std::io::ErrorKind::CrossesDevices` would be the tidy way to spell this but
/// it is not stable yet, and this is the one error whose message needs a
/// specific fix attached to it.
const EXDEV: i32 = 18;
