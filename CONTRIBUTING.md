# Contributing

Bug reports and patches are welcome.

The one thing to understand before changing anything: **the failure mode of this
program is destroying someone's work.** The tests are not a chore attached to
the feature. They are the feature.

## Running the tests

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
scripts/validate-examples.sh
```

`--workspace` matters. The repository root is itself a package (`reap-tests`) so
the integration tests can live in `tests/` at the top level, and a bare
`cargo build` would otherwise only build that.

CI runs all four on macOS aarch64, macOS x86_64, Linux x86_64 and Linux aarch64.

To check the Linux code paths from a Mac without a Linux machine:

```sh
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
cargo clippy --workspace --target x86_64-unknown-linux-musl -- -D warnings
```

`cargo check` and `cargo clippy` work for another target without a linker.

## How the workspace is laid out

- **`crates/reap-core`** holds every decision: config, rules, the walker, the
  planner, the guard stack. It is compiled with no filesystem write calls, no
  `unsafe`, and no `std::process::Command`. `tests/invariants.rs` scans the
  source and fails if any appear.
- **`crates/reap-platform`** is everything that talks to the operating system:
  process inspection, volume statistics, the git-ignore oracle. It is the only
  crate containing `unsafe`, in one module, for the macOS FFI.
- **`crates/reap-cli`** is the only crate that mutates the filesystem.
- **`tests/`** at the repository root holds the core and property tests.
  **`crates/reap-cli/tests/cli.rs`** holds the end-to-end tests that run the real
  binary.

If you find yourself wanting to write to disk from `reap-core`, that is the
design telling you the logic belongs somewhere else.

## The rules for changes

### Every invariant maps to a named test

The nine invariants are listed in [docs/safety-model.md](docs/safety-model.md).
Each has at least one test whose name references it, with a comment saying which
invariant it covers. Search for `invariant_` to find them.

If you change behaviour covered by an invariant, the test must still pass or the
invariant must change, and changing an invariant is a discussion, not a commit.

### Any change to a guard or the executor needs a test demonstrating the failure it prevents

Not a test that the new code runs. A test that fails without your change and
passes with it, written from the direction of "here is the thing that would have
gone wrong".

Concretely: if you add a guard, write the escape it blocks into
`tests/escapes.rs` first and watch it succeed, then add the guard and watch it
fail. If you change the executor, add the interruption or the symlink or the
permission error to `crates/reap-cli/src/executor.rs`'s test module.

### Planner tests assert exact sets

A test that checks "the path I expected is in the output" passes just as happily
when the planner also proposed the user's source tree. `tests/planner.rs`
compares the full candidate set with `assert_eq!`. Keep it that way.

### Guards fail towards refusing

Every guard is written so that an error, an ambiguity or a missing tool is a
rejection. `git` not installed means refuse. Process inspection failing means
refuse. A heartbeat that cannot be parsed means refuse. If you write a guard
that returns "pass" on an error path, that is a bug even if nothing catches it
yet.

### New dependencies need a reason

The dependency list is deliberately short, and every addition is something a
reader has to trust. If you need one, say why in the pull request.

## Adding a rule to the catalogue

`crates/reap-cli/src/catalog.rs` is what `reap init --detect` emits. To add a
stack:

1. Add the `Stack` variant and its detection (a program on `PATH`, or a
   directory in `$HOME`).
2. Add the `Entry` blocks, with an honest one-line comment about what each
   reclaims and a tier that reflects rebuild cost.
3. Add or extend the matching file in `examples/`, with expected sizes.
4. Add a row to [docs/recipes.md](docs/recipes.md).
5. Run `scripts/validate-examples.sh`.

Tier discipline: tier 0 is seconds to regenerate, tier 1 is minutes and
bandwidth, tier 2 slows every later build in every project. When in doubt, go
one tier higher.

## Documentation

The README is a deliverable, not an afterthought. If you change behaviour that
appears in it, update it in the same commit.

Every command shown in the README and in `docs/` must have been run as written
and produce the output shown. Real output, captured from a real run. If you
cannot run it, do not put it in.

House style, for consistency with what is there: second person, plain sentences,
no marketing language, no emoji, no em dashes.

## Commits and pull requests

- Small, reviewable commits. One idea each.
- The subject line says what changed, in the imperative. The body says why, and
  what you considered and rejected if that is not obvious.
- If a commit changes a guard, the message should say what it now refuses that
  it did not before, or the reverse.
- Pull requests should say how you tested it beyond the test suite, and on which
  platforms.

## Security

Do not open a public issue for a vulnerability. See [SECURITY.md](SECURITY.md).
The highest-severity class for this project is any path-handling bug that allows
deletion outside a declared root.
