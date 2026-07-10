#!/bin/sh
# ssh_config ProxyJump: the target alias jumps through a bastion alias
# (recursive config resolution), reaching inner-b which has NO direct
# route from this machine - success proves a real tunnel. Both hops get
# their own TOFU under their own names.
set -eu
SCENARIO="s06_config_proxyjump"
. "$(dirname "$0")/lib.sh"
new_sandbox "$SCENARIO"

cat > "$SSH_FILES_SSH_CONFIG" <<EOF
Host jump
  HostName localhost
  Port 2201
  User test
  IdentityFile $LIVE/keys/id_ed25519

Host innerb
  HostName inner-b
  User test
  IdentityFile $LIVE/keys/id_ed25519
  ProxyJump jump
EOF

export SF_BIN="$BIN"

expect <<'EOF'
set timeout 30
spawn $env(SF_BIN) innerb
expect {
    "Connecting to via localhost -> test@inner-b:22" {}
    timeout { puts "\nFAIL: config ProxyJump not reflected in connect line"; exit 1 }
}
expect {
    -re {continue connecting \(yes/no\)} { send "yes\r"; exp_continue }
    "Connected!" {}
    timeout { puts "\nFAIL: never connected through config jump"; exit 1 }
}
sleep 1
send "q"
expect eof
EOF

# Both the bastion and the tunneled target must be recorded, each under
# its own name.
assert_contains "$(known_hosts)" "localhost"
assert_contains "$(known_hosts)" "inner-b"

pass
