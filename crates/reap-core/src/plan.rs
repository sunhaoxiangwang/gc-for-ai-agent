//! The planner: walk the declared roots, match rules, measure, and emit
//! candidates.
//!
//! The planner is pure with respect to the filesystem: it reads and it returns
//! a value. It has no idea that deletion exists. Everything it produces is a
//! proposal that still has to survive the guard stack.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use walkdir::WalkDir;

use crate::config::{Config, Matcher, Rule, RuleKind, Tier};
use crate::fsmeta::{self, IDLE_PROBE_LIMIT};

/// One directory a rule proposes for reclamation.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// The path as walked, before canonicalization. The containment guard
    /// canonicalizes it and compares the result.
    pub path: PathBuf,
    /// The root this candidate was found under.
    pub root: PathBuf,
    /// Name of the rule that matched. Unique, and present in every log line.
    pub rule: String,
    pub tier: Tier,
    /// Depth below the root, where the root itself is 0.
    pub depth: usize,
    /// On-disk size of the tree.
    pub bytes: u64,
    /// How long the directory has looked untouched.
    pub idle: Duration,
    /// The rule's idle requirement, carried along so `explain` can show both
    /// numbers side by side.
    pub min_idle: Duration,
    /// Set when the rule demands positive evidence that no session owns this.
    pub orphaned_only: bool,
}

/// One tool-managed reclamation a command rule proposes.
#[derive(Debug, Clone)]
pub struct CommandCandidate {
    pub rule: String,
    pub tier: Tier,
    /// Literal argv, exactly as configured. Never interpolated.
    pub argv: Vec<String>,
}

/// Why the planner did not walk a root.
#[derive(Debug, Clone)]
pub struct SkippedRoot {
    pub path: PathBuf,
    pub reason: String,
}

/// Everything one planning pass produced.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub candidates: Vec<Candidate>,
    pub commands: Vec<CommandCandidate>,
    /// Roots that do not exist or could not be read. A missing root is a
    /// warning rather than an error so one config file can be shared across
    /// machines with different directory layouts.
    pub skipped_roots: Vec<SkippedRoot>,
    /// Names of rules that matched nothing. `doctor` reports these, because a
    /// rule that never matches is usually a typo.
    pub unmatched_rules: Vec<String>,
    /// Paths the walker could not read.
    pub walk_errors: Vec<String>,
}

impl Plan {
    /// Total on-disk size of every candidate, before guards run.
    pub fn total_bytes(&self) -> u64 {
        self.candidates.iter().map(|c| c.bytes).sum()
    }
}

/// Knobs the planner needs that are not configuration.
#[derive(Debug, Clone, Copy)]
pub struct PlanOptions {
    /// Reference instant for every idle calculation, so one run is consistent
    /// with itself and tests are deterministic.
    pub now: SystemTime,
    /// Highest tier to consider. A plain sweep passes 0.
    pub max_tier: u8,
    /// Cap on the shallow idle probe.
    pub idle_probe_limit: usize,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self {
            now: SystemTime::now(),
            max_tier: Tier::MAX,
            idle_probe_limit: IDLE_PROBE_LIMIT,
        }
    }
}

/// Whether a rule's matcher recognises this directory.
fn matches(rule: &Rule, path: &Path, name: &str, case_sensitive: bool) -> bool {
    match &rule.matcher {
        Matcher::DirName(want) => {
            if case_sensitive {
                name == want
            } else {
                // Rule matching declares its own case behaviour rather than
                // inheriting the filesystem's. APFS is case-insensitive by
                // default and ext4 is not; a config shared between them must
                // not change meaning.
                name.eq_ignore_ascii_case(want)
            }
        }
        Matcher::Glob { matcher, .. } => matcher.is_match(path),
        Matcher::None => false,
    }
}

