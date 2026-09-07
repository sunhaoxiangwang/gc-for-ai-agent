//! The `--json` schema.
//!
//! This is a public interface from the first release. Fields may be added; a
//! field that exists never changes meaning or disappears without a version bump
//! and a CHANGELOG migration note. `schema_version` identifies the shape.

use serde::Serialize;

/// Bumped only for a breaking change to the shape below.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub schema_version: u32,
    /// Version of the `reap` binary that produced this.
    pub reap_version: String,
    pub host: String,
    pub os: String,
    pub arch: String,
    /// The subcommand that ran: "report", "sweep", "explain", "doctor", ...
    pub command: String,
    /// Path of the configuration file that was used.
    pub config: String,
    pub started_at: String,
    pub duration_ms: u64,
    /// Free bytes on the filesystem holding the quarantine directory, before
    /// and after. Null when it could not be measured.
    pub free_before_bytes: Option<u64>,
    pub free_after_bytes: Option<u64>,
    /// True only when this run actually mutated the filesystem.
    pub applied: bool,
    /// The `dry_run` setting in effect.
    pub dry_run: bool,
    /// For `sweep --until-free`, the tier the run stopped at.
    pub stopped_at_tier: Option<u8>,
    pub candidates: Vec<CandidateReport>,
    pub commands: Vec<CommandReport>,
    pub totals: Totals,
    /// Human-readable remarks that are not errors: skipped roots, rules that
    /// matched nothing, a run that hit its time budget.
    pub notes: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateReport {
    pub path: String,
    pub root: String,
    pub rule: String,
    pub tier: u8,
    pub depth: usize,
    pub bytes: u64,
    pub idle_seconds: u64,
    /// True when the candidate survived every guard.
    pub selected: bool,
    /// Name of the first guard that rejected it, or null when selected.
    pub rejected_by: Option<String>,
    /// Human-readable explanation of the rejection.
    pub reason: Option<String>,
    /// True when this candidate was actually removed on this run.
    pub reclaimed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandReport {
    pub rule: String,
    pub tier: u8,
    pub argv: Vec<String>,
    pub executed: bool,
    /// Bytes the tool reported reclaiming, where its output says so.
    pub reclaimed_bytes: Option<u64>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub candidates: usize,
    pub selected: usize,
    pub rejected: usize,
    pub selected_bytes: u64,
    pub reclaimed_bytes: u64,
    pub commands_run: usize,
    pub errors: usize,
}
