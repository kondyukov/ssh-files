# ssh-files live test matrix

Docker-backed correctness rig. Wave 1 is fully automated; waves 2–4 are
human-driven (you operate the TUI) with scripted setup and verification
around each run.

## One-time setup

```sh
cd live-tests
./setup.sh                    # keys, fixtures, build image, containers up
cargo build --release         # from repo root; scenarios use the release binary
```

Latency (makes streaming-vs-sftp measurable and cancel testable):

```sh
NETEM_DELAY=40ms docker compose up -d --force-recreate
```

Topology: bastion `:2201`, gateway `:2202`, plain-a `:2203`,
sftponly `:2204`, pwonly `:2205`; **inner-b** is internal-only (reached
only through bastion/gateway). User `test`; pwonly password `pw-secret-1`;
keys `keys/id_ed25519` and `keys/id_enc` (passphrase `livetest`).

Every scenario runs in a hermetic sandbox (`runs/<name>/`): `HOME` is
redirected so `known_hosts`, default keys, and both config files resolve
there and never touch your machine.

## Wave 1 — connection layer (automated)

```sh
scenarios/run_all_wave1.sh
```

| # | Proves |
|---|--------|
| s00 | TOFU prompt records the key; reconnect is silent |
| s01 | Mismatched recorded key is refused (MITM path), nonzero exit |
| s02 | Encrypted `-i` key prompts for passphrase, then authenticates |
| s03 | pwonly: wrong password retried, right one connects |
| s04 | Three wrong passwords → clean failure, exactly 3 attempts, nonzero exit |
| s05 | ssh_config alias supplies HostName/User/Port/IdentityFile |
| s06 | config `ProxyJump` tunnels to internal-only inner-b; both hops recorded |
| s07 | `-J` overrides a config `ProxyJump` chain |
| s08 | Two-hop `-J` chain to inner-b; all three hosts recorded |

## Wave 2 — single-remote transfers (manual)

Launch with `./manual.sh <name> -- <args>`, drive the TUI, then run the
`verify.sh` line. Destination is the container's `/data/incoming` (fresh
tmpfs per container start). Accept TOFU (`yes`) on first connect each run.

**Pane root matters.** "Send tree" preserves each file's path relative to
the sending *pane root*, not to the selected directory. `Enter`/`l`
re-roots the pane into a directory; `Tab` merely expands it in place. So
to send `docs` as `docs/...`, press `Enter` down into `fixtures/tree` so
that `docs` is a top-level row, then select it. If you only Tab-expand
from a higher root, the transfer still works but arrives with the extra
leading directories (e.g. `fixtures/tree/docs/...`) and your `verify.sh`
path shifts accordingly.

The container's copy of the fixtures is at `/data/fixtures`; for
downloads, `Enter` the **right** (remote) pane to `/data/fixtures`. For
uploads, `Enter` the **left** (local) pane to `fixtures/tree`.

### w2.1 Upload preserve (streaming)
```sh
./manual.sh w2_up_preserve -- -i keys/id_ed25519 test@localhost:2203:/data/incoming
```
`Enter` the left pane into `fixtures/tree` (so `docs` is a top-level
row), put the cursor on `docs`, and send tree via the context menu
(`m` → "Send tree ->"; `u` is flat). Status should read
`Sending ... -> incoming [streaming]`. Quit, then verify in-container
(xargs would mangle the naughty names, hence the read loop):
```sh
docker compose exec -T plain-a sh -c \
  'cd /data/incoming && find . -type f | LC_ALL=C sort | while IFS= read -r f; do sha256sum "$f"; done' \
  | diff fixtures/manifest.sha256 -
docker compose exec -T plain-a sh -c 'find /data/incoming -type d'
```
Expect an empty diff (13 files, `.hidden.txt` included — hidden files
are shown and transferred by default) and `docs/sub/empty_dir/` in the
directory list. Toggling `.` before sending excludes hidden files; that
variant should arrive without `.hidden.txt`.

### w2.2 Upload flat
Same launch (new name). Flat sends *what you selected* into the
destination root: each selection root lands under its own name with its
contents intact; only the path above it is dropped. (For a top-level
selection, flat and tree are therefore identical.) Root the left pane at
`fixtures/tree`, Tab-expand `docs`, Space-select `naughty` and `sub`,
press `u`. Expect at the `incoming` root: `naughty/` (9 files) and
`sub/` (`nested.txt` + `empty_dir/`), **no** `docs/` prefix:
```sh
docker compose exec -T plain-a sh -c 'cd /data/incoming && find . | sort'
```

### w2.2b Flat collision is refused
The standing fixture `fixtures/collision/` holds `a/x.txt` and `b/x.txt`
(same basename, different contents, deliberately outside the manifest).
In an upload sandbox, root the left pane at `fixtures/collision`,
Tab-expand `a` and `b`, Space-select both `x.txt` files, press `u`.
The collision dialog must appear naming `x.txt`, and NOTHING may
transfer — confirm the destination is untouched:
```sh
docker compose exec -T plain-a sh -c 'ls /data/incoming'
```

### w2.3 Download to local
```sh
mkdir -p runs/w2_download_dest
./manual.sh w2_download -- -i keys/id_ed25519 test@localhost:2203:/data/fixtures
```
`Enter` the left (local) pane into `runs/w2_download_dest` (it receives at
its root). The right pane opens at `/data/fixtures`, so `docs` is already
a top-level row; cursor on it, `m` → "Send tree <-" (`d` is flat). Then:
```sh
./verify.sh runs/w2_download_dest docs
```

