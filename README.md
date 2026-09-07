# reap

`reap` reclaims disk space taken by build artifacts, dependency trees and tool
caches. You declare what is disposable in one TOML file, and a scheduler runs it
on a timer. It works the same way on macOS and Linux. It never deletes anything
by default: a dry run is the default, and turning that off takes two separate
opt-ins in two separate places.

## Why this exists

Build output accumulates faster than anyone cleans it up. A laptop with a dozen
checked-out projects is carrying tens of gigabytes of `target/`, `node_modules/`
and `DerivedData/` that nothing will ever read again. The usual fix is an
`rm -rf` you type from memory, which is dangerous, and which you skip on the
days you are busy. Automating it is worse, because a script with a delete in it
is a script nobody wants to read closely enough to trust.

The other half of the problem is that cleanup is always the last step, and the
last step is the one that gets dropped. A process that checks out a repository,
builds it and writes temporary files is frequently dead by the time cleanup is
due: it timed out, it crashed mid-build, or something killed it.

So: reclamation should be scheduled, declarative and guarded, not remembered.

## Safety model

This is deliberately above the install instructions. If you are deciding whether
to trust a program that deletes files, you need this before you run anything.

- **Nothing is deleted unless a rule in your config names it.** There are no
  heuristics, no "this looks like a build directory", nothing learned. If you
  did not write a rule for it, it is not a candidate.
- **Dry run is the default, and turning it off takes two opt-ins.** Removal
  requires `--apply` on the command line *and* `dry_run = false` in the config
  file. Either one alone does nothing. One is a thing you type in a moment; the
  other is a thing you edited on purpose.
- **Anything git does not consider ignored is never touched.** For any candidate
  inside a git work tree, `git check-ignore` must say it is ignored. If git is
  missing, or the repository is broken, or git refuses to answer, the candidate
  is refused. Not knowing is not permission.
- **A `.reap-keep` file protects a directory and everything below it**,
  permanently, with no config change and no restart. This is the escape hatch,
  and it is meant to be used.
- **Deletion goes through an atomic rename into a quarantine directory** on the
  same filesystem, and only then a recursive removal. An interrupted run leaves
  an orphaned directory in quarantine that the next run clears. It cannot leave
  a half-deleted tree where your build directory used to be.
- **`reap explain <path>` tells you exactly why any path would or would not be
  touched**, rule by rule and guard by guard.

There is also a hardcoded deny floor that no configuration can weaken: never
`/`, never your home directory itself, never a path with fewer than three
components, never anything containing a `.git`, `.ssh` or `.gnupg` component,
and never anything outside a root you declared.

The full guard stack, with the reasoning behind each guard and an explicit list
of what this does *not* protect against, is in
[docs/safety-model.md](docs/safety-model.md).

## Install

Supported platforms: macOS on Apple Silicon and Intel, Linux on x86_64 and
aarch64. All four are built and tested in CI. Windows is not supported and is
not planned.

### Prebuilt binary

```sh
curl -fsSL https://raw.githubusercontent.com/OWNER/reap/main/dist/install.sh | sh
```

The script detects your platform, downloads the matching release asset, verifies
its checksum against the published `SHA256SUMS`, and installs to
`/usr/local/bin`. It prints the next command to run. It does not install a
scheduler, and it never will without you asking.

