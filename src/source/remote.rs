//! Remote SFTP filesystem source implementation

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use russh::ChannelMsg;
use russh_sftp::client::SftpSession;
use std::sync::Arc;
use tokio::sync::OnceCell;

use super::mapping::{is_safe_component, SymlinkPolicy};
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
    /// Connection facts for rsync command generation (text only).
    cli_opts: RemoteCliOpts,
    /// How symbolic links reported by the server are treated during a walk.
    /// v0.3 is always `Skip`; the field is the seam a future follow-mode
    /// hangs off (see [`SymlinkPolicy`]).
    symlink_policy: SymlinkPolicy,
}

/// The command-line facts needed to reconstruct this connection for an
/// equivalent ssh/rsync invocation. Text generation only - nothing here
/// is ever executed.
#[derive(Clone, Default)]
pub struct RemoteCliOpts {
    pub port: u16,
    pub identity_files: Vec<String>,
    /// Preformatted `-J` chain (`user@host[:port],...`), if any.
    pub jump_chain: Option<String>,
}

impl RemoteSource {
    /// Create a new remote source
    pub fn new(
        sftp: Arc<SftpSession>,
        root_path: String,
        host: String,
        user: String,
        exec: Option<ExecHandle>,
        cli_opts: RemoteCliOpts,
    ) -> Self {
        Self {
            sftp,
            root_path,
            host,
            user,
            exec,
            exec_ok: OnceCell::new(),
            cli_opts,
            symlink_policy: SymlinkPolicy::default(),
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
                // A name that could not be a single safe component (embedded
                // separator, `/etc/...`, NUL) is a traversal probe. Never
                // show it: hidden here, it can never be selected and so can
                // never seed a transfer anchor. The transfer walk aborts on
                // the same names as a second line of defense.
                if !is_safe_component(&name) {
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

    fn rsync_prefix(&self) -> String {
        format!("{}@{}:", self.user, self.host)
    }

    fn rsync_ssh_command(&self) -> Option<String> {
        let o = &self.cli_opts;
        let mut parts = vec!["ssh".to_string()];
        if o.port != 0 && o.port != 22 {
            parts.push(format!("-p {}", o.port));
        }
        for id in &o.identity_files {
            parts.push(format!("-i {}", crate::transfer::quote_arg(id)));
        }
        if let Some(chain) = &o.jump_chain {
            parts.push(format!("-J {}", chain));
        }
        if parts.len() == 1 {
            // Plain `ssh host` needs no -e at all.
            return None;
        }
        Some(parts.join(" "))
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
            // The walk root is a node the user explicitly navigated to and
            // selected; its own metadata is followed as-is.
            is_symlink: false,
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

/// A hostile server can fabricate a bottomless tree — every `readdir`
/// returning yet another subdirectory — or an endless fan-out of entries,
/// walking the client into unbounded recursion and memory. Real trees stay
/// far below these limits (PATH_MAX alone bounds honest nesting well under
/// 256 components; the Linux kernel tree is ~80k files), so hitting either
/// is treated as an attack and aborts the transfer, never silently
/// truncates it.
const MAX_WALK_DEPTH: usize = 256;
const MAX_WALK_ENTRIES: usize = 1_000_000;

impl RemoteSource {
    /// Recursive walk below the root; components accumulate per level.
    async fn walk_inner(
        &self,
        dir: &str,
        prefix: &[String],
        include_hidden: bool,
        out: &mut Vec<WalkEntry>,
    ) -> Result<()> {
        if prefix.len() >= MAX_WALK_DEPTH {
            bail!(
                "refusing transfer: directory nesting exceeds {} levels at {} \
                 (possible malicious server)",
                MAX_WALK_DEPTH,
                dir
            );
        }

        let entries = self.sftp.read_dir(dir).await?;

        for entry in entries {
            let name = entry.file_name();
            // `.` and `..` are legitimate entries in every directory; skip
            // them quietly. Any *other* name that is not a single safe
            // component — one carrying a path separator, an absolute prefix,
            // or a NUL — is a path-traversal attempt by the server. Refuse
            // the whole transfer rather than compose a destination path that
            // could escape the root (scp CVE-2019-6111 class).
            if Self::is_dot_entry(&name) {
                continue;
            }
            if !is_safe_component(&name) {
                bail!(
                    "refusing transfer: server returned an unsafe directory entry {:?} \
                     under {} (possible path-traversal attack)",
                    name,
                    dir
                );
            }
            if !include_hidden && name.starts_with('.') {
                continue;
            }

            let child_path = format!("{}/{}", dir.trim_end_matches('/'), name);
            let mut components = prefix.to_vec();
            components.push(name);

            if out.len() >= MAX_WALK_ENTRIES {
                bail!(
                    "refusing transfer: more than {} entries in the selection \
                     (possible malicious server)",
                    MAX_WALK_ENTRIES
                );
            }

            let file_type = entry.file_type();

            // Symbolic links are neither followed nor descended into. Record
            // the link (so a future SymlinkPolicy::Follow has the data) and
            // move on; the mapper counts and drops it under the Skip policy.
            if file_type.is_symlink() {
                match self.symlink_policy {
                    SymlinkPolicy::Skip => {
                        out.push(WalkEntry {
                            full_path: child_path,
                            components,
                            is_dir: false,
                            is_symlink: true,
                            size: 0,
                        });
                        continue;
                    }
                    SymlinkPolicy::Follow => {
                        // Reserved for a future release; nothing constructs
                        // Follow yet, so this arm is unreachable in v0.3.
                        bail!("following symbolic links is not supported in this version");
                    }
                }
            }

            if file_type.is_dir() {
                out.push(WalkEntry {
                    full_path: child_path.clone(),
                    components: components.clone(),
                    is_dir: true,
                    is_symlink: false,
                    size: 0,
                });
                Box::pin(self.walk_inner(&child_path, &components, include_hidden, out)).await?;
            } else {
                out.push(WalkEntry {
                    full_path: child_path,
                    components,
                    is_dir: false,
                    is_symlink: false,
                    size: entry.metadata().size.unwrap_or(0),
                });
            }
        }

        Ok(())
    }
}

/// Tests against an in-process SFTP server that returns attacker-controlled
/// directory listings. No real filesystem can store a name like `../x` —
/// the lie happens at the protocol layer, so that is where the mock lives:
/// the real `SftpSession` client is bound to a hostile `russh_sftp` server
/// over an in-memory duplex pipe, exercising the exact wire path a
/// malicious server controls.
#[cfg(test)]
mod hostile_server_tests {
    use super::*;
    use crate::source::mapping::map_to_destination;
    use crate::source::CollectedFiles;
    use russh_sftp::protocol::{
        Attrs, File, FileAttributes, Handle, Name, Status, StatusCode,
    };
    use std::collections::{HashMap, HashSet};

    fn dir_attrs() -> FileAttributes {
        FileAttributes { permissions: Some(0o040755), ..Default::default() }
    }

    fn file_attrs(size: u64) -> FileAttributes {
        FileAttributes { size: Some(size), permissions: Some(0o100644), ..Default::default() }
    }

    fn link_attrs() -> FileAttributes {
        FileAttributes { permissions: Some(0o120777), ..Default::default() }
    }

    /// A server that answers `readdir` with whatever listing the test
    /// planted — including names no honest server would produce.
    struct HostileServer {
        /// Directory path -> the listing returned for it. `opendir` on a
        /// path not present here is refused, so a walk that tries to
        /// descend into something unexpected (e.g. a symlink) fails the
        /// test instead of silently succeeding.
        listings: HashMap<String, Vec<File>>,
        /// Open handles (handle == path) that have not yet been drained.
        fresh: HashSet<String>,
    }

    impl HostileServer {
        fn new(listings: HashMap<String, Vec<File>>) -> Self {
            Self { listings, fresh: HashSet::new() }
        }
    }

    impl russh_sftp::server::Handler for HostileServer {
        type Error = StatusCode;

        fn unimplemented(&self) -> Self::Error {
            StatusCode::OpUnsupported
        }

        async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            if self.listings.contains_key(&path) {
                Ok(Attrs { id, attrs: dir_attrs() })
            } else {
                Err(StatusCode::NoSuchFile)
            }
        }

        async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
            if !self.listings.contains_key(&path) {
                return Err(StatusCode::NoSuchFile);
            }
            self.fresh.insert(path.clone());
            Ok(Handle { id, handle: path })
        }

        async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
            if self.fresh.remove(&handle) {
                Ok(Name { id, files: self.listings[&handle].clone() })
            } else {
                Err(StatusCode::Eof)
            }
        }

        async fn close(&mut self, id: u32, _handle: String) -> Result<Status, Self::Error> {
            Ok(Status {
                id,
                status_code: StatusCode::Ok,
                error_message: String::new(),
                language_tag: "en-US".to_string(),
            })
        }
    }

    /// Bind a `RemoteSource` to a hostile server over an in-memory pipe.
    async fn hostile_source<H>(server: H) -> RemoteSource
    where
        H: russh_sftp::server::Handler + Send + 'static,
    {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        russh_sftp::server::run(server_io, server).await;
        let session = SftpSession::new(client_io).await.expect("sftp handshake");
        RemoteSource::new(
            Arc::new(session),
            "/".to_string(),
            "hostile.test".to_string(),
            "test".to_string(),
            None,
            RemoteCliOpts::default(),
        )
    }

    #[tokio::test]
    async fn traversal_name_in_readdir_aborts_walk() {
        // The scp CVE-2019-6111 shape: a listing entry whose name climbs
        // out of the destination root. The walk must refuse the transfer.
        let listings = HashMap::from([(
            "/data".to_string(),
            vec![
                File::new("innocent.txt", file_attrs(3)),
                File::new("../../escape.txt", file_attrs(9)),
            ],
        )]);
        let source = hostile_source(HostileServer::new(listings)).await;

        let err = source.walk("/data", true).await.expect_err("walk must refuse");
        let msg = format!("{err:#}");
        assert!(msg.contains("unsafe directory entry"), "unexpected error: {msg}");
    }

    #[tokio::test]
    async fn absolute_name_in_readdir_aborts_walk() {
        // `Path::join` on the local side would *replace* the destination
        // base with an absolute name — the worst escape. Must abort.
        let listings = HashMap::from([(
            "/data".to_string(),
            vec![File::new("/etc/cron.d/evil", file_attrs(9))],
        )]);
        let source = hostile_source(HostileServer::new(listings)).await;

        let err = source.walk("/data", true).await.expect_err("walk must refuse");
        assert!(format!("{err:#}").contains("unsafe directory entry"));
    }

    #[tokio::test]
    async fn symlinks_are_recorded_never_followed() {
        // `evil` links wherever the server chooses; `sub/loop` could be a
        // link cycle. Neither may be descended into (the server refuses
        // opendir on them, so descent would fail the walk) and both must
        // come back marked, so the mapper skips and counts them.
        let listings = HashMap::from([
            (
                "/data".to_string(),
                vec![
                    File::new("real.txt", file_attrs(4)),
                    File::new("evil", link_attrs()),
                    File::new("sub", dir_attrs()),
                ],
            ),
            ("/data/sub".to_string(), vec![File::new("loop", link_attrs())]),
        ]);
        let source = hostile_source(HostileServer::new(listings)).await;

        let entries = source.walk("/data", true).await.expect("walk succeeds");
        let links: Vec<&str> = entries
            .iter()
            .filter(|e| e.is_symlink)
            .map(|e| e.full_path.as_str())
            .collect();
        assert_eq!(links, vec!["/data/evil", "/data/sub/loop"]);

        let mut collected = CollectedFiles::new();
        map_to_destination(true, "data", entries, &mut collected);
        let rels: Vec<&str> =
            collected.files.iter().map(|f| f.relative_path.as_str()).collect();
        assert_eq!(rels, vec!["data/real.txt"]);
        assert_eq!(collected.skipped_symlinks, 2);
    }

    /// A server whose every directory contains exactly one more directory,
    /// forever — the fabricated-tree recursion attack.
    struct BottomlessServer {
        fresh: HashSet<String>,
    }

    impl russh_sftp::server::Handler for BottomlessServer {
        type Error = StatusCode;

        fn unimplemented(&self) -> Self::Error {
            StatusCode::OpUnsupported
        }

        async fn stat(&mut self, id: u32, _path: String) -> Result<Attrs, Self::Error> {
            Ok(Attrs { id, attrs: dir_attrs() })
        }

        async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
            self.fresh.insert(path.clone());
            Ok(Handle { id, handle: path })
        }

        async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
            if self.fresh.remove(&handle) {
                Ok(Name { id, files: vec![File::new("d", dir_attrs())] })
            } else {
                Err(StatusCode::Eof)
            }
        }

        async fn close(&mut self, id: u32, _handle: String) -> Result<Status, Self::Error> {
            Ok(Status {
                id,
                status_code: StatusCode::Ok,
                error_message: String::new(),
                language_tag: "en-US".to_string(),
            })
        }
    }

