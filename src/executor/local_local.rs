//! Local to local transfer executor
//!
//! Uses std::fs::copy for efficient file copying on the same filesystem.

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{TransferConfig, TransferExecutor, TransferProgress, TransferResult};
use crate::source::{CollectedFiles, FileSource};

pub struct LocalToLocalExecutor {
    source: Arc<dyn FileSource>,
    dest: Arc<dyn FileSource>,
}

impl LocalToLocalExecutor {
    pub fn new(
        source: Arc<dyn FileSource>,
        dest: Arc<dyn FileSource>,
        _config: TransferConfig,
    ) -> Self {
        Self { source, dest }
    }
}

#[async_trait]
impl TransferExecutor for LocalToLocalExecutor {
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

        // Copy files
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

            // Create parent directory if needed
            if let Some(parent) = self.dest.parent_path(&dest_path) {
                let _ = self.dest.create_dir(&parent).await;
            }

            // Use std::fs::copy for efficiency (blocking, but fast). The
            // copy lands in a .part file and is renamed into place, so even
            // a crash mid-copy never leaves a partial wearing a final name.
            // Cancel takes effect at file boundaries (the copy itself is a
            // single blocking operation), so no .part survives a cancel
            // here - unlike the streaming executors.
            let part_path = super::part_path(&dest_path);
            let source_path = file.full_path.clone();
            let part_clone = part_path.clone();

            let result = tokio::task::spawn_blocking(move || {
                std::fs::copy(&source_path, &part_clone)
            }).await;

            match result {
                Ok(Ok(copied)) => {
                    if let Err(e) =
                        super::finalize_part(self.dest.as_ref(), &part_path, &dest_path).await
                    {
                        return TransferResult::Error {
                            message: format!("Failed to move {} into place: {}", part_path, e),
                            files_completed,
                        };
                    }
                    files_completed += 1;

                    // Send completion progress
                    let _ = progress_tx.try_send(TransferProgress::new(
                        index,
                        total_files,
                        filename,
                        copied,
                        file.size,
                    ));
                }
                Ok(Err(e)) => {
                    let _ = self.dest.delete_file(&part_path).await;
                    return TransferResult::Error {
                        message: format!("Failed to copy {}: {}", filename, e),
                        files_completed,
                    };
                }
                Err(e) => {
                    let _ = self.dest.delete_file(&part_path).await;
                    return TransferResult::Error {
                        message: format!("Task error copying {}: {}", filename, e),
                        files_completed,
                    };
                }
            }
        }

        TransferResult::Success {
            files_transferred: files_completed,
        }
    }
}
