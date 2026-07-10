# ssh-files

A dual-pane file manager for local and remote (SSH/SFTP) filesystems.

![Flat transfer: cherry-pick files across remote folders and land them side by side locally](live-tests/demo/flat.gif)

Initial soft release of v0.2.0. 

Packet inspection needs to be performed to validate no network topology leakages. (e.g. with Wireshark). 

Linux ARM, Windows ARM, and macOS x64 versions need runtime testing on actual hardware. 

## Features

- Dual-pane interface (local + remote)
- Streaming transfers over raw exec channels — several times faster than
  SFTP on high-latency links (see [BENCHMARKS.md](BENCHMARKS.md)), with
  automatic SFTP fallback for exec-restricted servers
- Local mode (`--local`) for plain dual-pane file management without SSH
- Dual-remote mode (`--dual-remote`) — both panes on remote hosts, with
  direct remote-to-remote transfers
- ProxyJump (`-J`, ssh syntax) — multi-hop tunneling with per-hop
  authentication and host-key verification; `--virtual-relay` builds on it
  so the client never peers directly with either endpoint
- SSH/SFTP with password and key authentication
- `~/.ssh/config` support — host aliases, per-host users/ports/keys, and
  config-driven `ProxyJump`
- Host key verification against `known_hosts` (trust-on-first-use)
- Tree expansion with lazy loading
- Overwrite confirmation — one prompt per transfer batch, never per file
- Hidden-file toggle (`.`) — hidden files are shown and transferred by default; toggle to exclude them from view and transfers
- Hierarchical selection
- Flat or structure-preserving transfers
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
ssh-files --virtual-relay user@r user@a user@b   # dual-remote where each host is reached
                                                 # through relay r; the client never peers
                                                 # with a or b directly
ssh-files --bench user@host:/tmp # measure transfer throughput (default 256 MiB)
ssh-files --bench=64 user@host   # smaller benchmark for slow links
```

`-J` works like OpenSSH's ProxyJump: each hop is connected through the
previous one, and every hop is authenticated and host-key-verified under its
own name. A `-J` chain applies to every target of the chosen mode.
`--virtual-relay` also accepts four arguments (`RELAY_A HOST_A RELAY_B
HOST_B`) to give each endpoint its own relay, so A and B never share an
observed network peer. Each relay argument may itself be a multi-hop chain
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
(repeatable; all keys are tried in order), and `ProxyJump` (hops are
themselves resolved through the config, cycles are detected). Everything
given explicitly on the command line wins over the config, and a `-J` flag
replaces a config `ProxyJump` chain, exactly as with `ssh`. Unknown
directives are ignored; a missing or unparseable config never blocks a
connection.

Not supported (yet): `Include`, `Match` blocks, `ProxyCommand`, and
`Key=value` syntax (use `Key value`).

`--bench` compares the SFTP transfer path against a raw exec byte stream over
the same connection, plus the system `scp` as a baseline, and prints MiB/s for
each. Useful for diagnosing slow links before pointing fingers at the tool.

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

Cancelling a transfer stops immediately, mid-file: everything already
written stays at the destination, including the partially written current
file. Re-run the transfer (confirming overwrite) to complete it. The one
exception is local-to-local copies, which copy each file in a single
atomic operation — cancel there takes effect at the next file boundary,
so no partial files occur.

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