    #[tokio::test]
    async fn bottomless_tree_aborts_at_depth_cap() {
        let source = hostile_source(BottomlessServer { fresh: HashSet::new() }).await;

        let err = source.walk("/data", true).await.expect_err("walk must refuse");
        assert!(format!("{err:#}").contains("nesting exceeds"));
    }

    /// A server that pours out entries until well past the walk budget —
    /// the fan-out flavor of the same attack.
    struct FirehoseServer {
        batches_left: usize,
    }

    impl russh_sftp::server::Handler for FirehoseServer {
        type Error = StatusCode;

        fn unimplemented(&self) -> Self::Error {
            StatusCode::OpUnsupported
        }

        async fn stat(&mut self, id: u32, _path: String) -> Result<Attrs, Self::Error> {
            Ok(Attrs { id, attrs: dir_attrs() })
        }

        async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
            Ok(Handle { id, handle: path })
        }

        async fn readdir(&mut self, id: u32, _handle: String) -> Result<Name, Self::Error> {
            if self.batches_left == 0 {
                return Err(StatusCode::Eof);
            }
            self.batches_left -= 1;
            let base = self.batches_left;
            let files = (0..50_000)
                .map(|i| File::new(format!("f{}_{}", base, i), file_attrs(1)))
                .collect();
            Ok(Name { id, files })
        }

