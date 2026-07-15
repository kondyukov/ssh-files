#!/bin/sh
# A host key marked @revoked in known_hosts must be refused hard - no
# TOFU prompt (which is what a checker that skips marker lines degrades
# to), no connection, nonzero exit. The revoked entry is the server's
# *real* key, harvested with ssh-keyscan, so only the marker logic - not
# a key mismatch - can cause the refusal.
set -eu
SCENARIO="s11_revoked_key_refused"
. "$(dirname "$0")/lib.sh"
new_sandbox "$SCENARIO"

ssh-keyscan -p 2203 -t ed25519 localhost 2>/dev/null \
    | sed 's/^/@revoked /' > "$(known_hosts)"
grep -q '^@revoked \[localhost\]:2203 ssh-ed25519 ' "$(known_hosts)" \
    || fail "could not harvest server key"

export SF_BIN="$BIN" SF_KEY="$LIVE/keys/id_ed25519"

expect <<'EOF'
set timeout 20
spawn $env(SF_BIN) -i $env(SF_KEY) test@localhost:2203
expect {
    "REVOKED HOST KEY" {}
    -re {continue connecting} { puts "\nFAIL: revoked key fell back to TOFU"; exit 1 }
    "Connected!" { puts "\nFAIL: connected despite revoked key"; exit 1 }
    timeout { puts "\nFAIL: no revocation warning"; exit 1 }
}
expect {
    "Host key verification failed" {}
    timeout { puts "\nFAIL: no hard failure after warning"; exit 1 }
}
expect eof
catch wait result
if {[lindex $result 3] == 0} { puts "\nFAIL: exit code 0 on revoked key"; exit 1 }
EOF

pass
