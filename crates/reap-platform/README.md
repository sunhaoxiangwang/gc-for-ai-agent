# reap-platform

Platform abstraction for [`reap`](https://github.com/OWNER/reap): process
inspection, volume statistics, and the git-ignore oracle.

Everything that has to ask the operating system a question lives behind one of
the traits here, so `reap-core` stays free of `#[cfg(target_os)]`. On Linux this
reads `/proc`; on macOS it uses `libproc` plus hand-written bindings for
`PROC_PIDVNODEPATHINFO` and `PROC_PIDFDVNODEPATHINFO`, which `libproc` does not
expose.

This is the only crate in the workspace containing `unsafe`, in one module.

You probably want the [`reap-cli`](https://crates.io/crates/reap-cli) crate,
which provides the `reap` binary.

## License

Dual licensed under MIT or Apache-2.0, at your option.
