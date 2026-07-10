//! Remote SFTP filesystem source implementation

use anyhow::{Context, Result};
use async_trait::async_trait;
use russh::ChannelMsg;
use russh_sftp::client::SftpSession;
use std::sync::Arc;
use tokio::sync::OnceCell;

use super::{FileInfo, FileReader, FileSource, FileWriter, WalkEntry};
use crate::ssh::exec::{shell_quote, ExecReader, ExecWriter};
use crate::ssh::ExecHandle;

/// Remote SFTP filesystem source
pub struct RemoteSource {
    sftp: Arc<SftpSession>,
    root_path: String,
    host: String,
    user: String,
    /// Exec capability of the underlying connection, for the raw streaming
    /// fast path. None when the connection cannot exec (or in contexts that
    /// deliberately measure the pure SFTP path).
    exec: Option<ExecHandle>,
    /// Lazily probed: whether the server actually allows exec with a
    /// usable `cat`.
    exec_ok: OnceCell<bool>,
}

impl RemoteSource {
    /// Create a new remote source
    pub fn new(
        sftp: Arc<SftpSession>,
        root_path: String,
        host: String,
        user: String,
        exec: Option<ExecHandle>,
    ) -> Self {
        Self {
            sftp,
            root_path,
            host,
            user,
            exec,
            exec_ok: OnceCell::new(),
        }
    }

    /// Whether the raw streaming path is usable, probed once per source.
    /// Chrooted or `ForceCommand internal-sftp` servers fail the probe and
    /// all transfers fall back to SFTP.
    async fn exec_available(&self) -> bool {
        let Some(exec) = &self.exec else { return false };
        *self
            .exec_ok
            .get_or_init(|| async { Self::probe_exec(exec).await.unwrap_or(false) })
            .await
    }

    async fn probe_exec(exec: &ExecHandle) -> Result<bool> {
        let mut channel = exec.open_exec("cat /dev/null").await?;
        let mut status = None;
        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        Ok(status == Some(0))
    }

    /// Normalize a path (remove trailing slashes, handle relative)
    fn normalize_path(&self, path: &str) -> String {
        if path.starts_with('/') {
            path.trim_end_matches('/').to_string()
        } else {
            format!("{}/{}", self.root_path.trim_end_matches('/'), path)
        }
    }

    /// SFTP listings include the "." and ".." entries; these are never real
    /// children and must always be skipped.
    fn is_dot_entry(name: &str) -> bool {
        name == "." || name == ".."
    }

    /// Check if a name should be skipped (hidden files)
    fn should_skip(name: &str, include_hidden: bool) -> bool {
        Self::is_dot_entry(name) || (!include_hidden && name.starts_with('.'))
    }
}

// === FileReader Implementation ===

struct RemoteFileReader {
    file: russh_sftp::client::fs::File,
}

#[async_trait]
impl FileReader for RemoteFileReader {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        use tokio::io::AsyncReadExt;
        let n = self.file.read(buf).await?;
        Ok(n)
    }
}

// === FileWriter Implementation ===

struct RemoteFileWriter {
    file: russh_sftp::client::fs::File,
}

#[async_trait]
impl FileWriter for RemoteFileWriter {
    async fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        self.file.write_all(buf).await?;
        Ok(())
    }

    async fn flush(&mut self) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        self.file.flush().await?;
        Ok(())
    }
}

// === FileSource Implementation ===

#[async_trait]
impl FileSource for RemoteSource {
    fn is_remote(&self) -> bool {
        true
    }

    fn label(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }

    async fn list_dir(&self, path: &str, include_hidden: bool) -> Result<Vec<FileInfo>> {
        let path = self.normalize_path(path);
        let dir = self.sftp.read_dir(&path)
            .await
            .with_context(|| format!("Failed to read directory: {}", path))?;

        let mut entries: Vec<FileInfo> = dir
            .into_iter()
            .filter_map(|entry| {
                let name = entry.file_name();
                if Self::should_skip(&name, include_hidden) {
                    return None;
                }

                let is_dir = entry.file_type().is_dir();
                let size = entry.metadata().size.unwrap_or(0);
                Some(FileInfo { name, is_dir, size })
            })
            .collect();

        // Sort: directories first, then alphabetically (case-insensitive)
        entries.sort_by(|a, b| {
            match (a.is_dir, b.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            }
        });

        Ok(entries)
    }

    async fn create_dir(&self, path: &str) -> Result<()> {
        let path = self.normalize_path(path);
        
        // Try to create parent directories first
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let mut current = String::new();
        
        for part in parts {
            current = format!("{}/{}", current, part);
            // Ignore errors (directory might already exist)
            let _ = self.sftp.create_dir(&current).await;
        }
        
        Ok(())
    }