/// Whether the structural requirements around a candidate hold.
fn structure_ok(rule: &Rule, path: &Path, root: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if let Some(sibling) = &rule.require_sibling {
        if !fsmeta::entry_exists(parent, sibling) {
            return false;
        }
    }
    if let Some(ancestor_marker) = &rule.require_ancestor {
        // Symmetric with require_sibling: an entry with this name must exist in
        // some ancestor directory, starting at the candidate's parent and
        // walking up to (and including) the root.
        let mut cur = Some(parent);
        let mut found = false;
        while let Some(dir) = cur {
            if fsmeta::entry_exists(dir, ancestor_marker) {
                found = true;
                break;
            }
            if dir == root {
                break;
            }
            cur = dir.parent();
        }
        if !found {
            return false;
        }
    }
    true
}

/// The effective depth cap for a rule under a given root.
fn depth_cap(rule: &Rule, root_cap: usize) -> usize {
    rule.max_depth.unwrap_or(root_cap)
}

/// Walks every root and produces a plan.
///
/// `os` is the platform name used to filter rules; it is passed in rather than
/// read from `cfg!` so the planner stays free of conditional compilation and a
/// test can plan for the other platform.
pub fn plan(config: &Config, os: &str, opts: PlanOptions) -> Plan {
    let mut out = Plan::default();
    let mut matched_rules: std::collections::BTreeSet<String> = Default::default();

    let path_rules: Vec<&Rule> = config
        .active_rules(os)
        .filter(|r| r.kind == RuleKind::Path && r.tier.get() <= opts.max_tier)
        .collect();

    for root in &config.roots {
        if path_rules.is_empty() {
            break;
        }
        let md = match std::fs::symlink_metadata(&root.path) {
            Ok(md) => md,
            Err(e) => {
                out.skipped_roots.push(SkippedRoot {
                    path: root.path.clone(),
                    reason: format!("cannot stat: {e}"),
                });
                continue;
            }
        };
        if md.file_type().is_symlink() {
            // A symlinked root would let the containment check compare against
            // a prefix that is not where the walk actually went.
            out.skipped_roots.push(SkippedRoot {
                path: root.path.clone(),
                reason: "root is a symlink; declare the path it resolves to instead".to_owned(),
            });
            continue;
        }
        if !md.is_dir() {
            out.skipped_roots.push(SkippedRoot {
                path: root.path.clone(),
                reason: "not a directory".to_owned(),
            });
            continue;
        }

        // Walk as deep as the deepest rule needs, then apply each rule's own
        // cap when matching.
        let walk_depth = path_rules
            .iter()
            .map(|r| depth_cap(r, root.max_depth))
            .max()
            .unwrap_or(root.max_depth);

        let mut walker = WalkDir::new(&root.path)
            .follow_links(false)
            .max_depth(walk_depth)
            .sort_by_file_name()
            .into_iter();

        while let Some(step) = walker.next() {
            let entry = match step {
                Ok(e) => e,
                Err(e) => {
                    out.walk_errors.push(e.to_string());
                    continue;
                }
            };
            if entry.depth() == 0 {
                continue;
            }
            let ft = entry.file_type();
            if ft.is_symlink() {
                // Never descend through a symlink, and never propose one as a
                // candidate: its target may sit outside every root.
                continue;
            }
            if !ft.is_dir() {
                continue;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();

            let hit = path_rules.iter().find(|rule| {
                entry.depth() <= depth_cap(rule, root.max_depth)
                    && matches(rule, path, &name, config.case_sensitive_matching)
                    && structure_ok(rule, path, &root.path)
            });

            let Some(rule) = hit else { continue };

            let size = fsmeta::tree_size(path);
            let idle = fsmeta::idle_for(path, opts.now, opts.idle_probe_limit).unwrap_or_default();

            matched_rules.insert(rule.name.clone());
            out.candidates.push(Candidate {
                path: path.to_path_buf(),
                root: root.path.clone(),
                rule: rule.name.clone(),
                tier: rule.tier,
                depth: entry.depth(),
                bytes: size.bytes,
                idle,
                min_idle: rule.min_idle,
                orphaned_only: rule.orphaned_only,
            });

            // A matched directory is proposed whole. Descending into it would
            // produce nested candidates whose parent is already going away.
            walker.skip_current_dir();
        }
    }

    for rule in config
        .active_rules(os)
        .filter(|r| r.kind == RuleKind::Command && r.tier.get() <= opts.max_tier)
    {
        out.commands.push(CommandCandidate {
            rule: rule.name.clone(),
            tier: rule.tier,
            argv: rule.command.clone(),
        });
    }

    out.unmatched_rules = config
        .active_rules(os)
        .filter(|r| r.kind == RuleKind::Path && r.tier.get() <= opts.max_tier)
        .filter(|r| !matched_rules.contains(&r.name))
        .map(|r| r.name.clone())
        .collect();

    out.candidates.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

// ---------------------------------------------------------------------------
// Explaining one path
// ---------------------------------------------------------------------------

/// Why one rule did or did not select a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleOutcome {
    Matched,
    /// The rule does not run on this platform.
    WrongOs,
    /// The rule's tier is above the one being considered.
    TierExcluded,
    /// A command rule has no path to match.
    CommandRule,
    /// The path's directory name is not what the rule looks for.
    NameMismatch {
        want: String,
        got: String,
    },
    GlobMismatch {
        pattern: String,
    },
    /// The path lies deeper than the rule's depth cap allows.
    TooDeep {
        depth: usize,
        cap: usize,
    },
    MissingSibling {
        name: String,
    },
    MissingAncestor {
        name: String,
    },
}

impl RuleOutcome {
    pub fn matched(&self) -> bool {
        matches!(self, RuleOutcome::Matched)
    }

    /// One line suitable for `reap explain` output.
    pub fn describe(&self) -> String {
        match self {
            RuleOutcome::Matched => "matched".to_owned(),
            RuleOutcome::WrongOs => "rule does not apply to this platform".to_owned(),
            RuleOutcome::TierExcluded => "rule's tier is above the requested tier".to_owned(),
            RuleOutcome::CommandRule => "command rule, matches no path".to_owned(),
            RuleOutcome::NameMismatch { want, got } => {
                format!("directory is named {got:?}, rule wants {want:?}")
            }
            RuleOutcome::GlobMismatch { pattern } => format!("does not match glob {pattern}"),
            RuleOutcome::TooDeep { depth, cap } => {
                format!("depth {depth} is past the rule's cap of {cap}")
            }
            RuleOutcome::MissingSibling { name } => {
                format!("no {name:?} beside it, which the rule requires")
            }
            RuleOutcome::MissingAncestor { name } => {
                format!("no {name:?} in any ancestor directory, which the rule requires")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuleTrace {
    pub rule: String,
    pub tier: Tier,
    pub outcome: RuleOutcome,
}

/// The result of asking "what does reap think of this path?".
#[derive(Debug, Clone)]
pub struct PathExplanation {
    pub path: PathBuf,
    /// The root the path falls under, if any.
    pub root: Option<PathBuf>,
    /// Depth below that root.
    pub depth: Option<usize>,
    /// Whether the path exists and is a directory we would even consider.
    pub kind: PathKind,
    pub traces: Vec<RuleTrace>,
    /// Populated when some rule matched.
    pub candidate: Option<Candidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathKind {
    Directory,
    Symlink,
    NotADirectory,
    Missing(String),
}

/// Depth of `path` below `root`, or `None` when it is not below it.
fn depth_below(root: &Path, path: &Path) -> Option<usize> {
    path.strip_prefix(root)
        .ok()
        .map(|rest| rest.components().count())
}

/// Evaluates every rule against one path, recording an outcome for each.
///
/// This backs `reap explain`, which is the command a user reaches for the first
/// time the tool surprises them. It deliberately reports on rules that did not
/// match as well as the one that did.
pub fn explain(config: &Config, os: &str, path: &Path, opts: PlanOptions) -> PathExplanation {
    let kind = match std::fs::symlink_metadata(path) {
        Err(e) => PathKind::Missing(e.to_string()),
        Ok(md) if md.file_type().is_symlink() => PathKind::Symlink,
        Ok(md) if md.is_dir() => PathKind::Directory,
        Ok(_) => PathKind::NotADirectory,
    };

    // The innermost declared root containing the path wins, so a nested root
    // with a tighter depth cap takes precedence over an outer one.
    let mut best: Option<(&crate::config::Root, usize)> = None;
    for root in &config.roots {
        if let Some(d) = depth_below(&root.path, path) {
            if d > 0 && best.as_ref().is_none_or(|(_, bd)| d < *bd) {
                best = Some((root, d));
            }
        }
    }

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut traces = Vec::new();
    let mut candidate = None;

    for rule in &config.rules {
        let outcome = if !rule.applies_to_os(os) {
            RuleOutcome::WrongOs
        } else if rule.kind == RuleKind::Command {
            RuleOutcome::CommandRule
        } else if rule.tier.get() > opts.max_tier {
            RuleOutcome::TierExcluded
        } else {
            match &rule.matcher {
                Matcher::DirName(want) => {
                    let hit = if config.case_sensitive_matching {
                        name == *want
                    } else {
                        name.eq_ignore_ascii_case(want)
                    };
                    if hit {
                        RuleOutcome::Matched
                    } else {
                        RuleOutcome::NameMismatch {
                            want: want.clone(),
                            got: name.clone(),
                        }
                    }
                }
                Matcher::Glob { pattern, matcher } => {
                    if matcher.is_match(path) {
                        RuleOutcome::Matched
                    } else {
                        RuleOutcome::GlobMismatch {
                            pattern: pattern.clone(),
                        }
                    }
                }
                Matcher::None => RuleOutcome::CommandRule,
            }
        };

        // Depth and structure are only meaningful once the name or glob hit.
        let outcome = if outcome.matched() {
            match best {
                None => RuleOutcome::Matched,
                Some((root, depth)) => {
                    let cap = depth_cap(rule, root.max_depth);
                    if depth > cap {
                        RuleOutcome::TooDeep { depth, cap }
                    } else if let Some(sib) = &rule.require_sibling {
                        if path.parent().is_some_and(|p| fsmeta::entry_exists(p, sib)) {
                            check_ancestor(rule, path, &root.path)
                        } else {
                            RuleOutcome::MissingSibling { name: sib.clone() }
                        }
                    } else {
                        check_ancestor(rule, path, &root.path)
                    }
                }
            }
        } else {
            outcome
        };

        if outcome.matched() && candidate.is_none() {
            if let Some((root, depth)) = best {
                let size = fsmeta::tree_size(path);
                let idle =
                    fsmeta::idle_for(path, opts.now, opts.idle_probe_limit).unwrap_or_default();
                candidate = Some(Candidate {
                    path: path.to_path_buf(),
                    root: root.path.clone(),
                    rule: rule.name.clone(),
                    tier: rule.tier,
                    depth,
                    bytes: size.bytes,
                    idle,
                    min_idle: rule.min_idle,
                    orphaned_only: rule.orphaned_only,
                });
            }
        }

        traces.push(RuleTrace {
            rule: rule.name.clone(),
            tier: rule.tier,
            outcome,
        });
    }

    PathExplanation {
        path: path.to_path_buf(),
        root: best.map(|(r, _)| r.path.clone()),
        depth: best.map(|(_, d)| d),
        kind,
        traces,
        candidate,
    }
}

fn check_ancestor(rule: &Rule, path: &Path, root: &Path) -> RuleOutcome {
    let Some(marker) = &rule.require_ancestor else {
        return RuleOutcome::Matched;
    };
    let mut cur = path.parent();
    while let Some(dir) = cur {
        if fsmeta::entry_exists(dir, marker) {
            return RuleOutcome::Matched;
        }
        if dir == root {
            break;
        }
        cur = dir.parent();
    }
    RuleOutcome::MissingAncestor {
        name: marker.clone(),
    }
}
