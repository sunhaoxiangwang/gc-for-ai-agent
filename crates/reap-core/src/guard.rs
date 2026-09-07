//! The guard stack.
//!
//! Eight predicates, in a fixed order, cheap and decisive first. A candidate is
//! reclaimable only if every one of them passes. Each records why it said what
//! it said, so `reap explain` can show the whole chain and `--json` can carry
//! the rejection reason.
//!
//! The order is deliberate. The deny floor and the containment check are the
//! ones that stop catastrophe, cost almost nothing, and must therefore run
//! before anything that could go wrong on its own. The git-ignore check comes
//! before the two most expensive guards because it is the single predicate that
//! eliminates most of the ways a reaper eats source code.

use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use reap_platform::{GitOracle, IgnoreStatus, ProcessInspector};

use crate::config::Config;
use crate::heartbeat::HeartbeatIndex;
use crate::plan::Candidate;

/// Stable identifiers for the guards, used in output and in tests.
pub mod names {
    pub const DENY_FLOOR: &str = "deny-floor";
    pub const CONTAINMENT: &str = "containment";
    pub const DEPTH: &str = "depth";
    pub const PIN_MARKER: &str = "pin-marker";
    pub const HEARTBEAT: &str = "heartbeat";
    pub const GIT_IGNORE: &str = "git-ignore";
    pub const IDLE_AGE: &str = "idle-age";
    pub const PROCESS_LIVENESS: &str = "process-liveness";

    /// Every guard, in the order they run.
    pub const ORDER: [&str; 8] = [
        DENY_FLOOR,
        CONTAINMENT,
        DEPTH,
        PIN_MARKER,
        HEARTBEAT,
        GIT_IGNORE,
        IDLE_AGE,
        PROCESS_LIVENESS,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Pass(String),
    Reject(String),
    /// The guard did not apply. A skipped guard never permits anything the
    /// other guards would have refused.
    Skipped(String),
    /// The guard did not run because an earlier one already rejected.
    NotReached,
}

impl Verdict {
    pub fn rejected(&self) -> bool {
        matches!(self, Verdict::Reject(_))
    }

    pub fn detail(&self) -> &str {
        match self {
            Verdict::Pass(s) | Verdict::Reject(s) | Verdict::Skipped(s) => s,
            Verdict::NotReached => "an earlier guard already rejected this path",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Verdict::Pass(_) => "pass",
            Verdict::Reject(_) => "fail",
            Verdict::Skipped(_) => "skipped",
            Verdict::NotReached => "not reached",
        }
    }
}

#[derive(Debug, Clone)]
pub struct GuardResult {
    pub guard: &'static str,
    pub verdict: Verdict,
}

/// The outcome of running the whole stack against one candidate.
#[derive(Debug, Clone)]
pub struct Decision {
    pub selected: bool,
    pub results: Vec<GuardResult>,
    pub rejected_by: Option<&'static str>,
    pub reason: Option<String>,
    /// The canonical path, available once containment has passed. The executor
    /// operates on this, never on the path as walked.
    pub canonical: Option<PathBuf>,
}

/// Everything the guards need from outside themselves.
pub struct GuardContext<'a> {
    pub config: &'a Config,
    pub now: SystemTime,
    pub inspector: &'a dyn ProcessInspector,
    pub git: &'a dyn GitOracle,
    pub heartbeats: &'a HeartbeatIndex,
    /// Whether a pid names a running process. Injected so tests can drive the
    /// heartbeat backstop without spawning anything.
    pub pid_alive: &'a dyn Fn(u32) -> bool,
    /// Canonicalized declared roots. Computed once per run.
    pub roots: &'a [PathBuf],
}

// ---------------------------------------------------------------------------
// Guard 1: the deny floor
// ---------------------------------------------------------------------------

/// Path components that are never inside anything reap will remove, wherever
/// they appear.
///
/// `.git` is here because a rule that reached into a repository's object store
/// would destroy history that no build can regenerate. The credential
/// directories are here because the cost of being wrong about them is somebody
/// else's problem too.
const FORBIDDEN_COMPONENTS: &[&str] =
    &[".git", ".ssh", ".gnupg", ".aws", ".kube", ".password-store"];

