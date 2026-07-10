//! In-flight transfer state polled by the main loop.
//!
//! The progress and result types come straight from the executor layer;
//! this module only adds the UI-facing bookkeeping around them.

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub use crate::executor::{TransferProgress, TransferResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Download,
    Upload,
}

pub struct TransferState {
    /// Where the data is going, for status text: an arrow pointing at the
    /// receiving pane as laid out on screen plus its directory name, e.g.
    /// "-> docs". Matches the context menu's Send labels.
    pub route: String,
    /// Per-file byte counts, in transfer order, for overall progress.
    file_sizes: Vec<u64>,
    pub current_progress: Option<TransferProgress>,
    cancel_token: CancellationToken,
    result_rx: mpsc::Receiver<TransferResult>,
    progress_rx: mpsc::Receiver<TransferProgress>,
}

impl TransferState {
    pub fn new(
        route: String,
        file_sizes: Vec<u64>,
        cancel_token: CancellationToken,
        result_rx: mpsc::Receiver<TransferResult>,
        progress_rx: mpsc::Receiver<TransferProgress>,
    ) -> Self {
        Self {
            route,
            file_sizes,
            current_progress: None,
            cancel_token,
            result_rx,
            progress_rx,
        }
    }

    pub fn cancel(&self) {
        self.cancel_token.cancel();
    }

    pub fn poll_progress(&mut self) -> Option<TransferProgress> {
        match self.progress_rx.try_recv() {
            Ok(progress) => {
                self.current_progress = Some(progress.clone());
                Some(progress)
            }
            Err(_) => None,
        }
    }

    pub fn poll_result(&mut self) -> Option<TransferResult> {
        self.result_rx.try_recv().ok()
    }

    pub fn total_size(&self) -> u64 {
        self.file_sizes.iter().sum()
    }

    pub fn overall_progress(&self) -> f64 {
        if let Some(ref progress) = self.current_progress {
            let completed_size: u64 = self.file_sizes.iter()
                .take(progress.file_index)
                .sum();
            let current_file_progress = progress.bytes_transferred;
            let total = self.total_size();
            if total == 0 {
                100.0
            } else {
                // Clamped: a file that grows mid-transfer pushes transferred
                // bytes past the size recorded at collection time, and
                // Gauge::percent panics above 100.
                (((completed_size + current_file_progress) as f64 / total as f64) * 100.0)
                    .min(100.0)
            }
        } else {
            0.0
        }
    }
}
