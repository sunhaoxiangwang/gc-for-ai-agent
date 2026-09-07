//! Configuration parsing, machine overlays and validation.

#[path = "fixtures.rs"]
mod fixtures;

use std::path::{Path, PathBuf};
use std::time::Duration;

use fixtures::Fixture;
use reap_core::config::{self, LoadContext, RuleKind};
use reap_core::error::ConfigError;

const MINIMAL: &str = r#"
[[root]]
path = "{base}/code"

[[rule]]
name = "cargo-target"
kind = "path"
tier = 0
dir_name = "target"
require_sibling = "Cargo.toml"
min_idle = "6h"
"#;

fn ctx(home: &Path, hostname: &str) -> LoadContext {
    LoadContext {
        home: Some(home.to_path_buf()),
        hostname: hostname.to_owned(),
        explicit: None,
        env_config: None,
    }
}

#[test]
fn defaults_are_safe_when_nothing_is_specified() {
    let f = Fixture::new();
    let c = f.config(MINIMAL);
    assert!(c.dry_run, "dry_run must default to true");
    assert_eq!(c.pin_marker, ".reap-keep");
    assert_eq!(c.low_water_pct, 15);
    assert_eq!(c.target_free_pct, 25);
    assert_eq!(c.max_run, Duration::from_secs(300));
    assert!(c.case_sensitive_matching);
    assert!(c.deny.is_empty());
}

#[test]
fn an_unknown_key_is_a_hard_error() {
    let f = Fixture::new();
    // A rule silently disabled by a typo is worse than a startup failure.
    let err = f
        .try_config(&format!("{MINIMAL}\n[global]\ndry_runn = false\n"))
        .expect_err("typo accepted");
    assert!(matches!(err, ConfigError::Parse { .. }), "got {err:?}");
    assert!(err.to_string().contains("dry_runn"), "{err}");
}

#[test]
fn an_unknown_rule_key_is_a_hard_error() {
    let f = Fixture::new();
    let err = f
        .try_config(&MINIMAL.replace("min_idle = \"6h\"", "min_idl = \"6h\""))
        .expect_err("typo accepted");
    assert!(err.to_string().contains("min_idl"), "{err}");
}

#[test]
fn a_machine_overlay_overrides_global_keys_on_that_host_only() {
    let f = Fixture::new();
    let text = format!(
        r#"
[global]
dry_run = true
low_water_pct = 15

[machine."buildbox"]
dry_run = false
low_water_pct = 30
target_free_pct = 40
{MINIMAL}
"#
    );
    let text = text.replace("{base}", &f.base.display().to_string());
    let src = f.base.join("reap.toml");

    let here = config::parse(&text, &src, &ctx(&f.base, "laptop")).unwrap();
    assert!(here.dry_run);
    assert_eq!(here.low_water_pct, 15);
    assert_eq!(here.applied_overlay, None);

    let there = config::parse(&text, &src, &ctx(&f.base, "buildbox")).unwrap();
    assert!(!there.dry_run);
    assert_eq!(there.low_water_pct, 30);
    assert_eq!(there.applied_overlay.as_deref(), Some("buildbox"));
}

#[test]
fn a_machine_overlay_matches_a_fully_qualified_hostname() {
    let f = Fixture::new();
    let text =
        format!("[machine.\"buildbox\"]\nlow_water_pct = 30\ntarget_free_pct = 40\n{MINIMAL}")
            .replace("{base}", &f.base.display().to_string());
    let c = config::parse(
        &text,
        &f.base.join("reap.toml"),
        &ctx(&f.base, "buildbox.example.com"),
    )
    .unwrap();
    assert_eq!(c.low_water_pct, 30);
}

#[test]
fn tilde_is_expanded_from_home_and_nothing_else_is() {
    let home = Path::new("/home/someone");
    assert_eq!(
        config::expand_tilde("~/code", Some(home)).unwrap(),
        PathBuf::from("/home/someone/code")
    );
    assert_eq!(
        config::expand_tilde("~", Some(home)).unwrap(),
        PathBuf::from("/home/someone")
    );
    // No shell-style variable expansion: a config value means the same thing
    // regardless of the environment it is read in.
    assert_eq!(
        config::expand_tilde("$HOME/code", Some(home)).unwrap(),
        PathBuf::from("$HOME/code")
    );
    // A `~` that is not a path prefix is left alone.
    assert_eq!(
        config::expand_tilde("/tmp/~backup", Some(home)).unwrap(),
        PathBuf::from("/tmp/~backup")
    );
}

#[test]
fn tilde_without_a_home_is_an_error_not_a_relative_path() {
    let err = config::expand_tilde("~/code", None).expect_err("expanded without $HOME");
    assert!(matches!(err, ConfigError::NoHome { .. }));
}