/// Absolute paths that are never removed as a whole, whatever the config says.
const FORBIDDEN_EXACT: &[&str] = &[
    "/",
    "/Applications",
    "/bin",
    "/boot",
    "/dev",
    "/etc",
    "/home",
    "/Library",
    "/opt",
    "/private",
    "/proc",
    "/root",
    "/sbin",
    "/srv",
    "/sys",
    "/System",
    "/tmp",
    "/usr",
    "/Users",
    "/var",
    "/Volumes",
];

/// Directories directly under the home directory that are never removed whole.
const FORBIDDEN_IN_HOME: &[&str] = &[
    "Applications",
    "Desktop",
    "Documents",
    "Downloads",
    "Library",
    "Movies",
    "Music",
    "Pictures",
    "Public",
    ".config",
    ".local",
    ".ssh",
];

/// The minimum number of path components below the filesystem root.
///
/// `/a/b` has two and is refused; `/a/b/c` has three and is considered. This is
/// a blunt instrument on purpose: every path shallow enough to fail it is a
/// path whose removal would be a catastrophe rather than a cleanup.
pub const MIN_COMPONENTS: usize = 3;

/// Counts the components below the filesystem root.
fn component_count(path: &Path) -> usize {
    path.components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count()
}

/// The hardcoded floor, evaluated before any configuration is consulted.
///
/// Nothing in a config file can weaken this. It is checked against both the
/// path as walked and its canonical form, because the two can differ.
pub fn deny_floor(path: &Path, home: Option<&Path>) -> Verdict {
    if component_count(path) < MIN_COMPONENTS {
        return Verdict::Reject(format!(
            "{} has fewer than {MIN_COMPONENTS} path components",
            path.display()
        ));
    }
    for exact in FORBIDDEN_EXACT {
        if path == Path::new(exact) {
            return Verdict::Reject(format!("{exact} is never removable"));
        }
    }
    if let Some(home) = home {
        if path == home {
            return Verdict::Reject("the home directory itself is never removable".to_owned());
        }
        for name in FORBIDDEN_IN_HOME {
            if path == home.join(name) {
                return Verdict::Reject(format!("~/{name} is never removable"));
            }
        }
    }
    for comp in path.components() {
        if let Component::Normal(c) = comp {
            let c = c.to_string_lossy();
            if FORBIDDEN_COMPONENTS.contains(&c.as_ref()) {
                return Verdict::Reject(format!(
                    "{} contains a {c:?} component, which reap never removes or removes from",
                    path.display()
                ));
            }
        }
    }
    Verdict::Pass("not on the hardcoded deny floor".to_owned())
}

/// The user's own deny list, applied after the floor.
fn user_deny(path: &Path, deny: &[PathBuf]) -> Verdict {
    for d in deny {
        if path == d || path.starts_with(d) {
            return Verdict::Reject(format!(
                "{} is under the configured deny path {}",
                path.display(),
                d.display()
            ));
        }
    }
    Verdict::Pass("not on the configured deny list".to_owned())
}

// ---------------------------------------------------------------------------
// Guard 2: canonicalize and contain
// ---------------------------------------------------------------------------

/// Result of the containment check.
pub struct Containment {
    pub verdict: Verdict,
    pub canonical: Option<PathBuf>,
}

