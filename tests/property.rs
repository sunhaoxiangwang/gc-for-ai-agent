//! Property tests.
//!
//! The example-based tests check the cases we thought of. These check the
//! invariant over trees and rule sets nobody wrote down: whatever the planner
//! emits, it is inside a declared root and it is not something the deny floor
//! forbids.

#[path = "fixtures.rs"]
mod fixtures;

use std::collections::BTreeSet;
use std::path::PathBuf;

use fixtures::Fixture;
use proptest::prelude::*;
use reap_core::guard::{deny_floor, MIN_COMPONENTS};
use reap_core::plan::{plan, PlanOptions};

/// Directory names drawn from the ones that actually occur in build trees, plus
/// a few that only look like them.
fn dir_name() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "target".to_owned(),
        "node_modules".to_owned(),
        "build".to_owned(),
        "src".to_owned(),
        "__pycache__".to_owned(),
        ".venv".to_owned(),
        "docs".to_owned(),
        "target-practice".to_owned(),
        "my build".to_owned(),
        "..hidden".to_owned(),
    ])
}

/// A relative path of one to four of those components.
fn rel_path() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(dir_name(), 1..5)
}

/// Rules naming a subset of the same vocabulary.
fn rule_names() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(dir_name(), 1..4)
}

fn config_for(f: &Fixture, rules: &[String], max_depth: usize) -> reap_core::config::Config {
    let mut text = format!(
        "[global]\nquarantine = \"{{base}}/quarantine\"\nheartbeat_dir = \"{{base}}/heartbeats\"\n\n\
         [[root]]\npath = \"{{base}}/code\"\nmax_depth = {max_depth}\n"
    );
    // Rule names must be unique, and the same directory name may be drawn twice.
    let unique: BTreeSet<&String> = rules.iter().collect();
    for (i, name) in unique.iter().enumerate() {
        text.push_str(&format!(
            "\n[[rule]]\nname = \"r{i}\"\nkind = \"path\"\ntier = 0\ndir_name = \"{name}\"\nmin_idle = \"0s\"\n"
        ));
    }
    f.config(&text)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Invariant 2: every emitted candidate is a strict descendant of a
    /// declared root.
    ///
    /// The tree also contains directories outside the root and symlinks
    /// pointing back into it, so a planner that followed either would be
    /// caught.
    #[test]
    fn invariant_2_no_candidate_escapes_the_declared_roots(
        paths in prop::collection::vec(rel_path(), 1..8),
        rules in rule_names(),
        max_depth in 1usize..7,
    ) {
        let f = Fixture::new();
        f.dir("code");
        f.dir("outside");

        for parts in &paths {
            f.dir(&format!("code/{}", parts.join("/")));
            // The same shape outside the root, which must never be reached.
            f.dir(&format!("outside/{}", parts.join("/")));
        }
        // A link from inside the root to the tree outside it.
        f.symlink(&f.path("outside"), "code/link-out");

        let config = config_for(&f, &rules, max_depth);
        let root = f.path("code");
        let p = plan(&config, reap_platform::os_name(), PlanOptions::default());

        for c in &p.candidates {
            prop_assert!(
                c.path.starts_with(&root) && c.path != root,
                "candidate {} is not a strict descendant of {}",
                c.path.display(),
                root.display()
            );
            let canonical = c.path.canonicalize().unwrap_or_else(|_| c.path.clone());
            prop_assert!(
                canonical.starts_with(&root),
                "candidate {} resolves to {}, outside the root",
                c.path.display(),
                canonical.display()
            );
            prop_assert!(
                c.depth <= max_depth,
                "candidate {} is at depth {} with a cap of {max_depth}",
                c.path.display(),
                c.depth
            );
        }
    }

    /// Invariant 8: nothing the planner emits is refused by the deny floor.
    ///
    /// The two are independent: the planner is bounded by roots and rules, the
    /// floor by absolute path shape. This asserts they do not disagree.
    #[test]
    fn invariant_8_no_candidate_is_a_deny_floor_member(
        paths in prop::collection::vec(rel_path(), 1..8),
        rules in rule_names(),
    ) {
        let f = Fixture::new();
        f.dir("code");
        for parts in &paths {
            f.dir(&format!("code/{}", parts.join("/")));
        }

        let config = config_for(&f, &rules, 6);
        let home = PathBuf::from("/home/nobody");
        let p = plan(&config, reap_platform::os_name(), PlanOptions::default());

        for c in &p.candidates {
            let verdict = deny_floor(&c.path, Some(&home));
            prop_assert!(
                !verdict.rejected(),
                "planner emitted {}, which the deny floor refuses: {}",
                c.path.display(),
                verdict.detail()
            );
        }
    }

    /// Every candidate is deep enough that the floor's component minimum can
    /// never be the thing standing between it and removal.
    #[test]
    fn candidates_are_always_deeper_than_the_component_minimum(
        paths in prop::collection::vec(rel_path(), 1..6),
        rules in rule_names(),
    ) {
        let f = Fixture::new();
        f.dir("code");
        for parts in &paths {
            f.dir(&format!("code/{}", parts.join("/")));
        }
        let config = config_for(&f, &rules, 6);
        let p = plan(&config, reap_platform::os_name(), PlanOptions::default());

        for c in &p.candidates {
            let components = c.path.components()
                .filter(|x| matches!(x, std::path::Component::Normal(_)))
                .count();
            prop_assert!(
                components >= MIN_COMPONENTS,
                "{} has only {components} components",
                c.path.display()
            );
        }
    }

    /// The planner never proposes a directory nested inside another candidate.
    /// Two candidates where one contains the other would mean the executor
    /// tried to remove the same bytes twice.
    #[test]
    fn no_candidate_contains_another(
        paths in prop::collection::vec(rel_path(), 1..8),
        rules in rule_names(),
    ) {
        let f = Fixture::new();
        f.dir("code");
        for parts in &paths {
            f.dir(&format!("code/{}", parts.join("/")));
        }
        let config = config_for(&f, &rules, 6);
        let p = plan(&config, reap_platform::os_name(), PlanOptions::default());

        for a in &p.candidates {
            for b in &p.candidates {
                if a.path != b.path {
                    prop_assert!(
                        !b.path.starts_with(&a.path),
                        "{} is nested inside {}",
                        b.path.display(),
                        a.path.display()
                    );
                }
            }
        }
    }
}
