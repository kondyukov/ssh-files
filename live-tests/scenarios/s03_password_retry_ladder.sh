#!/bin/sh
# Password-only server: keys are rejected, the ladder falls through to the
# interactive password prompt, a wrong attempt is retried, and the right
# password connects.
set -eu
SCENARIO="s03_password_retry_ladder"
. "$(dirname "$0")/lib.sh"
new_sandbox "$SCENARIO"

export SF_BIN="$BIN"

expect <<'EOF'
set timeout 20
spawn $env(SF_BIN) test@localhost:2205
expect {
    -re {continue connecting \(yes/no\)} { send "yes\r"; exp_continue }
    -re {password: } { send "wrong-password\r" }
    timeout { puts "\nFAIL: no password prompt"; exit 1 }
}
expect {
    "Permission denied, please try again." {}
    timeout { puts "\nFAIL: wrong password not rejected with retry"; exit 1 }
}
expect {
    -re {password: } { send "pw-secret-1\r" }
    timeout { puts "\nFAIL: no second password prompt"; exit 1 }
}
expect {
    "Connected!" {}
    timeout { puts "\nFAIL: correct password did not connect"; exit 1 }
}
sleep 1
send "q"
expect eof
EOF

pass
