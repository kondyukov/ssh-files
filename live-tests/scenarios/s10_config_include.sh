#!/bin/sh
# ssh_config Include + Key=value: the alias's real settings live in an
# included config.d file (relative pattern, resolved against ~/.ssh, as
# ssh does), written in Key=value form. A Match block sits in the middle
# to prove it is dropped whole - its body must not leak into the alias.
# An unsupported-but-consequential directive must produce a warning.
set -eu
SCENARIO="s10_config_include"
. "$(dirname "$0")/lib.sh"
new_sandbox "$SCENARIO"

# The main config lives at ~/.ssh/config inside the sandbox HOME so the
# relative Include pattern exercises ssh's resolution rule.
mkdir -p "$SANDBOX/home/.ssh/config.d"
export SSH_FILES_SSH_CONFIG="$SANDBOX/home/.ssh/config"

cat > "$SANDBOX/home/.ssh/config" <<EOF
Include config.d/*.conf
Match host *.prod
  User wrong-user
EOF

cat > "$SANDBOX/home/.ssh/config.d/boxes.conf" <<EOF
Host incbox
  HostName=localhost
  Port = 2203
  User=test
  IdentityFile $LIVE/keys/id_ed25519
  ProxyCommand /usr/bin/false
EOF

export SF_BIN="$BIN"

expect <<'EOF'
set timeout 20
spawn $env(SF_BIN) incbox
expect {
    -re {Warning: ssh config: 'proxycommand'} { exp_continue }
    "Connecting to test@localhost:2203" {}
    -re {Username for} { puts "\nFAIL: included User not applied (prompted)"; exit 1 }
    timeout { puts "\nFAIL: included alias did not resolve"; exit 1 }
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
