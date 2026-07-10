#!/bin/sh
# An encrypted -i key triggers the passphrase prompt mid-ladder and
# authenticates once the passphrase is supplied.
set -eu
SCENARIO="s02_encrypted_key_passphrase"
. "$(dirname "$0")/lib.sh"
new_sandbox "$SCENARIO"

export SF_BIN="$BIN" SF_KEY="$LIVE/keys/id_enc"

expect <<'EOF'
set timeout 20
spawn $env(SF_BIN) -i $env(SF_KEY) test@localhost:2203
expect {
    -re {continue connecting \(yes/no\)} { send "yes\r"; exp_continue }
    -re {Enter passphrase for key} { send "livetest\r" }
    timeout { puts "\nFAIL: no passphrase prompt"; exit 1 }
}
expect {
    "Connected!" {}
    timeout { puts "\nFAIL: passphrase did not authenticate"; exit 1 }
}
sleep 1
send "q"
expect eof
EOF

pass
