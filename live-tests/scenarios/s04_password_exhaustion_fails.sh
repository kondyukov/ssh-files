#!/bin/sh
# Three wrong passwords exhaust the ladder: clean failure naming the host,
# nonzero exit, no crash and no infinite loop.
set -eu
SCENARIO="s04_password_exhaustion_fails"
. "$(dirname "$0")/lib.sh"
new_sandbox "$SCENARIO"

export SF_BIN="$BIN"

expect <<'EOF'
set timeout 20
set attempts 0
spawn $env(SF_BIN) test@localhost:2205
expect {
    -re {continue connecting \(yes/no\)} { send "yes\r"; exp_continue }
    -re {password: } {
        incr attempts
        if {$attempts > 3} { puts "\nFAIL: more than 3 password attempts"; exit 1 }
        send "nope-$attempts\r"
        exp_continue
    }
    -re {Authentication failed for test@} {}
    "Connected!" { puts "\nFAIL: connected with wrong passwords"; exit 1 }
    timeout { puts "\nFAIL: no terminal auth failure"; exit 1 }
}
expect eof
catch wait result
if {[lindex $result 3] == 0} { puts "\nFAIL: exit code 0 on auth failure"; exit 1 }
if {$attempts != 3} { puts "\nFAIL: expected exactly 3 attempts, got $attempts"; exit 1 }
EOF

pass
