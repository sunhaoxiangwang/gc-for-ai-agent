//! Configuration: the TOML file, its per-machine overlays, and validation.
//!
//! Two types per concept. The `Raw*` types mirror the file exactly and reject
//! unknown keys; the resolved types are what the rest of the program uses, with
//! `~` expanded, defaults filled in, globs compiled and every invariant checked.
//! Nothing downstream has to think about whether a value was present.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use globset::{GlobBuilder, GlobMatcher};
use serde::Deserialize;

use crate::error::ConfigError;

/// Default depth cap applied to a root that does not set its own.
pub const DEFAULT_MAX_DEPTH: usize = 8;
/// Default idle requirement for a path rule that does not set `min_idle`.
///
/// Deliberately long. A rule author who wants a shorter window says so; a rule
/// author who forgot gets the safe end of the range.
pub const DEFAULT_MIN_IDLE: Duration = Duration::from_secs(24 * 60 * 60);
/// Default filename that pins a directory against reclamation.
pub const DEFAULT_PIN_MARKER: &str = ".reap-keep";
/// A heartbeat older than `interval * this` is treated as stale.
pub const DEFAULT_STALENESS_MULTIPLIER: f64 = 3.0;

// ---------------------------------------------------------------------------
// Raw file shapes
// ---------------------------------------------------------------------------

/// The TOML file exactly as written.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    #[serde(default)]
    pub global: RawGlobal,
    /// `[machine."hostname"]` tables. Each accepts the same keys as `[global]`
    /// and overrides them on that host only.
    #[serde(default)]
    pub machine: std::collections::BTreeMap<String, RawGlobal>,
    #[serde(default, rename = "root")]
    pub roots: Vec<RawRoot>,
    #[serde(default, rename = "rule")]
    pub rules: Vec<RawRule>,
}

/// Settings that apply to the whole run. Every field is optional here so a
/// machine overlay can carry a subset.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawGlobal {
    pub dry_run: Option<bool>,
    pub quarantine: Option<String>,
    pub low_water_pct: Option<u8>,
    pub target_free_pct: Option<u8>,
    pub apfs_snapshot_slack_gb: Option<u64>,
    pub max_run_seconds: Option<u64>,
    pub case_sensitive_matching: Option<bool>,
    pub pin_marker: Option<String>,
    pub heartbeat_dir: Option<String>,
    pub heartbeat_staleness_multiplier: Option<f64>,
    pub deny: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRoot {
    pub path: String,
    pub max_depth: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRule {
    pub name: String,
    #[serde(default)]
    pub kind: RuleKind,
    pub tier: u8,
    pub dir_name: Option<String>,
    pub glob: Option<String>,
    pub require_sibling: Option<String>,
    pub require_ancestor: Option<String>,
    #[serde(default, with = "humantime_serde")]
    pub min_idle: Option<Duration>,
    pub orphaned_only: Option<bool>,
    pub max_depth: Option<usize>,
    pub os: Option<Vec<String>>,
    pub command: Option<Vec<String>>,
}

/// What a rule reclaims: a set of paths, or a tool's own garbage collector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleKind {
    /// Match paths and remove them.
    #[default]
    Path,
    /// Run a static argv array and let the tool reclaim its own storage.
    Command,
}

impl RuleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleKind::Path => "path",
            RuleKind::Command => "command",
        }
    }
}

// ---------------------------------------------------------------------------
// Resolved shapes
// ---------------------------------------------------------------------------

/// Rebuild cost, not confidence. Confidence is the guard stack's job and is
/// applied identically at every tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Tier(u8);

impl Tier {
    pub const MAX: u8 = 2;

    pub fn new(v: u8) -> Option<Self> {
        (v <= Self::MAX).then_some(Self(v))
    }

    pub fn get(self) -> u8 {
        self.0
    }

