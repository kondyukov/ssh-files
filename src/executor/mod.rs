//! Transfer executor layer - moves bytes between sources
//!
//! Each executor is optimized for a specific source combination:
//! - LocalToLocal: Uses std::fs::copy for efficiency
//! - LocalToRemote: Streams local files to SFTP
//! - RemoteToLocal: Streams SFTP files to local disk
//! - RemoteToRemote: Streams through client memory (SFTP → buffer → SFTP)

pub mod local_local;
pub mod local_remote;
pub mod remote_local;
pub mod remote_remote;

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::source::{FileSource, CollectedFiles};

/// Progress update during transfer
#[derive(Debug, Clone)]
pub struct TransferProgress {
    /// Index of current file (0-based)
    pub file_index: usize,
    /// Total number of files
    pub total_files: usize,
    /// Current filename
    pub filename: String,
    /// Bytes transferred for current file
    pub bytes_transferred: u64,
    /// Total bytes for current file
    pub bytes_total: u64,
}

impl TransferProgress {
    pub fn new(
        file_index: usize,
        total_files: usize,
        filename: String,
        bytes_transferred: u64,
        bytes_total: u64,
    ) -> Self {
        Self {
            file_index,
            total_files,
            filename,
            bytes_transferred,
            bytes_total,
        }
    }

    /// Percentage complete for the current file
    pub fn percent(&self) -> f64 {
        if self.bytes_total == 0 {
            100.0
        } else {
            (self.bytes_transferred as f64 / self.bytes_total as f64) * 100.0
        }
    }
}

/// Result of a transfer operation
#[derive(Debug, Clone)]
pub enum TransferResult {
    /// Transfer completed successfully
    Success {
        files_transferred: usize,
    },
    /// Transfer was cancelled
    Cancelled {
        files_completed: usize,
    },
    /// Transfer failed
    Error {
        message: String,
        files_completed: usize,
    },
}

/// Configuration for a transfer operation
#[derive(Debug, Clone)]
pub struct TransferConfig {
    /// Buffer size for streaming (default 1MB)
    pub buffer_size: usize,
    /// Progress update interval in bytes (default 1MB)
    pub progress_interval: u64,
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            buffer_size: 1024 * 1024,      // 1MB
            progress_interval: 1024 * 1024, // 1MB
        }
    }
}

/// Validate and prepare the destination directory structure before any
/// bytes move, so the fast streaming path never fails mid-transfer on a
/// missing parent.
///
/// For each needed directory (parents-first - the mapping module
/// guarantees that order): accept it if it already exists as a directory,
/// abort with a clear error if the path exists but is not a directory,
/// otherwise create it and verify the creation actually took effect
/// (remote mkdir is best-effort and can fail silently, e.g. on
/// permissions).
///
/// On failure returns the `TransferResult` the executor should report.
pub(crate) async fn prepare_destination_dirs(
    dest: &dyn FileSource,
    dest_base: &str,
    dirs: &[String],
    cancel: &CancellationToken,
) -> Result<(), TransferResult> {
    // The destination root itself must exist; everything else is created
    // relative to it.
    if !dest.is_dir(dest_base).await.unwrap_or(false) {
        return Err(TransferResult::Error {
            message: format!("Destination directory {} does not exist", dest_base),
            files_completed: 0,
        });
    }

    for dir in dirs {
        if cancel.is_cancelled() {
            return Err(TransferResult::Cancelled { files_completed: 0 });
        }

        let path = dest.join_path(dest_base, dir);

        match dest.exists(&path).await {
            Ok(true) => match dest.is_dir(&path).await {
                Ok(true) => continue,
                Ok(false) => {
                    return Err(TransferResult::Error {
                        message: format!(
                            "Destination path {} exists but is not a directory",
                            path
                        ),
                        files_completed: 0,
                    });
                }
                Err(e) => {
                    return Err(TransferResult::Error {
                        message: format!("Failed to inspect {}: {}", path, e),
                        files_completed: 0,
                    });
                }
            },
            Ok(false) => {
                if let Err(e) = dest.create_dir(&path).await {
                    return Err(TransferResult::Error {
                        message: format!("Failed to create directory {}: {}", path, e),
                        files_completed: 0,
                    });
                }
                if !dest.is_dir(&path).await.unwrap_or(false) {
                    return Err(TransferResult::Error {
                        message: format!("Could not create directory {}", path),
                        files_completed: 0,
                    });
                }
            }
            Err(e) => {
                return Err(TransferResult::Error {
                    message: format!("Failed to inspect {}: {}", path, e),
                    files_completed: 0,
                });
            }
        }
    }

    Ok(())
}

