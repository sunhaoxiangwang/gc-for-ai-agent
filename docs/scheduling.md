# Scheduling

`reap` is a one-shot command, not a resident daemon. The operating system owns
the timing.

That choice is deliberate. A daemon means a crash loop to supervise, resident
memory on a laptop that is doing nothing most of the time, and a difference
between "what happens on the timer" and "what happens when I run it". A one-shot
command has none of those, and it is trivially testable: the thing the timer
runs is the thing you can run yourself.

## The quick way

```sh
reap install            # picks systemd on Linux, launchd on macOS
reap install --dry-run  # print the unit and the activation commands, write nothing
reap uninstall
```

`install` writes the unit with your binary path and config path already filled
in, then activates it. `uninstall` deactivates and removes the unit, and prints
a list of everything it deliberately left behind: your config, your quarantine
directory, your heartbeat directory, your logs, and the binary.

Always look at `--dry-run` first. It prints the exact file contents and the
exact commands, and writes nothing.

## Linux: systemd user units

### What gets written

`~/.config/systemd/user/reap.service`:

```ini
[Unit]
Description=Reclaim build artifacts and tool caches
Documentation=https://github.com/sunhaoxiangwang/gc-for-ai-agent
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

`~/.config/systemd/user/reap.timer`:

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

### `ReadWritePaths`, and why it matters

`ProtectSystem=strict` makes the entire filesystem read-only to the unit.
`ReadWritePaths` then punches holes for exactly your declared roots and the
quarantine directory.

This is a **second layer of defence that does not depend on your config being
correct**. The guards are software, and software has bugs. `ReadWritePaths` is
enforced by the kernel: a bug in `reap` cannot express a write outside those
paths, whatever the planner decided.

`reap install --systemd` generates the list from your config. If you edit your
roots, regenerate the unit:

```sh
reap install --systemd
systemctl --user daemon-reload
```

`PrivateTmp=false` is set because a private `/tmp` would hide the real one, and
some configurations declare roots there.

### Timing

`OnCalendar=hourly` with `RandomizedDelaySec=15m` spreads the load so a fleet of
machines does not all sweep on the hour. `Persistent=true` catches up after the
machine was asleep, which on a laptop is most of the time.

To change the cadence, edit the timer and reload:

```sh
systemctl --user edit --full reap.timer
systemctl --user daemon-reload
```

### Checking it actually fired

```sh
systemctl --user list-timers reap.timer
systemctl --user status reap.service
journalctl --user -u reap.service -n 50
journalctl --user -u reap.service --since today | grep reclaiming
```

Every removal is logged before it happens, with path, byte count, rule and tier,
so the journal is a complete record of what went away.

Exit code 3 in `systemctl status` means nothing matched, which is not a failure.
Exit code 2 means the config still says `dry_run = true`.

### Surviving logout

A user timer only runs while you have a session, unless lingering is enabled:

```sh
loginctl enable-linger "$USER"
```

Without it, a laptop that you log out of will not sweep.

### As a system service

If you want one timer covering several users' directories, install the units
under `/etc/systemd/system/` instead, add `User=` and `Group=`, and replace
`%h` with absolute paths. Keep `ReadWritePaths` accurate: it is doing more work
in this configuration, not less.

```sh
sudo cp dist/systemd/reap.* /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now reap.timer
```

Run it as the user who owns the files, not as root. Nothing here needs root, and
the deny floor is a poor substitute for not having the privilege in the first
place.

## macOS: launchd

### What gets written

`~/Library/LaunchAgents/io.github.reap.plist`, with your binary and config paths
filled in, an hourly `StartInterval`, `ProcessType` `Background`,
`LowPriorityIO`, `Nice 10`, and output to `~/Library/Logs/reap/reap.log`.

`StartInterval` rather than `StartCalendarInterval`: launchd runs a missed
interval job once after wake, which is what a laptop wants. A calendar schedule
queues one run per hour the machine spent asleep.

### Activating

```sh
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.github.reap.plist
```

### Checking it actually fired

```sh
launchctl print gui/$(id -u)/io.github.reap
tail -f ~/Library/Logs/reap/reap.log
log show --predicate 'process == "reap"' --last 1h
```

`launchctl print` shows the last exit status. As above, 3 means nothing matched
and 2 means the config was never armed.

### Removing

```sh
launchctl bootout gui/$(id -u)/io.github.reap
rm ~/Library/LaunchAgents/io.github.reap.plist
```

or just `reap uninstall`.

### Full Disk Access

A LaunchAgent cannot read `~/Library/Caches`, `~/Documents`, `~/Desktop` or
`~/Downloads` without Full Disk Access, and the denial is a silent `EPERM`
rather than a prompt. The symptom is a scheduled run that reports nothing while
running the same command by hand works.

Grant it in System Settings, Privacy and Security, Full Disk Access. Add the
`reap` binary itself. `reap doctor` probes each root with a test read and tells
you when this is the problem.

### There is no `ReadWritePaths` on macOS

launchd has no equivalent. There is no way to tell it that a job may only write
to certain paths.

On macOS, the deny list in your config and the containment guard are the entire
sandbox. That is a real difference between the two platforms. Weigh it when
deciding how much to let run unattended, and consider keeping `dry_run = true`
on macOS for longer than you would on Linux.

`sandbox-exec` exists but is deprecated and undocumented, and building a policy
for it is not something this project can responsibly recommend.

## Neither: cron

If you have neither, a cron entry works. You lose `Persistent`, the sandboxing,
and the log integration, but the command is the same:

```cron
17 * * * * /usr/local/bin/reap --config "$HOME/.config/reap/reap.toml" sweep --until-free 25 --apply >> "$HOME/.cache/reap/reap.log" 2>&1
```

Use a minute other than 0 so you are not competing with everything else on the
machine.

## Scheduling the observation period first

The README asks you to run `reap report` for a few days before arming anything.
That advice is easy to give and easy to forget, so it is worth scheduling too.

An observation agent runs `report --json` on a timer and appends the result to a
log. It cannot delete: `report` has no mutation path at all, whatever the config
says. After a week you have a record of exactly what would have been reclaimed,
which is the evidence you actually want before setting `dry_run = false`.

Do not use `reap install` for this. That generates a `sweep --apply` unit, and
running it against a config that still says `dry_run = true` exits 2 every time
by design, so an unarmed scheduler shows up as a failure rather than succeeding
quietly. Observation wants a different command.

### macOS

Write `~/Library/LaunchAgents/io.github.reap.observe.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>io.github.reap.observe</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/sh</string>
    <string>-c</string>
    <string>mkdir -p "$HOME/.cache/reap/observations" &amp;&amp; /usr/local/bin/reap --no-color --json report --tier 2 --all &gt;&gt; "$HOME/.cache/reap/observations/$(date +%Y-%m-%d).json"</string>
  </array>
  <key>StartInterval</key><integer>21600</integer>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Background</string>
  <key>LowPriorityIO</key><true/>