    /// One line describing what the tier means, used by `doctor` and `report`.
    pub fn description(self) -> &'static str {
        match self.0 {
            0 => "near-free to regenerate",
            1 => "regenerable, costs real time",
            _ => "shared cache, slows every later build",
        }
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// How a path rule recognises a candidate.
#[derive(Debug, Clone)]
pub enum Matcher {
    /// The candidate directory's own name, compared whole.
    DirName(String),
    /// The candidate's full path, matched against a compiled glob.
    Glob {
        pattern: String,
        matcher: Box<GlobMatcher>,
    },
    /// Command rules match nothing; they have no path.
    None,
}

#[derive(Debug, Clone)]
pub struct Root {
    /// Canonical-ish absolute path with `~` expanded. Not canonicalized here:
    /// a root that does not exist yet is a warning, not a failure, so one
    /// config can be shared across machines with different layouts.
    pub path: PathBuf,
    /// As written in the file, for error messages.
    pub raw: String,
    pub max_depth: usize,
}

#[derive(Debug, Clone)]
pub struct Rule {
    /// Unique. Appears in every log line and in `--json`.
    pub name: String,
    pub kind: RuleKind,
    pub tier: Tier,
    pub matcher: Matcher,
    /// A file or directory with this name must exist beside the candidate,
    /// i.e. in the candidate's parent directory.
    pub require_sibling: Option<String>,
    /// A file or directory with this name must exist in some ancestor
    /// directory of the candidate, at or above its parent, up to the root.
    pub require_ancestor: Option<String>,
    pub min_idle: Duration,
    /// Select only on positive evidence that no live session owns the
    /// candidate. See `guard::heartbeat`.
    pub orphaned_only: bool,
    /// Overrides the root's depth cap for this rule.
    pub max_depth: Option<usize>,
    /// Platforms this rule applies to. Empty means every platform.
    pub os: Vec<String>,
    /// Static argv for a command rule. Never interpolated.
    pub command: Vec<String>,
}

impl Rule {
    /// Whether this rule runs on the given platform name.
    pub fn applies_to_os(&self, os: &str) -> bool {
        self.os.is_empty() || self.os.iter().any(|o| o == os)
    }
}

/// A fully resolved configuration. Everything downstream reads this.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where the file was loaded from.
    pub source: PathBuf,
    /// Hostname used to select the machine overlay.
    pub hostname: String,
    /// Name of the `[machine."..."]` table that applied, if any.
    pub applied_overlay: Option<String>,

    pub dry_run: bool,
    pub quarantine: PathBuf,
    pub low_water_pct: u8,
    pub target_free_pct: u8,
    pub apfs_snapshot_slack_bytes: u64,
    pub max_run: Duration,
    pub case_sensitive_matching: bool,
    pub pin_marker: String,
    pub heartbeat_dir: PathBuf,
    pub heartbeat_staleness_multiplier: f64,
    /// User-supplied deny paths. Additive to the hardcoded deny floor, which
    /// no configuration can weaken.
    pub deny: Vec<PathBuf>,

    pub roots: Vec<Root>,
    pub rules: Vec<Rule>,
}

impl Config {
    /// Rules that apply on this platform, in file order.
    pub fn active_rules<'a>(&'a self, os: &'a str) -> impl Iterator<Item = &'a Rule> {
        self.rules.iter().filter(move |r| r.applies_to_os(os))
    }

