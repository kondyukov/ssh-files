#!/bin/sh
# One-time rig setup: client keys, fixtures, image build, containers up.
# Re-runnable; regenerates fixtures but keeps existing client keys.
set -eu
cd "$(dirname "$0")"

# --- client keys -----------------------------------------------------------
mkdir -p keys
if [ ! -f keys/id_ed25519 ]; then
    ssh-keygen -t ed25519 -f keys/id_ed25519 -N '' -q -C ssh-files-live
    echo "generated keys/id_ed25519 (no passphrase)"
fi
if [ ! -f keys/id_enc ]; then
    ssh-keygen -t ed25519 -f keys/id_enc -N 'livetest' -q -C ssh-files-live-enc
    echo "generated keys/id_enc (passphrase: livetest)"
fi
cat keys/id_ed25519.pub keys/id_enc.pub > keys/authorized_keys

# --- fixtures ---------------------------------------------------------------
./mkfixtures.sh

# --- containers -------------------------------------------------------------
docker compose build
docker compose up -d
docker compose ps

cat <<'EOF'

Rig is up. Ports: bastion 2201, gateway 2202, plain-a 2203, sftponly 2204,
pwonly 2205; inner-b is internal-only (via bastion/gateway).
User: test. Password (pwonly): pw-secret-1. Keys: keys/id_ed25519,
keys/id_enc (passphrase "livetest").

Latency injection: NETEM_DELAY=40ms docker compose up -d --force-recreate

Run scenarios:   scenarios/run_all_wave1.sh
Tear down:       docker compose down          (add -v to also rotate host keys)
EOF
