# ssh-files

A dual-pane file manager for local and remote (SSH/SFTP) filesystems.

![Flat transfer: cherry-pick files across remote folders and land them side by side locally](live-tests/demo/flat.gif)

## Why ssh-files

- **Transfers skip SFTP when they can.** Bytes ride a raw exec stream
  whenever the server allows it, which helps most on high-latency links,
  and fall back to SFTP automatically, per server, when exec is
  restricted. [BENCHMARKS.md](BENCHMARKS.md) has measurements against
  OpenSSH sftp/scp and rsync, in both directions.
- **Multi-hop that behaves like ssh.** ProxyJump chains with per-hop
  authentication and per-hop host-key verification, from `-J` or
  `~/.ssh/config`.
- **Both panes can be remote.** Server-to-server transfers stream through
  the client, so no key material is forwarded and the servers never talk
  to each other.
- **Nothing to install remotely, nothing privileged locally.** A single
  static binary in userland talking to plain sshd.

## Features

- Dual-pane interface (local + remote)
- Streaming transfers over raw exec channels — noticeably faster than the
  SFTP fallback path on high-latency links (see
  [BENCHMARKS.md](BENCHMARKS.md)), with automatic SFTP fallback for
  exec-restricted servers, and `--sftp-only` to never use exec at all
- Local mode (`--local`) for plain dual-pane file management without SSH
- Dual-remote mode (`--dual-remote`) — both panes on remote hosts, with
  remote-to-remote transfers
- ProxyJump (`-J`, ssh syntax) — multi-hop tunneling with per-hop
  authentication and host-key verification; `--virtual-relay` builds on it
  for endpoint pairs that are each only reachable through their own bastion
- Authentication like `ssh`: explicit `-i` keys, then every SSH agent
  identity, then default key files, then keyboard-interactive (the PAM
  prompt conversation), then password
- `~/.ssh/config` support — host aliases, per-host users/ports/keys, and
  config-driven `ProxyJump`
- Host key verification against `known_hosts` (trust-on-first-use)
- Tree expansion with lazy loading
- Overwrite confirmation — one prompt per transfer batch, never per file
- Hidden-file toggle (`.`) — hidden files are shown and transferred by default; toggle to exclude them from view and transfers
- Hierarchical selection
- Flat or structure-preserving transfers
- Copy as rsync — the context menu puts the exact `rsync` command for the
  pending transfer in your clipboard; ssh-files never runs it
- Rename, delete with confirmation
- Mouse support and context menus
- Cross-platform (Windows, macOS, Linux)

## Install

### Pre-built Binaries

