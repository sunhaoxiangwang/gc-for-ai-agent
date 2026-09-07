# reap-core

Configuration, rule matching, the planner and the guard stack for
[`reap`](https://github.com/OWNER/reap), a guarded reclaimer for build artifacts
and tool caches.

This crate is read-only by construction. It contains no call that creates,
renames, truncates or removes anything on disk, no `unsafe`, and no process
spawning. A test scans the crate's source and fails if any of them appear, so
"the planner cannot delete" is a property of what is compiled rather than a
convention.

You probably want the [`reap-cli`](https://crates.io/crates/reap-cli) crate,
which provides the `reap` binary. This one is published so the pieces are
separable, not because it is useful on its own.

## License

Dual licensed under MIT or Apache-2.0, at your option.
