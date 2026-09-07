//! Generating scheduler units.
//!
//! `reap` is a one-shot command, not a resident daemon. The operating system
//! owns the timing: there is no crash loop to supervise, no resident memory on
//! a laptop, and identical behaviour whether a timer or a person runs it.
//!
//! The unit text is produced by pure functions so it can be tested, diffed and
//! printed by `install --dry-run` without writing anything.

use std::path::{Path, PathBuf};

use reap_core::config::Config;

/// The launchd job label, and the plist filename stem.
///
/// Reverse-DNS by convention. Change it in one place if you fork this; nothing
/// else depends on the string.
pub const LAUNCHD_LABEL: &str = "io.github.reap";

/// Which scheduler to install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Systemd,
    Launchd,
}

impl Kind {
    /// The scheduler native to this platform.
    pub fn native() -> Self {
        if cfg!(target_os = "macos") {
            Kind::Launchd
        } else {
            Kind::Systemd
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Systemd => "systemd",
            Kind::Launchd => "launchd",
        }
    }
}

/// One file an install would write.
#[derive(Debug, Clone)]
pub struct Unit {
    pub path: PathBuf,
    pub contents: String,
}

/// The commands to run after the files are in place, for the user to see.
#[derive(Debug, Clone)]
pub struct Plan {
    pub kind: Kind,
    pub units: Vec<Unit>,
    pub activate: Vec<Vec<String>>,
    pub deactivate: Vec<Vec<String>>,
}

/// XML-escapes a string for a plist value.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The systemd service and timer for a user unit.
///
/// `ReadWritePaths` is a second layer of defence that does not depend on the
/// configuration being correct: the service runs with the whole filesystem
/// read-only except the roots and the quarantine directory, so a bug in the
/// planner cannot express a write anywhere else. macOS has no launchd
/// equivalent, which `docs/scheduling.md` says plainly rather than implying
/// parity.
pub fn systemd_units(config: &Config, binary: &Path, config_path: &Path, home: &Path) -> Plan {
    let mut writable: Vec<String> = config
        .roots
        .iter()
        .map(|r| r.path.display().to_string())
        .collect();
    writable.push(config.quarantine.display().to_string());
    writable.sort();
    writable.dedup();

    let service = format!(
        "\
[Unit]
Description=Reclaim build artifacts and tool caches
Documentation=https://github.com/sunhaoxiangwang/gc-for-ai-agent
# Do not fight the machine for disk bandwidth while it is busy starting up.
After=default.target

[Service]
Type=oneshot
ExecStart={binary} --config {config} sweep --until-free {target} --apply
Nice=10
IOSchedulingClass=idle

# A second layer of defence, independent of the configuration. The filesystem
# is read-only to this unit except for the declared roots and the quarantine
# directory, so no bug in reap can express a write outside them.
ProtectSystem=strict
ProtectHome=read-only
PrivateTmp=false
NoNewPrivileges=true
RestrictSUIDSGID=true
ReadWritePaths={writable}

[Install]
WantedBy=default.target
",
        binary = binary.display(),
        config = config_path.display(),
        target = config.target_free_pct,
        writable = writable.join(" "),
    );

    let timer = "\
[Unit]
Description=Reclaim build artifacts and tool caches, hourly
Documentation=https://github.com/sunhaoxiangwang/gc-for-ai-agent

[Timer]
OnCalendar=hourly
# Spread the load so a fleet of machines does not all sweep on the hour.
RandomizedDelaySec=15m
# Catch up after the machine was asleep, which on a laptop is most of the time.
Persistent=true
Unit=reap.service

[Install]
WantedBy=timers.target
"
    .to_owned();

    let dir = home.join(".config/systemd/user");
    Plan {
        kind: Kind::Systemd,
        units: vec![
            Unit {
                path: dir.join("reap.service"),
                contents: service,
            },
            Unit {
                path: dir.join("reap.timer"),
                contents: timer,
            },
        ],
        activate: vec![
            vec!["systemctl".into(), "--user".into(), "daemon-reload".into()],
            vec![
                "systemctl".into(),
                "--user".into(),
                "enable".into(),
                "--now".into(),
                "reap.timer".into(),
            ],
        ],
        deactivate: vec![
            vec![
                "systemctl".into(),
                "--user".into(),
                "disable".into(),
                "--now".into(),
                "reap.timer".into(),
            ],
            vec!["systemctl".into(), "--user".into(), "daemon-reload".into()],
        ],
    }
}

