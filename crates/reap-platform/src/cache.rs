use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::Result;

use crate::ProcessInspector;

/// Wraps a [`ProcessInspector`] so the (expensive) inspection happens at most
/// once per run.
///
/// On macOS a full `proc_listpids` plus `proc_pidfdinfo` sweep costs tens of
/// milliseconds and thousands of syscalls; the liveness guard consults it once
/// per candidate, so caching is not optional.
pub struct CachedProcessInspector {
    inner: Box<dyn ProcessInspector>,
    cwds: OnceLock<Vec<PathBuf>>,
    open: OnceLock<Vec<PathBuf>>,
}

impl CachedProcessInspector {
    pub fn new(inner: Box<dyn ProcessInspector>) -> Self {
        Self {
            inner,
            cwds: OnceLock::new(),
            open: OnceLock::new(),
        }
    }
}

impl ProcessInspector for CachedProcessInspector {
    fn live_cwds(&self) -> Result<Vec<PathBuf>> {
        if let Some(v) = self.cwds.get() {
            return Ok(v.clone());
        }
        let v = self.inner.live_cwds()?;
        Ok(self.cwds.get_or_init(|| v).clone())
    }

    fn open_paths(&self) -> Result<Vec<PathBuf>> {
        if let Some(v) = self.open.get() {
            return Ok(v.clone());
        }
        let v = self.inner.open_paths()?;
        Ok(self.open.get_or_init(|| v).clone())
    }
}
