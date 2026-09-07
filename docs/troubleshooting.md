# Troubleshooting

Question and answer. Each has a diagnosis and a fix.

`reap doctor` is the first thing to run for almost all of these. It validates
the config, resolves every root, proves the quarantine rename works, probes for
permission problems, reports free space, and names every rule that matched
nothing.

## Nothing is reported at all

### `reap report` shows nothing, and no error, on macOS

**Diagnosis.** Full Disk Access is not granted. A process cannot read
`~/Library/Caches`, `~/Documents`, `~/Desktop` or `~/Downloads` without it, and
the denial is a silent `EPERM` rather than a prompt. This bites hardest for a
scheduled job, because the terminal you tested in may already have access while
the LaunchAgent does not.

**Fix.** `reap doctor` probes each root with a test read and reports it as a
failure with the remedy. Grant access in System Settings, Privacy and Security,
Full Disk Access, adding the `reap` binary itself (and your terminal, if you run
it by hand).

### `reap report` shows nothing on a machine that is definitely full

**Diagnosis.** Usually one of three things:

- Your roots do not cover where the space is going. `reap doctor` lists every
  root it resolved, and warns about roots that do not exist.
- Every rule matched nothing. `doctor` reports rules as "matched nothing", which
  usually means a typo in `dir_name`, or a `glob` that points outside every
  declared root.
- Everything matched but a guard refused it. Run `reap report --all`, which
  lists the held-back candidates with the guard that refused each one.

**Fix.** Work from `doctor` output, then `reap report --all`, then
`reap explain <path>` on one specific path.

### A rule I wrote matches nothing

**Diagnosis.** In order of likelihood: the glob points outside every root; the
`dir_name` has a typo; `require_sibling` or `require_ancestor` is not satisfied;
the tier is above what you asked for; `os` excludes this platform.

**Fix.** `reap explain <path>` on a directory you expect it to match. It reports
every rule with the specific reason it did not match, including "does not match
glob ..." with the expanded glob, which is usually enough to spot the problem.

## Nothing is deleted

### `reap sweep --apply` reports the list but removes nothing, exit code 2

**Diagnosis.** The config still has `dry_run = true`. Removal needs both opt-ins.

**Fix.** Set `dry_run = false` in the file named in the message, then run again.
The exit code is 2 rather than 0 on purpose, so a scheduled unit that was never
armed shows up as a failure instead of succeeding quietly forever.

### The timer runs but nothing happens

**Diagnosis.** Check the exit code first.

- **3** means nothing matched. That is not a failure. Either there was genuinely
  nothing to do, or your rules match nothing; `doctor` distinguishes these.
- **2** means the config was never armed. See above.
- **4** means partial failure. The output names what went wrong.

**Fix.**

```sh
# Linux
systemctl --user list-timers reap.timer
systemctl --user status reap.service
journalctl --user -u reap.service -n 50

# macOS
launchctl print gui/$(id -u)/io.github.reap
tail -50 ~/Library/Logs/reap/reap.log
```

On Linux, also check `loginctl enable-linger "$USER"`: without lingering, a user
timer does not run when you are logged out.

### A directory I expected to go is still there

**Diagnosis.** A guard refused it, or it is above the tier you swept.

**Fix.** `reap explain <path>` shows the whole guard stack with pass, fail or
skipped for each. The most common answers:

- **git-ignore.** Your `.gitignore` does not list it, so git considers it work.
  Add it to `.gitignore`, which is the correct fix, or move the directory out of
  the repository.
- **idle-age.** It is not old enough yet. The message says the actual age and
  the required one.
- **pin-marker.** There is a `.reap-keep` file in it or above it.
- **process-liveness.** Something is running inside it.
- Or it is tier 1 or 2, and a plain `sweep` only touches tier 0. Use
  `--tier 1` or `--until-free`.

## Quarantine and filesystems

### `doctor` says the quarantine rename fails with EXDEV

**Diagnosis.** Your quarantine directory is on a different filesystem than one
of your roots. `rename(2)` cannot cross filesystems, and `reap` uses an atomic
rename so that an interrupted run can never leave a half-deleted tree.

**Fix.** Move `quarantine` in your config onto the same filesystem as the roots.
If your roots span two filesystems, you currently need one config per
filesystem, each with its own quarantine.

On macOS, note that two APFS volumes in the same container share free space and
report different device numbers, yet `rename` between them still fails. That is
why `reap` tests with a real rename rather than comparing device numbers, and
why the answer is not "but they are on the same disk".

`reap` refuses to sweep when this check fails. It does not fall back to removing
in place, because that would trade the entire interruption guarantee for the
convenience of not stopping.

### `doctor` says it cannot create the quarantine directory

**Diagnosis.** Usually a config written for a server (`/var/lib/reap/quarantine`)
being run as an unprivileged user, or a read-only parent.

**Fix.** Point `quarantine` somewhere you can write, or create the directory and
`chown` it to the user the timer runs as.

### There are `staged-` directories in my quarantine

