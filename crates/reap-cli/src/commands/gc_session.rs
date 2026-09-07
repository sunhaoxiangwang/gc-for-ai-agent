//! `reap gc-session <id>`: reclaim one supervised session's leftovers.
//!
//! This is the narrow call a supervisor makes when a job ends, including when
//! it ends badly. It must be safe on an id that never existed and safe to call
//! twice, because the paths that most need it are the crash and timeout paths,
//! where nobody knows what has already run.
//!
//! It reclaims through the same rules and the same guards as a sweep. A session
//! workspace is not removable because a heartbeat named it; it is removable
//! because a rule in the config names it and every guard permitted it. That is
//! invariant 1, and `gc-session` is not an exception to it.

use anyhow::Result;
use reap_core::config::Tier;
use reap_core::plan::PlanOptions;
use tracing::info;

use crate::cli::GcSessionArgs;
use crate::commands::sweep;
use crate::context::Context;
use crate::exit;
use crate::output::Style;
use crate::select::{select, Selection};

/// Whether a session id is one we are willing to look for.
///
/// The id becomes a path component comparison, so anything that could be read
/// as a path is refused outright rather than sanitised.
pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub fn run(ctx: &Context, args: &GcSessionArgs) -> Result<i32> {
    let style = Style::detect(ctx.args.no_color, ctx.args.json);

    if !valid_session_id(&args.id) {
        anyhow::bail!(
            "{:?} is not a valid session id; ids are 1 to 128 characters of \
             letters, digits, dot, underscore and hyphen",
            args.id
        );
    }

    // Paths the session's own heartbeat claims, if the file is still there. A
    // supervisor that removes its heartbeat before calling this is the common
    // case, and then the id-as-directory-name match is what finds the work.
    let claimed: Vec<std::path::PathBuf> = ctx
        .heartbeats
        .entries
        .iter()
        .filter(|h| h.session_id == args.id)
        .flat_map(|h| h.paths.clone())
        .collect();

    let opts = PlanOptions {
        now: ctx.started_at,
        max_tier: Tier::MAX,
        ..Default::default()
    };
    let full = select(ctx, opts);

    let owned = |path: &std::path::Path| -> bool {
        path.components().any(|c| c.as_os_str() == args.id.as_str())
            || claimed.iter().any(|p| path.starts_with(p))
    };

    let judged: Vec<_> = full
        .judged
        .into_iter()
        .filter(|j| owned(&j.candidate.path))
        .collect();

    if judged.is_empty() {
        // Nothing to do is the expected answer for an id that already got
        // collected, or one that never existed.
        info!(session = %args.id, "no candidates belong to this session");
        if !ctx.args.json && !ctx.args.quiet {
            println!(
                "{}",
                style.dim(&format!(
                    "Nothing belonging to session {} is reclaimable.",
                    args.id
                ))
            );
        }
        if ctx.args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema_version": reap_core::schema::SCHEMA_VERSION,
                    "command": "gc-session",
                    "session": args.id,
                    "host": ctx.host,
                    "applied": false,
                    "candidates": [],
                    "totals": {"candidates": 0, "selected": 0},
                }))?
            );
        }
        return Ok(exit::NOTHING_TO_DO);
    }

    let selection = Selection {
        plan: reap_core::plan::Plan {
            // Command reclaimers are machine-wide, not session-scoped; running
            // `docker builder prune` because one job finished would be a
            // surprise well outside what was asked for.
            commands: Vec::new(),
            ..full.plan
        },
        judged,
    };

    sweep::execute(ctx, selection, args.apply, Tier::MAX, "gc-session", None)
}