/// Canonicalizes the candidate and proves it is still inside a declared root.
///
/// Three separate things have to hold:
///
/// * the path resolves;
/// * no component between the declared root and the candidate is a symlink, so
///   the path we checked is the path we would act on;
/// * the canonical result is a *strict* descendant of a canonical root, so a
///   root can never delete itself.
///
/// The scan for symlinked components runs from the declared root downwards.
/// Above the root it would be both noisy and pointless: `/tmp` is a symlink to
/// `/private/tmp` on every macOS machine, and a symlink above the root cannot
/// move the candidate out of the root, because containment is decided on the
/// canonical form of both. A symlink *below* the root can, which is why that
/// region is scanned one component at a time.
pub fn containment(path: &Path, declared_root: &Path, canonical_roots: &[PathBuf]) -> Containment {
    let reject = |msg: String| Containment {
        verdict: Verdict::Reject(msg),
        canonical: None,
    };

    let canonical = match path.canonicalize() {
        Ok(c) => c,
        Err(e) => return reject(format!("cannot canonicalize {}: {e}", path.display())),
    };

    // The candidate must sit under the root it was walked from, spelled the
    // same way, or we have no region to scan for symlinks.
    let Ok(rest) = path.strip_prefix(declared_root) else {
        return reject(format!(
            "{} is not below the root it was found under ({})",
            path.display(),
            declared_root.display()
        ));
    };
    if rest.as_os_str().is_empty() {
        return reject(format!(
            "{} is a declared root; reap only removes strict descendants",
            path.display()
        ));
    }

    // The symlink scan runs before the containment decision so that a symlink
    // pointing out of the root is diagnosed as a symlink rather than as a
    // path that happens to resolve elsewhere. Both are rejections; only one
    // tells the user what to fix.
    let mut cur = declared_root.to_path_buf();
    for comp in rest.components() {
        cur.push(comp);
        match std::fs::symlink_metadata(&cur) {
            Ok(md) if md.file_type().is_symlink() => {
                return reject(format!(
                    "{} is a symlink, so {} is reached through one",
                    cur.display(),
                    path.display()
                ))
            }
            Ok(_) => {}
            Err(e) => return reject(format!("cannot stat {}: {e}", cur.display())),
        }
    }

    let Some(root) = canonical_roots
        .iter()
        .find(|r| canonical.starts_with(r) && canonical != **r)
    else {
        return reject(format!(
            "{} resolves to {}, which is not strictly inside any declared root",
            path.display(),
            canonical.display()
        ));
    };

    // The declared root and the canonical root must describe the same
    // directory. If they do not, the walk and the containment decision are
    // talking about two different places.
    match declared_root.canonicalize() {
        Ok(c) if &c == root => {}
        Ok(c) => {
            return reject(format!(
                "root {} resolves to {}, not to the root {} that contains this path",
                declared_root.display(),
                c.display(),
                root.display()
            ))
        }
        Err(e) => {
            return reject(format!(
                "cannot canonicalize root {}: {e}",
                declared_root.display()
            ))
        }
    }

    Containment {
        verdict: Verdict::Pass(format!("strictly inside {}", root.display())),
        canonical: Some(canonical),
    }
}

// ---------------------------------------------------------------------------
// Guard 4: the pin marker
// ---------------------------------------------------------------------------

/// Rejects if the marker file exists in the candidate or in any ancestor up to
/// and including the root.
///
/// This is the user's escape hatch and it is deliberately the simplest thing in
/// the program: create a file, and everything below it is off limits forever,
/// with no config change and no restart.
pub fn pin_marker(path: &Path, root: &Path, marker: &str) -> Verdict {
    let mut cur = Some(path);
    while let Some(dir) = cur {
        if crate::fsmeta::entry_exists(dir, marker) {
            return Verdict::Reject(format!("{}/{marker} pins this path", dir.display()));
        }
        if dir == root {
            break;
        }
        cur = dir.parent();
    }
    Verdict::Pass(format!("no {marker} here or above, up to the root"))
}

// ---------------------------------------------------------------------------
// Guard 5: heartbeats
// ---------------------------------------------------------------------------

/// Whether `a` contains `b`, or they are the same path.
fn overlaps(a: &Path, b: &Path) -> bool {
    a == b || a.starts_with(b) || b.starts_with(a)
}

/// Rejects a candidate that a live heartbeat claims.
///
/// A live heartbeat protects the workspace it names, anything inside it, and
/// anything containing it. It also protects a directory whose own name is the
/// session id, which is how a supervisor's scratch directories are covered
/// without listing each one.
pub fn heartbeat(candidate: &Candidate, ctx: &GuardContext<'_>) -> Verdict {
    let multiplier = ctx.config.heartbeat_staleness_multiplier;
    let name = candidate
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());

    for hb in ctx.heartbeats.live(ctx.now, multiplier, ctx.pid_alive) {
        if let Some(reason) = &hb.unreadable {
            if hb.paths.is_empty() && Some(&hb.session_id) != name.as_ref() {
                // An unreadable heartbeat that names nothing we can compare
                // against does not protect this particular path.
                continue;
            }
            return Verdict::Reject(format!(
                "heartbeat {} could not be read ({reason}); an unreadable claim is still a claim",
                hb.file.display()
            ));
        }
        if Some(&hb.session_id) == name.as_ref() {
            return Verdict::Reject(format!("session {} is live", hb.session_id));
        }
        for p in &hb.paths {
            if overlaps(&candidate.path, p) {
                return Verdict::Reject(format!(
                    "live session {} claims {}",
                    hb.session_id,
                    p.display()
                ));
            }
        }
    }

    if candidate.orphaned_only {
        // This rule wants positive evidence that nothing owns the candidate.
        // "We could not check" is not that evidence.
        if let Some(reason) = &ctx.heartbeats.unreadable {
            return Verdict::Reject(format!(
                "rule requires positive evidence of orphanhood, but the heartbeat directory \
                 could not be read: {reason}"
            ));
        }
        return Verdict::Pass(format!(
            "heartbeat directory is readable and none of its {} entries claims this path",
            ctx.heartbeats.entries.len()
        ));
    }

    Verdict::Pass("no live heartbeat claims this path".to_owned())
}

