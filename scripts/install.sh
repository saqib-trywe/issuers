#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
#
# Builds and installs everything this repository ships, to ~/.local/bin.
#
# There are two installs rather than one, and that is the whole reason this
# script exists. `cargo install --path .` installs the root package's binaries
# — `Issuers` and `issuers-cli` — and stops there, because `issuers-mcp` is a
# *workspace member* rather than a third `[[bin]]` (docs/adr/0009). Installing
# the app and wondering why an agent cannot reach the tracker is the failure
# this prevents: the MCP client looks for `issuers-mcp` on PATH by name and
# reports only that it could not spawn it.

set -euo pipefail

# Run from the repository root whatever directory this was invoked from, so
# `--path .` means what it says.
cd "$(dirname "${BASH_SOURCE[0]}")/.."

say() { printf '\n\033[1m%s\033[0m\n' "$*"; }

# `cargo install --root R` writes binaries to `R/bin`, so this is ~/.local/bin
# — the user binary directory of the XDG spec, rather than Cargo's own. Cargo
# also keeps its record of what it installed in `R/.crates.toml` and
# `R/.crates2.json`, so uninstalling needs the same root:
# `cargo uninstall --root ~/.local issuers`.
root="$HOME/.local"
bin_dir="$root/bin"

# `--locked` because without it a fresh resolve can install against different
# dependency versions from the ones the tests just passed on — `gpui-component`
# in particular moves its API between minor releases — which is a difference
# you find out about at runtime.
#
# `--force` so that re-running this replaces the binaries rather than declining
# as already-installed. Iterating is the normal case.
install_from() {
    cargo install --path "$1" --root "$root" --locked --force
}

say "Installing Issuers and issuers-cli (the app and the CLI)"
install_from .

say "Installing issuers-mcp (separate: it is a workspace member, not a third binary)"
install_from crates/issuers-mcp

# Report what actually landed, by resolving each name the way the things that
# call them do. A successful `cargo install` followed by a binary that is not
# on PATH is the other half of the same confusion — and so is one that *is*,
# but resolves to an older copy earlier on PATH, such as one a previous version
# of this script left in ~/.cargo/bin.
say "Installed"
missing=0
shadowed=0
for binary in Issuers issuers-cli issuers-mcp; do
    if path="$(command -v "$binary" 2>/dev/null)"; then
        if [ "$path" = "$bin_dir/$binary" ]; then
            printf '  %-12s %s\n' "$binary" "$path"
        else
            printf '  %-12s %s  (SHADOWS %s)\n' "$binary" "$path" "$bin_dir/$binary"
            shadowed=1
        fi
    else
        printf '  %-12s NOT ON PATH\n' "$binary"
        missing=1
    fi
done

if [ "$missing" -ne 0 ]; then
    cat >&2 <<'HINT'

Something installed but is not resolvable by name. This script installs to
~/.local/bin; add it to PATH:

    export PATH="$HOME/.local/bin:$PATH"

HINT
    exit 1
fi

if [ "$shadowed" -ne 0 ]; then
    cat >&2 <<'HINT'

Something resolves to a copy other than the one just installed, so running it
by name runs an old build. If the old one came from an earlier install to
~/.cargo/bin, remove it:

    cargo uninstall issuers
    cargo uninstall issuers-mcp

or put ~/.local/bin ahead of it on PATH.

HINT
    exit 1
fi

cat <<'NEXT'

  Issuers      the app. Its window is the tracker; it also hosts the local API.
  issuers-cli  the CLI. A client of that API, so it needs the app running and
               exits 3 when it is not.
  issuers-mcp  the MCP server. Also a client of the API. It speaks JSON-RPC on
               stdin/stdout, so running it in a terminal looks like a hang —
               that is it waiting for a client. Configured in .mcp.json by
               bare name, which is why it has to be on PATH rather than
               anywhere.

Restart any MCP client that was already running: it resolved issuers-mcp at
startup and will not look again.
NEXT
