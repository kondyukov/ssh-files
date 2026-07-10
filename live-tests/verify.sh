#!/bin/sh
# Verify a destination tree against (a subset of) the fixture manifest.
#
#   verify.sh <dir>                    # full manifest must match under <dir>
#   verify.sh <dir> <prefix>           # only manifest entries under ./<prefix>
#   verify.sh <dir> <prefix> --flat    # entries under <prefix>, but expected
#                                      # at <dir> root under their basename
#
# Exits nonzero and prints a diff on any mismatch. Assertions about files
# that must be ABSENT (hidden files, partials) live in the scenarios.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
MANIFEST="$HERE/fixtures/manifest.sha256"

DIR="${1:?usage: verify.sh <dir> [prefix] [--flat]}"
PREFIX="${2:-}"
FLAT="${3:-}"

fail=0
count=0
while IFS= read -r line; do
    sum="${line%% *}"
    path="${line#* }"; path="${path# }"          # "./docs/report.txt"
    rel="${path#./}"

    case "$PREFIX" in
        "") ;;
        *) case "$rel" in "$PREFIX"/*|"$PREFIX") ;; *) continue ;; esac ;;
    esac

    if [ "$FLAT" = "--flat" ]; then
        target="$DIR/$(basename "$rel")"
    else
        target="$DIR/$rel"
    fi

    count=$((count + 1))
    if [ ! -f "$target" ]; then
        echo "MISSING: $target"
        fail=1
        continue
    fi
    actual="$(shasum -a 256 "$target" | cut -d' ' -f1)"
    if [ "$actual" != "$sum" ]; then
        echo "MISMATCH: $target"
        echo "  expected $sum"
        echo "  actual   $actual"
        fail=1
    fi
done < "$MANIFEST"

if [ "$count" -eq 0 ]; then
    echo "verify.sh: no manifest entries matched prefix '$PREFIX'" >&2
    exit 2
fi

if [ "$fail" -eq 0 ]; then
    echo "OK: $count file(s) verified against manifest"
fi
exit "$fail"