/// The launchd agent.
///
/// `StartInterval` rather than `StartCalendarInterval`: launchd runs a missed
/// interval job once after wake, which is the behaviour a laptop wants, and it
/// does not queue up one run per hour the machine spent asleep.
pub fn launchd_units(config: &Config, binary: &Path, config_path: &Path, home: &Path) -> Plan {
    let log_dir = home.join("Library/Logs/reap");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>

  <key>ProgramArguments</key>
  <array>
    <string>{binary}</string>
    <string>--config</string>
    <string>{config}</string>
    <string>sweep</string>
    <string>--until-free</string>
    <string>{target}</string>
    <string>--apply</string>
  </array>

  <!-- Hourly. launchd coalesces missed runs after a sleep rather than queueing
       one per hour the machine was asleep. -->
  <key>StartInterval</key>
  <integer>3600</integer>

  <key>RunAtLoad</key>
  <false/>

  <!-- Stay out of the way of whatever the person is actually doing. -->
  <key>ProcessType</key>
  <string>Background</string>
  <key>LowPriorityIO</key>
  <true/>
  <key>Nice</key>
  <integer>10</integer>

  <key>StandardOutPath</key>
  <string>{log_dir}/reap.log</string>
  <key>StandardErrorPath</key>
  <string>{log_dir}/reap.log</string>
</dict>
</plist>
"#,
        label = LAUNCHD_LABEL,
        binary = xml_escape(&binary.display().to_string()),
        config = xml_escape(&config_path.display().to_string()),
        target = config.target_free_pct,
        log_dir = xml_escape(&log_dir.display().to_string()),
    );

    let path = home.join(format!("Library/LaunchAgents/{LAUNCHD_LABEL}.plist"));
    let domain = format!("gui/{}", reap_platform::current_uid());
    Plan {
        kind: Kind::Launchd,
        units: vec![Unit {
            path: path.clone(),
            contents: plist,
        }],
        activate: vec![vec![
            "launchctl".into(),
            "bootstrap".into(),
            domain.clone(),
            path.display().to_string(),
        ]],
        deactivate: vec![vec![
            "launchctl".into(),
            "bootout".into(),
            format!("{domain}/{LAUNCHD_LABEL}"),
        ]],
    }
}

/// Builds the plan for a scheduler kind.
pub fn plan(kind: Kind, config: &Config, binary: &Path, config_path: &Path, home: &Path) -> Plan {
    match kind {
        Kind::Systemd => systemd_units(config, binary, config_path, home),
        Kind::Launchd => launchd_units(config, binary, config_path, home),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reap_core::config::LoadContext;

    fn config() -> Config {
        let ctx = LoadContext {
            home: Some(PathBuf::from("/home/someone")),
            hostname: "test".into(),
            explicit: None,
            env_config: None,
        };
        reap_core::config::parse(
            r#"
[global]
quarantine = "/home/someone/.cache/reap/quarantine"
target_free_pct = 30

[[root]]
path = "/home/someone/code"

[[root]]
path = "/home/someone/work"

[[rule]]
name = "cargo-target"
kind = "path"
tier = 0
dir_name = "target"
min_idle = "6h"
"#,
            Path::new("/home/someone/.config/reap/reap.toml"),
            &ctx,
        )
        .unwrap()
    }

    #[test]
    fn the_systemd_unit_confines_writes_to_the_roots_and_quarantine() {
        let p = systemd_units(
            &config(),
            Path::new("/usr/local/bin/reap"),
            Path::new("/home/someone/.config/reap/reap.toml"),
            Path::new("/home/someone"),
        );
        let service = &p.units[0].contents;

        assert!(service.contains("ProtectSystem=strict"));
        assert!(service.contains("ProtectHome=read-only"));
        // Every declared root, plus the quarantine directory, and nothing else.
        assert!(service.contains(
            "ReadWritePaths=/home/someone/.cache/reap/quarantine /home/someone/code /home/someone/work"
        ));
        assert!(service.contains("sweep --until-free 30 --apply"));
        assert_eq!(p.units[1].path.file_name().unwrap(), "reap.timer");
        assert!(p.units[1].contents.contains("Persistent=true"));
    }

    #[test]
    fn the_launchd_plist_is_well_formed_and_carries_the_same_command() {
        let p = launchd_units(
            &config(),
            Path::new("/usr/local/bin/reap"),
            Path::new("/home/someone/.config/reap/reap.toml"),
            Path::new("/home/someone"),
        );
        let plist = &p.units[0].contents;

        assert!(plist.starts_with("<?xml"));
        assert!(plist.contains(&format!("<string>{LAUNCHD_LABEL}</string>")));
        assert!(plist.contains("<string>--until-free</string>"));
        assert!(plist.contains("<string>30</string>"));
        assert!(plist.contains("<string>--apply</string>"));
        assert_eq!(
            plist.matches("<dict>").count(),
            plist.matches("</dict>").count()
        );
        assert_eq!(
            plist.matches("<array>").count(),
            plist.matches("</array>").count()
        );
    }

    #[test]
    fn a_path_with_xml_metacharacters_is_escaped() {
        let p = launchd_units(
            &config(),
            Path::new("/opt/a&b/reap"),
            Path::new("/opt/<weird>/reap.toml"),
            Path::new("/home/someone"),
        );
        let plist = &p.units[0].contents;
        assert!(plist.contains("/opt/a&amp;b/reap"));
        assert!(plist.contains("/opt/&lt;weird&gt;/reap.toml"));
        assert!(!plist.contains("/opt/<weird>"));
    }
}
