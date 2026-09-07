#!/usr/bin/env bash
#
# Validates every file in examples/ by running `reap doctor` against it.
#
# A config that does not load, names a root that is not a directory, or points
# its quarantine somewhere unusable is a broken example, and a broken example
# is worse than no example. This runs in CI on every push.
#
# Some examples describe servers rather than laptops and declare absolute system
# paths such as /var/lib/reap. This script never uses sudo: an example whose
# directories it cannot create is reported as skipped, with the reason. CI
# creates those paths itself before calling this, so nothing is skipped there.
#
# Usage:
#     scripts/validate-examples.sh [path-to-reap-binary]

set -euo pipefail

REAP="${1:-target/debug/reap}"
if [ ! -x "$REAP" ]; then
    echo "no reap binary at $REAP; build it first with: cargo build" >&2
    exit 1
fi
# Resolve now, because HOME is redirected below and a relative path would still
# work but an unqualified one in $PATH might not.
REAP="$(cd "$(dirname "$REAP")" && pwd)/$(basename "$REAP")"
REPO_ROOT="$(pwd)"

FIXTURE="$(mktemp -d)"
trap 'rm -rf "$FIXTURE"' EXIT

# The examples declare roots under ~, so this script gives them a home
# directory of its own. It must never write into the real one: a validation
# script that leaves a ~/code/sample behind on a contributor's machine is a bug.
export HOME="$FIXTURE"

# A small tree that looks like real work, so the examples have something to
# match and doctor exercises more than "this root does not exist".
mkdir -p "$HOME/code/sample/target/debug" \
         "$HOME/code/sample/node_modules/dep" \
         "$HOME/code/sample/__pycache__"
: > "$HOME/code/sample/Cargo.toml"
: > "$HOME/code/sample/package.json"
: > "$HOME/code/sample/target/debug/artifact"
: > "$HOME/code/sample/node_modules/dep/index.js"
: > "$HOME/code/sample/__pycache__/module.pyc"
git -C "$HOME/code/sample" init -q 2>/dev/null || true
printf '/target/\n/node_modules/\n__pycache__/\n' > "$HOME/code/sample/.gitignore"

# Paths the server-shaped examples declare, created only where we already have
# permission. Never with sudo.
for dir in /var/lib/reap/quarantine \
           /var/run/reap/heartbeats \
           /var/lib/agent/workspaces \
           /var/lib/agent/tmp; do
    [ -d "$dir" ] || mkdir -p "$dir" 2>/dev/null || true
done

status=0
skipped=0
for config in "$REPO_ROOT"/examples/*.toml; do
    printf '%-34s ' "examples/$(basename "$config")"
    if output="$("$REAP" --no-color --config "$config" doctor 2>&1)"; then
        echo "ok"
        continue
    fi
    code=$?
    # An example that declares a system path this account cannot create is not
    # a broken example; it is an example for a machine this is not. CI creates
    # those paths first, so there it validates like any other.
    if echo "$output" | grep -q "could not create the quarantine directory.*Permission denied"; then
        echo "skipped (needs a privileged path that does not exist here)"
        skipped=$((skipped + 1))
        continue
    fi
    echo "FAILED (exit $code)"
    # shellcheck disable=SC2001  # ${var//x/y} cannot anchor a per-line prefix
    echo "$output" | sed 's/^/    /'
    status=1
done

# Every example must also survive `report`, which is what a reader runs first.
for config in "$REPO_ROOT"/examples/*.toml; do
    "$REAP" --no-color --config "$config" report --json > /dev/null || {
        # Exit 3 means nothing matched, which is a legitimate outcome here.
        code=$?
        if [ "$code" -ne 3 ]; then
            echo "$config: report exited $code" >&2
            status=1
        fi
    }
done

echo
if [ "$status" -ne 0 ]; then
    echo "Some examples do not validate."
elif [ "$skipped" -gt 0 ]; then
    echo "All examples validate ($skipped skipped for want of a privileged path)."
else
    echo "All examples validate."
fi
exit "$status"
