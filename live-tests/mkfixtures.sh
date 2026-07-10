#!/bin/sh
# Generate the canonical fixture tree and its checksum manifest.
# The tree is mounted read-only into every server at /data/fixtures.
#
#   BIG_MIB=32 ./mkfixtures.sh    # bigger big.bin for cancel-mid-transfer
set -eu
cd "$(dirname "$0")"

BIG_MIB="${BIG_MIB:-8}"
TREE=fixtures/tree

rm -rf fixtures
mkdir -p "$TREE/docs/sub/empty_dir" "$TREE/docs/naughty"

printf 'report body\n' > "$TREE/docs/report.txt"
printf 'nested\n' > "$TREE/docs/sub/nested.txt"
printf 'secret\n' > "$TREE/docs/.hidden.txt"
dd if=/dev/urandom of="$TREE/docs/big.bin" bs=1048576 count="$BIG_MIB" 2>/dev/null

# The quoting gauntlet: every one of these must survive the exec streaming
# path (cat > <shell_quote(path)>) byte-identically. A quoting bug here is
# a correctness AND injection problem.
N="$TREE/docs/naughty"
printf 'space\n'     > "$N/with space.txt"
printf 'squote\n'    > "$N/it's.txt"
printf 'dquote\n'    > "$N/quote\"d.txt"
printf 'subshell\n'  > "$N/\$(danger).txt"
printf 'dash\n'      > "$N/-dash.txt"
printf 'unicode\n'   > "$N/héllo📁.txt"
printf 'semicolon\n' > "$N/semi;colon.txt"
printf 'backtick\n'  > "$N/back\`tick.txt"
printf 'glob\n'      > "$N/star*.txt"

# Collision fixture: two files with the same basename in different
# directories. Selecting both and sending flat maps them onto the same
# destination path - the collision dialog must fire and nothing may
# transfer. Lives OUTSIDE tree/ (and the manifest): it exists to be
# refused, not transferred, and never needs to reach a server.
mkdir -p fixtures/collision/a fixtures/collision/b
printf 'collision a\n' > fixtures/collision/a/x.txt
printf 'collision b\n' > fixtures/collision/b/x.txt

# World-readable so the container's unprivileged user can serve them.
chmod -R a+rX "$TREE" fixtures/collision

# Manifest: relative path + sha256 for every file, sorted. Hidden files
# are included — they are shown and transferred by default; scenarios that
# toggle them off (`.`) assert their absence separately.
(cd "$TREE" && find . -type f | LC_ALL=C sort | while IFS= read -r f; do
    shasum -a 256 "$f"
done) > fixtures/manifest.sha256

echo "fixtures: $(find "$TREE" -type f | wc -l | tr -d ' ') files, big.bin ${BIG_MIB} MiB"
echo "manifest: fixtures/manifest.sha256"
echo "collision pair: fixtures/collision/{a,b}/x.txt (not in manifest)"

# Regenerating replaces the directory the containers bind-mounted; running
# containers keep the old (deleted) inode and serve an empty /data/fixtures.
if docker compose ps --status running 2>/dev/null | grep -q plain-a; then
    echo "WARNING: containers are running with a now-stale fixtures mount."
    echo "         Run: docker compose up -d --force-recreate"
fi