/// Trait for executing transfers between sources
#[async_trait]
pub trait TransferExecutor: Send + Sync {
    /// Execute the transfer
    async fn execute(
        &self,
        files: CollectedFiles,
        dest_base: &str,
        progress_tx: mpsc::Sender<TransferProgress>,
        cancel: CancellationToken,
    ) -> TransferResult;
}

/// Create appropriate executor for source combination
pub fn create_executor(
    source: Arc<dyn FileSource>,
    dest: Arc<dyn FileSource>,
    config: TransferConfig,
) -> Box<dyn TransferExecutor> {
    match (source.is_remote(), dest.is_remote()) {
        (false, false) => Box::new(local_local::LocalToLocalExecutor::new(source, dest, config)),
        (false, true) => Box::new(local_remote::LocalToRemoteExecutor::new(source, dest, config)),
        (true, false) => Box::new(remote_local::RemoteToLocalExecutor::new(source, dest, config)),
        (true, true) => Box::new(remote_remote::RemoteToRemoteExecutor::new(source, dest, config)),
    }
}

/// End-to-end pipeline tests over the local filesystem: walk a selected
/// subtree, map destination paths, prepare directories, and move real
/// bytes - the same layering `App::start_transfer_from_pane` drives, with
/// `LocalSource` standing in for both endpoints. The remote executors
/// share this structure and are exercised against live hosts.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{mapping, LocalSource};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    /// Source layout used by every test:
    ///   docs/report.txt      "report body"
    ///   docs/big.bin         3 MiB patterned
    ///   docs/sub/nested.txt  "nested"
    ///   docs/sub/empty_dir/
    ///   docs/.hidden.txt     (must not transfer: hidden files excluded)
    fn build_source(root: &Path) -> Vec<u8> {
        let docs = root.join("docs");
        fs::create_dir_all(docs.join("sub").join("empty_dir")).unwrap();
        fs::write(docs.join("report.txt"), b"report body").unwrap();
        fs::write(docs.join("sub").join("nested.txt"), b"nested").unwrap();
        fs::write(docs.join(".hidden.txt"), b"secret").unwrap();
        let big: Vec<u8> = (0..3 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
        fs::write(docs.join("big.bin"), &big).unwrap();
        big
    }

    /// Drive the full pipeline as the app does: walk the selected subtree
    /// ("docs", hidden files excluded), map onto destination paths, then
    /// execute through the executor picked by `create_executor`. `anchor`
    /// is the tree-relative path the selection carries (the walked data is
    /// the same either way; mapping is purely lexical).
    async fn run_pipeline(
        src_root: &Path,
        dest_base: &str,
        anchor: &str,
        preserve: bool,
        cancel: CancellationToken,
    ) -> (TransferResult, Vec<TransferProgress>) {
        let source: Arc<dyn FileSource> = Arc::new(LocalSource::new(src_root.to_path_buf()));
        let dest: Arc<dyn FileSource> = Arc::new(LocalSource::new(Path::new(dest_base).to_path_buf()));

        let entries = source
            .walk(&src_root.join("docs").to_string_lossy(), false)
            .await
            .unwrap();
        let mut collected = CollectedFiles::new();
        mapping::map_to_destination(preserve, anchor, entries, &mut collected);

        let exec = create_executor(source, dest, TransferConfig::default());
        let (progress_tx, mut progress_rx) = mpsc::channel(100);
        let result = exec.execute(collected, dest_base, progress_tx, cancel).await;

        let mut progress = Vec::new();
        while let Ok(p) = progress_rx.try_recv() {
            progress.push(p);
        }
        (result, progress)
    }

    #[tokio::test]
    async fn preserve_transfer_moves_bytes_and_structure() {
        let src = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        let big = build_source(src.path());

        let (result, progress) = run_pipeline(
            src.path(),
            &dest.path().to_string_lossy(),
            "docs",
            true,
            CancellationToken::new(),
        )
        .await;

        assert!(
            matches!(result, TransferResult::Success { files_transferred: 3 }),
            "got: {:?}",
            result
        );

        let docs = dest.path().join("docs");
        assert_eq!(fs::read(docs.join("report.txt")).unwrap(), b"report body");
        assert_eq!(fs::read(docs.join("sub").join("nested.txt")).unwrap(), b"nested");
        assert_eq!(fs::read(docs.join("big.bin")).unwrap(), big);
        // Empty directories are created even though no file forces them.
        assert!(docs.join("sub").join("empty_dir").is_dir());
        // Hidden files were excluded at walk time, not merely skipped late.
        assert!(!docs.join(".hidden.txt").exists());

        assert!(!progress.is_empty(), "expected progress events");
        assert!(progress.iter().any(|p| p.total_files == 3));
    }

    #[tokio::test]
    async fn flat_transfer_strips_ancestry_keeps_contents() {
        let src = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        build_source(src.path());

        // Selection sits at proj/docs in the tree; flat drops `proj` but
        // sends `docs` itself as one intact unit.
        let (result, _) = run_pipeline(
            src.path(),
            &dest.path().to_string_lossy(),
            "proj/docs",
            false,
            CancellationToken::new(),
        )
        .await;

        assert!(matches!(result, TransferResult::Success { files_transferred: 3 }));
        let docs = dest.path().join("docs");
        assert!(docs.join("report.txt").is_file());
        assert!(docs.join("sub").join("nested.txt").is_file());
        assert!(docs.join("big.bin").is_file());
        assert!(docs.join("sub").join("empty_dir").is_dir());
        assert!(!dest.path().join("proj").exists(), "ancestry must be stripped");
    }

    #[tokio::test]
    async fn transfer_overwrites_existing_destination_files() {
        let src = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        build_source(src.path());

        // Pre-existing file with different content: the app has already
        // asked the user (overwrite confirm); the executor must replace it.
        fs::create_dir_all(dest.path().join("docs")).unwrap();
        fs::write(dest.path().join("docs").join("report.txt"), b"stale junk").unwrap();

        let (result, _) = run_pipeline(
            src.path(),
            &dest.path().to_string_lossy(),
            "docs",
            true,
            CancellationToken::new(),
        )
        .await;

        assert!(matches!(result, TransferResult::Success { .. }));
        assert_eq!(
            fs::read(dest.path().join("docs").join("report.txt")).unwrap(),
            b"report body"
        );
    }

    #[tokio::test]
    async fn cancelled_before_start_transfers_nothing() {
        let src = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        build_source(src.path());

        let cancel = CancellationToken::new();
        cancel.cancel();
        let (result, _) =
            run_pipeline(src.path(), &dest.path().to_string_lossy(), "docs", true, cancel).await;

        assert!(
            matches!(result, TransferResult::Cancelled { files_completed: 0 }),
            "got: {:?}",
            result
        );
        assert!(!dest.path().join("docs").join("report.txt").exists());
    }

    #[tokio::test]
    async fn missing_destination_root_fails_before_moving_bytes() {
        let src = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();
        build_source(src.path());

        let missing = dest.path().join("nope");
        let (result, _) = run_pipeline(
            src.path(),
            &missing.to_string_lossy(),
            "docs",
            true,
            CancellationToken::new(),
        )
        .await;

        match result {
            TransferResult::Error { message, files_completed } => {
                assert!(message.contains("does not exist"), "got: {}", message);
                assert_eq!(files_completed, 0);
            }
            other => panic!("expected Error, got: {:?}", other),
        }
    }
}