// ---------------------------------------------------------------------------
// Guard 6: git ignore
// ---------------------------------------------------------------------------

/// Finds the work tree containing `path`, by looking for a `.git` entry in it
/// or any ancestor.
///
/// `.git` may be a directory or, in a worktree or submodule, a file.
pub fn find_worktree(path: &Path) -> Option<PathBuf> {
    let mut cur = Some(path);
    while let Some(dir) = cur {
        if crate::fsmeta::entry_exists(dir, ".git") {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

/// Requires that git considers the candidate ignored.
///
/// Anything git tracks, or would track, is somebody's work. This one predicate
/// removes most of the ways a reclaimer can eat source code, which is why an
/// inconclusive answer counts as a rejection: no git, a broken repository, or a
/// refusal all mean we do not know, and not knowing is not permission.
pub fn git_ignore(path: &Path, git: &dyn GitOracle) -> Verdict {
    let Some(worktree) = find_worktree(path) else {
        return Verdict::Skipped("not inside a git work tree".to_owned());
    };
    match git.check_ignore(&worktree, path) {
        IgnoreStatus::Ignored => Verdict::Pass(format!(
            "git in {} considers it ignored",
            worktree.display()
        )),
        IgnoreStatus::NotIgnored => Verdict::Reject(format!(
            "git in {} does not ignore this path, so it is tracked work",
            worktree.display()
        )),
        IgnoreStatus::Unknown(why) => Verdict::Reject(format!(
            "could not ask git about {}: {why}",
            worktree.display()
        )),
    }
}

// ---------------------------------------------------------------------------
// Guard 8: process liveness
// ---------------------------------------------------------------------------

/// Rejects a candidate that a running process is working inside.
///
/// This is the backstop behind the heartbeat contract, for the case where the
/// supervisor never wrote one. Failure to inspect is a rejection: we cannot
/// show the directory is idle, so we do not act as though it is.
pub fn process_liveness(path: &Path, inspector: &dyn ProcessInspector) -> Verdict {
    let cwds = match inspector.live_cwds() {
        Ok(v) => v,
        Err(e) => {
            return Verdict::Reject(format!("could not inspect running processes: {e}"));
        }
    };
    if let Some(hit) = cwds.iter().find(|p| p.starts_with(path)) {
        return Verdict::Reject(format!(
            "a running process has its working directory at {}",
            hit.display()
        ));
    }

    let open = match inspector.open_paths() {
        Ok(v) => v,
        Err(e) => {
            return Verdict::Reject(format!("could not inspect open files: {e}"));
        }
    };
    if let Some(hit) = open.iter().find(|p| p.starts_with(path)) {
        return Verdict::Reject(format!("a running process holds {} open", hit.display()));
    }

    Verdict::Pass("no running process is working inside it".to_owned())
}

// ---------------------------------------------------------------------------
// The stack
// ---------------------------------------------------------------------------

/// Runs every guard in order against one candidate.
///
/// Evaluation stops at the first rejection; the remaining guards are recorded
/// as `NotReached` so `explain` shows the full stack either way.
pub fn evaluate(candidate: &Candidate, ctx: &GuardContext<'_>) -> Decision {
    let home = home_of(ctx.config);
    let mut results: Vec<GuardResult> = Vec::with_capacity(names::ORDER.len());
    let mut canonical = None;

    let push = |guard: &'static str, verdict: Verdict, results: &mut Vec<GuardResult>| {
        let rejected = verdict.rejected();
        results.push(GuardResult { guard, verdict });
        rejected
    };

    // 1. Deny floor: hardcoded first, then the user's additions.
    let floor = match deny_floor(&candidate.path, home.as_deref()) {
        Verdict::Pass(_) => user_deny(&candidate.path, &ctx.config.deny),
        other => other,
    };
    if push(names::DENY_FLOOR, floor, &mut results) {
        return finish(results, canonical);
    }

    // 2. Canonicalize and contain.
    let contained = containment(&candidate.path, &candidate.root, ctx.roots);
    canonical = contained.canonical.clone();
    if push(names::CONTAINMENT, contained.verdict, &mut results) {
        return finish(results, None);
    }
    // The floor applies to the resolved path too: canonicalization can land
    // somewhere the original spelling did not reveal.
    if let Some(canon) = &canonical {
        if canon != &candidate.path {
            if let v @ Verdict::Reject(_) = deny_floor(canon, home.as_deref()) {
                results.push(GuardResult {
                    guard: names::DENY_FLOOR,
                    verdict: v,
                });
                return finish(results, None);
            }
        }
    }

    // 3. Depth.
    let depth = if candidate.depth > candidate.depth_cap {
        Verdict::Reject(format!(
            "depth {} is past the cap of {}",
            candidate.depth, candidate.depth_cap
        ))
    } else {
        Verdict::Pass(format!(
            "depth {} is within the cap of {}",
            candidate.depth, candidate.depth_cap
        ))
    };
    if push(names::DEPTH, depth, &mut results) {
        return finish(results, canonical);
    }

    // 4. Pin marker.
    let pin = pin_marker(&candidate.path, &candidate.root, &ctx.config.pin_marker);
    if push(names::PIN_MARKER, pin, &mut results) {
        return finish(results, canonical);
    }

    // 5. Heartbeat.
    let hb = heartbeat(candidate, ctx);
    if push(names::HEARTBEAT, hb, &mut results) {
        return finish(results, canonical);
    }

    // 6. Git ignore.
    let git = git_ignore(&candidate.path, ctx.git);
    if push(names::GIT_IGNORE, git, &mut results) {
        return finish(results, canonical);
    }

    // 7. Idle age.
    let idle = if candidate.idle < candidate.min_idle {
        Verdict::Reject(format!(
            "idle for {} but the rule requires {}",
            crate::time::human_duration(candidate.idle),
            crate::time::human_duration(candidate.min_idle)
        ))
    } else {
        Verdict::Pass(format!(
            "idle for {}, at least the {} the rule requires",
            crate::time::human_duration(candidate.idle),
            crate::time::human_duration(candidate.min_idle)
        ))
    };
    if push(names::IDLE_AGE, idle, &mut results) {
        return finish(results, canonical);
    }

    // 8. Process liveness.
    let live = process_liveness(&candidate.path, ctx.inspector);
    if push(names::PROCESS_LIVENESS, live, &mut results) {
        return finish(results, canonical);
    }

    finish(results, canonical)
}

/// Fills in the guards that never ran and computes the overall verdict.
fn finish(mut results: Vec<GuardResult>, canonical: Option<PathBuf>) -> Decision {
    let rejected = results
        .iter()
        .find(|r| r.verdict.rejected())
        .map(|r| (r.guard, r.verdict.detail().to_owned()));

    for guard in names::ORDER {
        if !results.iter().any(|r| r.guard == guard) {
            results.push(GuardResult {
                guard,
                verdict: Verdict::NotReached,
            });
        }
    }
    // Keep the canonical guard order regardless of the order results arrived.
    results.sort_by_key(|r| {
        names::ORDER
            .iter()
            .position(|g| *g == r.guard)
            .unwrap_or(usize::MAX)
    });

    match rejected {
        Some((guard, reason)) => Decision {
            selected: false,
            results,
            rejected_by: Some(guard),
            reason: Some(reason),
            canonical: None,
        },
        None => Decision {
            selected: true,
            results,
            rejected_by: None,
            reason: None,
            canonical,
        },
    }
}

/// The home directory, derived from the config rather than the environment so
/// the deny floor is reproducible under test.
fn home_of(config: &Config) -> Option<PathBuf> {
    // Every default path in the config is rooted at $HOME, so the quarantine
    // directory's ancestry is a reliable source when one was not expanded.
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| config.quarantine.parent().map(Path::to_path_buf))
}

/// The declared roots, canonicalized, for the containment check.
///
/// A root that does not resolve is dropped rather than failing the run: the
/// planner already reported it as skipped, and a config shared across machines
/// will routinely name directories that exist on only some of them.
pub fn canonical_roots(config: &Config) -> Vec<PathBuf> {
    config
        .roots
        .iter()
        .filter_map(|r| r.path.canonicalize().ok())
        .collect()
}

/// How long a candidate has left to wait before the idle guard would pass it.
pub fn idle_remaining(candidate: &Candidate) -> Duration {
    candidate.min_idle.saturating_sub(candidate.idle)
}