#[test]
fn the_search_path_is_ordered_and_does_not_merge() {
    let home = Path::new("/home/someone");
    assert_eq!(
        config::search_path(None, Some("/env/reap.toml"), Some(home)),
        vec![
            PathBuf::from("/env/reap.toml"),
            PathBuf::from("/home/someone/.config/reap/reap.toml"),
            PathBuf::from("/etc/reap/reap.toml"),
        ]
    );
    // An explicit --config short-circuits the whole search.
    assert_eq!(
        config::search_path(
            Some(Path::new("/x.toml")),
            Some("/env/reap.toml"),
            Some(home)
        ),
        vec![PathBuf::from("/x.toml")]
    );
}

#[test]
fn duplicate_rule_names_are_rejected() {
    let f = Fixture::new();
    let err = f
        .try_config(&format!("{MINIMAL}{MINIMAL}"))
        .expect_err("duplicate names accepted");
    assert!(err.to_string().contains("more than once"), "{err}");
}

#[test]
fn a_tier_above_two_is_rejected() {
    let f = Fixture::new();
    let err = f
        .try_config(&MINIMAL.replace("tier = 0", "tier = 3"))
        .expect_err("tier 3 accepted");
    assert!(err.to_string().contains("tiers are 0, 1 or 2"), "{err}");
}

#[test]
fn a_rule_must_match_exactly_one_way() {
    let f = Fixture::new();
    let both = MINIMAL.replace(
        "dir_name = \"target\"",
        "dir_name = \"target\"\nglob = \"/x/*\"",
    );
    assert!(f
        .try_config(&both)
        .expect_err("both matchers accepted")
        .to_string()
        .contains("matches one way"));

    let neither = MINIMAL.replace("dir_name = \"target\"\n", "");
    assert!(f
        .try_config(&neither)
        .expect_err("no matcher accepted")
        .to_string()
        .contains("neither dir_name nor glob"));
}

#[test]
fn command_rules_reject_path_only_fields_and_substitution_syntax() {
    let f = Fixture::new();
    let base = r#"
[[root]]
path = "{base}/code"

[[rule]]
name = "docker"
kind = "command"
tier = 2
command = ["docker", "builder", "prune", "-f"]
"#;
    assert!(f.try_config(base).is_ok());

    let with_path_field = base.replace("tier = 2", "tier = 2\nmin_idle = \"1h\"");
    assert!(f
        .try_config(&with_path_field)
        .expect_err("min_idle on a command rule accepted")
        .to_string()
        .contains("only applies to path rules"));

    // Command argv must be literal. Guards 4 through 8 cannot protect a command
    // rule, so the compensating control is that nothing in it is computed.
    let interpolated = base.replace("\"-f\"", "\"$WORKSPACE\"");
    assert!(f
        .try_config(&interpolated)
        .expect_err("interpolation accepted")
        .to_string()
        .contains("must be literal"));
}

#[test]
fn a_glob_containing_a_parent_component_is_rejected() {
    let f = Fixture::new();
    let text = r#"
[[root]]
path = "{base}/code"

[[rule]]
name = "sneaky"
kind = "path"
tier = 0
glob = "{base}/code/../../*"
min_idle = "1h"
"#;
    assert!(f
        .try_config(text)
        .expect_err("`..` in a glob accepted")
        .to_string()
        .contains("`..`"));
}

#[test]
fn path_rules_without_a_root_are_rejected() {
    let f = Fixture::new();
    let text = r#"
[[rule]]
name = "cargo-target"
kind = "path"
tier = 0
dir_name = "target"
min_idle = "1h"
"#;
    assert!(f
        .try_config(text)
        .expect_err("rules without roots accepted")
        .to_string()
        .contains("no [[root]] is declared"));
}

#[test]
fn a_config_with_no_rules_is_rejected() {
    let f = Fixture::new();
    assert!(f
        .try_config("[[root]]\npath = \"{base}/code\"\n")
        .expect_err("empty rule set accepted")
        .to_string()
        .contains("no [[rule]] is declared"));
}

#[test]
fn a_target_below_the_low_water_mark_is_rejected() {
    let f = Fixture::new();
    let text = format!("[global]\nlow_water_pct = 40\ntarget_free_pct = 10\n{MINIMAL}");
    assert!(f
        .try_config(&text)
        .expect_err("unreachable target accepted")
        .to_string()
        .contains("could never reach its target"));
}

#[test]
fn min_idle_defaults_to_a_day_when_omitted() {
    let f = Fixture::new();
    let c = f.config(&MINIMAL.replace("min_idle = \"6h\"\n", ""));
    assert_eq!(c.rules[0].min_idle, Duration::from_secs(86_400));
}

#[test]
fn kind_defaults_to_path() {
    let f = Fixture::new();
    let c = f.config(&MINIMAL.replace("kind = \"path\"\n", ""));
    assert_eq!(c.rules[0].kind, RuleKind::Path);
}
