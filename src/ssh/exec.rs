//! Raw exec-channel streaming: the fast transfer path.
//!
//! SFTP acknowledges every read/write request, which caps throughput at
//! roughly one chunk per round-trip. Streaming bytes through a remote
//! `cat` on an exec channel removes that per-chunk handshake entirely;
//! the only confirmation is the process exit status at the end. On
//! high-latency links this is several times faster (see BENCHMARKS.md).

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use russh::client::Msg;
use russh::{Channel, ChannelMsg};

use crate::source::{FileReader, FileWriter};

/// Wrap a path in single quotes for a POSIX shell, escaping embedded quotes.
pub fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', r"'\''"))
}

/// Streams bytes into a remote process's stdin (e.g. `cat > file`).
pub struct ExecWriter {
    channel: Channel<Msg>,
}

impl ExecWriter {
    pub fn new(channel: Channel<Msg>) -> Self {
        Self { channel }
    }
}

#[async_trait]
impl FileWriter for ExecWriter {
    async fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        self.channel
            .data(buf)
            .await
            .map_err(|e| anyhow!("stream write failed: {}", e))
    }

    /// Close the stream and wait for the remote process to confirm success.
    async fn flush(&mut self) -> Result<()> {
        self.channel.eof().await?;

        let mut status = None;
        while let Some(msg) = self.channel.wait().await {
            match msg {
                ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                ChannelMsg::Close => break,
                _ => {}
            }
        }

        match status {
            Some(0) | None => Ok(()),
            Some(code) => Err(anyhow!("remote write process exited with status {}", code)),
        }
    }
}

/// Streams bytes from a remote process's stdout (e.g. `cat file`).
///
/// A dropped connection is indistinguishable from end-of-stream here, so
/// callers must verify the byte count against the expected file size.
pub struct ExecReader {
    channel: Channel<Msg>,
    buffer: Vec<u8>,
    offset: usize,
    done: bool,
}

impl ExecReader {
    pub fn new(channel: Channel<Msg>) -> Self {
        Self {
            channel,
            buffer: Vec::new(),
            offset: 0,
            done: false,
        }
    }
}

#[async_trait]
impl FileReader for ExecReader {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        loop {
            // Drain buffered data first
            if self.offset < self.buffer.len() {
                let n = (self.buffer.len() - self.offset).min(buf.len());
                buf[..n].copy_from_slice(&self.buffer[self.offset..self.offset + n]);
                self.offset += n;
                return Ok(n);
            }

            if self.done {
                return Ok(0);
            }

            match self.channel.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    self.buffer.clear();
                    self.buffer.extend_from_slice(&data);
                    self.offset = 0;
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    if exit_status != 0 {
                        self.done = true;
                        return Err(anyhow!(
                            "remote read process exited with status {}",
                            exit_status
                        ));
                    }
                }
                // Keep draining after EOF so a late nonzero exit status is
                // still observed; Close or channel teardown ends the stream.
                Some(ChannelMsg::Eof) => {}
                Some(ChannelMsg::Close) | None => self.done = true,
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_paths_for_shell() {
        assert_eq!(shell_quote("/tmp/plain.txt"), "'/tmp/plain.txt'");
        assert_eq!(
            shell_quote("/tmp/it's here.txt"),
            r"'/tmp/it'\''s here.txt'"
        );
        assert_eq!(shell_quote("/tmp/$HOME `cmd` \"x\""), "'/tmp/$HOME `cmd` \"x\"'");
    }

    /// Inside POSIX single quotes nothing is interpreted, so the only
    /// character shell_quote must touch is the quote itself; everything
    /// else - including newlines - passes through literally. The csh
    /// family breaks this contract (`!` and newline stay special there);
    /// see SECURITY.md for that assumption and the --sftp-only escape
    /// hatch.
    #[test]
    fn hostile_names_stay_inert() {
        assert_eq!(shell_quote("$(rm -rf ~)"), "'$(rm -rf ~)'");
        assert_eq!(shell_quote("a\nb.txt"), "'a\nb.txt'");
        assert_eq!(shell_quote("a\rb.txt"), "'a\rb.txt'");
        assert_eq!(shell_quote("danger!bang.txt"), "'danger!bang.txt'");
        assert_eq!(shell_quote("-rf"), "'-rf'");
        // Adjacent quotes compose: each embedded quote closes, escapes,
        // and reopens.
        assert_eq!(shell_quote("''"), r"''\'''\'''");
    }
}
