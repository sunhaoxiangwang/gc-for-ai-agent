# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The configuration format and the `--json` schema are public interfaces from the
first release. Breaking either requires a minor version bump before 1.0 and a
major bump after, plus an entry here with a migration note.

## [Unreleased]

## [0.1.0] - 2026-09-06

First release.

### Added

- `reap init [--detect]`, which looks for the toolchains actually installed and
  the directories where code actually lives, and writes a config containing only
  rules that can match something.
- `reap doctor`, which validates the config, resolves every root, proves the
  quarantine directory works with a real test rename, probes each root for
  permission problems, reports free space with and without APFS snapshot slack,
  names rules that matched nothing, and lists the cache-sharing environment
  variables that are unset.
- `reap report [--all] [--tier N]`, which shows what could be reclaimed and
  deletes nothing.
- `reap explain <path>`, which reports every rule and every guard, in order,
  with the reason for each.
- `reap sweep [--tier N] [--until-free PCT] [--apply]`. `--until-free` escalates
  through the tiers, re-reading free space between each, and stops as soon as
  the target is met.
- `reap gc-session <id> [--apply]`, for a supervisor to call when a job ends.
  Safe on an unknown id and safe to call twice.
- `reap install [--systemd|--launchd] [--dry-run]` and `reap uninstall`.
- A stable `--json` document, `schema_version` 1.
- Eight-guard safety stack: deny floor, containment, depth, pin marker,
  heartbeat, git-ignore, idle age, process liveness.
- Two-opt-in gating: removal requires `--apply` and `dry_run = false`.
- Quarantine: an atomic rename into a staging directory on the same filesystem,
  then a recursive removal, with a run manifest so an interrupted run is
  recovered by the next one without re-planning.
- Per-machine configuration overlays, and `REAP_HOSTNAME` to test them.
- Seven worked example configurations in `examples/`, validated in CI.
- systemd user units with `ReadWritePaths` generated from the declared roots,
  and a launchd agent.

### Known limitations

- macOS has no launchd equivalent of systemd's `ReadWritePaths`. On macOS the
  deny list and the containment guard are the entire sandbox.
- The quarantine directory must be on the same filesystem as every root. Roots
  spanning two filesystems need one config each.
- Recovery is possible only for an interrupted run, not a completed one.

[Unreleased]: https://github.com/OWNER/reap/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/OWNER/reap/releases/tag/v0.1.0
