#!/bin/sh
# w2.5 + w2.5b, automated: round-trip the full hostile-filename gauntlet
# (naughty/ quoting cases + hostile/ names the manifest can't express)
# through the real TUI, driven headlessly by vhs, against plain-a.
#
# Two sessions per byte path: upload work/{naughty,hostile} to
# /data/incoming, then download into a fresh back/ dir - no overwrite
# prompts, fully deterministic. Bytes are verified with diff -r against
# the canonical fixtures (readdir-based, so the names don't faze it),
# and both destinations are checked for .part residue.
#
#   scenarios/w25_roundtrip.sh          # streaming path, then --sftp-only
#
# Requires: containers up, release binary, vhs (brew install vhs).
set -eu
. "$(dirname "$0")/lib.sh"
cd "$LIVE"

# new_sandbox redirects HOME, but the docker CLI finds its compose plugin
# via ~/.docker/cli-plugins - run docker with the real HOME.
REAL_HOME="$HOME"
dexec() {
    HOME="$REAL_HOME" docker compose exec -T plain-a sh -c "$1"
}

command -v vhs >/dev/null 2>&1 || { echo "SKIP: vhs not installed"; exit 0; }
[ -x "$BIN" ] || { echo "SKIP: release binary missing"; exit 0; }
docker compose ps --status running 2>/dev/null | grep -q plain-a \
    || { echo "SKIP: containers not running"; exit 0; }

run_variant() {
    VARIANT="$1"    # "" or "--sftp-only"
    NAME="w25_roundtrip${VARIANT:+_sftp}"
    SCENARIO="w2.5 roundtrip ${VARIANT:-streaming}"
    new_sandbox "$NAME"

    mkdir -p "$SANDBOX/work" "$SANDBOX/back"
    cp -R fixtures/tree/docs/naughty "$SANDBOX/work/naughty"
    cp -R fixtures/hostile "$SANDBOX/work/hostile"

    # TOFU once, headlessly, so the TUI opens straight into the panes.
    printf 'yes\n' | sf -i keys/id_ed25519 --bench=1 \
        test@localhost:2203:/data/incoming >/dev/null 2>&1 \
        || fail "TOFU seeding failed"

    dexec 'rm -rf /data/incoming/* /data/incoming/.[!.]* /data/incoming/..?*' \
        2>/dev/null || true

    cat > "$SANDBOX/env.sh" <<EOF
export HOME="$SANDBOX/home"
export SSH_FILES_CONFIG="$SANDBOX/config.toml"
export SSH_FILES_SSH_CONFIG="$SANDBOX/ssh_config"
export SSH_AUTH_SOCK=
EOF

    LAUNCH="$BIN $VARIANT -i $LIVE/keys/id_ed25519 test@localhost:2203:/data/incoming"
    # vhs's parser rejects absolute paths in Output; keep it relative to
    # $LIVE (where this script runs vhs from).
    cat > "$SANDBOX/roundtrip.tape" <<EOF
Output runs/$NAME/session.gif
Set Width 1000
Set Height 620
Set TypingSpeed 10ms

# Session 1: upload the gauntlet (select all, send flat; both dirs are
# top-level selections, so flat == tree here). Startup focus is the
# REMOTE pane - focus left before selecting.
Hide
Type ". $SANDBOX/env.sh && cd $SANDBOX/work && clear"
Enter
Sleep 1s
Show
Type "$LAUNCH"
Enter
Sleep 7s
Left
Sleep 1s
Type "a"
Sleep 1s
Type "u"
Sleep 16s
Type "q"
Sleep 2s

# Session 2: download it back into an empty dir (remote pane is focused
# at startup; no name collisions, so no prompts).
Hide
Type "cd $SANDBOX/back && clear"
Enter
Sleep 1s
Show
Type "$LAUNCH"
Enter
Sleep 7s
Type "a"
Sleep 1s
Type "d"
Sleep 16s
Type "q"
Sleep 2s
EOF

    vhs "$SANDBOX/roundtrip.tape" >/dev/null 2>&1 || fail "vhs run failed"

    # Byte-for-byte round trip, verified against the canonical fixtures.
    diff -r fixtures/tree/docs/naughty "$SANDBOX/back/naughty" \
        || fail "naughty/ round trip differs"
    diff -r fixtures/hostile "$SANDBOX/back/hostile" \
        || fail "hostile/ round trip differs"

    # Nothing half-arrived anywhere: no .part residue, exact file count.
    # Count with one printed byte per file, not `| wc -l` - the embedded
    # newline in the gauntlet makes line counts lie.
    COUNT=$(dexec 'find /data/incoming -type f -exec printf x \; | wc -c' | tr -d ' \r')
    [ "$COUNT" = "13" ] || fail "expected 13 files on server, got $COUNT"
    PARTS=$(dexec 'find /data/incoming -name "*.part" -exec printf x \; | wc -c' | tr -d ' \r')
    [ "$PARTS" = "0" ] || fail "$PARTS .part files left on server"
    find "$SANDBOX/back" -name '*.part' | grep -q . \
        && fail ".part residue in download dir"

    pass
}

run_variant ""
run_variant "--sftp-only"
