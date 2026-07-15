#!/bin/sh
# Run every fully-automated wave-1 scenario in order; stop on first failure.
# Prerequisites: ../setup.sh has been run, containers are up, and the
# release binary is current (cargo build --release).
set -eu
cd "$(dirname "$0")"

for s in s[0-9][0-9]_*.sh; do
    echo "=== $s ==="
    ./"$s"
    echo
done

echo "Wave 1: all scenarios passed."
