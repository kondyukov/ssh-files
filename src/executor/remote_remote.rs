//! Remote to remote transfer executor
//!
//! Streams files between two remote servers through the client.
//! Data flows: Source → Client memory → Destination (the servers never
//! talk to each other), but no intermediate local storage is used.
//!
//! Each leg independently prefers the raw exec streaming path and falls
//! back to SFTP where the server disallows exec, so a transfer may run
//! streaming/streaming, streaming/sftp, or any other combination.

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{TransferConfig, TransferExecutor, TransferProgress, TransferResult};
use crate::source::{CollectedFiles, FileSource};

pub struct RemoteToRemoteExecutor {
    source: Arc<dyn FileSource>,
    dest: Arc<dyn FileSource>,
    config: TransferConfig,
}

impl RemoteToRemoteExecutor {
    pub fn new(
        source: Arc<dyn FileSource>,
        dest: Arc<dyn FileSource>,
        config: TransferConfig,
    ) -> Self {
        Self { source, dest, config }
    }
}

#[async_trait]
impl TransferExecutor for RemoteToRemoteExecutor {
    async fn execute(
        &self,
        files: CollectedFiles,
        dest_base: &str,
        progress_tx: mpsc::Sender<TransferProgress>,
        cancel: CancellationToken,
    ) -> TransferResult {
        let total_files = files.files.len();
        let mut files_completed = 0;

        // Validate and create the destination directory structure up front.
        if let Err(result) =
            super::prepare_destination_dirs(self.dest.as_ref(), dest_base, &files.dirs, &cancel)
                .await
        {
            return result;
        }

        // Transfer files (stream through client memory)
        for (index, file) in files.files.into_iter().enumerate() {
            if cancel.is_cancelled() {
                return TransferResult::Cancelled { files_completed };
            }

            let filename = self.source
                .file_name(&file.full_path)
                .unwrap_or_else(|| file.relative_path.clone());

            let dest_path = self.dest.join_path(dest_base, &file.relative_path);

            // Send initial progress
            let _ = progress_tx.try_send(TransferProgress::new(
                index,
                total_files,
                filename.clone(),
                0,
                file.size,
            ));

            // Prefer the raw streaming path on each leg independently (no
            // per-chunk acknowledgments); fall back to SFTP where a server
            // does not allow exec. On a streamed read, end-of-stream and a
            // dropped connection look identical, so the byte count is
            // verified after the copy loop.
            let mut streamed_read = true;
            let mut reader = match self.source.open_stream_read(&file.full_path).await {
                Ok(Some(r)) => r,
                _ => {
                    streamed_read = false;
                    match self.source.open_read(&file.full_path).await {
                        Ok(r) => r,
                        Err(e) => {
                            return TransferResult::Error {
                                message: format!("Failed to open source {}: {}", filename, e),
                                files_completed,
                            };
                        }
                    }
                }
            };

            let mut writer = match self.dest.open_stream_write(&dest_path).await {
                Ok(Some(w)) => w,
                _ => match self.dest.open_write(&dest_path).await {
                    Ok(w) => w,
                    Err(e) => {
                        return TransferResult::Error {
                            message: format!("Failed to create dest {}: {}", dest_path, e),
                            files_completed,
                        };
                    }
                },
            };

            // Stream data through client
            // Using a moderate buffer size since we're bottlenecked by network anyway
            let mut buffer = vec![0u8; self.config.buffer_size];
            let mut bytes_transferred = 0u64;
            let mut last_progress_update = 0u64;

            loop {
                if cancel.is_cancelled() {
                    // Stop immediately; the partially written file is
                    // deliberately left in place (re-running overwrites it).
                    return TransferResult::Cancelled { files_completed };
                }

                // Read from source SFTP
                let n = match reader.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => {
                        let _ = self.dest.delete_file(&dest_path).await;
                        return TransferResult::Error {
                            message: format!("Read error on {}: {}", filename, e),
                            files_completed,
                        };
                    }
                };

                // Write to destination SFTP
                if let Err(e) = writer.write_all(&buffer[..n]).await {
                    let _ = self.dest.delete_file(&dest_path).await;
                    return TransferResult::Error {
                        message: format!("Write error on {}: {}", dest_path, e),
                        files_completed,
                    };
                }

                bytes_transferred += n as u64;

                // Throttled progress updates
                if bytes_transferred - last_progress_update >= self.config.progress_interval
                    || bytes_transferred == file.size
                {
                    last_progress_update = bytes_transferred;
                    let _ = progress_tx.try_send(TransferProgress::new(
                        index,
                        total_files,
                        filename.clone(),
                        bytes_transferred,
                        file.size,
                    ));
                }
            }

            // A streamed read that ends early means the connection died or
            // the file changed size; never accept a truncated file.
            if streamed_read && bytes_transferred != file.size {
                let _ = self.dest.delete_file(&dest_path).await;
                return TransferResult::Error {
                    message: format!(
                        "Incomplete stream for {}: got {} of {} bytes",
                        filename, bytes_transferred, file.size
                    ),
                    files_completed,
                };
            }

            // Flush destination. For a streamed write this also waits for
            // the remote process's exit status - the only write confirmation
            // the streaming path has.
            if let Err(e) = writer.flush().await {
                return TransferResult::Error {
                    message: format!("Flush error on {}: {}", dest_path, e),
                    files_completed,
                };
            }

            files_completed += 1;
        }

        TransferResult::Success {
            files_transferred: files_completed,
        }
    }
}
