# Configuration reference

One TOML file. Every key, its type, its default, and what it does.

## Where the file is found

Checked in order; the first one that exists wins. There is no merging across
locations, because a config assembled from two files is harder to reason about
than one that is simply wrong.

1. `--config <path>`
2. `$REAP_CONFIG`
3. `~/.config/reap/reap.toml`
4. `/etc/reap/reap.toml`

`~` at the start of a path value is expanded using `$HOME`. Nothing else is
expanded: no `$VAR`, no command substitution. A config value should mean the
same thing regardless of the environment it is read in, and shell-style
expansion inside a path that is about to be deleted is a surface nobody needs.

**Unknown keys are a hard error.** A typo that silently disables a rule is worse
than a config that refuses to load.

## `[global]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `dry_run` | bool | `true` | When true, nothing is ever removed. Removal also needs `--apply`. |
| `quarantine` | path | `~/.cache/reap/quarantine` | Staging directory for the atomic rename. Must be absolute and on the same filesystem as every root. |
| `low_water_pct` | 0-100 | `15` | Free-space percentage below which the machine is considered under pressure. |
| `target_free_pct` | 0-100 | `25` | Free-space percentage `sweep --until-free` aims for. Must be at least `low_water_pct`. |
| `apfs_snapshot_slack_gb` | int | `20` | Allowance added to free space when local snapshots exist. See below. |
| `max_run_seconds` | int | `300` | Stop starting new work after this long. 0 means no limit. |
| `case_sensitive_matching` | bool | `true` | Whether `dir_name` and `glob` match case-sensitively. |
| `pin_marker` | filename | `.reap-keep` | A file with this name protects its directory and everything below it. |
| `heartbeat_dir` | path | `~/.cache/reap/heartbeats` | Where a supervisor writes one file per running job. |
| `heartbeat_staleness_multiplier` | float >= 1.0 | `3.0` | A heartbeat is stale after `interval_seconds * this`. |
| `deny` | list of paths | `[]` | Paths never touched, in addition to the hardcoded deny floor. |

### `case_sensitive_matching`

Rule matching declares its own case behaviour rather than inheriting the
filesystem's. APFS is case-insensitive by default and ext4 is not, so without
this a config shared between a Mac and a Linux box would quietly mean two
different things. Leave it `true` unless you know you want otherwise.

### `apfs_snapshot_slack_gb`

APFS reports space held by Time Machine local snapshots as used, even though the
kernel releases it on demand under pressure. A naive low-water trigger therefore
fires over space the machine already has, and escalates to tier 2 for no reason.

This allowance is added to available space **only when local snapshots actually
exist**, so a machine with Time Machine disabled is never told it has 20 GB it
does not have. On Linux the snapshot list is always empty and the allowance never
applies. `reap doctor` shows both numbers.

### `deny`

Additive to the hardcoded floor described in
[safety-model.md](safety-model.md). A path here refuses that path and anything
below it. Nothing you put here can *weaken* the floor.

## `[machine."<hostname>"]`

Accepts exactly the same keys as `[global]`, and overrides them on that host
only. Anything not mentioned falls through to `[global]`.

```toml
[global]
dry_run = true
quarantine = "~/.cache/reap/quarantine"

[machine."buildbox"]
dry_run = false
quarantine = "/var/lib/reap/quarantine"
low_water_pct = 20
target_free_pct = 40
```

Matching is on the exact hostname first, then on the hostname with its DNS
suffix removed, so `[machine."buildbox"]` also applies on
`buildbox.example.com`.

Overlays cannot add roots or rules. Cross-machine portability comes from a
different mechanism: **a root that does not exist is skipped with a warning
rather than being an error**, so one file can declare `~/code` and
`/var/lib/agent/workspaces` and do the right thing on both machines.

Set `REAP_HOSTNAME` to test an overlay without finding a machine of that name:

```sh
REAP_HOSTNAME=buildbox reap doctor
```

## `[[root]]`

Nothing outside a declared root is ever looked at. At least one is required if
you have any path rules.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `path` | path | required | Absolute, or `~`-prefixed. |
| `max_depth` | int > 0 | `8` | How deep below the root to walk. The root itself is depth 0. |

A root that is a symlink is refused: the path the walker descends and the path
the containment check compares against would be two different places. Declare
what it resolves to instead.

Prefer several specific roots to one broad one. It is faster, and it is a
narrower declaration of what you are willing to have touched.