If you would rather not pipe a script into a shell, read
[dist/install.sh](dist/install.sh) first, or download the asset for your
platform from the [releases page](https://github.com/OWNER/reap/releases) by
hand.

### From crates.io

```sh
cargo install reap-cli
```

The crate is `reap-cli`; the binary it installs is `reap`. (The name `reap` on
crates.io belongs to an unrelated project.)

### From source

```sh
git clone https://github.com/OWNER/reap
cd reap
cargo build --release
```

The binary lands at `target/release/reap`. Copy it somewhere on your `PATH`.

### Homebrew

There is no Homebrew tap. If you want one, say so in an issue; until then use
one of the routes above.

## Quickstart

Three commands, none of which delete anything.

**1. Write a config for this machine.**

```sh
reap init --detect
```

`--detect` looks for the toolchains you actually have and the directories where
you actually keep code, and writes rules only for those. On a machine with Rust,
Node and Python installed:

```
wrote ~/.config/reap/reap.toml

Detected on this machine
  Common             always included
  Rust               cargo is on PATH
  Node               node is on PATH
  Python             python3 is on PATH

Roots
  ~/code

Next
  reap doctor      check that everything resolves
  reap report      see what could be reclaimed. Deletes nothing.

dry_run is true, so nothing will be removed until you change it.
```

Open the file. It is commented throughout, and it is the whole of what `reap` is
allowed to do.

**2. Check that everything resolves.**

```sh
reap doctor
```

```
Configuration
  file            ~/.config/reap/reap.toml
  host            workstation
  machine overlay none matched this host
  dry_run         true  (nothing will be removed until this is false and --apply is passed)
  pin marker      .reap-keep
  max run         5m
  case matching   case-sensitive

Roots
  ok   ~/code  (depth 6, 4 entries)
  ok   ~/.cargo/registry  (depth 2, 2 entries)

Quarantine
  directory       ~/.cache/reap/quarantine
  ok   rename from ~/code succeeds
  ok   rename from ~/.cargo/registry succeeds

Disk
  free            839 GiB of 926 GiB (90.6%)
  low water       15%   target 25%
  local snapshots 0
  No local snapshots, so free space is what it says. The slack allowance applies only when snapshots exist.

Heartbeats
  directory       ~/.cache/reap/heartbeats
  none ~/.cache/reap/heartbeats does not exist
  Nothing is claiming a workspace. Rules with orphaned_only will reject every candidate until this directory exists.

Rules
  ok   tier 0 python-bytecode              1 matched, 1 would be reclaimed
  ok   tier 0 python-tool-caches           1 matched, 1 would be reclaimed
  ok   tier 0 cargo-target                 1 matched, 1 would be reclaimed
  warn tier 0 coverage-output              matched nothing
  ok   tier 1 node-modules                 1 matched, 1 would be reclaimed
  ok   tier 1 python-venv                  1 matched, 1 would be reclaimed
  ok   tier 2 cargo-registry-cache         2 matched, 2 would be reclaimed
  ok   tier 2 npm-cache                    command

Tools
  ok   git version 2.50.1

Prevention
  unset CARGO_TARGET_DIR           one shared Rust build directory instead of one per project
  ...
```

`doctor` proves the quarantine directory works by doing a real test rename out
of each root, so you find out now rather than during a sweep. The `Prevention`
section is worth reading; see [Prevention](#prevention) below.

**3. See what could be reclaimed.**

```sh
reap report
```

```
    SIZE  TIER  IDLE  RULE                  PATH
--------  ----  ----  --------------------  --------------------------------------------------------
1.70 GiB  0     5d    cargo-target          ~/code/api-server/target
 894 MiB  1     21d   node-modules          ~/code/dashboard/node_modules
 693 MiB  1     45d   python-venv           ~/code/data-pipeline/.venv
 470 MiB  2     95d   cargo-registry-cache  ~/.cargo/registry/src/index.crates.io-6f17d22bba15001f
 340 MiB  2     95d   cargo-registry-cache  ~/.cargo/registry/cache/index.crates.io-6f17d22bba15001f
38.0 MiB  0     6h    python-bytecode       ~/code/data-pipeline/src/__pycache__
11.0 MiB  0     3d    python-tool-caches    ~/code/data-pipeline/.pytest_cache

4.08 GiB across 7 directories

Command reclaimers (size not known until run)
  tier 2  npm-cache  npm cache verify

This command deletes nothing. Run `reap explain <path>` to see the guards for one path.
```

Add `--all` to see the things that matched a rule but were held back by a guard,
and why.

### Your first real deletion

Deleting requires two separate opt-ins, so a single mistake in either place
cannot remove anything.

First, edit `~/.config/reap/reap.toml` and change one line:

```toml
dry_run = false
```

Then run:

```sh
reap sweep --apply
```

```
RECLAIMED  TIER  RULE                PATH
---------  ----  ------------------  ------------------------------------
 1.70 GiB  0     cargo-target        ~/code/api-server/target
 38.0 MiB  0     python-bytecode     ~/code/data-pipeline/src/__pycache__
 11.0 MiB  0     python-tool-caches  ~/code/data-pipeline/.pytest_cache

Reclaimed 1.74 GiB across 3 directories.
Free space 839 GiB -> 841 GiB.
```

A plain `sweep` touches tier 0 only, which is why the `node_modules` and `.venv`
from the report above are still there. Tiers are explained below.

If you run `reap sweep --apply` while the config still says `dry_run = true`,
you get the full report, nothing is removed, and the exit code is 2 with a
message saying which file to edit. That is deliberate: a scheduled job that was
never armed should show up as a failure rather than succeed quietly forever.

## Adopt it gradually

The correct way to start using a program that deletes files is not to turn
everything on at once.

1. **Run `reap report` for a few days and read the output.** Confirm the rules
   select what you expect and nothing you care about. This costs you nothing,
   because `report` cannot delete.
2. **Turn on `--apply` by hand, for tier 0 only.** Run `reap sweep --apply`
   yourself, a few times, and look at what it did. Tier 0 is the stuff that
   costs seconds to regenerate.
3. **Install the scheduler and let it run.** Once nothing has surprised you for
   a week or two, `reap install` puts it on a timer.

If you skip to step 3 on day one, you will eventually be surprised, and it will
be at the worst possible moment.

## How it decides what to delete

Three things, in order: a rule has to name it, its tier has to be in scope, and
every guard has to pass.

### Rules

A rule names something disposable. There are two kinds.

A **path rule** matches directories and removes them:

```toml
[[rule]]
name = "cargo-target"
kind = "path"
tier = 0
dir_name = "target"
require_sibling = "Cargo.toml"
min_idle = "48h"
```

`require_sibling` is why a photographer's `~/code/photos/target/` folder is safe:
it has no `Cargo.toml` next to it, so it is not a candidate at all.

A **command rule** asks a tool to clean up after itself:

```toml
[[rule]]
name = "docker-build-cache"
kind = "command"
tier = 2
command = ["docker", "builder", "prune", "-f"]
```

Some storage belongs to a tool with its own garbage collector, and invoking that
tool is both safer and more correct than deleting its directories. Docker is the
clearest case: removing files under the daemon corrupts it.

### Tiers

Tiers are **rebuild cost, not confidence**. Every tier gets exactly the same
guards. A tier 2 rule is not less carefully checked; it is just more expensive
to be wrong about, so it needs a reason to run.

| Tier | Cost to regenerate | Examples |
| --- | --- | --- |
| 0 | Seconds. Near-free. | `__pycache__`, `.pytest_cache`, `.mypy_cache`, coverage output, `target/` in an idle workspace |
| 1 | Minutes, and bandwidth. | `node_modules`, `.venv`, `DerivedData`, Playwright and Puppeteer browser downloads, dangling Docker images |
| 2 | Slows every later build. | Cargo registry cache, sccache and ccache stores, the pnpm store, the Go build cache, Gradle caches, the Docker build cache |

A plain `reap sweep` touches tier 0. Anything higher needs either an explicit
`--tier N` or a measured shortfall in free space via `--until-free`.

### Guards

Every candidate runs the whole stack, in this order, cheapest and most decisive
first. One failure is enough.

1. **Deny floor.** The hardcoded list no config can weaken, plus your own `deny`.
2. **Canonicalize and contain.** The path must resolve, no component between the
   root and the candidate may be a symlink, and the result must be a strict
   descendant of a declared root.
3. **Depth.** Within the root's cap, or the rule's own.
4. **Pin marker.** Refused if `.reap-keep` exists in the candidate or any
   ancestor up to the root.
5. **Heartbeat.** Refused if a live heartbeat claims it. Absence of a heartbeat
   means unprotected, not protected.
6. **Git ignore.** Inside a work tree, `git check-ignore` must say it is ignored.
   An error, a missing git, or a refusal all count as no.
7. **Idle age.** The directory and a bounded sample of its immediate children
   must be older than the rule's `min_idle`.
8. **Process liveness.** Refused if any running process has its working
   directory or an open file inside it. Failing to inspect counts as no.

Guards 4 through 8 have no path to work on for command rules. The compensating
control is that a command rule's argv array must be literal: it is validated at
load time to contain no substitution syntax, and it is executed directly rather
than through a shell.

### Seeing it refuse

The most useful thing `reap` does is decline. Here it is refusing a directory
that matched a rule cleanly:

```
$ reap explain ~/code/dashboard/node_modules
path: ~/code/dashboard/node_modules
root: ~/code (depth 2)

Rules
    no   tier 0  python-bytecode              directory is named "node_modules", rule wants "__pycache__"
    no   tier 0  python-tool-caches           does not match glob **/.{pytest,mypy,ruff}_cache
    no   tier 0  cargo-target                 directory is named "node_modules", rule wants "target"
    no   tier 0  coverage-output              does not match glob **/{htmlcov,coverage}
  match  tier 1  node-modules                 matched
    no   tier 1  python-venv                  does not match glob **/{.venv,venv}
    no   tier 2  cargo-registry-cache         does not match glob ~/.cargo/registry/{cache,src}/*
    no   tier 2  npm-cache                    command rule, matches no path

Candidate
  rule      node-modules
  tier      1 (regenerable, costs real time)
  size      894 MiB
  idle      21d (rule requires 14d)

Guards
   pass     deny-floor         not on the configured deny list
   pass     containment        strictly inside ~/code
   pass     depth              depth 2 is within the cap of 6
   FAIL     pin-marker         ~/code/dashboard/.reap-keep pins this path
   -        heartbeat          not reached; an earlier guard already rejected
   -        git-ignore         not reached; an earlier guard already rejected
   -        idle-age           not reached; an earlier guard already rejected
   -        process-liveness   not reached; an earlier guard already rejected

Result: held back by the pin-marker guard. reap would not touch it.
```

`explain` exits 0 when a path would be reclaimed and 3 when it would not, so you
can ask the question from a script.

## Configuration

One TOML file. It is looked for in this order, and the first hit wins with no
merging between locations:

1. `--config <path>`
2. `$REAP_CONFIG`
3. `~/.config/reap/reap.toml`
4. `/etc/reap/reap.toml`

Unknown keys are a hard error rather than a warning. A typo that silently
disables a rule is worse than a config that refuses to load.

```toml
[global]
# Both of these must change for anything to be removed: this, and --apply.
dry_run = true

# Staging area for the atomic rename. Must be on the same filesystem as every
# root; `reap doctor` proves it with a real test rename.
quarantine = "~/.cache/reap/quarantine"

# Used by `sweep --until-free`.
low_water_pct = 15
target_free_pct = 25

# APFS reports space held by Time Machine local snapshots as used even though
# the kernel releases it on demand. Without this allowance, a low-water trigger
# fires over space the machine already has.
apfs_snapshot_slack_gb = 20

# Stop starting new work after this long.
max_run_seconds = 300

# Create a file with this name to protect a directory and everything below it.
pin_marker = ".reap-keep"

# Where a supervisor writes one file per running job.
heartbeat_dir = "~/.cache/reap/heartbeats"

# Added to the hardcoded deny floor, which no config can weaken.
deny = []

# Overrides for one host only. See "one file, several machines" below.
[machine."buildbox"]
quarantine = "/var/lib/reap/quarantine"
low_water_pct = 20

[[root]]
path = "~/code"
max_depth = 4

[[rule]]
name = "cargo-target"
kind = "path"
tier = 0
dir_name = "target"
require_sibling = "Cargo.toml"
min_idle = "6h"
os = ["linux", "macos"]
```

### Rule fields

| Field | Applies to | Meaning |
| --- | --- | --- |
| `name` | both | Unique. Appears in every log line and in `--json`. |
| `kind` | both | `"path"` (default) or `"command"`. |
| `tier` | both | 0, 1 or 2. Rebuild cost, not confidence. |
| `dir_name` | path | Match a directory by its exact name. |
| `glob` | path | Match the full path against a glob. Mutually exclusive with `dir_name`. |
| `require_sibling` | path | A file or directory with this name must exist beside the candidate. |
| `require_ancestor` | path | A file or directory with this name must exist in some ancestor, up to the root. |
| `min_idle` | path | How long untouched before it is a candidate. Defaults to `24h`. |
| `orphaned_only` | path | Require positive evidence that no live session owns it. |
| `max_depth` | path | Override the root's depth cap for this rule. |
| `os` | both | `["linux"]`, `["macos"]`, or both. Omit for both. |
| `command` | command | Literal argv. No substitution syntax is accepted. |

### One file, several machines

A `[machine."<hostname>"]` table overrides `[global]` on that host only, so one
config can live in a dotfiles repository and be shared. A root that does not
exist on a given machine is skipped with a warning rather than being an error,
which is what makes the same file work on a laptop and a build server with
different layouts.

Set `REAP_HOSTNAME` to check an overlay without finding a machine of that name:

```sh
REAP_HOSTNAME=buildbox reap doctor
```

### Adding a rule safely

1. Write the rule.
2. Run `reap report` and look at what it now selects.
3. Run `reap explain <path>` on one of the new candidates, and on something
   nearby that you expect it to leave alone.
4. Only then let it run with `--apply`.

The full reference for every key, with types and defaults, is in
[docs/configuration.md](docs/configuration.md).

## Recipes

Complete, runnable versions of each are in [examples/](examples/). Try one
without installing anything:

```sh
reap --config examples/rust.toml report
```

**Rust.** `target/` directories, and at tier 2 the shared crate cache. Usually
the single biggest win on a Rust machine: a few active projects easily come to
10 to 40 GB. See [examples/rust.toml](examples/rust.toml).

**Node, npm and pnpm.** `node_modules`, bundler output, and at tier 2 the
package manager caches. Rarely under 200 MB per project and often over 1 GB. See
[examples/node.toml](examples/node.toml).

**Python, venvs and uv.** Bytecode and tool caches are small and add up to a few
hundred megabytes. Virtual environments are the weight: 200 MB to several GB
each once anything scientific is installed. See
[examples/python.toml](examples/python.toml).

**Xcode and iOS.** Worth calling out separately. `DerivedData` and unavailable
simulator runtimes routinely account for tens of gigabytes, commonly 20 to 80 GB
on a machine with a few active projects, and nothing else on this list touches
them. See [examples/macos-ios.toml](examples/macos-ios.toml).

**Go.** `go clean -cache` as a command rule, so Go manages its own cache. In
[examples/monorepo.toml](examples/monorepo.toml) and generated by
`init --detect`.

**Java and Gradle.** `~/.gradle/caches`, commonly several GB. See
[examples/monorepo.toml](examples/monorepo.toml).

**Docker.** All command rules, because Docker's storage is the daemon's. Build
cache is commonly 5 to 50 GB on a machine that builds images. See
[examples/docker.toml](examples/docker.toml).

**Machines running unattended jobs.** CI runners, build farms and machines
running coding agents. See
[examples/agent-workspaces.toml](examples/agent-workspaces.toml) and the section
below.

Expected sizes for each, with more detail, are in
[docs/recipes.md](docs/recipes.md).

## Scheduling

`reap` is a one-shot command, not a resident daemon. The operating system owns
the timing, so there is no crash loop to supervise, nothing resident on your
laptop, and identical behaviour whether a timer or you invokes it.

```sh
reap install            # picks systemd or launchd for your platform
reap install --dry-run  # print the unit and the activation commands, write nothing
reap uninstall
```

### Linux, systemd user units

`reap install --systemd` writes `~/.config/systemd/user/reap.service`:

```ini
[Unit]
Description=Reclaim build artifacts and tool caches
After=default.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/reap --config %h/.config/reap/reap.toml sweep --until-free 25 --apply
Nice=10
IOSchedulingClass=idle

ProtectSystem=strict
ProtectHome=read-only
PrivateTmp=false
NoNewPrivileges=true
RestrictSUIDSGID=true
ReadWritePaths=%h/code %h/.cache/reap/quarantine

[Install]
WantedBy=default.target
```

and `~/.config/systemd/user/reap.timer`:

```ini
[Unit]
Description=Reclaim build artifacts and tool caches, hourly

[Timer]
OnCalendar=hourly
RandomizedDelaySec=15m
Persistent=true
Unit=reap.service

[Install]
WantedBy=timers.target
```

then runs:

```sh
systemctl --user daemon-reload
systemctl --user enable --now reap.timer
```

`ReadWritePaths` is the part worth understanding. It lists exactly your declared
roots plus the quarantine directory, and `ProtectSystem=strict` makes everything
else read-only to the unit. That is a second layer of defence enforced by the
kernel, independent of whether your config is correct: even a bug in `reap`
cannot express a write outside those paths.

Check that it fired:

```sh
systemctl --user list-timers reap.timer
journalctl --user -u reap.service -n 50
```

### macOS, launchd

`reap install --launchd` writes `~/Library/LaunchAgents/io.github.reap.plist`
and runs:

```sh
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.github.reap.plist
```

Check that it is loaded, and read its output:

```sh
launchctl print gui/$(id -u)/io.github.reap
tail -f ~/Library/Logs/reap/reap.log
```

**macOS has no launchd equivalent of `ReadWritePaths`.** There is no way to tell
launchd that a job may only write to certain paths. On macOS the deny list in
your config and the containment guard are the entire sandbox. That is a real
difference between the two platforms, not a detail, and you should weigh it when
deciding how much to let run unattended there.

Long-form instructions, including running as a system service and where logs go,
are in [docs/scheduling.md](docs/scheduling.md).

## Using it with coding agents or CI

On a machine where something else checks out repositories, builds them and
writes temporary files, the pattern has three parts:

1. **Your supervisor writes a heartbeat file per running job.** `reap` refuses
   to touch anything a live heartbeat claims.
2. **Your supervisor calls `reap gc-session <id> --apply` on every exit path**,
   including crashes and timeouts. It is safe on an id that never existed and
   safe to call twice.
3. **A timer runs `reap sweep --until-free 25 --apply` regardless.** This is the
   part that catches whatever step 2 missed, and the only part that does not
   depend on a process choosing to run it.

Step 3 is the one that matters. Steps 1 and 2 are optimisations.

### The heartbeat contract

This is the integration surface, so it is specified exactly.

- One file per running job at `<heartbeat_dir>/<session-id>.json`.
- `session-id` matches `[A-Za-z0-9._-]{1,128}`.
- The body is a JSON object. Only `session_id` is required:

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

- Rewrite or touch the file at least every `interval_seconds` (default 60).
- A heartbeat is **live** while
  `now - updated_at < interval_seconds * heartbeat_staleness_multiplier`
  (default 3.0), or while `pid` names a running process. The file's own mtime is
  used as a floor for `updated_at`, so touching the file is enough.
- A live heartbeat protects the paths it names, anything inside them, anything
  containing them, and any directory whose own name is the session id.
- A heartbeat that exists but cannot be parsed is treated as live. An unreadable
  claim of ownership is still a claim.
- **Absence of a heartbeat means unprotected, not protected.** A rule that needs
  the opposite sets `orphaned_only = true`, which requires the heartbeat
  directory to be readable and refuses when it is not.

The boundary is worth stating plainly: the only thing a job is permitted to do
about disk space is invoke `reap sweep --apply` or `reap gc-session <id>
--apply`. It computes no paths of its own and runs no removal command of its
own. Every decision about what may be deleted lives in the config file.

## Prevention

Most of the reclaimable bytes on a busy machine come from duplication that never
needed to exist: twenty copies of the same crate, the same wheel, the same
Chromium build. Setting these in your shell profile collapses the per-project
copies into one shared cache each.

```sh
export CARGO_TARGET_DIR="$HOME/.cache/cargo-target"
export RUSTC_WRAPPER=sccache
export PLAYWRIGHT_BROWSERS_PATH="$HOME/.cache/ms-playwright"
export PUPPETEER_CACHE_DIR="$HOME/.cache/puppeteer"
export UV_CACHE_DIR="$HOME/.cache/uv"
export PIP_CACHE_DIR="$HOME/.cache/pip"
export GOCACHE="$HOME/.cache/go-build"
export GRADLE_USER_HOME="$HOME/.gradle"
```

pnpm already keeps one content-addressed store across projects, which is most of
the reason to use it; `pnpm config set store-dir` moves it if you need to.

`reap doctor` reports which of these are unset, because each unset one silently
reintroduces N-way duplication.

Being honest about it: for a lot of people this section will save more space
than the rest of this program does, and it does not require running anything on
a timer.

## Troubleshooting

**`reap` reports nothing on macOS, and no error.**
Full Disk Access is not granted. A scheduled job cannot read `~/Library/Caches`,
`~/Documents`, `~/Desktop` or `~/Downloads` without it, and the denial is a
silent `EPERM` rather than a prompt. `reap doctor` detects this and says so.
Grant it in System Settings, Privacy and Security, Full Disk Access, adding the
`reap` binary (and your terminal, if you run it by hand).

**Free space did not change after a sweep on macOS.**
APFS local snapshots. Time Machine keeps hourly local snapshots, and the space
they hold is reported as used even though the kernel releases it on demand.
Deleting a file inside a snapshot's window does not immediately return its
blocks. Check with `tmutil listlocalsnapshots /`, which `reap doctor` also
reports. The `apfs_snapshot_slack_gb` setting stops this from triggering
spurious escalation; it does not make the snapshots go away.

**`doctor` says the quarantine rename fails, with EXDEV.**
Your quarantine directory is on a different filesystem than one of your roots,
and `rename(2)` cannot cross filesystems. Move `quarantine` in your config onto
the same filesystem as the roots. On macOS note that two APFS volumes in the
same container share free space but still fail this, which is why `reap` tests
with a real rename instead of comparing device numbers. `reap` refuses to sweep
rather than falling back to deleting in place.

**It deleted something I wanted.**
Put a `.reap-keep` file in that directory, or any directory above it, and it is
protected permanently. Before that, `reap explain <path>` would have told you it
was a candidate. If the sweep has staged the tree but not yet removed it, it is
still in your quarantine directory under a `staged-` name, and the
`manifest.jsonl` beside it says where each one came from. Move it back. Once the
recursive removal has run, it is gone; there is no undo, and nothing here
pretends otherwise.

**The timer runs but nothing happens.**
Exit code 3 means nothing matched, which is not a failure. Run `reap doctor` and
look for rules reported as "matched nothing", which usually means a typo in
`dir_name` or a `glob` that points outside every declared root. Exit code 2 with
a message about `dry_run` means the config was never armed.

**It is slow on a large tree.**
Lower `max_depth` on your roots, or set a tighter `max_depth` on individual
rules that only ever match near the top. Prefer several specific roots over one
broad one. `max_run_seconds` bounds any single run: it stops starting new work
when the budget is spent, finishes what is in flight, and exits 0 with a note.

More, in question and answer form, in
[docs/troubleshooting.md](docs/troubleshooting.md).

## Comparison to alternatives

- **[cargo-sweep](https://github.com/holmgr/cargo-sweep)** cleans Rust build
  artifacts, with more precision than `reap` has about which artifacts within a
  `target/` directory are stale. If Rust is all you care about, it is a smaller
  thing to install and understand, and you should probably use it.
- **[kondo](https://github.com/tbillington/kondo)** finds and cleans project
  artifacts across many languages, interactively or in bulk. It needs no config,
  which is the tradeoff: `reap` will not touch anything you did not declare, and
  `kondo` will not make you declare anything.
- **[npkill](https://github.com/voidcosmos/npkill)** is an interactive
  `node_modules` finder. Good at exactly that, and pleasant to use.
- **`docker system prune`** already does what the Docker recipe here does. The
  only thing `reap` adds is running it on the same schedule and under the same
  free-space threshold as everything else.
- **BleachBit, and macOS Storage Management** clean a much broader range of
  application caches, browser data and system files. They are aimed at a whole
  machine rather than at a developer's build output, and neither is meant to run
  unattended on a timer.

What `reap` adds: one declarative config covering every language and both
platforms, guards designed so that unattended scheduled operation is safe, and a
workflow that starts with a dry run and requires two deliberate opt-ins before
anything is removed.

When to use something else: if you work in one language, a single-purpose tool
is less to learn and less to trust. If you want to clean up once and move on,
`kondo` or `npkill` will get you there faster than writing a config will. `reap`
earns its complexity only if you want this to keep happening without you.

## Contributing

Bug reports and patches are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) covers
running the tests, the requirement that every invariant in the safety model maps
to a named test, and the rule that any change to a guard or to the executor
needs a test demonstrating the failure it prevents.

## License

Dual licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your
option. This is the Rust ecosystem convention.

## Security

Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md),
not in a public issue. The highest severity class for this project is any
path-handling bug that allows deletion outside a declared root; if you find one,
that is the thing to report.