Download from [Releases](https://github.com/kondyukov/ssh-files/releases):

| Platform | File |
|----------|------|
| Linux x64 | `ssh-files-linux-x64.tar.gz` |
| Linux ARM64 | `ssh-files-linux-arm64.tar.gz` |
| Windows x64 | `ssh-files-windows-x64.zip` |
| Windows ARM64 | `ssh-files-windows-arm64.zip` |
| macOS Intel | `ssh-files-macos-x64.tar.gz` |
| macOS ARM | `ssh-files-macos-arm64.tar.gz` |

Every release also ships a `SHA256SUMS` for integrity checks. Linux
builds are fully static (musl); nothing needs admin privileges.

### One-Line Install

**Linux / macOS:**
```bash
curl -sSL https://raw.githubusercontent.com/kondyukov/ssh-files/main/install.sh | sh
```

**Windows (PowerShell):**
```powershell
irm https://raw.githubusercontent.com/kondyukov/ssh-files/main/install.ps1 | iex
```

### Build from Source

Requires [Rust](https://rustup.rs) 1.85+.

```bash
cargo build --release
```

See [BUILD.md](BUILD.md) for portable builds and cross-compilation.

## Usage

```bash
ssh-files user@hostname
ssh-files -i ~/.ssh/id_rsa user@hostname
ssh-files user@hostname:2222
ssh-files -J bastion user@hostname               # ProxyJump: tunnel through one or more hops
ssh-files -J u1@hop1,u2@hop2:2022 user@hostname  # multi-hop chain, ssh -J syntax
ssh-files --local                # both panes in the current directory
ssh-files --local ~/Downloads    # right pane opens in ~/Downloads
ssh-files --dual-remote user@a:/x user@b:/y      # dual-pane remote-to-remote browsing
ssh-files --virtual-relay user@r user@a user@b   # dual-remote where both endpoints are
                                                 # reached only through relay r (bastion,
                                                 # jump box, or segmented network)
ssh-files --sftp-only user@host  # never open exec channels: all bytes move over
                                 # the SFTP protocol (see SECURITY.md)
ssh-files --bench user@host:/tmp # measure transfer throughput (default 256 MiB)
ssh-files --bench=64 user@host   # smaller benchmark for slow links
```

`-J` works like OpenSSH's ProxyJump: each hop is connected through the
previous one, and every hop is authenticated and host-key-verified under its
own name. A `-J` chain applies to every target of the chosen mode.
`--virtual-relay` also accepts four arguments (`RELAY_A HOST_A RELAY_B
HOST_B`) to give each endpoint its own relay — for hosts that sit behind
different bastions, or in segmented networks that cannot (or must not)
route to each other. Each relay argument may itself be a multi-hop chain
in `-J` syntax: `user@r1,user@r2`.

### SSH config

Hosts are resolved through `~/.ssh/config` (override the path with
`$SSH_FILES_SSH_CONFIG`), so aliases work the same as with `ssh`:

```
Host hpc
  HostName login.cluster.example.com
  User researcher
  Port 2222
  IdentityFile ~/.ssh/cluster_ed25519
  ProxyJump bastion.example.com
```

makes `ssh-files hpc` connect as `researcher` through the bastion with the
right key. Supported directives: `HostName`, `User`, `Port`, `IdentityFile`
(repeatable; all keys are tried in order), `ProxyJump` (hops are
themselves resolved through the config, cycles are detected), and
`Include` (glob patterns, `~`, and relative paths resolved against
`~/.ssh`, as `ssh` does). Both `Key value` and `Key=value` spellings are
accepted. Everything given explicitly on the command line wins over the
config, and a `-J` flag replaces a config `ProxyJump` chain, exactly as
with `ssh`.

Not supported (yet): `Match` blocks and `ProxyCommand` — both are
ignored with a startup warning. Other unknown directives are skipped
silently, and a missing or unparseable config never blocks a connection.

`--bench` compares the SFTP transfer path against a raw exec byte stream over
the same connection, plus the system `scp` as a baseline, and prints MiB/s for
each. Useful for diagnosing slow links.

### Transfer behavior

Transfers come in two modes. **Tree** recreates each item's full path as
shown in the sending pane (relative to that pane's root) under the
destination root. **Flat** sends exactly what you selected into the
destination root: each selected file or directory lands under its own
name, and a selected directory keeps its contents intact — like dragging
items into a folder. The modes differ only in whether the path *above*
each selected item is kept; neither restructures a directory's insides.

![Tree transfer: upload a project with its structure intact, then recreate a single nested file's full path](live-tests/demo/tree.gif)

The status line shows which byte path a transfer uses: `[streaming]` for the
raw exec fast path, `[sftp]` when the server restricts exec (e.g.
`ForceCommand internal-sftp`) and transfers fall back to the SFTP protocol.
Remote-to-remote transfers stream through the client (the servers never
talk to each other) and pick the byte path per side independently; when
the sides differ the label reads `[read/write]`, e.g. `[streaming/sftp]`
for a source that streams into a destination that only allows SFTP.

Prefer rsync for a particular transfer? The context menu's "Copy rsync
flat/tree" entries generate the equivalent command — same selection
roots, same direction, same structure semantics, same hidden-file
setting, with the connection's port, keys, and ProxyJump chain
reproduced in `-e` — and place it in the system clipboard for you to
inspect and run. ssh-files never executes it. (Dual-remote panes get no
entry: rsync has no third-party transfer mode.)

Files arrive under a temporary `.part` name and are renamed into place
only once every byte is verified — a file wearing its final name is never
truncated, no matter how the transfer ended. Cancelling stops
immediately, mid-file: completed files stay, and the in-flight file
remains as a visible `<name>.part`. Re-run the transfer (confirming
overwrite) to complete it. Errored transfers delete their `.part`
instead. Local-to-local copies are the one variation: each file is a
single atomic copy, so cancel takes effect at the next file boundary and
no partial ever exists.

## Keys

| Key | Action |
|-----|--------|
| `↑↓` / `jk` | Navigate |
| `←` / `→` | Switch panes |
| `Enter` / `l` | Enter directory |
| `Backspace` / `h` | Parent directory |
| `Tab` | Expand / collapse |
| `PgUp` / `PgDn` | Page up / down |
| `g` / `G` | Top / bottom |
| `Space` | Toggle selection |
| `a` / `A` | Select all / deselect all |
| `d` / `y` | Send flat `<-` (right pane to left) |
| `u` | Send flat `->` (left pane to right) |
| `F2` | Rename |
| `Del` / `x` | Delete |
| `m` | Context menu |
| `.` | Show/hide hidden files |
| `r` / `F5` | Refresh |
| `?` | Help (shows current bindings) |
| `q` / `Ctrl+C` | Quit |

Press `?` in the app for the authoritative list — it reflects your configuration.

## Configuration

Key bindings and colors can be customized in `config.toml`, looked up in this order:

1. `$SSH_FILES_CONFIG` (explicit path)
2. `~/.config/ssh-files/config.toml` (or `$XDG_CONFIG_HOME/ssh-files/config.toml`) — all platforms
3. Platform config dir: `%APPDATA%\ssh-files\config\config.toml` (Windows), `~/Library/Application Support/ssh-files/config.toml` (macOS)

Bindings live in a `[keys]` table mapping a key combo to an action. User entries
override the defaults per key; bind a key to `"none"` to remove a default binding.

```toml
[keys]
"ctrl+d" = "download_preserve"  # bind a new combo
"F6" = "upload_preserve"
"y" = "refresh"                 # rebind a default
"q" = "none"                    # unbind (Ctrl+C still quits)
```

Key combos: single characters (`"g"`, `"G"`, `"?"`), named keys (`up`, `down`, `left`,
`right`, `enter`, `esc`, `tab`, `backspace`, `delete`, `home`, `end`, `pageup`,
`pagedown`, `space`, `F1`–`F12`), with optional `ctrl+`, `alt+`, `shift+` modifiers.

Actions: `move_up`, `move_down`, `page_up`, `page_down`, `go_to_top`,
`go_to_bottom`, `focus_left`, `focus_right`, `enter_dir`, `go_up`,
`toggle_expand`, `toggle_select`, `select_all`, `deselect_all`,
`download_flat`, `download_preserve`, `upload_flat`, `upload_preserve`
(the download/upload actions send right-pane-to-left and left-pane-to-right
respectively, in every mode), `copy_path`, `rename`, `delete`,
`context_menu`, `toggle_hidden`, `refresh`, `toggle_help`, `quit`.

### Theme

Colors live in a `[theme]` table. Each entry overrides one slot of the palette
selected for your terminal's color support. Values can be hex RGB (`"#61afef"`),
a 256-color palette index (`75`), or a named ANSI color (`black`, `red`, `green`,
`yellow`, `blue`, `magenta`, `cyan`, `gray`, `darkgray`, `lightred`, `lightgreen`,
`lightyellow`, `lightblue`, `lightmagenta`, `lightcyan`, `white`, `reset`).

```toml
[theme]
border_focused = "#61afef"
directory = "#98c379"
selected_bg = 238
status_text = "yellow"
```

Slots: `border_focused`, `border_unfocused`, `directory`, `file`, `selected_bg`,
`selected_fg`, `marked_indicator`, `status_text`, `size`, `help_key`, `help_desc`,
`dimmed`.

Theme overrides are ignored when colors are disabled (`NO_COLOR` or `--color none`).

### Icons

Icons auto-detect from the locale: UTF-8 terminals get the unicode set
(📁 📄 ▼ ▶ ● ◐), everything else falls back to ASCII (`/ - v > * ~`).
Override in a `[ui]` table:

```toml
[ui]
icons = "ascii"   # or "unicode" / "auto"
```

Invalid entries in either section are reported on startup and skipped; the app
never fails to start because of a bad config.

## Status

v0.2.1, hardening release: keyboard-interactive auth and ssh_config
`Include`, defenses against malicious servers (path traversal, symlink
tricks, fabricated trees, `@revoked` host keys), a copy-as-rsync menu
entry, and supply-chain measures (dependency auditing, pinned CI,
attested release binaries). Connection handling, host-key verification,
and transfer correctness (including a hostile-filename gauntlet) are
exercised against a containerized multi-host matrix — see
[live-tests/RUNBOOK.md](live-tests/RUNBOOK.md). What the tool does with
keys, passwords, and host verification — and where it knowingly diverges
from OpenSSH — is written down in [SECURITY.md](SECURITY.md). Still
pending: packet-capture review of the multi-hop modes, and runtime testing
of the Linux ARM64, Windows ARM64, and macOS x64 binaries on real
hardware.

## Future Work

- **Lazy clipboard** — copy/cut/paste across panes and modes, resolved at
  paste time rather than copy time
- **Direct tar / zip transfers** — bundle a selection into a single archive
  stream on the wire, cutting per-file overhead for many-small-file trees
- **WebDAV and S3 backends (exploratory)** — the pane/source abstraction is
  backend-agnostic; open question

## License

Licensed under the Apache License, Version 2.0. See the LICENSE file, and
NOTICE for attribution.

Copyright 2026 Grigoriy Kondyukov.

#### Note: not affiliated with `russh` or `OpenSSH`. Thanks and appreciation goes to them for the infrastructure they've built. 
### Built with Claude, but not *vibe* coded. 