## `[[rule]]`

Nothing is removable unless a rule names it.

| Key | Kind | Type | Default | Meaning |
| --- | --- | --- | --- | --- |
| `name` | both | string | required | Unique across the file. Appears in every log line and in `--json`. |
| `kind` | both | `"path"` or `"command"` | `"path"` | What sort of rule this is. |
| `tier` | both | 0, 1 or 2 | required | Rebuild cost. See below. |
| `dir_name` | path | string | | Match a directory by its exact name. No slashes. |
| `glob` | path | glob | | Match the full path. Mutually exclusive with `dir_name`. |
| `require_sibling` | path | filename | | Must exist in the candidate's parent directory. |
| `require_ancestor` | path | filename | | Must exist in some ancestor directory, up to the root. |
| `min_idle` | path | duration | `24h` | How long untouched before it is a candidate. |
| `orphaned_only` | path | bool | `false` | Require positive evidence that no live session owns it. |
| `max_depth` | path | int | root's value | Override the root's depth cap for this rule. |
| `os` | both | list | both | `["linux"]`, `["macos"]`, or omit for both. |
| `command` | command | list of strings | required | Literal argv. |

Exactly one of `dir_name` or `glob` is required for a path rule. A command rule
must set `command` and may not set any of the path-only fields.

### Tiers

Rebuild cost, not confidence. Every tier gets exactly the same guards.

- **0**: seconds to regenerate. `__pycache__`, tool caches, coverage output,
  `target/` in an idle workspace.
- **1**: minutes and bandwidth. `node_modules`, `.venv`, `DerivedData`, browser
  downloads.
- **2**: slows every later build in every project. Shared package caches, build
  caches.

A plain `reap sweep` touches tier 0. Higher tiers need `--tier N` or
`--until-free`.

### `dir_name` and `glob`

`dir_name` compares the directory's own name, whole. It cannot contain a slash.

