//! Local to remote transfer executor
//!
//! Streams local files to remote SFTP destination.

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{TransferConfig, TransferExecutor, TransferProgress, TransferResult};
use crate::source::{CollectedFiles, FileSource};

pub struct LocalToRemoteExecutor {
    source: Arc<dyn FileSource>,
    dest: Arc<dyn FileSource>,
    config: TransferConfig,
}

impl LocalToRemoteExecutor {
    pub fn new(
        source: Arc<dyn FileSource>,
        dest: Arc<dyn FileSource>,
        config: TransferConfig,
    ) -> Self {
        Self { source, dest, config }
    }
}

#[async_trait]
impl TransferExecutor for LocalToRemoteExecutor {
    async fn execute(
        &self,
        files: CollectedFiles,
        dest_base: &str,
        progress_tx: mpsc::Sender<TransferProgress>,
        cancel: CancellationToken,
    ) -> TransferResult {
        let total_files = files.files.len();
        let mut files_completed = 0;

        // Validate and create the destination directory structure up front,
        // so the streaming fast path never hits a missing parent.
        if let Err(result) =
            super::prepare_destination_dirs(self.dest.as_ref(), dest_base, &files.dirs, &cancel)
                .await
        {
            return result;
        }

        // Transfer files
        for (index, file) in files.files.into_iter().enumerate() {
            if cancel.is_cancelled() {
                return TransferResult::Cancelled { files_completed };
            }

            let filename = self.source
                .file_name(&file.full_path)
                .unwrap_or_else(|| file.relative_path.clone());

            let dest_path = self.dest.join_path(dest_base, &file.relative_path);
            // Bytes land in a .part file and are moved into place on
            // completion, so a truncated arrival never wears a final name.
            let part_path = super::part_path(&dest_path);

            // Send initial progress
            let _ = progress_tx.try_send(TransferProgress::new(
                index,
                total_files,
                filename.clone(),
                0,
                file.size,
            ));

            // Open source and destination
            let mut reader = match self.source.open_read(&file.full_path).await {
                Ok(r) => r,
                Err(e) => {
                    return TransferResult::Error {
                        message: format!("Failed to open {}: {}", filename, e),
                        files_completed,
                    };
                }
            };

            // Prefer the raw streaming path (no per-chunk acknowledgments);
            // fall back to SFTP when the server does not allow exec.
            let mut writer = match self.dest.open_stream_write(&part_path).await {
                Ok(Some(w)) => w,
                _ => match self.dest.open_write(&part_path).await {
                    Ok(w) => w,
                    Err(e) => {
                        return TransferResult::Error {
                            message: format!("Failed to create {}: {}", dest_path, e),
                            files_completed,
                        };
                    }
                },
            };

            // Stream data
            let mut buffer = vec![0u8; self.config.buffer_size];
            let mut bytes_transferred = 0u64;
            let mut last_progress_update = 0u64;

            loop {
                if cancel.is_cancelled() {
                    // Stop immediately; the partial .part file is
                    // deliberately left in place as visible incompleteness
                    // (re-running the transfer overwrites it).
                    return TransferResult::Cancelled { files_completed };
                }

                let n = match reader.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => {
                        // Errored files are untrustworthy: clean up the
                        // partial (unlike cancel, which keeps it).
                        let _ = self.dest.delete_file(&part_path).await;
                        return TransferResult::Error {
                            message: format!("Read error on {}: {}", filename, e),
                            files_completed,
                        };
                    }
                };

                if let Err(e) = writer.write_all(&buffer[..n]).await {
                    let _ = self.dest.delete_file(&part_path).await;
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

            // Flush and close. For a streamed write this also waits for the
            // remote process's exit status - the only write confirmation
            // the streaming path has.
            if let Err(e) = writer.flush().await {
                let _ = self.dest.delete_file(&part_path).await;
                return TransferResult::Error {
                    message: format!("Flush error on {}: {}", dest_path, e),
                    files_completed,
                };
            }

            // All bytes written: move the .part into its final name.
            if let Err(e) = super::finalize_part(self.dest.as_ref(), &part_path, &dest_path).await {
                // The bytes are complete; only the rename failed. Leave the
                // .part for manual salvage rather than deleting good data.
                return TransferResult::Error {
                    message: format!("Failed to move {} into place: {}", part_path, e),
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
