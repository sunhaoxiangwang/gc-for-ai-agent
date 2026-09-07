# Safety model

This document is the argument for why it is reasonable to let this program
delete files on a timer. It covers the guard stack in order with the reasoning
behind each guard, the quarantine mechanism, the two-opt-in design, and an
explicit statement of what none of it protects against.

If you only read one section, read [What this does not protect
against](#what-this-does-not-protect-against).

## The shape of the argument

Three separate things have to be true before a byte is removed.

1. **A rule you wrote names it.** There is no inference. `reap` does not decide
   a directory looks disposable.
2. **Its tier is in scope.** A plain sweep touches tier 0. Higher tiers require
   an explicit flag or a measured shortfall in free space.
3. **Every guard passes.** Eight predicates, all of which must agree.

Then removal happens in two steps, and the first one is atomic.

The reason to separate 1 from 3 is that they fail differently. A bad rule
selects too much; the guards are what stop a bad rule from being a disaster. A
guard that is too strict costs you disk space; a guard that is too loose costs
you work. Every guard here is written to fail towards refusing.

## The invariants

These are correctness requirements, not preferences. Each has at least one test
that names it. `tests/invariants.rs`, `tests/guards.rs`, `tests/escapes.rs` and
`crates/reap-cli/tests/cli.rs` are where they live.

1. Nothing is deletable unless a configured rule names it.
2. Every candidate is canonicalized, and the canonical result must remain a
   strict descendant of a declared root.
3. The planner cannot delete. `reap-core` contains no filesystem write code.
4. Dry run is the default. Mutation requires `--apply` and `dry_run = false`.
5. Never delete anything git does not consider ignored.
6. Never delete an active workspace.
7. Deletion is preceded by an atomic rename into quarantine.
8. A hardcoded deny floor wins over all configuration.
9. Every mutation is logged with path, byte count, rule name and tier, before it
   happens.

## Invariant 3, and why it is structural

`reap-core` holds the config parser, the walker, the planner and the guard
stack: every decision about what to reclaim. It is compiled without any call
that creates, renames, truncates or removes anything, without `unsafe`, and
without `std::process::Command`. `tests/invariants.rs` scans the crate's source
and fails on any of them, and both `reap-core` and `reap-cli` declare
`#![forbid(unsafe_code)]`.

This is why the git-ignore oracle lives in `reap-platform` even though it is not
platform-specific: it shells out to `git`, and `reap-core` is not allowed to
spawn processes.

The consequence is that no bug in the planner or in a guard can become a
deletion. Whatever else goes wrong, the worst a bug in `reap-core` can do is
propose the wrong thing to a caller that then runs it past the guards again.

## The guard stack

Ordered cheapest and most decisive first. Evaluation stops at the first
rejection; the rest are reported as "not reached" rather than silently omitted,
so `reap explain` always shows the whole stack.

### 1. Deny floor

Hardcoded, then your `deny` list. No configuration can weaken the hardcoded
part.

Refused unconditionally:

- `/`, and your home directory itself.
- Any path with fewer than three components below the filesystem root. `/a/b` is
  refused; `/a/b/c` is considered. This is blunt on purpose: every path shallow
  enough to fail it is a path whose removal is a catastrophe rather than a
  cleanup.
- The usual system directories as whole paths: `/usr`, `/var`, `/etc`, `/bin`,
  `/System`, `/Library`, `/Applications`, `/Users`, `/home`, and the rest.
- The standard directories directly under your home: `Documents`, `Desktop`,
  `Downloads`, `Library`, `.config`, `.local`, `.ssh`, and so on.
- Anything containing a `.git`, `.ssh`, `.gnupg`, `.aws`, `.kube` or
  `.password-store` component, anywhere in the path. A build regenerates; a
  repository's object store and a private key do not.

Your `deny` list is checked after, and refuses a path or anything below it.

The floor is checked against the path as walked and again against its canonical
form, because the two can differ.

### 2. Canonicalize and contain

Three things must hold:

- The path resolves. A broken symlink is a rejection, not an error.
- No component between the declared root and the candidate is a symlink.
- The canonical result is a **strict** descendant of a canonical declared root,
  so a root can never remove itself.

The symlink scan runs from the declared root downwards, not from `/`. Above the
root it would reject every macOS machine, because `/tmp` is a symlink to
`/private/tmp`, and it would add nothing: containment is decided by comparing
canonical forms, so a symlink above the root cannot move a candidate out of the
root. A symlink *below* the root can, which is why that region is checked one
component at a time.

The planner also refuses to walk a symlinked root at all, and never proposes a
symlink as a candidate.

### 3. Depth

Within the root's `max_depth`, or the rule's own override. The planner applies
this while walking; the guard re-checks it, so a candidate built some other way
(by `explain`, say) gets the same test.

### 4. Pin marker

Refused if `.reap-keep` exists in the candidate directory or in any ancestor up
to and including the root. The search stops at the root, so a marker above your
declared root protects nothing.

This is the escape hatch, and it is deliberately the simplest thing in the
program: create a file, and everything below it is off limits, permanently, with
no config change and no restart. It is meant to be used, and it should be the
first thing you reach for when `reap` proposes something you want to keep.

### 5. Heartbeat

Liveness of a workspace comes from whatever supervises the work saying so, not
from `reap` guessing at mtimes. The contract is in
[configuration.md](configuration.md).

A live heartbeat protects the paths it names, anything inside them, anything
containing them, and any directory whose own name is the session id. A heartbeat
is live while it is recent enough, or while the pid it names is running: a
supervisor can die without cleaning up while the work it started carries on.

A heartbeat file that exists but cannot be parsed is treated as live. An
unreadable claim of ownership is still a claim.

**Absence of a heartbeat means unprotected, not protected.** This is the
important asymmetry. If nothing is claiming a directory, the other seven guards
are what decide. A rule that needs the opposite sets `orphaned_only = true`,
which requires the heartbeat directory to be readable and refuses when it is
not, so "we could not check" never reads as "nothing owns this".

### 6. Git ignore

For any candidate inside a git work tree, `git check-ignore --quiet` must exit 0.

This single predicate removes most of the ways a reclaimer can eat source code.
Anything git tracks, or would track, is somebody's work. You already maintain
the list of what is disposable in a repository; it is called `.gitignore`.

An inconclusive answer is a rejection: git not installed, a broken repository, a
refusal over ownership, a signal. Not knowing is not permission.

Outside a work tree the guard is skipped and reported as skipped. A skipped
guard never permits anything the other guards would have refused.

### 7. Idle age

The directory's own mtime, plus a bounded probe of up to 256 immediate children.
A directory's mtime only changes when an entry is added or removed, so it alone
would call an actively-written-into build directory idle.

The probe is deliberately shallow. This runs for every candidate, and a deep
walk to answer "was this touched recently" would cost more than the reclamation
saves. Writes deeper in the tree that touch nothing near the top are not caught
here on purpose; guard 8 is what covers an active build.

A future mtime, from clock skew or a restored archive, reads as zero idle time,
which is the conservative direction.

### 8. Process liveness

Refused if any running process has its working directory or an open file inside
the candidate. On Linux this reads `/proc/*/cwd` and `/proc/*/fd/*`; on macOS it
uses `proc_listpids` with `PROC_PIDVNODEPATHINFO` and `PROC_PIDFDVNODEPATHINFO`.

Permission errors on other users' processes are expected and skipped. But a
*total* failure to inspect is a rejection: we cannot show the directory is idle,
so we do not act as though it is.

The inspection is cached once per run. On macOS it costs thousands of syscalls,
and running it per candidate would dominate the runtime.

### Command rules

Guards 4 through 8 have no path to work on. The compensating control is that a
command rule's argv array must be literal: it is validated at load time to
contain no `$` or backtick, it is never built from anything discovered at run
time, and it is executed directly rather than through a shell.

Command rules still respect tiers, free-space thresholds, and dry run. In a dry
run they print what they would execute.

## Quarantine, and the two-step removal

```
log intent (path, bytes, rule, tier)
rename(candidate, quarantine/staged-<unique>)
append to the run manifest
...
remove_dir_all each quarantine entry
```

The rename is atomic and O(1). The moment it returns, the workspace is clean.
The recursive removal that follows is slow and interruptible, and nothing
depends on it finishing.

This is what makes an interruption safe. Kill the process at any point and the
worst state you can be in is: some trees are sitting in quarantine under
`staged-` names, and a `manifest.jsonl` beside them says where each came from.
The next run clears them without re-planning and without consulting the guards
again, because they were judged before they got there and they are already
outside every root.

There is no state in which a build directory is half-removed.

Two consequences worth knowing:

- **The quarantine directory must be on the same filesystem as every root**,
  because `rename(2)` cannot cross filesystems. `reap` proves this with a real
  test rename at startup, in `doctor` and again at the top of every sweep,
  rather than comparing device numbers. On macOS two APFS volumes in one
  container share free space and report different `st_dev`, yet `rename` between
  them still fails with `EXDEV`, so the device number is not a reliable test.
- **If the rename check fails, `reap` refuses to sweep.** It does not fall back
  to removing in place. Falling back would trade the entire interruption
  guarantee for the convenience of not stopping.

Removal never follows a symlink. A symlinked directory inside a staged tree is
unlinked, not descended into. Per-entry errors are collected rather than fatal;
the run reports them and exits 4.

## The two opt-ins

Mutation requires `--apply` on the command line **and** `dry_run = false` in the
config file.

These are different kinds of decision. `--apply` is something you type in a
moment, possibly from shell history, possibly in the wrong terminal. `dry_run =
false` is a line you edited in a file, on purpose, having looked at it. Requiring
both means no single mistake in either place removes anything.

Running `--apply` against a config that still says `dry_run = true` prints the
full dry report, removes nothing, and exits **2**. That is deliberate rather than
a quiet success: a scheduled unit that was never armed should show up in
`systemctl status`, not report success forever while doing nothing.

## What this does not protect against

Stating this plainly is more useful than any of the above.

- **A rule you wrote that is wrong.** If you write a rule matching `dir_name =
  "src"`, the guards will happily let it through in any repository whose
  `.gitignore` lists `src`. The guards constrain *where* and *when*, not
  *whether your rule made sense*. Run `reap report` before arming anything.
- **A `.gitignore` that is wrong.** The git-ignore guard trusts your repository.
  If you have ignored a directory of work you care about, `reap` will treat it
  as disposable.
- **Anything outside a git work tree.** The strongest guard in the stack is
  skipped there. Directories that are not in a repository lean entirely on the
  other seven, and the pin marker is your control.
- **Recovery after removal.** Quarantine protects you from an *interrupted* run,
  not from a completed one. Once the recursive removal has finished, the data is
  gone. There is no undo and no trash bin.
- **A tool's own garbage collector.** Command rules run what you configured.
  `docker builder prune -f` does what Docker decides it does, and `reap` neither
  knows nor constrains that.
- **Other people's processes on a shared machine.** Process inspection sees what
  your user is allowed to see. On a multi-user machine, another user's build
  running inside a directory you can write to may not be visible.
- **Malicious local input.** The threat model is accidents, not an attacker who
  can already write into your roots. Someone who can create files where `reap`
  looks can influence what it selects, within the guards.
- **macOS sandboxing.** systemd's `ReadWritePaths` gives Linux a kernel-enforced
  second layer independent of the config. launchd has no equivalent. On macOS,
  the deny list and the containment guard are the entire sandbox.
- **Disk quotas.** Linux offers XFS project quotas and ZFS datasets to cap a
  runaway build before it fills a disk. macOS offers nothing equivalent. `reap`
  is reclamation, not prevention, and it cannot stop a single build from filling
  a volume between two runs.

## If you find a hole

A path-handling bug that allows deletion outside a declared root is the
highest-severity class of bug this project can have. Report it privately as
described in [SECURITY.md](../SECURITY.md) rather than in a public issue.
