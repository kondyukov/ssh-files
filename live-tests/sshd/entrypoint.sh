#!/bin/sh
# Boot one sshd role container: persistent host key, authorized_keys copy,
# optional latency injection, then sshd in the foreground.
set -eu

# Host key lives in the /keys volume so it survives restarts (silent
# reconnects must not re-trigger TOFU). Wipe the volume to simulate a
# rotated/compromised host key.
mkdir -p /keys
[ -f /keys/ssh_host_ed25519_key ] || ssh-keygen -t ed25519 -f /keys/ssh_host_ed25519_key -N '' -q

# authorized_keys is mounted root-owned; copy it into place with the
# ownership and modes sshd's StrictModes expects.
if [ -f /auth/authorized_keys ]; then
    mkdir -p /home/test/.ssh
    cp /auth/authorized_keys /home/test/.ssh/authorized_keys
    chown -R test:test /home/test/.ssh
    chmod 700 /home/test/.ssh
    chmod 600 /home/test/.ssh/authorized_keys
fi

# Writable upload target (tmpfs, mode set in compose; chown for tidiness).
mkdir -p /data/incoming
chown test:test /data/incoming 2>/dev/null || true

# Optional latency, e.g. NETEM_DELAY=40ms. Failure is non-fatal: the rig
# still works, just without realistic RTT.
if [ -n "${NETEM_DELAY:-}" ]; then
    tc qdisc add dev eth0 root netem delay "$NETEM_DELAY" 2>/dev/null \
        || echo "netem: could not inject delay (missing NET_ADMIN?)" >&2
fi

exec /usr/sbin/sshd -D -e -f "/etc/ssh/roles/${ROLE:-plain}.conf"
