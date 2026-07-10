#!/bin/sh
# TOFU on first contact: prompt appears, accepting records the key, and a
# second connection is silent (no re-prompt). The foundation scenario -
# it also proves the HOME sandbox actually captures known_hosts.
set -eu
SCENARIO="s00_tofu_and_reconnect"
. "$(dirname "$0")/lib.sh"
new_sandbox "$SCENARIO"

export SF_BIN="$BIN" SF_KEY="$LIVE/keys/id_ed25519"

expect <<'EOF'
set timeout 20
spawn $env(SF_BIN) -i $env(SF_KEY) test@localhost:2203
expect {
    -re {continue connecting \(yes/no\)} { send "yes\r" }
    timeout { puts "\nFAIL: no TOFU prompt"; exit 1 }
}
expect {
    "Connected!" {}
    timeout { puts "\nFAIL: never connected after TOFU accept"; exit 1 }
}
sleep 1
send "q"
expect eof
EOF

[ -s "$(known_hosts)" ] || fail "known_hosts is empty after TOFU accept"
assert_contains "$(known_hosts)" "localhost"
grep "localhost" "$(known_hosts)" | head -1

# Second connect: must NOT prompt (silent reconnect against recorded key).
expect <<'EOF'
set timeout 20
spawn $env(SF_BIN) -i $env(SF_KEY) test@localhost:2203
expect {
    -re {continue connecting} { puts "\nFAIL: TOFU re-prompted on reconnect"; exit 1 }
    "Connected!" {}
    timeout { puts "\nFAIL: reconnect did not complete"; exit 1 }
}
sleep 1
send "q"
expect eof
EOF

pass
