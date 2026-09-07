# reap

`reap` reclaims disk space taken by build artifacts, dependency trees and tool
caches. You declare what is disposable in one TOML file, and a scheduler runs it
on a timer. It works the same way on macOS and Linux.

It never deletes anything by default. A dry run is the default, and turning that
off requires two separate opt-ins: `--apply` on the command line and
`dry_run = false` in the config file.

```sh
cargo install reap-cli   # the binary is called `reap`

reap init --detect       # write a config for this machine
reap doctor              # check that everything resolves
reap report              # see what could be reclaimed. Deletes nothing.
```

## Safety

- Nothing is deleted unless a rule in your config names it. No heuristics.
- Anything git does not consider ignored is never touched.
- A `.reap-keep` file protects a directory and everything below it.
- Deletion goes through an atomic rename into quarantine, so an interrupted run
  cannot leave a half-deleted tree.
- `reap explain <path>` shows exactly why any path would or would not be
  touched, rule by rule and guard by guard.

Full documentation, the guard stack, worked configurations for Rust, Node,
Python, Xcode, Go, Gradle and Docker, and the scheduling setup for systemd and
launchd are in the repository:

<https://github.com/sunhaoxiangwang/gc-for-ai-agent>

## License

Dual licensed under MIT or Apache-2.0, at your option.
