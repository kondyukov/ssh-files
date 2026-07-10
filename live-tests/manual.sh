#!/bin/sh
# Launch ssh-files inside a named hermetic sandbox for a human-driven
# scenario (waves 2-3). You drive the TUI; setup and verification are
# scripted around you.
#
#   ./manual.sh <run-name> [--keep] -- <ssh-files args...>
#
#   ./manual.sh w2_upload -- -i keys/id_ed25519 test@localhost:2203:/data/incoming
#
# --keep reuses the existing sandbox (e.g. to reconnect without re-TOFU).
set -eu
cd "$(dirname "$0")"
LIVE="$(pwd)"
BIN="$LIVE/../target/release/ssh-files"

NAME="${1:?usage: manual.sh <run-name> [--keep] -- <args...>}"
shift
KEEP=0
if [ "${1:-}" = "--keep" ]; then KEEP=1; shift; fi
[ "${1:-}" = "--" ] && shift

SANDBOX="$LIVE/runs/$NAME"
if [ "$KEEP" -eq 0 ]; then
    rm -rf "$SANDBOX"
fi
mkdir -p "$SANDBOX/home/.ssh"
chmod 700 "$SANDBOX/home/.ssh"
touch "$SANDBOX/home/.ssh/known_hosts"
touch "$SANDBOX/config.toml"    # empty = defaults; an explicit-but-missing path warns

HOME="$SANDBOX/home" \
SSH_FILES_CONFIG="$SANDBOX/config.toml" \
SSH_FILES_SSH_CONFIG="$SANDBOX/ssh_config" \
SSH_AUTH_SOCK= \
exec "$BIN" "$@"
