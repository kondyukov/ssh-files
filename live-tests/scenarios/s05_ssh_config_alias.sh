#!/bin/sh
# ssh_config resolution: a bare alias picks up HostName, User, Port, and
# IdentityFile from the sandbox ssh config - no prompts, straight connect.
set -eu
SCENARIO="s05_ssh_config_alias"
. "$(dirname "$0")/lib.sh"
new_sandbox "$SCENARIO"

cat > "$SSH_FILES_SSH_CONFIG" <<EOF
Host boxa
  HostName localhost
  Port 2203
  User test
  IdentityFile $LIVE/keys/id_ed25519
EOF

export SF_BIN="$BIN"

expect <<'EOF'
set timeout 20
spawn $env(SF_BIN) boxa
expect {
    "Connecting to test@localhost:2203" {}
    -re {Username for} { puts "\nFAIL: config User not applied (prompted)"; exit 1 }
    timeout { puts "\nFAIL: alias did not resolve"; exit 1 }
}
expect {
    -re {continue connecting \(yes/no\)} { send "yes\r"; exp_continue }
    "Connected!" {}
    timeout { puts "\nFAIL: alias resolved but never connected"; exit 1 }
}
sleep 1
send "q"
expect eof
EOF

pass
