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
then keyboard-interactive (the server's prompt conversation, relayed to
the terminal — the PAM reality on hardened Linux hosts; s09), then
password (s03/s04). Interactive secret prompts share one three-attempt
budget across the last two methods, like ssh's NumberOfPasswordPrompts.
Private keys are read, never written or copied; encrypted keys prompt
for their passphrase (s02). The agent is only ever asked to *sign* —
never to reveal a key — and agent forwarding does not exist in this
codebase. Passwords and passphrases are prompted per connection and held
in memory only.

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

`@revoked` markers are honored with OpenSSH pattern semantics
(comma-separated `*`/`?` globs, `!` negation, `[host]:port`, hashed
entries): a key marked revoked for a matching host is refused outright
— never downgraded to a first-use prompt — and errors reading the file
also refuse the connection (s11). `@cert-authority` is **not**
supported, because this client does not speak host certificates at
all; a host trusted only through a CA line will present as unknown and
fall back to the TOFU prompt. The prompt says so itself: when a
`@cert-authority` entry matches the host, it carries a note that the
CA line exists but cannot be used, so the configuration is never
ignored silently. If your fleet trust is CA-based, verify the
fingerprint at that prompt against your CA's issuance records.

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

## Hostile directory listings

A remote server chooses the bytes it returns from a directory listing —
the names, the types, the link targets. ssh-files treats a listing as
untrusted input, because a malicious or compromised server can use it to
make a client write outside the directory the user picked (the scp
`CVE-2019-6111` class).

- **Path traversal.** Every name that becomes part of a destination path
  must be a single component that names one entry *inside* its parent:
  non-empty, not `.`/`..`, and free of `/`, `\`, or NUL. A server that
  returns `../../.bashrc` or `/etc/cron.d/evil` in a `readdir` is refused
  — the whole transfer aborts with a named error, rather than composing a
  path that escapes the destination root. The check runs at the wire
  boundary (the SFTP walk) and again in the path mapper that every
  executor trusts, so no single layer is load-bearing. Hostile names are
  also hidden from the browse view, so one can never be selected as a
  transfer anchor. (Local filesystems are not treated this way: the OS
  never yields `.`/`..` or separator-bearing names from `readdir`.)

  No honest filesystem can even *store* a name like `../x`, so these
  cases cannot be staged on the containerized rig — the lie happens at
  the protocol layer. They are tested there: an in-process SFTP server
  that returns hostile listings is bound to the real client over an
  in-memory pipe (`hostile_server_tests` in `src/source/remote.rs`),
  exercising the exact wire path a malicious server controls.

- **Fabricated trees.** A server can also lie *structurally*: a
  directory that contains another directory forever, or one that pours
  out entries without end, walking the client into unbounded recursion
  and memory. Enumeration is therefore budgeted: nesting deeper than
  256 levels or a selection exceeding 1,000,000 entries aborts the
  transfer with a named error. Honest trees stay far below both (the
  Linux kernel tree is ~80k files; PATH_MAX bounds real nesting well
  under the cap), so the limits are attack signals, never silent
  truncation.

- **Symbolic links.** Links reported by the server are **not followed,
  descended into, or transferred** — they are skipped and the count is
  shown in the status line (`N symlinks skipped`), so a partial transfer
  is never silently mistaken for a complete one. This is deliberately
  more conservative than OpenSSH: `scp -r` follows links and `rsync`
  (without `-l`/`-L`) copies them *as* links. We do neither, because
  following a link would let the server materialize a target of its
  choosing (`/etc/shadow`, or `/dev/zero` for an unbounded read) under an
  innocuous name, and we have no API to recreate a link at the
  destination. A future release may add an opt-in `scp -r`-style
  follow mode; the walk already records links so it can be built safely
  (with destination link-creation and loop detection) rather than bolted
  on. Local symlinks are unaffected — a local source is not
  attacker-controlled and local links are commonly intentional.

## ssh_config

Honored: `HostName`, `User`, `Port`, `IdentityFile`, `ProxyJump` (with
cycle detection), and `Include` with ssh's resolution rules (globs, `~`,
relative patterns against `~/.ssh`, depth-capped, cycle-safe; s10). Both
`Key value` and `Key=value` spellings are accepted.

`Match` blocks and `ProxyCommand` are not supported; each produces a
startup warning. `Match` blocks are dropped whole — leaving them in
place would be worse than ignoring them, because a naive parser would
misattribute the block's body to the enclosing `Host`. A missing or
unparseable config never blocks a connection. If your security posture
depends on config-driven routing, verify the connection path (the TOFU
prompts name every hop).

## Data integrity

Files arrive as `<name>.part` and are renamed into place only after the
byte count is verified, so a truncated file can never masquerade as
complete: a crash at any moment leaves at most a visibly-partial
`.part`. Errored or short transfers delete their `.part`; a
user-initiated cancel deliberately leaves it in place for a re-run to
overwrite. See "Transfer behavior" in the README.

## Generated commands

The context menu can produce the rsync command equivalent to a pending
transfer ("Copy rsync flat/tree"). That is text placed in the clipboard
for you to inspect and run yourself — ssh-files never executes rsync (or
any other local program) on your behalf, and does not even probe whether
rsync is installed. Arguments are quoted for your local shell;
remote-side argument handling assumes rsync ≥ 3.2.4, where remote args
are protected by default.

## Supply chain

What you can verify about the binaries and the tree that produces them:

- **Provenance.** Every release archive carries a GitHub build
  attestation created by the job that built it:
  `gh attestation verify <archive> --repo <owner>/ssh-files` proves it
  was built by the public release workflow from this repository at a
  stated commit. Deliberately keyless (sigstore): there is no long-lived
  signing key to steal. Honest scope: the attestation proves the binary
  matches the public source — it does not vouch for the source itself.
  The `SHA256SUMS` file guards download integrity only; it is produced
  by the same pipeline, so it is not an authenticity mechanism.
- **Dependencies.** All 350+ crates come from crates.io exclusively —
  no git dependencies, enforced by `cargo deny` (`deny.toml`), which
  also gates RustSec advisories and license compliance weekly and on
  every manifest change, not just on pushes (advisories are published
  against unchanged lockfiles). Every build — CI, release, local
  scenarios — runs `--locked`. The two currently ignored advisories are
  written down in `deny.toml` with their reasoning, not hidden.
- **Toolchain and workflows.** The Rust toolchain is pinned to an exact
  version; every GitHub Action is pinned to a full commit SHA (a
  mutable tag is an attack vector — tags have been rewritten in the
  wild), with dependabot keeping both pins and crates fresh; workflows
  run with read-only tokens except the narrowly-scoped release grants.

Not covered, so you don't over-trust the above: a compromise of the
maintainer's own machine or account, and the human review of dependency
*code* (we gate on advisories and provenance, not line-by-line audits —
cargo-vet needs a sustained team to mean anything).

## Reporting

Please report suspected vulnerabilities via GitHub security advisories
(preferred) or an issue on the repository.