**Diagnosis.** A previous run was interrupted between the rename and the
removal. This is the designed failure mode, not a problem.

**Fix.** Nothing. The next run clears them and says so in a note. If you want
something back, it is still there: the `manifest.jsonl` beside them says which
original path each `staged-` directory came from. Move it back before the next
run.

### There are `.reap-rename-test-` directories in my roots

**Diagnosis.** The startup rename check creates one, renames it into quarantine
and removes it. If the process was killed at exactly the wrong moment, one can
survive.

**Fix.** They are empty and safe to delete.

## Free space

### Free space did not change after a sweep on macOS

**Diagnosis.** APFS local snapshots. Time Machine keeps hourly local snapshots,
and the blocks held by a snapshot are reported as used even after you delete the
files. The kernel releases them on demand under pressure, which is why the space
is called purgeable.

**Fix.** Nothing is wrong. Check what exists:

```sh
tmutil listlocalsnapshots /
```

`reap doctor` reports the count, and `apfs_snapshot_slack_gb` stops this from
triggering spurious escalation to tier 2. It does not make the snapshots go
away, and it is not meant to: they are your backups.

If you genuinely need the space now, `tmutil thinlocalsnapshots / <bytes> 4`
asks the system to purge some. That is a Time Machine operation, and `reap` will
not do it for you.

### `sweep --until-free` says it ran every tier and is still short

**Diagnosis.** There genuinely is not enough reclaimable material under your
rules to reach the target, or the space is going somewhere your roots do not
cover.

**Fix.** Look at what is actually large. `reap report --tier 2` shows everything
your rules can see; if the total is well below what you need, the space is
elsewhere. A `du -sh ~/* | sort -h` will usually find it, and then it is a
question of adding a root or a rule.

## Performance

### It is slow on a large tree

**Diagnosis.** The walk is proportional to how much of the filesystem you told
it to look at. Size measurement is the expensive part, and it runs for every
matched candidate.

**Fix.**

- Lower `max_depth` on your roots. This is the single biggest lever.
- Set a tighter `max_depth` on individual rules that only ever match near the
  top. A rule with `max_depth = 3` under a root with `max_depth = 8` stops the
  walker much earlier for that rule.
- Prefer several specific roots over one broad one. `~/code` and `~/work` beat
  `~` by a wide margin.
- `max_run_seconds` bounds any single run: it stops starting new work when the
  budget is spent, finishes what is in flight, and exits 0 with a note. The next
  run continues.

### It hammers the disk while I am working

**Diagnosis.** The generated scheduler units already set `Nice=10` and idle IO
scheduling on Linux, and `ProcessType Background` with `LowPriorityIO` on macOS.
If you are running it by hand, none of that applies.

**Fix.** Lower `max_run_seconds`, or run it less often, or run it by hand at a
time that suits you. On Linux you can also `systemd-run --user --nice=19` it.

## Configuration

### The config will not load and the error names a key I did not write

**Diagnosis.** Unknown keys are a hard error, and the message names the key. A
typo in `dry_runn` or `min_idl` reads as an unknown key.

**Fix.** Fix the spelling. The full list of valid keys is in
[configuration.md](configuration.md).

### My `[machine."..."]` overlay is not applying

**Diagnosis.** The hostname does not match. `reap doctor` prints the hostname it
detected and whether an overlay applied.

**Fix.** Match the exact hostname, or the short form: `[machine."buildbox"]`
applies on both `buildbox` and `buildbox.example.com`. To test without finding a
machine of that name:

```sh
REAP_HOSTNAME=buildbox reap doctor
```

### A root that exists on my other machine breaks this one

**Diagnosis.** It should not. A root that does not exist is skipped with a
warning, not an error. That is what makes one config shareable across machines.

**Fix.** If you are seeing a failure rather than a warning, the root exists but
is not a directory, or is a symlink (declare what it resolves to), or cannot be
read (a permissions or TCC problem).

## Recovery

### It deleted something I wanted

**Diagnosis.** A rule you wrote named it, and every guard passed.

**Fix, in order of usefulness:**

1. **Prevent the next one.** Put a `.reap-keep` file in that directory or any
   directory above it. It is protected permanently, with no config change.
2. **Check quarantine.** If the sweep staged the tree but has not yet finished
   removing it, it is still under your quarantine directory with a `staged-`
   name, and `manifest.jsonl` says where it came from. Move it back.
3. **Once the removal has run, it is gone.** There is no undo and no trash bin.
   This is worth being clear about rather than hedging: the quarantine step
   protects against an interrupted run, not against a completed one.

Then work out why. `reap explain <path>` on a similar path will show you which
rule matched and which guards passed, and the answer is usually either a rule
that was broader than you meant or a `.gitignore` entry you had forgotten about.

### How do I make sure this never happens

Run `reap report` for a few days before arming anything, use `.reap-keep`
liberally, and keep `dry_run = true` until the output has stopped surprising
you. The three-stage rollout in the README exists for this reason.