</dict>
</plist>
```

```sh
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.github.reap.observe.plist
```

### Linux

```sh
systemd-run --user --on-calendar='*-*-* 06,18:00:00' --unit=reap-observe \
  /bin/sh -c 'mkdir -p "$HOME/.cache/reap/observations" && reap --no-color --json report --tier 2 --all >> "$HOME/.cache/reap/observations/$(date +%%Y-%%m-%%d).json"'
```

### Reading the result

Every path that was ever selected, across the whole period:

```sh
cat ~/.cache/reap/observations/*.json \
  | grep -o '"path": "[^"]*"' | sort -u
```

Read that list. If everything on it is something you would have deleted by hand
without thinking about it, you are ready to set `dry_run = false`. If anything
on it makes you pause, that is a rule to narrow or a `.reap-keep` to place, and
it is much cheaper to learn now.

### Stopping it

```sh
# macOS
launchctl bootout gui/$(id -u)/io.github.reap.observe
rm ~/Library/LaunchAgents/io.github.reap.observe.plist

# Linux
systemctl --user stop reap-observe.timer
```

## What to schedule

The generated units run:

```sh
reap sweep --until-free 25 --apply
```

This escalates: tier 0 first, then a fresh look at free space, then tier 1, then
tier 2, stopping as soon as the target is met and reporting which tier it
stopped at. A machine that got what it needed from tier 0 never pays the cost of
throwing away a shared cache.

If you would rather never touch tiers 1 and 2 unattended, schedule a plain
sweep instead:

```sh
reap sweep --apply
```

and handle real pressure by hand.
