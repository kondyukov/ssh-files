#!/bin/sh
# Keyboard-interactive-only server (PAM): password auth is disabled, so
# the client must run the prompt conversation. A wrong answer is retried
# ("Permission denied"), the right one connects. This is the hardened-
# Linux default posture (PasswordAuthentication no + UsePAM yes).
set -eu
SCENARIO="s09_kbdint_auth"
. "$(dirname "$0")/lib.sh"
new_sandbox "$SCENARIO"

export SF_BIN="$BIN"

expect <<'EOF'
set timeout 20
spawn $env(SF_BIN) test@localhost:2206
expect {
    -re {continue connecting \(yes/no\)} { send "yes\r"; exp_continue }
    -re {[Pp]assword: ?} { send "wrong-password\r" }
    timeout { puts "\nFAIL: no keyboard-interactive prompt"; exit 1 }
}
expect {
    "Permission denied, please try again." {}
    timeout { puts "\nFAIL: wrong answer not rejected with retry"; exit 1 }
}
expect {
    -re {[Pp]assword: ?} { send "pw-secret-1\r" }
    timeout { puts "\nFAIL: no second keyboard-interactive prompt"; exit 1 }
}
expect {
    "Connected!" {}
    timeout { puts "\nFAIL: correct answer did not connect"; exit 1 }
}
sleep 1
send "q"
expect eof
EOF

pass
