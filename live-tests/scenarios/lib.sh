#!/bin/sh
# Shared plumbing for live scenarios. Source this, then use:
#
#   new_sandbox <name>      fresh $SANDBOX with an empty .ssh/; exports the
#                           env that makes ssh-files fully hermetic
#   sf ...                  run ssh-files (release build) inside the sandbox
#   known_hosts             path to the sandbox known_hosts
#   assert_contains f s     / assert_not_contains f s
#   pass / fail msg         scenario verdict with consistent output
#
# Hermeticity: HOME is redirected (known_hosts, default keys, app config
# and ssh config lookups all resolve under it), the agent is disconnected,
# and both config env overrides point into the sandbox.

LIVE="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$LIVE/../target/release/ssh-files"
SANDBOX=""

new_sandbox() {
    SANDBOX="$LIVE/runs/$1"
    rm -rf "$SANDBOX"
    mkdir -p "$SANDBOX/home/.ssh"
    chmod 700 "$SANDBOX/home/.ssh"
    : > "$SANDBOX/home/.ssh/known_hosts"

    : > "$SANDBOX/config.toml"    # empty = defaults; an explicit-but-missing path warns

    export HOME="$SANDBOX/home"
    export SSH_FILES_CONFIG="$SANDBOX/config.toml"       # app config (empty = defaults)
    export SSH_FILES_SSH_CONFIG="$SANDBOX/ssh_config"    # ssh config: absent = none
    unset SSH_AUTH_SOCK || true                          # no agent: deterministic ladder
}

sf() {
    "$BIN" "$@"
}

known_hosts() {
    echo "$SANDBOX/home/.ssh/known_hosts"
}

assert_contains() {
    if ! grep -q "$2" "$1" 2>/dev/null; then
        fail "expected '$2' in $1"
    fi
}

assert_not_contains() {
    if grep -q "$2" "$1" 2>/dev/null; then
        fail "did not expect '$2' in $1"
    fi
}

pass() {
    echo "PASS: $SCENARIO"
}

fail() {
    echo "FAIL: $SCENARIO - $*" >&2
    exit 1
}
