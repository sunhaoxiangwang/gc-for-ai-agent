#!/bin/sh
#
# Install reap.
#
# Detects your platform, downloads the matching release asset, verifies its
# checksum against the published SHA256SUMS, and installs the binary. It then
# prints the next command to run.
#
# It does NOT install a scheduler, enable anything, or write a config. Those are
# separate, deliberate steps you take yourself.
#
# Usage:
#     curl -fsSL https://raw.githubusercontent.com/sunhaoxiangwang/gc-for-ai-agent/main/dist/install.sh | sh
#
# Environment:
#     REAP_VERSION   version to install (default: latest release)
#     REAP_BIN_DIR   where to install  (default: /usr/local/bin)
#     REAP_REPO      owner/name        (default: sunhaoxiangwang/gc-for-ai-agent)

set -eu

REPO="${REAP_REPO:-sunhaoxiangwang/gc-for-ai-agent}"
BIN_DIR="${REAP_BIN_DIR:-/usr/local/bin}"

say() { printf '%s\n' "$*"; }
err() { printf 'error: %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || err "this script needs $1, which is not on your PATH"
}

need uname
need mktemp
need tar

# curl or wget, whichever is present.
if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1" -o "$2"; }
    fetch_stdout() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO "$2" "$1"; }
    fetch_stdout() { wget -qO- "$1"; }
else
    err "this script needs curl or wget"
fi

# --- platform ---------------------------------------------------------------

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
    Darwin) os_part="apple-darwin" ;;
    Linux)  os_part="unknown-linux-musl" ;;
    *)      err "reap supports macOS and Linux only; this is $os. Windows is not supported." ;;
esac

case "$arch" in
    arm64|aarch64) arch_part="aarch64" ;;
    x86_64|amd64)  arch_part="x86_64" ;;
    *)             err "unsupported architecture: $arch" ;;
esac

target="${arch_part}-${os_part}"

# --- version ----------------------------------------------------------------

version="${REAP_VERSION:-}"
if [ -z "$version" ]; then
    say "Looking up the latest release..."
    # Read the tag from the redirect target of /releases/latest rather than the
    # API, so this works without a token and without jq.
    version="$(fetch_stdout "https://api.github.com/repos/${REPO}/releases/latest" \
        | sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p' | head -n 1)"
    [ -n "$version" ] || err "could not determine the latest version; set REAP_VERSION"
fi

# Accept both "0.1.0" and "v0.1.0".
tag="$version"
case "$tag" in v*) bare="${tag#v}" ;; *) bare="$tag"; tag="v$tag" ;; esac

asset="reap-${bare}-${target}.tar.gz"
base="https://github.com/${REPO}/releases/download/${tag}"

# --- download and verify ----------------------------------------------------

tmp="$(mktemp -d)"
# shellcheck disable=SC2064
trap "rm -rf '$tmp'" EXIT INT TERM

say "Downloading ${asset} (${tag})..."
fetch "${base}/${asset}" "${tmp}/${asset}" \
    || err "could not download ${base}/${asset}
Check that ${tag} exists and has an asset for ${target}."

say "Verifying checksum..."
fetch "${base}/SHA256SUMS" "${tmp}/SHA256SUMS" \
    || err "could not download ${base}/SHA256SUMS; refusing to install unverified"

expected="$(grep " ${asset}\$" "${tmp}/SHA256SUMS" | awk '{print $1}' | head -n 1)"
[ -n "$expected" ] || err "SHA256SUMS has no entry for ${asset}; refusing to install"

if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "${tmp}/${asset}" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "${tmp}/${asset}" | awk '{print $1}')"
else
    err "this script needs sha256sum or shasum to verify the download"
fi

if [ "$expected" != "$actual" ]; then
    err "checksum mismatch for ${asset}
  expected ${expected}
  actual   ${actual}
Refusing to install. This is worth investigating rather than retrying."
fi

# --- install ----------------------------------------------------------------

tar -xzf "${tmp}/${asset}" -C "$tmp"
[ -f "${tmp}/reap" ] || err "the archive did not contain a reap binary"
chmod +x "${tmp}/reap"

install_to() {
    dest="$1"
    if [ -w "$dest" ] || { [ ! -e "$dest" ] && [ -w "$(dirname "$dest")" ]; }; then
        cp "${tmp}/reap" "$dest/reap" 2>/dev/null && return 0
    fi
    return 1
}

if install_to "$BIN_DIR"; then
    installed="${BIN_DIR}/reap"
elif command -v sudo >/dev/null 2>&1; then
    say "${BIN_DIR} is not writable; using sudo."
    sudo cp "${tmp}/reap" "${BIN_DIR}/reap"
    installed="${BIN_DIR}/reap"
else
    err "cannot write to ${BIN_DIR} and sudo is not available.
Set REAP_BIN_DIR to somewhere you can write, for example:
  REAP_BIN_DIR=\"\$HOME/.local/bin\" sh install.sh"
fi

say ""
say "Installed ${installed}"
"$installed" --version || true

case ":$PATH:" in
    *":${BIN_DIR}:"*) ;;
    *) say ""
       say "Note: ${BIN_DIR} is not on your PATH." ;;
esac

say ""
say "Next:"
say "  reap init --detect     write a config for this machine"
say "  reap doctor            check that everything resolves"
say "  reap report            see what could be reclaimed. Deletes nothing."
say ""
say "Nothing is deleted until you set dry_run = false in the config AND pass"
say "--apply. No scheduler has been installed."