### w2.4 SFTP fallback (sftponly)
```sh
./manual.sh w2_sftp -- -i keys/id_ed25519 test@localhost:2204:/data/incoming
```
Status **must** read `[sftp]`, not `[streaming]` (server forces
internal-sftp). Transfer still completes; verify bytes as in w2.1.

### w2.5 Naughty names round-trip
Upload only `naughty` to a plain server (left pane rooted at
`fixtures/tree/docs`, cursor on `naughty`, send tree or flat — it lands
as `naughty/` either way), then download it back to a fresh local dir
and verify against the manifest subset (`--flat` checks basenames, so
point it at the directory itself):
```sh
./verify.sh <local-dest>/naughty docs/naughty --flat
```
Any mismatch or missing file is a `shell_quote` bug.

### w2.6 Cancel mid-transfer  (needs NETEM_DELAY)
Start a big upload, press `q` during `big.bin`, confirm cancel. Expected
(documented behavior): stops immediately, mid-file; already-written files
remain and the partial `big.bin` is left in place at less than its full
size. Verify the partial exists and is truncated:
```sh
docker compose exec -T plain-a sh -c 'ls -l /data/incoming/big.bin'
```
Re-running the transfer prompts to overwrite and completes it.

### w2.7 Overwrite prompt is per-batch
Upload `docs` twice. The second run must prompt **once** for the whole
batch, never once per file.

### w2.8 rename / delete
On a plain server, rename a file, delete a file, delete a directory that
contains a dotfile, delete a naughty-named file. Refresh (`r`) and confirm
each took effect.

## Wave 3 — multi-host (manual)

### w3.1 dual-remote transfer
Remote-to-remote picks the byte path per side; the label shows
`[read/write]` when the sides differ.

Mixed: plain-a (exec ok) → sftponly (exec forced off):
```sh
./manual.sh w3_dual -- -i keys/id_ed25519 --dual-remote \
    test@localhost:2203:/data/fixtures test@localhost:2204:/data/incoming
```
Send left→right: status must read `[streaming/sftp]`. For the reverse
direction the left pane must first leave the read-only fixtures mount:
`Backspace` up to `/data`, `Enter` into `incoming` (writable tmpfs), then
send something from sftponly's incoming back: `[sftp/streaming]`. Verify
bytes on the destination container.

Both streaming: plain-a → bastion:
```sh
./manual.sh w3_dual2 -- -i keys/id_ed25519 --dual-remote \
    test@localhost:2203:/data/fixtures test@localhost:2201:/data/incoming
```
Send left→right: status must read `[streaming]` (single label - the
sides agree).

### w3.2 virtual-relay to internal hosts
```sh
./manual.sh w3_vrelay -- -i keys/id_ed25519 --virtual-relay \
    test@localhost:2201 test@inner-b test@inner-b
```
(Single relay, both endpoints inner-b for a simple loop; or use gateway +
two distinct inner hosts if added.) Proves endpoints reached only through
the relay. Accept TOFU for relay and endpoints.

### w3.3 bench through a hop  (needs NETEM_DELAY)
```sh
HOME=runs/w3_bench/home SSH_AUTH_SOCK= \
  ../target/release/ssh-files -i keys/id_ed25519 \
  -J test@localhost:2201 --bench=32 test@inner-b:/data/incoming
```
Confirms streaming beats SFTP *through the tunnel* — i.e. the 32 MiB hop
window is not throttling. Record the MiB/s numbers.


| link | SFTP up / down | exec up / down |
|---|---|---|
| direct plain-a | 60.8 / 88.7 | 91.4 / 144.0 |
| via bastion → inner-b | 47.7 / 72.5 | 130.5 / 150.3 |
| via bastion, 40ms netem | 2.0 / 2.4 | 17.0 / 23.4 |

Streaming through the hop runs at line speed (no window throttling); under
latency SFTP's request/ack framing costs ~8-10×. The scp baseline
self-skips in sandboxes: OpenSSH stores non-22 ports as `[host]:port` in
known_hosts, our entries are plain `host`, and BatchMode can't prompt.

## Wave 4 — robustness (manual)

- **Network drop:** mid-transfer, `docker network disconnect
  live-tests_frontnet plain-a`. Expect a clean error in the status line, no
  hang, terminal restored on quit. Reconnect: `docker network connect`.
- **Server killed:** `docker compose stop plain-a` mid-session → clean error.
- **Tiny terminal:** resize to ~20 columns during a transfer; no panic
  (the release binary aborts on panic, so a crash is a hard fail).

## Demo recording

The README welcome GIFs are recorded against this same rig with vhs
(`brew install vhs`). Fixtures include the demo material; record with
latency so the progress bar is visible:

```sh
NETEM_DELAY=40ms docker compose up -d --force-recreate
demo/record.sh            # or: demo/record.sh tree|flat
```

The tapes' keystrokes are position-coupled to fixtures/demo — if those
fixtures change, re-walk the sequences (see comments in the .tape files).

## Teardown

```sh
docker compose down       # keep host keys (volumes persist)
docker compose down -v     # also drop host keys -> next up re-TOFUs
rm -rf runs               # clear sandboxes
```