        async fn close(&mut self, id: u32, _handle: String) -> Result<Status, Self::Error> {
            Ok(Status {
                id,
                status_code: StatusCode::Ok,
                error_message: String::new(),
                language_tag: "en-US".to_string(),
            })
        }
    }

    #[tokio::test]
    async fn entry_firehose_aborts_at_budget() {
        // 21 batches x 50k = 1.05M entries in one directory, just past the
        // 1M budget.
        let source = hostile_source(FirehoseServer { batches_left: 21 }).await;

        let err = source.walk("/data", true).await.expect_err("walk must refuse");
        assert!(format!("{err:#}").contains("entries in the selection"));
    }

    #[tokio::test]
    async fn hostile_names_are_hidden_from_browse() {
        // A traversal-bearing name must never appear in the pane, so it
        // can never be selected as a transfer anchor.
        let listings = HashMap::from([(
            "/data".to_string(),
            vec![
                File::new("real.txt", file_attrs(4)),
                File::new("../../escape.txt", file_attrs(9)),
                File::new("evil", link_attrs()),
            ],
        )]);
        let source = hostile_source(HostileServer::new(listings)).await;

        let names: Vec<String> = source
            .list_dir("/data", true)
            .await
            .expect("list succeeds")
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(names, vec!["evil", "real.txt"]);
    }
}
