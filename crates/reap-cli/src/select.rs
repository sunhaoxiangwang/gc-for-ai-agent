//! Planning plus guards: the one path every command takes to decide what is
//! reclaimable.
//!
//! Keeping this in a single place is what makes `report`, `explain` and `sweep`
//! agree with each other. A command that assembled its own subset of the guard
//! stack would eventually disagree with what `explain` told the user.

use reap_core::guard::{evaluate, Decision};
use reap_core::plan::{plan, Candidate, CommandCandidate, Plan, PlanOptions};
use reap_core::schema::{CandidateReport, CommandReport};

use crate::context::Context;

/// A candidate together with the verdict of every guard.
pub struct Judged {
    pub candidate: Candidate,
    pub decision: Decision,
}

impl Judged {
    pub fn selected(&self) -> bool {
        self.decision.selected
    }
}

pub struct Selection {
    pub plan: Plan,
    pub judged: Vec<Judged>,
}

impl Selection {
    pub fn selected(&self) -> impl Iterator<Item = &Judged> {
        self.judged.iter().filter(|j| j.selected())
    }

    pub fn selected_bytes(&self) -> u64 {
        self.selected().map(|j| j.candidate.bytes).sum()
    }

    pub fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        for s in &self.plan.skipped_roots {
            notes.push(format!("root {} skipped: {}", s.path.display(), s.reason));
        }
        if !self.plan.unmatched_rules.is_empty() {
            notes.push(format!(
                "rules that matched nothing: {}",
                self.plan.unmatched_rules.join(", ")
            ));
        }
        notes
    }

    pub fn candidate_reports(&self) -> Vec<CandidateReport> {
        self.judged
            .iter()
            .map(|j| CandidateReport {
                path: j.candidate.path.display().to_string(),
                root: j.candidate.root.display().to_string(),
                rule: j.candidate.rule.clone(),
                tier: j.candidate.tier.get(),
                depth: j.candidate.depth,
                bytes: j.candidate.bytes,
                idle_seconds: j.candidate.idle.as_secs(),
                selected: j.decision.selected,
                rejected_by: j.decision.rejected_by.map(str::to_owned),
                reason: j.decision.reason.clone(),
                reclaimed: false,
            })
            .collect()
    }

    pub fn command_reports(&self) -> Vec<CommandReport> {
        self.plan.commands.iter().map(command_report).collect()
    }
}

pub fn command_report(c: &CommandCandidate) -> CommandReport {
    CommandReport {
        rule: c.rule.clone(),
        tier: c.tier.get(),
        argv: c.argv.clone(),
        executed: false,
        reclaimed_bytes: None,
        exit_code: None,
        error: None,
    }
}

/// Plans, then runs the full guard stack over every candidate.
pub fn select(ctx: &Context, opts: PlanOptions) -> Selection {
    let plan = plan(&ctx.config, ctx.os, opts);
    let guards = ctx.guards();
    let judged = plan
        .candidates
        .iter()
        .map(|c| Judged {
            candidate: c.clone(),
            decision: evaluate(c, &guards),
        })
        .collect();
    Selection { plan, judged }
}
