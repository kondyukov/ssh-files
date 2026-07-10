//! Transfer benchmark (`--bench`).
//!
//! Measures the SFTP path the executors actually use against a raw exec
//! byte stream over the same authenticated connection, with the system's
//! `scp` as an external baseline. The three numbers answer: how much the
//! SFTP request/acknowledge framing costs us, what the raw SSH channel can
//! do, and how we compare to the reference implementation on this link.

use std::io::Write as _;
use std::time::{Duration, Instant};

use anyhow::Result;
use russh::ChannelMsg;

use crate::cli::ConnectionInfo;
use crate::source::{FileSource, RemoteSource};
use crate::ssh::SftpClientShared;

/// Chunk size used for all in-process transfers (matches the executors).
const BLOCK: usize = 1024 * 1024;

pub async fn run(sftp: &SftpClientShared, conn: &ConnectionInfo, size_mib: u64) -> Result<()> {
    let size = size_mib * 1024 * 1024;
    let dir = conn
        .remote_path
        .clone()
        .unwrap_or_else(|| String::from("/tmp"));
    let remote_path = format!("{}/.ssh-files-bench.tmp", dir.trim_end_matches('/'));

    println!(
        "Benchmarking {} MiB transfers with {} (remote file: {})",
        size_mib,
        conn.display_name(),
        remote_path
    );

    let block = pseudo_random_block();
    // No exec handle: the SFTP measurements must stay on the pure SFTP
    // path, not silently use the streaming fast path.
    let source = RemoteSource::new(
        sftp.sftp(),
        dir.clone(),
        conn.host.clone(),
        conn.user.clone(),
        None,
    );

    // --- SFTP upload: the exact path our executors use ---
    let start = Instant::now();
    let mut writer = source.open_write(&remote_path).await?;
    let mut sent = 0u64;
    while sent < size {
        let n = BLOCK.min((size - sent) as usize);
        writer.write_all(&block[..n]).await?;
        sent += n as u64;
    }
    writer.flush().await?;
    report("SFTP upload", size, start.elapsed());

    // --- SFTP download ---
    let start = Instant::now();
    let mut reader = source.open_read(&remote_path).await?;
    let mut buf = vec![0u8; BLOCK];
    let mut received = 0u64;
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        received += n as u64;
    }
    anyhow::ensure!(
        received == size,
        "SFTP download returned {} of {} bytes",
        received,
        size
    );
    report("SFTP download", size, start.elapsed());

    // --- exec upload: raw byte stream into a remote `cat` ---
    let start = Instant::now();
    let mut channel = sftp
        .open_exec(&format!("cat > '{}'", remote_path))
        .await?;
    let mut sent = 0u64;
    while sent < size {
        let n = BLOCK.min((size - sent) as usize);
        channel.data(&block[..n]).await?;
        sent += n as u64;
    }
    channel.eof().await?;
    let status = wait_close(&mut channel).await;
    anyhow::ensure!(
        matches!(status, Some(0) | None),
        "remote cat exited with status {:?}",
        status
    );
    report("exec upload (cat)", size, start.elapsed());

    // --- exec download: raw byte stream out of a remote `cat` ---
    let start = Instant::now();
    let mut channel = sftp.open_exec(&format!("cat '{}'", remote_path)).await?;
    let mut received = 0u64;
    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::Data { data } => received += data.len() as u64,
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    anyhow::ensure!(
        received == size,
        "exec download returned {} of {} bytes",
        received,
        size
    );
    report("exec download (cat)", size, start.elapsed());

    // --- external scp baseline (best effort; needs non-interactive auth) ---
    scp_baseline(conn, &remote_path, size, &block);

    // Cleanup
    let _ = sftp.sftp().remove_file(&remote_path).await;

    Ok(())
}

/// Drain a channel after EOF, returning the remote exit status if reported.
async fn wait_close(channel: &mut russh::Channel<russh::client::Msg>) -> Option<u32> {
    let mut status = None;
    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    status
}

fn report(label: &str, bytes: u64, elapsed: Duration) {
    let mib = bytes as f64 / (1024.0 * 1024.0);
    let secs = elapsed.as_secs_f64();
    println!("  {:<20} {:>8.1} MiB/s  ({:.2}s)", label, mib / secs, secs);
}

fn report_skipped(label: &str, reason: &str) {
    println!("  {:<20} skipped ({})", label, reason);
}

/// Time the system `scp` against the same target, as a reference point.
/// Requires non-interactive auth (agent or identity file); silently skips
/// otherwise via BatchMode.
fn scp_baseline(conn: &ConnectionInfo, remote_path: &str, size: u64, block: &[u8]) {
    let local_up = std::env::temp_dir().join("ssh-files-bench-up.tmp");
    let local_down = std::env::temp_dir().join("ssh-files-bench-down.tmp");

    let write_local = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&local_up)?;
        let mut written = 0u64;
        while written < size {
            let n = BLOCK.min((size - written) as usize);
            file.write_all(&block[..n])?;
            written += n as u64;
        }
        file.sync_all()
    };
    if let Err(e) = write_local() {
        report_skipped("scp upload", &e.to_string());
        return;
    }

    let target = format!("{}@{}:{}", conn.user, conn.host, remote_path);
    let scp_cmd = |from: &str, to: &str| {
        let mut cmd = std::process::Command::new("scp");
        cmd.arg("-q")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-P")
            .arg(conn.port.to_string());
        if let Some(identity) = conn.identity_files.first() {
            cmd.arg("-i").arg(identity);
        }
        cmd.arg(from).arg(to);
        cmd
    };

    let start = Instant::now();
    match scp_cmd(&local_up.to_string_lossy(), &target).status() {
        Ok(s) if s.success() => report("scp upload", size, start.elapsed()),
        Ok(s) => report_skipped("scp upload", &format!("scp exited with {}", s)),
        Err(e) => report_skipped("scp upload", &e.to_string()),
    }

    let start = Instant::now();
    match scp_cmd(&target, &local_down.to_string_lossy()).status() {
        Ok(s) if s.success() => report("scp download", size, start.elapsed()),
        Ok(s) => report_skipped("scp download", &format!("scp exited with {}", s)),
        Err(e) => report_skipped("scp download", &e.to_string()),
    }

    let _ = std::fs::remove_file(&local_up);
    let _ = std::fs::remove_file(&local_down);
}

/// 1 MiB of deterministic xorshift noise: incompressible enough that
/// compression anywhere in the stack cannot skew results, and identical
/// across runs so they are comparable.
fn pseudo_random_block() -> Vec<u8> {
    let mut block = vec![0u8; BLOCK];
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    for chunk in block.chunks_mut(8) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let bytes = state.to_le_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
    block
}
