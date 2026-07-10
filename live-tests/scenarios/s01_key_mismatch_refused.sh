#!/bin/sh
# A recorded host key that does not match the server's must be refused
# hard (possible MITM) - no prompt, no connection. Seeding the sandbox
# known_hosts with a fake key simulates a rotated/compromised server
# without touching the containers.
set -eu
SCENARIO="s01_key_mismatch_refused"
. "$(dirname "$0")/lib.sh"
new_sandbox "$SCENARIO"

ssh-keygen -t ed25519 -f "$SANDBOX/fake" -N '' -q
printf '[localhost]:2203 %s\n' "$(cut -d' ' -f1-2 "$SANDBOX/fake.pub")" \
    > "$(known_hosts)"

export SF_BIN="$BIN" SF_KEY="$LIVE/keys/id_ed25519"

expect <<'EOF'
set timeout 20
spawn $env(SF_BIN) -i $env(SF_KEY) test@localhost:2203
expect {
    "REMOTE HOST IDENTIFICATION HAS CHANGED" {}
    -re {continue connecting} { puts "\nFAIL: mismatched key fell back to TOFU"; exit 1 }
    "Connected!" { puts "\nFAIL: connected despite key mismatch"; exit 1 }
    timeout { puts "\nFAIL: no mismatch warning"; exit 1 }
}
expect {
    "Host key verification failed" {}
    timeout { puts "\nFAIL: no hard failure after warning"; exit 1 }
}
expect eof
catch wait result
if {[lindex $result 3] == 0} { puts "\nFAIL: exit code 0 on mismatch"; exit 1 }
EOF

pass