`glob` matches the full path. `~` is expanded. `..` components are rejected at
load time. Supported syntax is [globset](https://docs.rs/globset)'s: `*` matches
within one path segment, `**` matches across segments, `{a,b}` alternates, `?`
matches one character, `[a-z]` matches a class.

```toml
glob = "~/Library/Developer/Xcode/DerivedData/*"
glob = "**/.{pytest,mypy,ruff}_cache"
glob = "**/{.venv,venv}"
```

A glob that points outside every declared root can never match. `reap doctor`
reports rules that matched nothing, which is how you notice.

### `require_sibling` and `require_ancestor`

Both take a bare filename, not a path.

`require_sibling` demands an entry with that name in the candidate's **parent**
directory. This is what makes `dir_name = "target"` safe: with
`require_sibling = "Cargo.toml"`, a folder named `target` that is not a Rust
build directory is not a candidate at all.

`require_ancestor` demands an entry with that name in **some ancestor**
directory, from the candidate's parent up to and including the root. Use it to
scope a rule to repositories:

```toml
[[rule]]
name = "python-venv"
kind = "path"
tier = 1
glob = "**/{.venv,venv}"
require_ancestor = ".git"
min_idle = "30d"
```

That rule ignores a long-lived virtual environment you keep outside any
checkout.

### `min_idle`

A duration string: `30s`, `6h`, `14d`, `2weeks`. Parsed by
[humantime](https://docs.rs/humantime).

Measured from the newer of the directory's own mtime and the mtimes of up to 256
of its immediate children. It is deliberately a shallow probe; see
[safety-model.md](safety-model.md).

Defaults to `24h` when omitted. That is deliberately long: a rule author who
wants a shorter window says so, and one who forgot gets the safe end.

### `orphaned_only`

For path rules on machines running supervised jobs. Requires **positive
evidence** that nothing owns the candidate: the heartbeat directory must be
readable, and nothing in it may claim the path.

This inverts the usual default. Normally, absence of a heartbeat means
unprotected. With `orphaned_only`, a missing or unreadable heartbeat directory
is a rejection, so "we could not check" never reads as "nothing owns this".

### `command`

Literal argv, executed directly rather than through a shell. An element
containing `$` or a backtick is rejected at load time.

Guards 4 through 8 cannot apply to a command rule, because there is no path. The
literal-argv requirement is the compensating control: nothing in the command is
computed from anything discovered at run time.

Rules for these are shipped in `examples/` and emitted by `reap init --detect`:
`docker builder prune`, `docker image prune`,
`xcrun simctl delete unavailable`, `go clean -cache`, `pnpm store prune`,
`uv cache prune`, `npm cache verify`.

Where the tool reports how much it reclaimed, `reap` parses and records it.
Docker's `Total reclaimed space:` line is the one currently understood. Where a
tool reports nothing, the report says "size not reported" rather than inventing
a zero.

## The heartbeat contract

`reap` refuses to touch anything a live heartbeat claims. Whatever supervises
your jobs writes these; `reap` only reads them.

- One file per running job at `<heartbeat_dir>/<session-id>.json`.
- `session-id` matches `[A-Za-z0-9._-]{1,128}`.
- The body is a JSON object. Only `session_id` is required.

```json
{
  "session_id": "job-42",
  "workspace": "/var/lib/agent/workspaces/job-42",
  "tmp": ["/var/lib/agent/tmp/job-42"],
  "pid": 5150,
  "interval_seconds": 60,
  "updated_at": "2026-09-06T14:03:11Z"
}
```

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `session_id` | string | filename stem | The session's id. |
| `workspace` | absolute path | none | The directory this job owns. |
| `tmp` | list of absolute paths | `[]` | Further directories it owns. |
| `pid` | int | none | The job's process. Used as a liveness backstop. |
| `interval_seconds` | int | `60` | How often the supervisor rewrites this file. |
| `updated_at` | RFC 3339 | file mtime | When it was last written. |

Rules:

- Rewrite or touch the file at least every `interval_seconds`.
- A heartbeat is **live** while
  `now - updated_at < interval_seconds * heartbeat_staleness_multiplier`, or
  while `pid` names a running process. A supervisor can die without cleaning up
  while the work it started carries on.
- `updated_at` is taken as the newer of the declared value and the file's own
  mtime, so a supervisor that only touches the file works.
- A live heartbeat protects the paths it names, anything inside them, anything
  containing them, and any directory whose own name is the session id.
- A file that exists but cannot be parsed is treated as live.
- Relative paths in `workspace` or `tmp` are ignored.

Call `reap gc-session <id> --apply` when a job ends, on every exit path
including crashes. It is safe on an id that never existed and safe to call
twice. It reclaims through the same rules and guards as a sweep, narrowed to
that session, and it does not run machine-wide command reclaimers.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success. |
| 1 | Internal error. |
| 2 | Configuration error, including `--apply` against `dry_run = true`. |
| 3 | Nothing to do. Not a failure. |
| 4 | Partial failure. Some work succeeded and some did not. |

## The `--json` schema

Stable from the first release. Fields may be added; a field that exists never
changes meaning or disappears without a version bump and a CHANGELOG migration
note. `schema_version` identifies the shape.

```json
{
  "schema_version": 1,
  "reap_version": "0.1.0",
  "host": "workstation",
  "os": "macos",
  "arch": "aarch64",
  "command": "sweep",
  "config": "/home/someone/.config/reap/reap.toml",
  "started_at": "2026-09-06T14:03:11Z",
  "duration_ms": 412,
  "free_before_bytes": 901000000000,
  "free_after_bytes": 903000000000,
  "applied": true,
  "dry_run": false,
  "stopped_at_tier": 0,
  "candidates": [
    {
      "path": "/home/someone/code/api-server/target",
      "root": "/home/someone/code",
      "rule": "cargo-target",
      "tier": 0,
      "depth": 2,
      "bytes": 1821376512,
      "idle_seconds": 432000,
      "selected": true,
      "rejected_by": null,
      "reason": null,
      "reclaimed": true
    }
  ],
  "commands": [
    {
      "rule": "docker-build-cache",
      "tier": 2,
      "argv": ["docker", "builder", "prune", "-f"],
      "executed": true,
      "reclaimed_bytes": 1324997410,
      "exit_code": 0,
      "error": null
    }
  ],
  "totals": {
    "candidates": 7,
    "selected": 3,
    "rejected": 4,
    "selected_bytes": 1871000000,
    "reclaimed_bytes": 1871000000,
    "commands_run": 1,
    "errors": 0
  },
  "notes": [],
  "errors": []
}
```

`applied` is the machine-readable truth about whether this run mutated anything.
`rejected_by` names the guard that refused a candidate, and `reason` explains it
in words.
