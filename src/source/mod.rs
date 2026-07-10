//! Source layer - abstracts filesystem operations
//!
//! The `FileSource` trait provides a unified interface for interacting with
//! both local and remote filesystems. Each pane has its own source, allowing
//! for any combination: local-local, local-remote, or remote-remote.

pub mod local;
pub mod mapping;
pub mod remote;

use anyhow::Result;
use async_trait::async_trait;

/// Information about a file or directory
#[derive(Debug, Clone)]
pub struct FileInfo {
    /// File or directory name
    pub name: String,
    /// Whether this is a directory
    pub is_dir: bool,
    /// Size in bytes (0 for directories)
    pub size: u64,
}

/// A file with its path information for transfer operations
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Full absolute path
    pub full_path: String,
    /// Path relative to some base (for structure-preserving transfers)
    pub relative_path: String,
    /// Size in bytes
    pub size: u64,
}

impl FileEntry {
    pub fn new(full_path: String, relative_path: String, size: u64) -> Self {
        Self { full_path, relative_path, size }
    }
}

/// Result of collecting files for transfer
#[derive(Debug, Clone, Default)]
pub struct CollectedFiles {
    /// Files to transfer
    pub files: Vec<FileEntry>,
    /// Directories to create (in order, for structure-preserving)
    pub dirs: Vec<String>,
    /// Total bytes of all files
    pub total_bytes: u64,
}

impl CollectedFiles {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_file(&mut self, entry: FileEntry) {
        self.total_bytes += entry.size;
        self.files.push(entry);
    }

    pub fn add_dir(&mut self, relative_path: String) {
        if !self.dirs.contains(&relative_path) {
            self.dirs.push(relative_path);
        }
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Handle for reading a file (async streaming)
#[async_trait]
pub trait FileReader: Send {
    /// Read bytes into buffer, returns number of bytes read (0 = EOF)
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize>;
}

/// Handle for writing a file (async streaming)
#[async_trait]
pub trait FileWriter: Send {
    /// Write all bytes from buffer
    async fn write_all(&mut self, buf: &[u8]) -> Result<()>;
    /// Flush and close the file
    async fn flush(&mut self) -> Result<()>;
}

/// Unified interface for filesystem operations
///
/// This trait abstracts over local filesystem and SFTP operations,
/// allowing the application to work with any source type uniformly.
#[async_trait]
pub trait FileSource: Send + Sync {
    /// Check if this is a remote source
    fn is_remote(&self) -> bool;

    /// Get display label (e.g., "Local" or "user@host")
    fn label(&self) -> String;

    // === Directory Operations ===

    /// List contents of a directory. Hidden entries (dotfiles) are included
    /// only when `include_hidden` is set.
    async fn list_dir(&self, path: &str, include_hidden: bool) -> Result<Vec<FileInfo>>;

    /// Create a directory (and parents if needed)
    async fn create_dir(&self, path: &str) -> Result<()>;

    // === File Metadata ===

    /// Check if a path exists
    async fn exists(&self, path: &str) -> Result<bool>;

    /// Check if path is a directory
    async fn is_dir(&self, path: &str) -> Result<bool>;

    // === File Operations ===

    /// Open a file for reading
    async fn open_read(&self, path: &str) -> Result<Box<dyn FileReader>>;

    /// Open a file for writing (creates or truncates)
    async fn open_write(&self, path: &str) -> Result<Box<dyn FileWriter>>;

    // === Raw streaming (fast path) ===

    /// Open a raw streaming writer that avoids per-chunk protocol
    /// acknowledgments, when the source supports one (e.g. an exec channel
    /// on an SSH connection). Returns Ok(None) when unsupported; callers
    /// fall back to `open_write`.
    async fn open_stream_write(&self, _path: &str) -> Result<Option<Box<dyn FileWriter>>> {
        Ok(None)
    }

    /// Streaming counterpart of `open_read`. Returns Ok(None) when
    /// unsupported. NOTE: end-of-stream is indistinguishable from a dropped
    /// connection, so callers must verify the received byte count.
    async fn open_stream_read(&self, _path: &str) -> Result<Option<Box<dyn FileReader>>> {
        Ok(None)
    }

    /// Run any lazy capability probes now (called once at startup), so
    /// `streaming_ready` can answer synchronously afterwards.
    async fn warm_capabilities(&self) {}

    /// Whether the raw streaming fast path is available on this source:
    /// Some(true/false) once probed, None when not yet known. Feeds status
    /// labels only - executors still decide per file via `open_stream_*`.
    fn streaming_ready(&self) -> Option<bool> {
        None
    }

    /// Rename a file or directory
    async fn rename(&self, from: &str, to: &str) -> Result<()>;

    /// Delete a file
    async fn delete_file(&self, path: &str) -> Result<()>;

    /// Delete a directory and all its contents
    async fn delete_dir_recursive(&self, path: &str) -> Result<()>;

    // === Enumeration for Transfers ===

    /// Enumerate the subtree rooted at `path` with neutral path components;
    /// destination layout is decided exclusively by `mapping`. The root
    /// itself is the first entry (empty `components`); the root is always
    /// included, while hidden entries are filtered during recursion when
    /// `include_hidden` is unset.
    async fn walk(&self, path: &str, include_hidden: bool) -> Result<Vec<mapping::WalkEntry>>;

    // === Path Utilities ===

    /// Join two path components with appropriate separator
    fn join_path(&self, base: &str, name: &str) -> String;

    /// Get parent directory of a path
    fn parent_path(&self, path: &str) -> Option<String>;

    /// Get filename from a path
    fn file_name(&self, path: &str) -> Option<String>;
}

// Re-export types
pub use local::LocalSource;
pub use mapping::WalkEntry;
pub use remote::RemoteSource;
