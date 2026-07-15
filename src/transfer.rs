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

/// One side of a generated rsync command.
pub struct RsyncEndpoint {
    /// `user@host:` for a remote side, empty for local.
    pub prefix: String,
    /// The `-e` ssh command a remote side needs when its connection uses
    /// a non-default port, identity files, or a ProxyJump chain.
    pub ssh_command: Option<String>,
}

/// Build the rsync command TEXT equivalent to the transfer the GUI would
/// run: the selection roots (paths relative to `source_root`) moved into
/// `dest_root`. This string is only ever copied to the clipboard for the
/// user to inspect and run themselves - ssh-files never executes rsync.
///
/// Flat mode is rsync's native behavior - each source lands under its
/// own name - and tree mode is `--relative` with the `/./` pivot at the
/// pane root. Neither mode restructures a directory's insides, exactly
/// like the app's transfers. Targets rsync >= 3.2.4, where remote args
/// are protected by default.
pub fn build_rsync_command(
    source: &RsyncEndpoint,
    dest: &RsyncEndpoint,
    source_root: &str,
    dest_root: &str,
    selections: &[String],
    preserve: bool,
    include_hidden: bool,
) -> String {
    let mut cmd: Vec<String> = vec!["rsync".into()];
    cmd.push(if preserve { "-aR".into() } else { "-a".into() });
    if !include_hidden {
        // The app's walk skips dot-entries at every depth; --exclude on a
        // bare pattern matches basenames at every depth too.
        cmd.push("--exclude".into());
        cmd.push(quote_arg(".*"));
    }
    if let Some(ssh) = source.ssh_command.as_ref().or(dest.ssh_command.as_ref()) {
        cmd.push("-e".into());
        cmd.push(quote_arg(ssh));
    }

    let root = source_root.trim_end_matches('/');
    for rel in selections {
        let path = if preserve {
            // The /./ marker tells --relative to recreate only the path
            // below it, i.e. relative to the pane root - tree semantics.
            format!("{}/./{}", root, rel)
        } else {
            format!("{}/{}", root, rel)
        };
        cmd.push(quote_arg(&format!("{}{}", source.prefix, path)));
    }

    let dest_dir = format!("{}{}/", dest.prefix, dest_root.trim_end_matches('/'));
    cmd.push(quote_arg(&dest_dir));
    cmd.join(" ")
}

/// Quote an argument for the user's shell only when it needs it, so the
/// common command stays readable. The safe set deliberately includes
/// `:@~` (remote prefixes) and glob-free path punctuation.
pub fn quote_arg(arg: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "@%+=:,./~_-".contains(c);
    if !arg.is_empty() && arg.chars().all(safe) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod rsync_tests {
    use super::*;

    fn local() -> RsyncEndpoint {
        RsyncEndpoint { prefix: String::new(), ssh_command: None }
    }
    fn remote() -> RsyncEndpoint {
        RsyncEndpoint {
            prefix: "test@media-server:".into(),
            ssh_command: Some("ssh -p 2203 -i /keys/id_ed25519".into()),
        }
    }

    #[test]
    fn flat_upload_is_plain_multisource_rsync() {
        let cmd = build_rsync_command(
            &local(),
            &remote(),
            "/work",
            "/data/incoming",
            &["video1_data/vid1.mp4".into(), "video2_data/vid2.mp4".into()],
            false,
            true,
        );
        assert_eq!(
            cmd,
            "rsync -a -e 'ssh -p 2203 -i /keys/id_ed25519' \
            /work/video1_data/vid1.mp4 /work/video2_data/vid2.mp4 \
            test@media-server:/data/incoming/"
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        );
    }

    #[test]
    fn tree_download_uses_relative_pivot() {
        let cmd = build_rsync_command(
            &remote(),
            &local(),
            "/data/fixtures",
            "/work/dest",
            &["docs/sub".into()],
            true,
            true,
        );
        assert!(cmd.starts_with("rsync -aR "), "got: {}", cmd);
        assert!(
            cmd.contains("test@media-server:/data/fixtures/./docs/sub"),
            "got: {}",
            cmd
        );
        assert!(cmd.ends_with(" /work/dest/"), "got: {}", cmd);
    }

    #[test]
    fn hostile_paths_are_quoted_and_hidden_excluded() {
        let cmd = build_rsync_command(
            &local(),
            &remote(),
            "/work",
            "/in",
            &["$(danger).txt".into()],
            false,
            false,
        );
        assert!(cmd.contains("--exclude '.*'"), "got: {}", cmd);
        assert!(cmd.contains("'/work/$(danger).txt'"), "got: {}", cmd);
        // The composite remote dest stays one readable argument.
        assert!(cmd.ends_with(" test@media-server:/in/"), "got: {}", cmd);
    }
}
