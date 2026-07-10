//! Local filesystem source implementation

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{FileInfo, FileReader, FileSource, FileWriter, WalkEntry};

/// Local filesystem source
pub struct LocalSource {
    root_path: PathBuf,
}

impl LocalSource {
    /// Create a new local source rooted at the given path
    pub fn new(root_path: PathBuf) -> Self {
        Self { root_path }
    }

    /// Convert string path to PathBuf, handling relative paths
    fn to_path(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root_path.join(p)
        }
    }

    /// Check if a name should be skipped (hidden files)
    fn should_skip(name: &str, include_hidden: bool) -> bool {
        !include_hidden && name.starts_with('.')
    }
}

// === FileReader Implementation ===

struct LocalFileReader {
    file: File,
}

#[async_trait]
impl FileReader for LocalFileReader {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let n = self.file.read(buf).await?;
        Ok(n)
    }
}

// === FileWriter Implementation ===

struct LocalFileWriter {
    file: File,
}

#[async_trait]
impl FileWriter for LocalFileWriter {
    async fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        self.file.write_all(buf).await?;
        Ok(())
    }

    async fn flush(&mut self) -> Result<()> {
        self.file.flush().await?;
        Ok(())
    }
}

// === FileSource Implementation ===

#[async_trait]
impl FileSource for LocalSource {
    fn is_remote(&self) -> bool {
        false
    }

    fn label(&self) -> String {
        "Local".to_string()
    }

    async fn list_dir(&self, path: &str, include_hidden: bool) -> Result<Vec<FileInfo>> {
        let path = self.to_path(path);
        let mut entries = Vec::new();

        let mut dir = tokio::fs::read_dir(&path)
            .await
            .with_context(|| format!("Failed to read directory: {}", path.display()))?;

        while let Some(entry) = dir.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden files
            if Self::should_skip(&name, include_hidden) {
                continue;
            }

            let metadata = entry.metadata().await?;
            entries.push(FileInfo {
                name,
                is_dir: metadata.is_dir(),
                size: metadata.len(),
            });
        }

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
        let path = self.to_path(path);
        tokio::fs::create_dir_all(&path)
            .await
            .with_context(|| format!("Failed to create directory: {}", path.display()))?;
        Ok(())
    }

    async fn exists(&self, path: &str) -> Result<bool> {
        let path = self.to_path(path);
        Ok(path.exists())
    }

    async fn is_dir(&self, path: &str) -> Result<bool> {
        let path = self.to_path(path);
        Ok(path.is_dir())
    }

    async fn open_read(&self, path: &str) -> Result<Box<dyn FileReader>> {
        let path = self.to_path(path);
        let file = File::open(&path)
            .await
            .with_context(|| format!("Failed to open file: {}", path.display()))?;
        Ok(Box::new(LocalFileReader { file }))
    }

    async fn open_write(&self, path: &str) -> Result<Box<dyn FileWriter>> {
        let path = self.to_path(path);

        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }

        let file = File::create(&path)
            .await
            .with_context(|| format!("Failed to create file: {}", path.display()))?;
        Ok(Box::new(LocalFileWriter { file }))
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        let from_path = self.to_path(from);
        let to_path = self.to_path(to);
        tokio::fs::rename(&from_path, &to_path)
            .await
            .with_context(|| format!("Failed to rename {} to {}", from_path.display(), to_path.display()))?;
        Ok(())
    }

    async fn delete_file(&self, path: &str) -> Result<()> {
        let path = self.to_path(path);
        tokio::fs::remove_file(&path)
            .await
            .with_context(|| format!("Failed to delete file: {}", path.display()))?;
        Ok(())
    }

    async fn delete_dir_recursive(&self, path: &str) -> Result<()> {
        let path = self.to_path(path);
        tokio::fs::remove_dir_all(&path)
            .await
            .with_context(|| format!("Failed to delete directory: {}", path.display()))?;
        Ok(())
    }

    async fn walk(&self, path: &str, include_hidden: bool) -> Result<Vec<WalkEntry>> {
        let root = self.to_path(path);
        let metadata = tokio::fs::metadata(&root)
            .await
            .with_context(|| format!("Failed to stat {}", root.display()))?;

        let is_dir = metadata.is_dir();
        let mut entries = vec![WalkEntry {
            full_path: root.to_string_lossy().to_string(),
            components: Vec::new(),
            is_dir,
            size: if is_dir { 0 } else { metadata.len() },
        }];

        if is_dir {
            self.walk_inner(&root, &[], include_hidden, &mut entries).await?;
        }

        Ok(entries)
    }

    fn join_path(&self, base: &str, name: &str) -> String {
        let path = Path::new(base).join(name);
        path.to_string_lossy().to_string()
    }

    fn parent_path(&self, path: &str) -> Option<String> {
        Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
    }

    fn file_name(&self, path: &str) -> Option<String> {
        Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
    }
}

impl LocalSource {
    /// Recursive walk below the root; components accumulate per level.
    async fn walk_inner(
        &self,
        dir: &Path,
        prefix: &[String],
        include_hidden: bool,
        out: &mut Vec<WalkEntry>,
    ) -> Result<()> {
        let mut entries = tokio::fs::read_dir(dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            if Self::should_skip(&name, include_hidden) {
                continue;
            }

            let metadata = entry.metadata().await?;
            let mut components = prefix.to_vec();
            components.push(name);
            let entry_path = entry.path();

            if metadata.is_dir() {
                out.push(WalkEntry {
                    full_path: entry_path.to_string_lossy().to_string(),
                    components: components.clone(),
                    is_dir: true,
                    size: 0,
                });
                Box::pin(self.walk_inner(&entry_path, &components, include_hidden, out)).await?;
            } else {
                out.push(WalkEntry {
                    full_path: entry_path.to_string_lossy().to_string(),
                    components,
                    is_dir: false,
                    size: metadata.len(),
                });
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_list_dir() {
        let temp = TempDir::new().unwrap();
        let source = LocalSource::new(temp.path().to_path_buf());

        // Create test files
        tokio::fs::write(temp.path().join("file1.txt"), "hello").await.unwrap();
        tokio::fs::write(temp.path().join("file2.txt"), "world").await.unwrap();
        tokio::fs::create_dir(temp.path().join("subdir")).await.unwrap();
        tokio::fs::write(temp.path().join(".hidden"), "hidden").await.unwrap();

        let entries = source.list_dir(temp.path().to_str().unwrap(), false).await.unwrap();

        // Should have 3 entries (hidden excluded)
        assert_eq!(entries.len(), 3);

        // Directories should come first
        assert!(entries[0].is_dir);
        assert_eq!(entries[0].name, "subdir");

        // With hidden entries included
        let entries = source.list_dir(temp.path().to_str().unwrap(), true).await.unwrap();
        assert_eq!(entries.len(), 4);
        assert!(entries.iter().any(|e| e.name == ".hidden"));
    }

    #[tokio::test]
    async fn test_join_path() {
        let source = LocalSource::new(PathBuf::from("/tmp"));

        let joined = source.join_path("/home/user", "docs");
        assert!(joined.contains("docs"));
    }
}