    /// Rules excluded by the current platform, for `doctor` to report.
    pub fn inactive_rules<'a>(&'a self, os: &'a str) -> impl Iterator<Item = &'a Rule> {
        self.rules.iter().filter(move |r| !r.applies_to_os(os))
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Expands a leading `~` using `$HOME`. No other expansion happens: a config
/// value should mean the same thing regardless of the environment it is read
/// in, and shell-style variable expansion in a path that will be deleted is an
/// injection surface we do not need.
pub fn expand_tilde(s: &str, home: Option<&Path>) -> Result<PathBuf, ConfigError> {
    if s == "~" || s.starts_with("~/") {
        let home = home.ok_or_else(|| ConfigError::NoHome { path: s.to_owned() })?;
        return Ok(if s == "~" {
            home.to_path_buf()
        } else {
            home.join(&s[2..])
        });
    }
    Ok(PathBuf::from(s))
}

/// The ordered list of places a config is looked for, given the environment.
///
/// First hit wins. There is no merging across locations: a half-applied config
/// assembled from two files is harder to reason about than one that is simply
/// wrong.
pub fn search_path(
    explicit: Option<&Path>,
    env_config: Option<&str>,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    if let Some(p) = explicit {
        return vec![p.to_path_buf()];
    }
    let mut out = Vec::new();
    if let Some(e) = env_config {
        out.push(PathBuf::from(e));
    }
    if let Some(h) = home {
        out.push(h.join(".config/reap/reap.toml"));
    }
    out.push(PathBuf::from("/etc/reap/reap.toml"));
    out
}

/// Inputs the loader needs from the outside world, passed in rather than read
/// from the process environment so tests can drive every branch.
#[derive(Debug, Clone)]
pub struct LoadContext {
    pub home: Option<PathBuf>,
    pub hostname: String,
    pub explicit: Option<PathBuf>,
    pub env_config: Option<String>,
}

/// Finds and loads the configuration.
pub fn load(ctx: &LoadContext) -> Result<Config, ConfigError> {
    let candidates = search_path(
        ctx.explicit.as_deref(),
        ctx.env_config.as_deref(),
        ctx.home.as_deref(),
    );
    for path in &candidates {
        if path.is_file() {
            return load_from(path, ctx);
        }
    }
    // An explicitly named file that is missing is an error about that file,
    // not about the search path.
    if let Some(explicit) = &ctx.explicit {
        return Err(ConfigError::Read {
            path: explicit.clone(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        });
    }
    Err(ConfigError::NotFound {
        searched: candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
    })
}

/// Loads one specific file.
pub fn load_from(path: &Path, ctx: &LoadContext) -> Result<Config, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&text, path, ctx)
}

/// Parses and validates configuration text.
pub fn parse(text: &str, source: &Path, ctx: &LoadContext) -> Result<Config, ConfigError> {
    let raw: RawConfig = toml::from_str(text).map_err(|source_err| ConfigError::Parse {
        path: source.to_path_buf(),
        source: source_err,
    })?;
    resolve(raw, source, ctx)
}

fn invalid(source: &Path, message: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        path: source.to_path_buf(),
        message: message.into(),
    }
}

