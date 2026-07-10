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
- **The 32 MiB client receive window beats scp's ~2 MiB on downloads**
  (14.0 vs 8.4 MiB/s): the reference implementation stalls on this link's
  bandwidth-delay product; we don't.
- **Upload parity with scp** (4.5 vs 4.3) suggests both hit the same
  ceiling: either the server's advertised channel window or the uplink
  bandwidth itself. Distinguishing the two requires a striping experiment
  (two parallel channels: if throughput doubles, it was the window).

These numbers motivated the streaming transfer path (exec `cat`
sink/source with SFTP fallback) that the executors now use by default.