    async fn exists(&self, path: &str) -> Result<bool> {
        let path = self.normalize_path(path);
        match self.sftp.metadata(&path).await {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    async fn is_dir(&self, path: &str) -> Result<bool> {
        let path = self.normalize_path(path);
        match self.sftp.metadata(&path).await {
            Ok(metadata) => Ok(metadata.is_dir()),
            Err(_) => Ok(false),
        }
    }

    async fn open_read(&self, path: &str) -> Result<Box<dyn FileReader>> {
        let path = self.normalize_path(path);
        let file = self.sftp.open(&path)
            .await
            .with_context(|| format!("Failed to open file: {}", path))?;
        Ok(Box::new(RemoteFileReader { file }))
    }

    async fn open_write(&self, path: &str) -> Result<Box<dyn FileWriter>> {
        let path = self.normalize_path(path);

        // Create parent directories if needed
        if let Some(parent) = self.parent_path(&path) {
            let _ = self.create_dir(&parent).await;
        }

        let file = self.sftp.create(&path)
            .await
            .with_context(|| format!("Failed to create file: {}", path))?;
        Ok(Box::new(RemoteFileWriter { file }))
    }

    async fn open_stream_write(&self, path: &str) -> Result<Option<Box<dyn FileWriter>>> {
        if !self.exec_available().await {
            return Ok(None);
        }
        let exec = self.exec.as_ref().expect("exec_available implies handle");
        let path = self.normalize_path(path);
        let channel = exec
            .open_exec(&format!("cat > {}", shell_quote(&path)))
            .await?;
        Ok(Some(Box::new(ExecWriter::new(channel))))
    }

    async fn open_stream_read(&self, path: &str) -> Result<Option<Box<dyn FileReader>>> {
        if !self.exec_available().await {
            return Ok(None);
        }
        let exec = self.exec.as_ref().expect("exec_available implies handle");
        let path = self.normalize_path(path);
        let channel = exec
            .open_exec(&format!("cat {}", shell_quote(&path)))
            .await?;
        Ok(Some(Box::new(ExecReader::new(channel))))
    }

    async fn warm_capabilities(&self) {
        let _ = self.exec_available().await;
    }

    fn streaming_ready(&self) -> Option<bool> {
        if self.exec.is_none() {
            // No exec handle at all: the probe would never run, but the
            // answer is already known.
            return Some(false);
        }
        self.exec_ok.get().copied()
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        let from_path = self.normalize_path(from);
        let to_path = self.normalize_path(to);
        self.sftp.rename(&from_path, &to_path)
            .await
            .with_context(|| format!("Failed to rename {} to {}", from_path, to_path))?;
        Ok(())
    }

    async fn delete_file(&self, path: &str) -> Result<()> {
        let path = self.normalize_path(path);
        self.sftp.remove_file(&path)
            .await
            .with_context(|| format!("Failed to delete file: {}", path))?;
        Ok(())
    }

    async fn delete_dir_recursive(&self, path: &str) -> Result<()> {
        let path = self.normalize_path(path);

        // List contents and delete recursively. Hidden files must be deleted
        // too, or removing the then-non-empty directory fails.
        let entries = self.sftp.read_dir(&path).await?;

        for entry in entries {
            let name = entry.file_name();
            if Self::is_dot_entry(&name) {
                continue;
            }
            
            let child_path = format!("{}/{}", path.trim_end_matches('/'), name);
            
            if entry.file_type().is_dir() {
                // Recursively delete subdirectory
                Box::pin(self.delete_dir_recursive(&child_path)).await?;
            } else {
                self.sftp.remove_file(&child_path).await?;
            }
        }
        
        // Now delete the empty directory
        self.sftp.remove_dir(&path).await?;
        
        Ok(())
    }

    async fn walk(&self, path: &str, include_hidden: bool) -> Result<Vec<WalkEntry>> {
        let root = self.normalize_path(path);
        let metadata = self
            .sftp
            .metadata(&root)
            .await
            .with_context(|| format!("Failed to stat {}", root))?;

        let is_dir = metadata.is_dir();
        let mut entries = vec![WalkEntry {
            full_path: root.clone(),
            components: Vec::new(),
            is_dir,
            size: if is_dir { 0 } else { metadata.size.unwrap_or(0) },
        }];

        if is_dir {
            self.walk_inner(&root, &[], include_hidden, &mut entries).await?;
        }

        Ok(entries)
    }

    fn join_path(&self, base: &str, name: &str) -> String {
        format!("{}/{}", base.trim_end_matches('/'), name)
    }

    fn parent_path(&self, path: &str) -> Option<String> {
        let path = path.trim_end_matches('/');
        if let Some(pos) = path.rfind('/') {
            if pos == 0 {
                Some("/".to_string())
            } else {
                Some(path[..pos].to_string())
            }
        } else {
            None
        }
    }

    fn file_name(&self, path: &str) -> Option<String> {
        let path = path.trim_end_matches('/');
        path.rsplit('/').next().map(|s| s.to_string())
    }
}

impl RemoteSource {
    /// Recursive walk below the root; components accumulate per level.
    async fn walk_inner(
        &self,
        dir: &str,
        prefix: &[String],
        include_hidden: bool,
        out: &mut Vec<WalkEntry>,
    ) -> Result<()> {
        let entries = self.sftp.read_dir(dir).await?;

        for entry in entries {
            let name = entry.file_name();
            if Self::should_skip(&name, include_hidden) {
                continue;
            }

            let child_path = format!("{}/{}", dir.trim_end_matches('/'), name);
            let mut components = prefix.to_vec();
            components.push(name);

            if entry.file_type().is_dir() {
                out.push(WalkEntry {
                    full_path: child_path.clone(),
                    components: components.clone(),
                    is_dir: true,
                    size: 0,
                });
                Box::pin(self.walk_inner(&child_path, &components, include_hidden, out)).await?;
            } else {
                out.push(WalkEntry {
                    full_path: child_path,
                    components,
                    is_dir: false,
                    size: entry.metadata().size.unwrap_or(0),
                });
            }
        }

        Ok(())
    }
}
