#!/bin/sh
# Record the welcome GIFs with vhs (https://github.com/charmbracelet/vhs)
# against the live-test rig.
#
#   ./record.sh [tree|flat]        # default: both
#
# Prereqs, from live-tests/:
#   ./mkfixtures.sh                              # demo fixtures included
#   cargo build --release                        # from repo root
#   NETEM_DELAY=40ms docker compose up -d --force-recreate
#
# The latency matters: without it transfers finish in a blink and the GIF
# shows no progress. 40ms also makes [streaming] earn its keep on camera.
#
# Each take gets a fresh hermetic sandbox (runs/demo_<name>/) with an
# ssh_config alias so the on-screen command is just
# `ssh-files media-server:...`. HostName is media-server.localhost -
# *.localhost resolves to loopback - so the pane title shows a clean
# hostname instead of localhost:2203. TOFU is accepted headlessly before
# recording (a tiny --bench run) so the tape opens straight into the TUI.
# Local-pane paths are staged under /tmp/demo so no real $HOME path
# appears in the published GIF.
set -eu
cd "$(dirname "$0")/.."
LIVE="$(pwd)"
BIN="$(cd .. && pwd)/target/release/ssh-files"
STAGE=/tmp/demo

command -v vhs >/dev/null 2>&1 || { echo "vhs not installed (brew install vhs)"; exit 1; }
[ -x "$BIN" ] || { echo "release binary missing: cargo build --release (repo root)"; exit 1; }
[ -d fixtures/demo/shoot ] || { echo "demo fixtures missing: ./mkfixtures.sh"; exit 1; }
docker compose ps --status running 2>/dev/null | grep -q plain-a \
    || { echo "containers not running: NETEM_DELAY=40ms docker compose up -d"; exit 1; }
if ! docker compose exec -T plain-a sh -c 'tc qdisc show dev eth0 | grep -q netem'; then
    echo "WARNING: no netem latency - transfers will be too fast to film."
    echo "         NETEM_DELAY=40ms docker compose up -d --force-recreate"
fi

# prep <name>: fresh sandbox + alias + headless TOFU accept (bench against
# the writable incoming dir; TOFU is per-host, not per-path), then write
# the env file the tape sources (Hide'd) before launching.
prep() {
    SANDBOX="$LIVE/runs/demo_$1"
    rm -rf "$SANDBOX"
    mkdir -p "$SANDBOX/home/.ssh"
    chmod 700 "$SANDBOX/home/.ssh"
    : > "$SANDBOX/home/.ssh/known_hosts"
    : > "$SANDBOX/config.toml"
    cat > "$SANDBOX/ssh_config" <<EOF
Host media-server
  HostName media-server.localhost
  Port 2203
  User test
  IdentityFile $LIVE/keys/id_ed25519
EOF
    printf 'yes\n' | HOME="$SANDBOX/home" SSH_FILES_CONFIG="$SANDBOX/config.toml" \
        SSH_FILES_SSH_CONFIG="$SANDBOX/ssh_config" SSH_AUTH_SOCK= \
        "$BIN" --bench=1 media-server:/data/incoming >/dev/null 2>&1 \
        || { echo "TOFU seeding failed - is the rig healthy?"; exit 1; }
    # Lives OUTSIDE the staged pane root so it never appears in the GIF.
    cat > "$STAGE-env.sh" <<EOF
export HOME="$SANDBOX/home"
export SSH_FILES_CONFIG="$SANDBOX/config.toml"
export SSH_FILES_SSH_CONFIG="$SANDBOX/ssh_config"
export SSH_AUTH_SOCK=
export PATH="$(dirname "$BIN"):\$PATH"
EOF
}

reset_incoming() {
    docker compose exec -T plain-a sh -c 'rm -rf /data/incoming/* /data/incoming/..?* /data/incoming/.[!.]*' 2>/dev/null || true
}

record_tree() {
    rm -rf "$STAGE"; mkdir -p "$STAGE"
    cp -R fixtures/demo/project "$STAGE/project"
    prep tree
    reset_incoming
    echo "recording demo/tree.gif ..."
    vhs demo/tree.tape
}

record_flat() {
    rm -rf "$STAGE"; mkdir -p "$STAGE/downloads"
    prep flat
    reset_incoming
    echo "recording demo/flat.gif ..."
    vhs demo/flat.tape
}

case "${1:-both}" in
    tree) record_tree ;;
    flat) record_flat ;;
    both) record_tree; record_flat ;;
    *) echo "usage: record.sh [tree|flat]"; exit 1 ;;
esac
ls -lh demo/*.gif
