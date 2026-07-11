# Transfer benchmarks

Run with `ssh-files --bench[=SIZE_MIB] user@host:/dir`. The benchmark
compares the pure SFTP path against a raw exec byte stream over the same
authenticated connection, with the system `scp` as an external baseline.
Data is incompressible (deterministic xorshift noise).

## Baseline: 2026-06-11, 256 MiB, WAN link (~45ms RTT)

Client: macOS, russh with 32 MiB receive window. Server: stock OpenSSH.

| Path                | Throughput | Time    |
|---------------------|-----------:|--------:|
| SFTP upload         |  0.7 MiB/s | 346.96s |
| SFTP download       |  1.3 MiB/s | 189.76s |
| exec upload (cat)   |  4.5 MiB/s |  56.61s |
| exec download (cat) | 14.0 MiB/s |  18.23s |
| scp upload          |  4.3 MiB/s |  59.37s |
| scp download        |  8.4 MiB/s |  30.38s |

Readings:

- **SFTP framing cost is the dominant loss**: the raw stream over the same
  connection is 6.4x faster up, 10.8x down. russh-sftp awaits each request
  serially, so SFTP throughput is roughly one chunk per round-trip.
- **The 32 MiB client receive window helps on downloads** (14.0 vs
  scp's 8.4 MiB/s): scp's ~2 MiB window stalls on this link's
  bandwidth-delay product, the larger window doesn't.
- **Upload parity with scp** (4.5 vs 4.3) suggests both hit the same
  ceiling: either the server's advertised channel window or the uplink
  bandwidth itself. Distinguishing the two requires a striping experiment
  (two parallel channels: if throughput doubles, it was the window).

These numbers motivated the streaming transfer path (exec `cat`
sink/source with SFTP fallback) that the executors now use by default.

## Cross-tool comparison: 2026-07-10, 32 MiB, containerized rig

The June baseline compares the app against itself (plus scp). This
section benchmarks the same transfer against the tools people usually
reach for, under controlled latency, so the "faster than SFTP" claim is
scoped to what was actually measured.

Setup: the live-tests rig (`plain-a`, stock Alpine sshd), once with
`NETEM_DELAY=40ms` and once direct. Client: OpenSSH 10.2p1 (macOS; note
its `scp` speaks the SFTP protocol since 9.0), stock macOS openrsync
client against GNU rsync 3.4.3 on the server. External tools were timed
over a pre-established ControlMaster socket so the handshake is excluded
(parity with `--bench`, which measures after connect); median of 3 runs,
fresh destination each run. `ssh-files` numbers from `--bench=32`, two
runs agreeing within noise.

ssh-files rows were re-measured the same day after the russh 0.46 → 0.62
/ russh-sftp 2.3 upgrade (which raised the SFTP path's upload from 3.7
to 23.1 MiB/s under latency and roughly doubled its direct-link rates);
the external-tool rows are unchanged measurements from the same rig.

40ms injected latency:

| Path                  | Up (MiB/s) | Down (MiB/s) |
|-----------------------|-----------:|-------------:|
| ssh-files exec stream |       32.4 |         20.4 |
| ssh-files SFTP path   |       23.1 |          4.8 |
| OpenSSH sftp          |       17.9 |         23.1 |
| OpenSSH scp           |       17.8 |         22.8 |
| rsync (cold copy)     |       19.6 |         28.6 |

Direct (no injected latency):

| Path                  | Up (MiB/s) | Down (MiB/s) |
|-----------------------|-----------:|-------------:|
| ssh-files exec stream |      139.9 |        127.9 |
| ssh-files SFTP path   |      131.2 |        119.2 |
| OpenSSH sftp          |      127.9 |        142.8 |
| OpenSSH scp           |      123.7 |        131.8 |
| rsync (cold copy)     |      107.3 |        118.4 |

Readings:

- **The README's "faster than the SFTP fallback" is a claim about the
  app's own two paths** — the exec stream vs the SFTP fallback it would
  otherwise use (1.4x up, 4x down under latency here). That is the
  choice the app makes for you per server; it is not a claim about
  OpenSSH's sftp.
- **Modern OpenSSH sftp is heavily pipelined** and nothing like the
  naive one-request-per-round-trip profile: 18/23 MiB/s under 40ms.
- **Uploads under latency: the exec stream was the fastest measured**
  (32 vs 18–23 for the others).
- **Downloads under latency remain the weak flank**: the exec stream
  (20.4) and especially the SFTP path (4.8) trail pipelined sftp and
  rsync (23–29). The suspected limiter is client-side receive
  window/flow-control on inbound data; an open optimization target, not
  a protocol ceiling (uploads over the identical channels run 1.5–5x
  the rate).
- **On a fast local link everything converges** — differences are noise
  next to disk and crypto.
- **rsync's real advantage is not exercised here**: these are cold full
  copies. For re-syncing mostly-unchanged trees, rsync's delta transfer
  wins on transferred bytes and there is no ambition to compete with it.
