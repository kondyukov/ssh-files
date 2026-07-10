#!/bin/sh
# Two-hop chain: client -> bastion (localhost:2201) -> gateway (resolved by
# bastion's DNS, inside the docker network) -> inner-b. Every hop is
# authenticated and TOFU'd under its own name; the nested tunnel-in-tunnel
# handshake is the thing being proven.
set -eu
SCENARIO="s08_multihop_jump"
. "$(dirname "$0")/lib.sh"
new_sandbox "$SCENARIO"

export SF_BIN="$BIN" SF_KEY="$LIVE/keys/id_ed25519"

expect <<'EOF'
set timeout 40
spawn $env(SF_BIN) -i $env(SF_KEY) -J test@localhost:2201,test@gateway test@inner-b
expect {
    "Connecting to via localhost,gateway -> test@inner-b:22" {}
    timeout { puts "\nFAIL: no chained connect line"; exit 1 }
}
expect {
    -re {continue connecting \(yes/no\)} { send "yes\r"; exp_continue }
    "Connected!" {}
    timeout { puts "\nFAIL: never connected through 2-hop chain"; exit 1 }
}
sleep 1
send "q"
expect eof
EOF

# All three hosts recorded, each under its own name.
assert_contains "$(known_hosts)" "localhost"
assert_contains "$(known_hosts)" "gateway"
assert_contains "$(known_hosts)" "inner-b"

pass
