# Security

ssh-files is an SSH client: it handles private keys, passwords, and host
verification. This document says exactly what it does with them, which
assumptions it makes, and where it knowingly diverges from OpenSSH — so
that you can audit the claims instead of trusting them.

The SSH implementation is [russh](https://github.com/Eugeny/russh), not
OpenSSH, with [ring](https://github.com/briansmith/ring) as the crypto
backend (russh's other option is aws-lc-rs; there is no OpenSSL anywhere
in the tree). Everything below that verifies hosts, parses configs, or
builds remote command lines is this project's responsibility and is
exercised by the containerized test matrix in
[live-tests/RUNBOOK.md](live-tests/RUNBOOK.md); scenario numbers below
refer to it.

## Credentials

Authentication tries, in order: keys given with `-i` or `IdentityFile`,
every identity the SSH agent holds, default key files (`~/.ssh/id_*`),
then password prompts (three attempts, s03/s04). Private keys are read,
never written or copied; encrypted keys prompt for their passphrase
(s02). The agent is only ever asked to *sign* — never to reveal a key —
and agent forwarding does not exist in this codebase. Passwords and
passphrases are prompted per connection and held in memory only.

In dual-remote and virtual-relay modes, server-to-server transfers are
bridged through the client precisely so that no credential material has
to reach either server: neither host learns anything about the other.

## Host key verification

Trust-on-first-use: the first connection shows the server's key
fingerprint and asks; accepted keys are recorded in `~/.ssh/known_hosts`.
A later mismatch is a hard refusal, not a warning (s01). In multi-hop
chains (`-J`, config `ProxyJump`, `--virtual-relay`), every hop is
authenticated and verified under its own name (s06–s08).

The file format matches OpenSSH: non-22 ports are recorded as
`[host]:port` (byte-identical to `ssh-keyscan` output, verified on the
test rig), and hashed entries (`HashKnownHosts yes`, the Debian/Ubuntu
default) are read and matched. One migration note: versions before 0.3
recorded non-22 hosts under the plain host name; those old entries no
longer match, so the first reconnect to such a host re-prompts and
records the correct form.

## The exec streaming fast path

For speed, transfers use raw exec channels when the server allows it:
`cat <path>` to read, `cat > <path>` to write. That means file names are
embedded in a remote command line, which is a shell-injection surface,
and it is treated as one:

- Every path is wrapped in POSIX single quotes, inside which the shell
  interprets *nothing*; the only character that needs handling is the
  single quote itself (`'` → `'\''`). `$(...)`, backticks, semicolons,
  globs, newlines, and leading dashes are inert by construction
  (unit-tested in `src/ssh/exec.rs`).
- The live-test gauntlet round-trips files named `$(danger).txt`,
  `` back`tick.txt ``, `it's.txt`, `quote"d.txt`, an embedded newline, a
  carriage return, `danger!bang.txt`, and a trailing space, byte-for-byte
  through the real TUI on both byte paths — automated in
  `live-tests/scenarios/w25_roundtrip.sh` (w2.5).
- Streamed reads are checked against the expected byte count; a short
  arrival is deleted and reported, never left looking complete.

**Assumption:** the remote login shell is POSIX-compatible (sh, bash,
zsh, dash, ...). The csh family does not honor single-quote semantics
(`!` history expansion and embedded newlines stay special), so on hosts
with csh/tcsh login shells the streaming path is not safe and you should
use `--sftp-only`.

Hostile names are also a *display* problem: printed raw, an embedded
newline shreds the TUI and a raw ESC could smuggle ANSI sequences to the
terminal. Every filename-bearing string is sanitized at the rendering
boundary (`src/ui`): C0/C1 control characters and DEL become U+FFFD —
which neutralizes ESC- and CSI-initiated sequences — and the explicit
Unicode bidi overrides (U+202A–U+202E, U+2066–U+2069, the
right-to-left-override name-spoofing trick) are replaced likewise. The
transferred bytes are untouched; only what the terminal is shown is
filtered.

**Opting out entirely:** `--sftp-only` never opens exec channels; every
byte moves over the SFTP protocol and no remote command line is ever
built from a file name. Servers that restrict exec (e.g. `ForceCommand
internal-sftp`) get the same treatment automatically — the capability is
probed once per connection with `cat /dev/null` and transfers fall back
to SFTP (w2.4). The status line always shows which path is in use:
`[streaming]` or `[sftp]`.

## ssh_config

Only a subset is honored: `HostName`, `User`, `Port`, `IdentityFile`,
`ProxyJump` (with cycle detection). `Include`, `Match`, and
`ProxyCommand` are not supported yet, and a missing or unparseable
config never blocks a connection — on configs that rely on those
directives, behavior diverges from `ssh` silently. If your security
posture depends on config-driven routing, verify the connection path
(the TOFU prompts name every hop).

## Data integrity

Files arrive as `<name>.part` and are renamed into place only after the
byte count is verified, so a truncated file can never masquerade as
complete: a crash at any moment leaves at most a visibly-partial
`.part`. Errored or short transfers delete their `.part`; a
user-initiated cancel deliberately leaves it in place for a re-run to
overwrite. See "Transfer behavior" in the README.

## Reporting

Please report suspected vulnerabilities via GitHub security advisories
(preferred) or an issue on the repository.