fn resolve(raw: RawConfig, source: &Path, ctx: &LoadContext) -> Result<Config, ConfigError> {
    let home = ctx.home.as_deref();

    // Machine overlay. Matching is on the exact hostname first, then on the
    // hostname with its DNS suffix removed, so `[machine."buildbox"]` applies
    // on `buildbox.example.com` too.
    let short = ctx.hostname.split('.').next().unwrap_or(&ctx.hostname);
    let (applied_overlay, overlay) = if let Some(o) = raw.machine.get(&ctx.hostname) {
        (Some(ctx.hostname.clone()), Some(o))
    } else if let Some(o) = raw.machine.get(short) {
        (Some(short.to_owned()), Some(o))
    } else {
        (None, None)
    };

    macro_rules! pick {
        ($field:ident) => {
            overlay
                .and_then(|o| o.$field.clone())
                .or_else(|| raw.global.$field.clone())
        };
    }

    let dry_run = pick!(dry_run).unwrap_or(true);
    let low_water_pct = pick!(low_water_pct).unwrap_or(15);
    let target_free_pct = pick!(target_free_pct).unwrap_or(25);
    let apfs_snapshot_slack_gb = pick!(apfs_snapshot_slack_gb).unwrap_or(20);
    let max_run_seconds = pick!(max_run_seconds).unwrap_or(300);
    let case_sensitive_matching = pick!(case_sensitive_matching).unwrap_or(true);
    let pin_marker = pick!(pin_marker).unwrap_or_else(|| DEFAULT_PIN_MARKER.to_owned());
    let staleness = pick!(heartbeat_staleness_multiplier).unwrap_or(DEFAULT_STALENESS_MULTIPLIER);
    let quarantine = expand_tilde(
        &pick!(quarantine).unwrap_or_else(|| "~/.cache/reap/quarantine".to_owned()),
        home,
    )?;
    let heartbeat_dir = expand_tilde(
        &pick!(heartbeat_dir).unwrap_or_else(|| "~/.cache/reap/heartbeats".to_owned()),
        home,
    )?;

    if low_water_pct > 100 {
        return Err(invalid(source, "low_water_pct must be between 0 and 100"));
    }
    if target_free_pct > 100 {
        return Err(invalid(source, "target_free_pct must be between 0 and 100"));
    }
    if target_free_pct < low_water_pct {
        return Err(invalid(
            source,
            format!(
                "target_free_pct ({target_free_pct}) is below low_water_pct ({low_water_pct}); \
                 a sweep triggered at the low-water mark could never reach its target"
            ),
        ));
    }
    if !(staleness.is_finite() && staleness >= 1.0) {
        return Err(invalid(
            source,
            "heartbeat_staleness_multiplier must be a finite number >= 1.0",
        ));
    }
    if pin_marker.is_empty() || pin_marker.contains('/') {
        return Err(invalid(
            source,
            "pin_marker must be a bare filename, not a path",
        ));
    }
    if !quarantine.is_absolute() {
        return Err(invalid(
            source,
            format!(
                "quarantine must be an absolute path, got {}",
                quarantine.display()
            ),
        ));
    }

    let mut deny = Vec::new();
    for d in pick!(deny).unwrap_or_default() {
        deny.push(expand_tilde(&d, home)?);
    }

    // Roots.
    if raw.roots.is_empty() && raw.rules.iter().any(|r| r.kind == RuleKind::Path) {
        return Err(invalid(
            source,
            "path rules are configured but no [[root]] is declared; \
             nothing outside a declared root is ever considered",
        ));
    }
    let mut roots = Vec::new();
    let mut seen_roots = BTreeSet::new();
    for r in &raw.roots {
        let path = expand_tilde(&r.path, home)?;
        if !path.is_absolute() {
            return Err(invalid(
                source,
                format!("root path must be absolute, got {}", r.path),
            ));
        }
        if !seen_roots.insert(path.clone()) {
            return Err(invalid(
                source,
                format!("root {} is declared more than once", r.path),
            ));
        }
        let max_depth = r.max_depth.unwrap_or(DEFAULT_MAX_DEPTH);
        if max_depth == 0 {
            return Err(invalid(
                source,
                format!(
                    "root {} has max_depth = 0, which can never match anything",
                    r.path
                ),
            ));
        }
        roots.push(Root {
            path,
            raw: r.path.clone(),
            max_depth,
        });
    }

    // Rules.
    let mut rules = Vec::new();
    let mut seen_names = BTreeSet::new();
    for r in &raw.rules {
        let name = r.name.clone();
        if name.is_empty() {
            return Err(invalid(source, "a rule has an empty name"));
        }
        if !seen_names.insert(name.clone()) {
            return Err(invalid(
                source,
                format!("rule name {name:?} is used more than once; names must be unique"),
            ));
        }
        let tier = Tier::new(r.tier).ok_or_else(|| {
            invalid(
                source,
                format!("rule {name:?} has tier {}, but tiers are 0, 1 or 2", r.tier),
            )
        })?;
        let os = r.os.clone().unwrap_or_default();
        for o in &os {
            if o != "linux" && o != "macos" {
                return Err(invalid(
                    source,
                    format!("rule {name:?} lists os = {o:?}; only \"linux\" and \"macos\" exist"),
                ));
            }
        }

        let rule = match r.kind {
            RuleKind::Command => {
                let command = r.command.clone().unwrap_or_default();
                if command.is_empty() {
                    return Err(invalid(
                        source,
                        format!("rule {name:?} is kind = \"command\" but has no command array"),
                    ));
                }
                for arg in &command {
                    // Command arrays are executed directly, never through a
                    // shell, and are never built from anything discovered at
                    // run time. Rejecting substitution syntax makes that
                    // guarantee visible in the config rather than implicit.
                    if arg.contains('$') || arg.contains('`') {
                        return Err(invalid(
                            source,
                            format!(
                                "rule {name:?} command argument {arg:?} contains substitution \
                                 syntax; command arrays must be literal"
                            ),
                        ));
                    }
                }
                for (field, present) in [
                    ("dir_name", r.dir_name.is_some()),
                    ("glob", r.glob.is_some()),
                    ("require_sibling", r.require_sibling.is_some()),
                    ("require_ancestor", r.require_ancestor.is_some()),
                    ("min_idle", r.min_idle.is_some()),
                    ("orphaned_only", r.orphaned_only.is_some()),
                    ("max_depth", r.max_depth.is_some()),
                ] {
                    if present {
                        return Err(invalid(
                            source,
                            format!(
                                "rule {name:?} is kind = \"command\" but sets {field}, which only \
                                 applies to path rules"
                            ),
                        ));
                    }
                }
                Rule {
                    name,
                    kind: RuleKind::Command,
                    tier,
                    matcher: Matcher::None,
                    require_sibling: None,
                    require_ancestor: None,
                    min_idle: Duration::ZERO,
                    orphaned_only: false,
                    max_depth: None,
                    os,
                    command,
                }
            }
            RuleKind::Path => {
                if r.command.is_some() {
                    return Err(invalid(
                        source,
                        format!("rule {name:?} is kind = \"path\" but sets command"),
                    ));
                }
                let matcher = match (&r.dir_name, &r.glob) {
                    (Some(d), None) => {
                        if d.contains('/') {
                            return Err(invalid(
                                source,
                                format!(
                                    "rule {name:?} dir_name {d:?} contains a slash; dir_name \
                                     matches a single directory name, use glob for paths"
                                ),
                            ));
                        }
                        Matcher::DirName(d.clone())
                    }
                    (None, Some(g)) => {
                        let expanded = expand_tilde(g, home)?;
                        let pattern = expanded.to_string_lossy().into_owned();
                        if pattern.split('/').any(|c| c == "..") {
                            return Err(invalid(
                                source,
                                format!(
                                    "rule {name:?} glob {g:?} contains a `..` component; \
                                     globs must name paths directly"
                                ),
                            ));
                        }
                        let glob = GlobBuilder::new(&pattern)
                            .case_insensitive(!case_sensitive_matching)
                            .literal_separator(true)
                            .build()
                            .map_err(|e| {
                                invalid(source, format!("rule {name:?} has an invalid glob: {e}"))
                            })?;
                        Matcher::Glob {
                            pattern,
                            matcher: Box::new(glob.compile_matcher()),
                        }
                    }
                    (None, None) => {
                        return Err(invalid(
                            source,
                            format!("rule {name:?} sets neither dir_name nor glob"),
                        ))
                    }
                    (Some(_), Some(_)) => {
                        return Err(invalid(
                            source,
                            format!(
                                "rule {name:?} sets both dir_name and glob; a rule matches one way"
                            ),
                        ))
                    }
                };
                for (field, value) in [
                    ("require_sibling", &r.require_sibling),
                    ("require_ancestor", &r.require_ancestor),
                ] {
                    if let Some(v) = value {
                        if v.is_empty() || v.contains('/') {
                            return Err(invalid(
                                source,
                                format!("rule {name:?} {field} must be a bare filename"),
                            ));
                        }
                    }
                }
                Rule {
                    name,
                    kind: RuleKind::Path,
                    tier,
                    matcher,
                    require_sibling: r.require_sibling.clone(),
                    require_ancestor: r.require_ancestor.clone(),
                    min_idle: r.min_idle.unwrap_or(DEFAULT_MIN_IDLE),
                    orphaned_only: r.orphaned_only.unwrap_or(false),
                    max_depth: r.max_depth,
                    os,
                    command: Vec::new(),
                }
            }
        };
        rules.push(rule);
    }

    if rules.is_empty() {
        return Err(invalid(
            source,
            "no [[rule]] is declared; reap deletes nothing unless a rule names it",
        ));
    }

    Ok(Config {
        source: source.to_path_buf(),
        hostname: ctx.hostname.clone(),
        applied_overlay,
        dry_run,
        quarantine,
        low_water_pct,
        target_free_pct,
        apfs_snapshot_slack_bytes: apfs_snapshot_slack_gb.saturating_mul(1024 * 1024 * 1024),
        max_run: Duration::from_secs(max_run_seconds),
        case_sensitive_matching,
        pin_marker,
        heartbeat_dir,
        heartbeat_staleness_multiplier: staleness,
        deny,
        roots,
        rules,
    })
}
