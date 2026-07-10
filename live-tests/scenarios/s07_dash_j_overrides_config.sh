#!/bin/sh
# -J on the command line replaces a config ProxyJump chain (OpenSSH
# semantics). The config points at bastion (localhost:2201); -J forces
# gateway (127.0.0.1:2202) - the connect line must show the -J hop only.
set -eu
SCENARIO="s07_dash_j_overrides_config"
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

export SF_BIN="$BIN" SF_KEY="$LIVE/keys/id_ed25519"

expect <<'EOF'
set timeout 30
spawn $env(SF_BIN) -i $env(SF_KEY) -J test@127.0.0.1:2202 innerb
expect {
    "Connecting to via 127.0.0.1 -> test@inner-b:22" {}
    "via localhost" { puts "\nFAIL: config jump not overridden by -J"; exit 1 }
    timeout { puts "\nFAIL: no connect line"; exit 1 }
}
expect {
    -re {continue connecting \(yes/no\)} { send "yes\r"; exp_continue }
    "Connected!" {}
    timeout { puts "\nFAIL: never connected through -J hop"; exit 1 }
}
sleep 1
send "q"
expect eof
EOF

pass
