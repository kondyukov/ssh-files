use anyhow::Result;
use ratatui::layout::Rect;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::action::{Action, TransferMode};
use crate::cli::ConnectionInfo;
use crate::clipboard::{
    create_clipboard, ClipboardItem, ClipboardOp, FileClipboard, SystemClipboard,
};
use crate::executor::{self, TransferConfig};
use crate::file_tree::FileTree;
use crate::input::{ContextKind, ContextMenuState, ContextStack, InputContext};
use crate::keymap::Keymap;
use crate::source::{
    mapping, CollectedFiles, FileInfo, FileSource, LocalSource, RemoteCliOpts, RemoteSource,
};
use crate::ssh::SftpClientShared;
use crate::theme::Theme;
use crate::ui::geometry;
use crate::ui::icons::IconSet;
use crate::transfer::{
    build_rsync_command, RsyncEndpoint, TransferDirection, TransferResult, TransferState,
};

/// Human summary of a selection split into files and directories,
/// e.g. "3 files, 1 directory". Zero-count parts are omitted.
pub(crate) fn selection_summary(files: usize, dirs: usize) -> String {
    let mut parts = Vec::new();
    if files > 0 {
        parts.push(format!("{} file{}", files, if files == 1 { "" } else { "s" }));
    }
    if dirs > 0 {
        parts.push(format!("{} director{}", dirs, if dirs == 1 { "y" } else { "ies" }));
    }
    parts.join(", ")
}

/// Status wording for the probed streaming capability. `None` (probe never
/// ran) reads as sftp: that is what the executor will fall back to.
fn streaming_label(ready: Option<bool>) -> &'static str {
    match ready {
        Some(true) => "streaming",
        _ => "sftp",
    }
}

/// Last component of a directory path for compact display (e.g. context
/// menu labels); filesystem roots fall back to the path itself.
/// Connection facts a remote source needs to reconstruct an equivalent
/// ssh command line for rsync generation (text only, never executed).
fn remote_cli_opts(conn: &ConnectionInfo) -> RemoteCliOpts {
    let jump_chain = if conn.jumps.is_empty() {
        None
    } else {
        Some(
            conn.jumps
                .iter()
                .map(|j| {
                    if j.port == 22 {
                        format!("{}@{}", j.user, j.host)
                    } else {
                        format!("{}@{}:{}", j.user, j.host, j.port)
                    }
                })
                .collect::<Vec<_>>()
                .join(","),
        )
    };
    RemoteCliOpts {
        port: conn.port,
        identity_files: conn.identity_files.clone(),
        jump_chain,
    }
}

fn dir_display_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Local,  // Left pane (historically local, but can now be any source)
    Remote, // Right pane (historically remote, but can now be any source)
}

/// State for rename modal
pub struct RenameState {
    pub pane: Pane,
    pub original_path: String,
    pub original_name: String,
    pub input: String,
    pub cursor_pos: usize,
    pub is_dir: bool,
}

/// State for delete confirmation modal
pub struct DeleteState {
    pub pane: Pane,
    pub items: Vec<DeleteItem>,
}

pub struct DeleteItem {
    pub full_path: String,
    pub relative_path: String,
    pub is_dir: bool,
}

/// A fully prepared transfer awaiting user confirmation (e.g. overwrite).
pub struct PendingTransfer {
    pub source: Arc<dyn FileSource>,
    pub dest: Arc<dyn FileSource>,
    pub collected: CollectedFiles,
    pub dest_base: String,
    pub direction: TransferDirection,
}

pub struct App {
    pub focus: Pane,
    pub should_quit: bool,
    pub show_hidden: bool,
    // The two panes are "left slot" and "right slot"; `Pane::Local`/`Remote`
    // are their historical names. Either may be backed by any source - a
    // pane is remote iff its source reports so, never by position.
    pub local_tree: FileTree,
    pub remote_tree: FileTree,
    pub status: String,
    pub transfer: Option<TransferState>,
    pub input: ContextStack,
    pub keymap: Keymap,
    pub pane_areas: Option<(Rect, Rect)>,
    pub theme: Theme,
    pub icons: &'static IconSet,
    // Clipboard
    pub file_clipboard: Option<FileClipboard>,
    pub system_clipboard: Box<dyn SystemClipboard>,
    // Sources for dual-pane abstraction
    pub left_source: Option<Arc<dyn FileSource>>,
    pub right_source: Option<Arc<dyn FileSource>>,
}

impl App {
    /// Shared constructor: build the app from a fully-resolved source and
    /// starting root for each pane. The pane's tree path semantics follow
    /// `source.is_remote()`, so any source combination works.
    async fn new_with_sources(
        left: (Arc<dyn FileSource>, String),
        right: (Arc<dyn FileSource>, String),
        status: String,
        theme: Theme,
        keymap: Keymap,
        icons: &'static IconSet,
    ) -> Result<Self> {
        let (left_source, left_root) = left;
        let (right_source, right_root) = right;

        // Hidden files are visible (and transferred) by default; `.` hides.
        let left_entries = left_source.list_dir(&left_root, true).await?;
        let mut local_tree = FileTree::new(left_root, left_source.is_remote());
        local_tree.set_entries(
            left_entries.into_iter().map(|e| (e.name, e.is_dir, e.size)).collect(),
        );

        let right_entries = right_source.list_dir(&right_root, true).await?;
        let mut remote_tree = FileTree::new(right_root, right_source.is_remote());
        remote_tree.set_entries(
            right_entries.into_iter().map(|e| (e.name, e.is_dir, e.size)).collect(),
        );

        // Probe transfer capabilities now (one cheap roundtrip per remote)
        // so status labels can report streaming vs sftp synchronously, and
        // the fallback decision is made up front rather than mid-transfer.
        left_source.warm_capabilities().await;
        right_source.warm_capabilities().await;

        Ok(Self {
            focus: Pane::Remote,
            should_quit: false,
            show_hidden: true,
            local_tree,
            remote_tree,
            status,
            transfer: None,
            input: ContextStack::new(),
            keymap,
            pane_areas: None,
            theme,
            icons,
            file_clipboard: None,
            system_clipboard: create_clipboard(),
            left_source: Some(left_source),
            right_source: Some(right_source),
        })
    }

    /// Standard mode: local left pane, remote right pane. `sftp_only`
    /// withholds the exec handle, so every transfer takes the SFTP path
    /// and no remote command line is ever built from a file name.
    pub async fn new_connected(
        sftp: SftpClientShared,
        conn: &ConnectionInfo,
        theme: Theme,
        keymap: Keymap,
        icons: &'static IconSet,
        sftp_only: bool,
    ) -> Result<Self> {
        let local_path = std::env::current_dir()?;
        let local_root = local_path.to_string_lossy().to_string();
        let left: Arc<dyn FileSource> = Arc::new(LocalSource::new(local_path));

        let (remote_root, _entries, status) = Self::resolve_remote_path(&sftp, conn).await?;
        let exec = if sftp_only { None } else { Some(sftp.exec_handle()) };
        let right: Arc<dyn FileSource> = Arc::new(RemoteSource::new(
            sftp.sftp(),
            remote_root.clone(),
            conn.host.clone(),
            conn.user.clone(),
            exec,
            remote_cli_opts(conn),
        ));

        Self::new_with_sources(
            (left, local_root),
            (right, remote_root),
            status,
            theme,
            keymap,
            icons,
        )
        .await
    }

    /// Local mode: both panes browse the local filesystem.
    pub async fn new_local(
        left_path: PathBuf,
        right_path: PathBuf,
        theme: Theme,
        keymap: Keymap,
        icons: &'static IconSet,
    ) -> Result<Self> {
        let left_root = left_path.to_string_lossy().to_string();
        let right_root = right_path.to_string_lossy().to_string();
        let left: Arc<dyn FileSource> = Arc::new(LocalSource::new(left_path));
        let right: Arc<dyn FileSource> = Arc::new(LocalSource::new(right_path));

        Self::new_with_sources(
            (left, left_root),
            (right, right_root),
            String::from("Local mode"),
            theme,
            keymap,
            icons,
        )
        .await
    }

    /// Dual-remote mode: both panes are remote hosts, with direct
    /// remote-to-remote transfers between them.
    #[allow(clippy::too_many_arguments)]
    pub async fn new_dual_remote(
        left_sftp: SftpClientShared,
        left_conn: &ConnectionInfo,
        right_sftp: SftpClientShared,
        right_conn: &ConnectionInfo,
        theme: Theme,
        keymap: Keymap,
        icons: &'static IconSet,
        sftp_only: bool,
    ) -> Result<Self> {
        let (left_root, _le, _ls) = Self::resolve_remote_path(&left_sftp, left_conn).await?;
        let (right_root, _re, _rs) = Self::resolve_remote_path(&right_sftp, right_conn).await?;

        let left_exec = if sftp_only { None } else { Some(left_sftp.exec_handle()) };
        let right_exec = if sftp_only { None } else { Some(right_sftp.exec_handle()) };
        let left: Arc<dyn FileSource> = Arc::new(RemoteSource::new(
            left_sftp.sftp(),
            left_root.clone(),
            left_conn.host.clone(),
            left_conn.user.clone(),
            left_exec,
            remote_cli_opts(left_conn),
        ));
        let right: Arc<dyn FileSource> = Arc::new(RemoteSource::new(
            right_sftp.sftp(),
            right_root.clone(),
            right_conn.host.clone(),
            right_conn.user.clone(),
            right_exec,
            remote_cli_opts(right_conn),
        ));

        let status = format!("{} <-> {}", left_conn.host, right_conn.host);
        Self::new_with_sources(
            (left, left_root),
            (right, right_root),
            status,
            theme,
            keymap,
            icons,
        )
        .await
    }

    async fn resolve_remote_path(
        sftp: &SftpClientShared,
        conn: &ConnectionInfo,
    ) -> Result<(String, Vec<FileInfo>, String)> {
        if let Some(ref path) = conn.remote_path {
            if let Ok(entries) = sftp.list_dir(path).await {
                return Ok((path.clone(), entries, format!("Connected to {}", conn.host)));
            }
        }

        let home = format!("/home/{}", conn.user);
        if let Ok(entries) = sftp.list_dir(&home).await {
            let status = if conn.remote_path.is_some() {
                format!("Path not accessible, opened {}", home)
            } else {
                format!("Connected to {}", conn.host)
            };
            return Ok((home, entries, status));
        }

        let win_home = format!("/Users/{}", conn.user);
        if let Ok(entries) = sftp.list_dir(&win_home).await {
            let status = if conn.remote_path.is_some() {
                format!("Path not accessible, opened {}", win_home)
            } else {
                format!("Connected to {}", conn.host)
            };
            return Ok((win_home, entries, status));
        }

        let entries = sftp.list_dir("/").await?;
        let status = if conn.remote_path.is_some() {
            String::from("Path not accessible, opened /")
        } else {
            format!("Connected to {}", conn.host)
        };
        Ok((String::from("/"), entries, status))
    }

    pub fn is_transferring(&self) -> bool {
        self.transfer.is_some()
    }

    /// Kind of the context currently owning input.
    pub fn context_kind(&self) -> ContextKind {
        self.input.top().kind()
    }

    pub fn rename_state(&self) -> Option<&RenameState> {
        self.input.iter().rev().find_map(|ctx| match ctx {
            InputContext::Rename(state) => Some(state),
            _ => None,
        })
    }

    fn rename_state_mut(&mut self) -> Option<&mut RenameState> {
        self.input.iter_mut().rev().find_map(|ctx| match ctx {
            InputContext::Rename(state) => Some(state),
            _ => None,
        })
    }

    pub fn delete_state(&self) -> Option<&DeleteState> {
        self.input.iter().rev().find_map(|ctx| match ctx {
            InputContext::DeleteConfirm(state) => Some(state),
            _ => None,
        })
    }

    pub fn context_menu_state(&self) -> Option<&ContextMenuState> {
        self.input.iter().rev().find_map(|ctx| match ctx {
            InputContext::ContextMenu(state) => Some(state),
            _ => None,
        })
    }

    pub fn context_menu_state_mut(&mut self) -> Option<&mut ContextMenuState> {
        self.input.iter_mut().rev().find_map(|ctx| match ctx {
            InputContext::ContextMenu(state) => Some(state),
            _ => None,
        })
    }

    pub fn collision_files(&self) -> Option<&[String]> {
        self.input.iter().rev().find_map(|ctx| match ctx {
            InputContext::CollisionWarning { files } => Some(files.as_slice()),
            _ => None,
        })
    }

    fn help_state_mut(&mut self) -> Option<(&mut u16, &mut u16)> {
        self.input.iter_mut().rev().find_map(|ctx| match ctx {
            InputContext::Help { scroll, viewport } => Some((scroll, viewport)),
            _ => None,
        })
    }

    /// Scroll the help modal by `delta` lines (i32::MIN/MAX jump to the
    /// ends). The stored value saturates here; the renderer clamps it to
    /// the actual content and writes it back.
    pub fn help_scroll(&mut self, delta: i32) {
        if let Some((scroll, _)) = self.help_state_mut() {
            *scroll = (*scroll as i64).saturating_add(delta as i64).clamp(0, u16::MAX as i64) as u16;
        }
    }

    /// Scroll the help modal by whole viewports (as recorded at render
    /// time), for PageUp/PageDown.
    pub fn help_scroll_page(&mut self, pages: i32) {
        let Some((_, viewport)) = self.help_state_mut() else { return };
        let page = (*viewport).max(1) as i32;
        self.help_scroll(pages.saturating_mul(page));
    }

    /// Render-time write-back: clamp the help scroll to the real content
    /// height and record the viewport, returning the effective scroll.
    pub fn clamp_help_scroll(&mut self, max_scroll: u16, viewport_height: u16) -> u16 {
        match self.help_state_mut() {
            Some((scroll, viewport)) => {
                *scroll = (*scroll).min(max_scroll);
                *viewport = viewport_height;
                *scroll
            }
            None => 0,
        }
    }

    /// Apply a semantic action to the application. All fallible operations
    /// report errors through the status line.
    pub async fn dispatch(&mut self, action: Action) {
        match action {
            Action::MoveUp => self.move_up(),
            Action::MoveDown => self.move_down(),
            Action::PageUp => self.page_move(-1),
            Action::PageDown => self.page_move(1),
            Action::GoToTop => self.go_to_top(),
            Action::GoToBottom => self.go_to_bottom(),
            Action::EnterDir => {
                if let Err(e) = self.enter_dir().await {
                    self.status = format!("Error: {}", e);
                }
            }
            Action::GoUp => {
                if let Err(e) = self.go_up().await {
                    self.status = format!("Error: {}", e);
                }
            }
            Action::ToggleExpand => {
                if let Err(e) = self.toggle_expand().await {
                    self.status = format!("Error: {}", e);
                }
            }
            Action::FocusPane(pane) => self.focus = pane,
            Action::ToggleSelect => self.toggle_select(),
            Action::SelectAll => self.select_all(),
            Action::DeselectAll => self.deselect_all(),
            Action::Transfer { direction, mode } => {
                let result = match (direction, mode) {
                    (TransferDirection::Download, TransferMode::Flat) => {
                        self.start_download_flat().await
                    }
                    (TransferDirection::Download, TransferMode::Preserve) => {
                        self.start_download_preserve().await
                    }
                    (TransferDirection::Upload, TransferMode::Flat) => {
                        self.start_upload_flat().await
                    }
                    (TransferDirection::Upload, TransferMode::Preserve) => {
                        self.start_upload_preserve().await
                    }
                };
                if let Err(e) = result {
                    self.status = format!("Transfer failed: {}", e);
                }
            }
            Action::CopyRsync { direction, mode } => {
                let source_pane = match direction {
                    TransferDirection::Upload => Pane::Local,
                    TransferDirection::Download => Pane::Remote,
                };
                self.copy_rsync_command(source_pane, mode == TransferMode::Preserve);
            }
            Action::ClipboardCopy => self.clipboard_copy(),
            Action::ClipboardCut => self.clipboard_cut(),
            Action::Paste => {
                if let Err(e) = self.start_paste().await {
                    self.status = format!("Paste failed: {}", e);
                }
            }
            Action::CopyPath => self.copy_path_to_clipboard(),
            Action::StartRename => self.start_rename(),
            Action::ConfirmRename => {
                if let Err(e) = self.confirm_rename().await {
                    self.status = format!("Rename failed: {}", e);
                }
            }
            Action::CancelRename => self.cancel_rename(),
            Action::StartDelete => self.start_delete(),
            Action::ConfirmDelete => {
                if let Err(e) = self.confirm_delete().await {
                    self.status = format!("Delete failed: {}", e);
                }
            }
            Action::CancelDelete => self.cancel_delete(),
            Action::ToggleHelp => {
                if self.input.pop_kind(ContextKind::Help).is_none() {
                    self.input.push(InputContext::Help { scroll: 0, viewport: 0 });
                }
            }
            Action::OpenContextMenu => self.open_context_menu_at_cursor(),
            Action::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                let _ = self.refresh_pane(Pane::Local).await;
                let _ = self.refresh_pane(Pane::Remote).await;
                self.status = if self.show_hidden {
                    String::from("Showing hidden files")
                } else {
                    String::from("Hiding hidden files")
                };
            }
            Action::Refresh => {
                if let Err(e) = self.refresh().await {
                    self.status = format!("Refresh failed: {}", e);
                }
            }
            Action::Quit => {
                if self.is_transferring() {
                    self.input.push(InputContext::CancelConfirm);
                } else {
                    self.should_quit = true;
                }
            }
        }
    }

    fn current_tree(&self) -> &FileTree {
        match self.focus {
            Pane::Local => &self.local_tree,
            Pane::Remote => &self.remote_tree,
        }
    }

    fn current_tree_mut(&mut self) -> &mut FileTree {
        match self.focus {
            Pane::Local => &mut self.local_tree,
            Pane::Remote => &mut self.remote_tree,
        }
    }

    fn pane_tree(&self, pane: Pane) -> &FileTree {
        match pane {
            Pane::Local => &self.local_tree,
            Pane::Remote => &self.remote_tree,
        }
    }

    fn pane_tree_mut(&mut self, pane: Pane) -> &mut FileTree {
        match pane {
            Pane::Local => &mut self.local_tree,
            Pane::Remote => &mut self.remote_tree,
        }
    }

    /// A pane is remote iff its backing source reports so - never decided
    /// by pane position, so left-remote (dual-remote mode) works.
    fn pane_is_remote(&self, pane: Pane) -> bool {
        let source = match pane {
            Pane::Local => &self.left_source,
            Pane::Remote => &self.right_source,
        };
        source.as_ref().map(|s| s.is_remote()).unwrap_or(false)
    }

    fn pane_source(&self, pane: Pane) -> Result<Arc<dyn FileSource>> {
        let source = match pane {
            Pane::Local => &self.left_source,
            Pane::Remote => &self.right_source,
        };
        source
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No source for pane"))
    }

    /// List a directory through the pane's `FileSource`, honoring the
    /// hidden-files toggle.
    async fn list_pane_dir(&self, pane: Pane, path: &str) -> Result<Vec<(String, bool, u64)>> {
        let source = self.pane_source(pane)?;
        let entries = source.list_dir(path, self.show_hidden).await?;
        Ok(entries.into_iter().map(|e| (e.name, e.is_dir, e.size)).collect())
    }

    pub fn move_up(&mut self) {
        self.current_tree_mut().move_up();
    }

    pub fn move_down(&mut self) {
        self.current_tree_mut().move_down();
    }

    /// Move the focused pane's cursor by one viewport page (PageUp /
    /// PageDown). Page size comes from the pane's rendered height, so it
    /// tracks terminal resizes; before the first frame it falls back.
    pub fn page_move(&mut self, direction: isize) {
        let page = self
            .pane_areas
            .map(|(left, right)| {
                let area = match self.focus {
                    Pane::Local => left,
                    Pane::Remote => right,
                };
                geometry::visible_height(area)
            })
            .unwrap_or(10)
            .max(1);
        self.current_tree_mut().move_by(direction.saturating_mul(page as isize));
    }

    pub fn go_to_top(&mut self) {
        self.current_tree_mut().go_to_top();
    }

    pub fn go_to_bottom(&mut self) {
        self.current_tree_mut().go_to_bottom();
    }

    pub fn toggle_select(&mut self) {
        let tree = self.current_tree_mut();
        tree.toggle_select();
        let (files, dirs) = tree.selection_counts();
        self.status = if files + dirs > 0 {
            format!("Selected {}", selection_summary(files, dirs))
        } else {
            String::from("Selection cleared")
        };
    }

    pub fn select_all(&mut self) {
        let tree = self.current_tree_mut();
        tree.select_all();
        let (files, dirs) = tree.selection_counts();
        self.status = if files + dirs > 0 {
            format!("Selected {}", selection_summary(files, dirs))
        } else {
            String::from("Selection cleared")
        };
    }

    pub fn deselect_all(&mut self) {
        self.current_tree_mut().clear_selection();
        self.status = String::from("Selection cleared");
    }

    /// Replace a pane's tree with a fresh listing of `new_root`.
    async fn load_pane_root(&mut self, pane: Pane, new_root: String) -> Result<()> {
        let entries = self.list_pane_dir(pane, &new_root).await?;
        let is_remote = self.pane_is_remote(pane);
        let tree = match pane {
            Pane::Local => &mut self.local_tree,
            Pane::Remote => &mut self.remote_tree,
        };
        *tree = FileTree::new(new_root, is_remote);
        tree.set_entries(entries);
        Ok(())
    }

    pub async fn toggle_expand(&mut self) -> Result<()> {
        let pane = self.focus;
        if let Some((parent_idx, path_to_load)) = self.current_tree_mut().toggle_expand() {
            match self.list_pane_dir(pane, &path_to_load).await {
                Ok(children) => {
                    self.current_tree_mut().set_children(parent_idx, children);
                }
                Err(e) => {
                    self.status = format!("Error: {}", e);
                    self.current_tree_mut().collapse();
                }
            }
        }
        Ok(())
    }

    pub async fn go_up(&mut self) -> Result<()> {
        let pane = self.focus;
        let current_path = self.current_tree().root_path.clone();

        let new_path = if self.pane_is_remote(pane) {
            if current_path == "/" {
                return Ok(());
            }
            if let Some(pos) = current_path.rfind('/') {
                if pos == 0 { String::from("/") } else { current_path[..pos].to_string() }
            } else {
                String::from("/")
            }
        } else {
            let mut path = PathBuf::from(&current_path);
            if !path.pop() {
                return Ok(());
            }
            path.to_string_lossy().to_string()
        };

        self.load_pane_root(pane, new_path).await
    }

    pub async fn enter_dir(&mut self) -> Result<()> {
        let node = match self.current_tree().get_cursor_node() {
            Some(n) if n.is_dir => n,
            _ => return Ok(()),
        };

        self.load_pane_root(self.focus, node.full_path).await
    }

    pub async fn refresh(&mut self) -> Result<()> {
        self.refresh_pane(self.focus).await?;
        self.status = String::from("Refreshed");
        Ok(())
    }

    /// Spawn a prepared transfer and report it in the status line.
    fn launch_transfer(&mut self, pending: PendingTransfer) {
        let file_count = pending.collected.file_count();
        let note = pending.collected.skipped_note();
        self.spawn_transfer(
            pending.source,
            pending.dest,
            pending.collected,
            pending.dest_base,
            pending.direction,
        );
        self.status = format!("Starting transfer of {} file(s)...{}", file_count, note);
    }

    /// Proceed with the transfer held by the overwrite confirmation.
    pub fn confirm_overwrite(&mut self) {
        if let Some(InputContext::OverwriteConfirm { pending, .. }) =
            self.input.pop_kind(ContextKind::OverwriteConfirm)
        {
            self.launch_transfer(pending);
        }
    }

    /// Abandon the transfer held by the overwrite confirmation.
    pub fn cancel_overwrite(&mut self) {
        if self.input.pop_kind(ContextKind::OverwriteConfirm).is_some() {
            self.status = String::from("Transfer cancelled");
        }
    }

    pub fn overwrite_files(&self) -> Option<&[String]> {
        self.input.iter().rev().find_map(|ctx| match ctx {
            InputContext::OverwriteConfirm { files, .. } => Some(files.as_slice()),
            _ => None,
        })
    }

    /// Create the right executor for the source/dest pair, spawn the
    /// transfer task, and install it as the active transfer.
    fn spawn_transfer(
        &mut self,
        source: Arc<dyn FileSource>,
        dest: Arc<dyn FileSource>,
        collected: CollectedFiles,
        dest_base: String,
        direction: TransferDirection,
    ) {
        // Status route, matching the context menu's Send labels: the arrow
        // points at the receiving pane as laid out on screen. Remote routes
        // also carry which byte path was chosen: the raw exec stream or
        // SFTP. Each remote side decides independently, so remote-to-remote
        // shows one label when they agree and "read/write" when they differ
        // (e.g. [streaming/sftp]: source streams, destination fell back).
        let arrow = match direction {
            TransferDirection::Upload => "->",
            TransferDirection::Download => "<-",
        };
        let mode: Option<String> = match (source.is_remote(), dest.is_remote()) {
            (false, false) => None,
            (true, true) => {
                let read = streaming_label(source.streaming_ready());
                let write = streaming_label(dest.streaming_ready());
                Some(if read == write {
                    read.to_string()
                } else {
                    format!("{}/{}", read, write)
                })
            }
            (true, false) => Some(streaming_label(source.streaming_ready()).to_string()),
            (false, true) => Some(streaming_label(dest.streaming_ready()).to_string()),
        };
        let route = match mode {
            Some(mode) => format!("{} {} [{}]", arrow, dir_display_name(&dest_base), mode),
            None => format!("{} {}", arrow, dir_display_name(&dest_base)),
        };

        let exec = executor::create_executor(source, dest, TransferConfig::default());

        let (progress_tx, progress_rx) = mpsc::channel(10);
        let (result_tx, result_rx) = mpsc::channel(1);
        let cancel = CancellationToken::new();

        let file_sizes: Vec<u64> = collected.files.iter().map(|f| f.size).collect();

        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            let result = exec.execute(collected, &dest_base, progress_tx, cancel_clone).await;
            let _ = result_tx.send(result).await;
        });

        self.transfer = Some(TransferState::new(
            route,
            file_sizes,
            cancel,
            result_rx,
            progress_rx,
        ));
    }

    pub async fn poll_transfer(&mut self) {
        if let Some(ref mut transfer) = self.transfer {
            while let Some(progress) = transfer.poll_progress() {
                self.status = format!(
                    "Sending {} {} ({}/{}) - {:.1}%",
                    progress.filename,
                    transfer.route,
                    progress.file_index + 1,
                    progress.total_files,
                    progress.percent()
                );
            }

            if let Some(result) = transfer.poll_result() {
                let route = transfer.route.clone();

                match result {
                    TransferResult::Success { files_transferred } => {
                        self.status = format!(
                            "Sent {} file(s) {}",
                            files_transferred,
                            route
                        );
                    }
                    TransferResult::Cancelled { files_completed } => {
                        self.status = format!("Transfer cancelled ({} file(s) completed)", files_completed);
                    }
                    TransferResult::Error { message, files_completed } => {
                        self.status = format!("Transfer failed: {} ({} completed)", message, files_completed);
                    }
                }

                self.transfer = None;
                let _ = self.refresh_pane(Pane::Local).await;
                let _ = self.refresh_pane(Pane::Remote).await;
            }
        }
    }

    /// Re-list a pane's current root, preserving expanded directories and
    /// the cursor (by path, falling back to the old row index).
    async fn refresh_pane(&mut self, pane: Pane) -> Result<()> {
        let (root, cursor, expanded, cursor_path) = {
            let tree = self.pane_tree(pane);
            (
                tree.root_path.clone(),
                tree.cursor,
                tree.expanded_dirs(),
                tree.cursor_path(),
            )
        };

        let entries = self.list_pane_dir(pane, &root).await?;
        let is_remote = self.pane_is_remote(pane);
        {
            let tree = self.pane_tree_mut(pane);
            *tree = FileTree::new(root, is_remote);
            tree.set_entries(entries);
        }

        // Re-expand what the user had open. Parents come before children
        // (display order), so each directory's node exists by the time it
        // is looked up; directories that vanished are skipped silently.
        for dir in expanded {
            if self.pane_tree(pane).find_visible_dir(&dir).is_none() {
                continue;
            }
            let Ok(children) = self.list_pane_dir(pane, &dir).await else {
                continue;
            };
            let tree = self.pane_tree_mut(pane);
            if let Some(index) = tree.find_visible_dir(&dir) {
                tree.expand_with_children(index, children);
            }
        }

        let tree = self.pane_tree_mut(pane);
        let restored = cursor_path.is_some_and(|path| tree.cursor_to_path(&path));
        if !restored {
            tree.cursor = cursor;
            tree.clamp_cursor();
        }
        Ok(())
    }

    pub fn cancel_transfer(&mut self) {
        if let Some(ref transfer) = self.transfer {
            transfer.cancel();
            self.status = String::from("Cancelling transfer...");
        }
    }

    /// Which pane (and its area) is under the given screen position.
    fn pane_at(&self, x: u16, y: u16) -> Option<(Pane, Rect)> {
        let (local_area, remote_area) = self.pane_areas?;
        if geometry::contains(local_area, x, y) {
            Some((Pane::Local, local_area))
        } else if geometry::contains(remote_area, x, y) {
            Some((Pane::Remote, remote_area))
        } else {
            None
        }
    }

    /// Focus the pane under (x, y) and move its cursor to the clicked row.
    /// Returns the pane hit, if any.
    fn focus_click(&mut self, x: u16, y: u16) -> Option<Pane> {
        let (pane, area) = self.pane_at(x, y)?;
        self.focus = pane;

        let tree = match pane {
            Pane::Local => &mut self.local_tree,
            Pane::Remote => &mut self.remote_tree,
        };
        let clicked_index = geometry::row_at(area, tree.cursor, y);
        if clicked_index < tree.visible_count() {
            tree.cursor = clicked_index;
        }
        Some(pane)
    }

    pub fn handle_click(&mut self, x: u16, y: u16) {
        let _ = self.focus_click(x, y);
    }

    pub fn open_context_menu(&mut self, x: u16, y: u16) {
        let Some(pane) = self.focus_click(x, y) else { return };
        self.push_context_menu(pane, (x, y));
    }

    /// Open the context menu for the focused pane, anchored at its cursor
    /// row - the keyboard counterpart of a right-click.
    pub fn open_context_menu_at_cursor(&mut self) {
        let Some(pos) = self.cursor_screen_pos() else { return };
        self.push_context_menu(self.focus, pos);
    }

    /// Screen position of the focused pane's cursor row, for anchoring the
    /// keyboard-opened context menu.
    fn cursor_screen_pos(&self) -> Option<(u16, u16)> {
        let (local_area, remote_area) = self.pane_areas?;
        let (area, tree) = match self.focus {
            Pane::Local => (local_area, &self.local_tree),
            Pane::Remote => (remote_area, &self.remote_tree),
        };
        // Cursor's on-screen row: its visible-row index minus the scroll,
        // inside the pane border. Indent past the tree decorations so the
        // menu reads as attached to the item.
        let row = tree.cursor - geometry::scroll_offset(tree.cursor, geometry::visible_height(area));
        Some((area.x + 4, area.y + 1 + row as u16))
    }

    fn push_context_menu(&mut self, pane: Pane, pos: (u16, u16)) {
        let items = self.context_menu_items(pane);
        self.input.push(InputContext::ContextMenu(ContextMenuState {
            pos,
            bounds: None,
            selected: 0,
            items,
        }));
    }

    fn context_menu_items(&self, pane: Pane) -> Vec<(Action, String)> {
        let tree = match pane {
            Pane::Local => &self.local_tree,
            Pane::Remote => &self.remote_tree,
        };

        let is_dir = tree.get_cursor_node().map(|n| n.is_dir).unwrap_or(false);

        // Count selected items (use selection count or 1 if nothing selected)
        let selected_count = if tree.has_selection() { tree.selection_count() } else { 1 };
        let has_multiple = selected_count > 1;

        let mut items: Vec<(Action, String)> = Vec::new();

        // Only show Open/Expand for directories, and only when single item
        if is_dir && !has_multiple {
            items.push((Action::EnterDir, "Open".to_string()));
            items.push((Action::ToggleExpand, "Expand/Collapse".to_string()));
        }

        // Send to the other pane, flat or with the tree structure kept.
        // The arrow points at the receiving pane as laid out on screen,
        // named by the directory it is showing.
        let count_str = if has_multiple { format!(" ({} items)", selected_count) } else { String::new() };

        let (direction, arrow, dest_root) = match pane {
            Pane::Remote => (TransferDirection::Download, "<-", &self.local_tree.root_path),
            Pane::Local => (TransferDirection::Upload, "->", &self.remote_tree.root_path),
        };
        let dest = dir_display_name(dest_root);
        items.push((
            Action::Transfer { direction, mode: TransferMode::Flat },
            format!("Send flat {} {}{}", arrow, dest, count_str),
        ));
        items.push((
            Action::Transfer { direction, mode: TransferMode::Preserve },
            format!("Send tree {} {}{}", arrow, dest, count_str),
        ));

        // The same transfer as an rsync command in the clipboard, for the
        // user to inspect and run themselves. rsync has no third-party
        // mode, so dual-remote panes get no entry.
        let both_remote = self.left_source.as_ref().is_some_and(|s| s.is_remote())
            && self.right_source.as_ref().is_some_and(|s| s.is_remote());
        if !both_remote {
            items.push((
                Action::CopyRsync { direction, mode: TransferMode::Flat },
                format!("Copy rsync flat {} {}", arrow, dest),
            ));
            items.push((
                Action::CopyRsync { direction, mode: TransferMode::Preserve },
                format!("Copy rsync tree {} {}", arrow, dest),
            ));
        }

        // Clipboard (copy/cut/paste) is deferred past this release; restore
        // these entries when it lands completely and cleanly.
        //
        // if has_multiple {
        //     items.push((Action::ClipboardCopy, format!("Copy {} items", selected_count)));
        //     items.push((Action::ClipboardCut, format!("Cut {} items", selected_count)));
        // } else {
        //     items.push((Action::ClipboardCopy, "Copy".to_string()));
        //     items.push((Action::ClipboardCut, "Cut".to_string()));
        // }
        //
        // // Paste - only show if clipboard has items
        // if let Some(ref cb) = self.file_clipboard {
        //     let paste_label = if cb.items.len() > 1 {
        //         format!("Paste {} items", cb.items.len())
        //     } else {
        //         "Paste".to_string()
        //     };
        //     items.push((Action::Paste, paste_label));
        // }

        // Copy path to system clipboard
        items.push((Action::CopyPath, "Copy path".to_string()));

        items.push((Action::ToggleSelect, "Toggle Select".to_string()));

        // Smart select all label - same predicate select_all() toggles on
        let all_selected = tree.all_selected();
        if all_selected {
            items.push((Action::SelectAll, "Deselect All".to_string()));
        } else {
            items.push((Action::SelectAll, "Select All".to_string()));
        }

        // Rename - only for single item
        if !has_multiple {
            items.push((Action::StartRename, "Rename".to_string()));
        }

        // Delete with count
        if has_multiple {
            items.push((Action::StartDelete, format!("Delete {} items", selected_count)));
        } else {
            items.push((Action::StartDelete, "Delete".to_string()));
        }

        items.push((Action::Refresh, "Refresh".to_string()));

        items
    }

    pub fn context_menu_up(&mut self) {
        if let Some(state) = self.context_menu_state_mut() {
            if state.selected > 0 {
                state.selected -= 1;
            }
        }
    }

    pub fn context_menu_down(&mut self) {
        if let Some(state) = self.context_menu_state_mut() {
            if state.selected + 1 < state.items.len() {
                state.selected += 1;
            }
        }
    }

    pub fn context_menu_execute(&mut self) -> Option<Action> {
        match self.input.pop_kind(ContextKind::ContextMenu) {
            Some(InputContext::ContextMenu(state)) => {
                state.items.get(state.selected).map(|(action, _)| *action)
            }
            _ => None,
        }
    }

    pub fn close_context_menu(&mut self) {
        self.input.pop_kind(ContextKind::ContextMenu);
    }

    /// Update selection when hovering over context menu
    pub fn context_menu_hover(&mut self, x: u16, y: u16) {
        let Some(state) = self.context_menu_state_mut() else { return };
        let Some(bounds) = state.bounds else { return };

        if geometry::contains_inner(bounds, x, y) {
            let hover_index = y.saturating_sub(bounds.y + 1) as usize;
            if hover_index < state.items.len() {
                state.selected = hover_index;
            }
        }
    }

    /// Handle click on context menu, returns action if clicked on item
    pub fn context_menu_click(&mut self, x: u16, y: u16) -> Option<Action> {
        let state = self.context_menu_state()?;
        let (bounds, item_count) = (state.bounds?, state.items.len());

        // Check if click is inside menu content area (excluding borders)
        if geometry::contains_inner(bounds, x, y) {
            let clicked_index = y.saturating_sub(bounds.y + 1) as usize;
            if clicked_index < item_count {
                if let Some(state) = self.context_menu_state_mut() {
                    state.selected = clicked_index;
                }
                return self.context_menu_execute();
            }
        }

        // Click outside menu or on border - close it
        self.close_context_menu();
        None
    }

    // === Rename Modal ===

    pub fn start_rename(&mut self) {
        let pane = self.focus;
        let tree = match pane {
            Pane::Local => &self.local_tree,
            Pane::Remote => &self.remote_tree,
        };

        let node = match tree.get_cursor_node() {
            Some(n) => n,
            None => return,
        };

        let name = node.name.clone();
        
        // Position cursor before extension (if file has extension)
        let cursor_pos = if !node.is_dir {
            if let Some(dot_pos) = name.rfind('.') {
                if dot_pos > 0 {
                    dot_pos
                } else {
                    name.len()
                }
            } else {
                name.len()
            }
        } else {
            name.len()
        };

        self.input.push(InputContext::Rename(RenameState {
            pane,
            original_path: node.full_path.clone(),
            original_name: name.clone(),
            input: name,
            cursor_pos,
            is_dir: node.is_dir,
        }));
    }

    pub fn rename_input(&mut self, c: char) {
        if let Some(state) = self.rename_state_mut() {
            state.input.insert(state.cursor_pos, c);
            state.cursor_pos += 1;
        }
    }

    pub fn rename_backspace(&mut self) {
        if let Some(state) = self.rename_state_mut() {
            if state.cursor_pos > 0 {
                state.cursor_pos -= 1;
                state.input.remove(state.cursor_pos);
            }
        }
    }

    pub fn rename_delete(&mut self) {
        if let Some(state) = self.rename_state_mut() {
            if state.cursor_pos < state.input.len() {
                state.input.remove(state.cursor_pos);
            }
        }
    }

    pub fn rename_cursor_left(&mut self) {
        if let Some(state) = self.rename_state_mut() {
            if state.cursor_pos > 0 {
                state.cursor_pos -= 1;
            }
        }
    }

    pub fn rename_cursor_right(&mut self) {
        if let Some(state) = self.rename_state_mut() {
            if state.cursor_pos < state.input.len() {
                state.cursor_pos += 1;
            }
        }
    }

    pub fn rename_cursor_home(&mut self) {
        if let Some(state) = self.rename_state_mut() {
            state.cursor_pos = 0;
        }
    }

    pub fn rename_cursor_end(&mut self) {
        if let Some(state) = self.rename_state_mut() {
            state.cursor_pos = state.input.len();
        }
    }

    pub fn cancel_rename(&mut self) {
        self.input.pop_kind(ContextKind::Rename);
    }

    pub async fn confirm_rename(&mut self) -> Result<()> {
        let state = match self.input.pop_kind(ContextKind::Rename) {
            Some(InputContext::Rename(state)) => state,
            _ => return Ok(()),
        };

        let new_name = state.input.trim();
        if new_name.is_empty() || new_name == state.original_name {
            return Ok(());
        }

        // Build the new path and rename on the pane's backing filesystem
        let source = self.pane_source(state.pane)?;
        let new_path = source
            .parent_path(&state.original_path)
            .map(|parent| source.join_path(&parent, new_name))
            .unwrap_or_else(|| new_name.to_string());

        let result = source.rename(&state.original_path, &new_path).await;

        match result {
            Ok(()) => {
                self.status = format!("Renamed to {}", new_name);
                let _ = self.refresh_pane(state.pane).await;
            }
            Err(e) => {
                self.status = format!("Rename failed: {}", e);
            }
        }

        Ok(())
    }

    // === Delete Modal ===

    pub fn start_delete(&mut self) {
        let pane = self.focus;
        let tree = match pane {
            Pane::Local => &self.local_tree,
            Pane::Remote => &self.remote_tree,
        };

        // Get items to delete
        let selected = tree.get_selected_for_transfer();
        if selected.is_empty() {
            return;
        }

        let items: Vec<DeleteItem> = selected.iter().map(|n| DeleteItem {
            full_path: n.full_path.clone(),
            relative_path: n.relative_path.clone(),
            is_dir: n.is_dir,
        }).collect();

        self.input.push(InputContext::DeleteConfirm(DeleteState { pane, items }));
    }

    pub fn cancel_delete(&mut self) {
        self.input.pop_kind(ContextKind::DeleteConfirm);
    }

    pub async fn confirm_delete(&mut self) -> Result<()> {
        let state = match self.input.pop_kind(ContextKind::DeleteConfirm) {
            Some(InputContext::DeleteConfirm(state)) => state,
            _ => return Ok(()),
        };

        let source = self.pane_source(state.pane)?;
        let mut success_count = 0;
        let mut error_count = 0;

        for item in &state.items {
            let result = if item.is_dir {
                source.delete_dir_recursive(&item.full_path).await
            } else {
                source.delete_file(&item.full_path).await
            };

            match result {
                Ok(()) => success_count += 1,
                Err(_) => error_count += 1,
            }
        }

        let _ = self.refresh_pane(state.pane).await;

        // Clear selection
        match state.pane {
            Pane::Local => self.local_tree.clear_selection(),
            Pane::Remote => self.remote_tree.clear_selection(),
        }

        if error_count > 0 {
            self.status = format!("Deleted {} item(s), {} failed", success_count, error_count);
        } else {
            self.status = format!("Deleted {} item(s)", success_count);
        }

        Ok(())
    }

    /// Copy selected items to internal clipboard
    pub fn clipboard_copy(&mut self) {
        self.clipboard_set(ClipboardOp::Copy);
    }

    /// Cut selected items to internal clipboard
    pub fn clipboard_cut(&mut self) {
        self.clipboard_set(ClipboardOp::Cut);
    }

    fn clipboard_set(&mut self, operation: ClipboardOp) {
        let pane = self.focus;
        let tree = match pane {
            Pane::Local => &self.local_tree,
            Pane::Remote => &self.remote_tree,
        };

        let nodes = tree.get_selected_for_transfer();
        if nodes.is_empty() {
            return;
        }

        let items: Vec<ClipboardItem> = nodes.iter().map(|n| ClipboardItem {
            full_path: n.full_path.clone(),
            name: n.name.clone(),
            is_dir: n.is_dir,
        }).collect();

        let clipboard = FileClipboard::new(operation, pane, items);
        
        // Also set paths in system clipboard
        let paths_text = clipboard.paths_as_text();
        let _ = self.system_clipboard.set_text(&paths_text);

        let op_name = match operation {
            ClipboardOp::Copy => "Copied",
            ClipboardOp::Cut => "Cut",
        };
        let files = clipboard.items.iter().filter(|i| !i.is_dir).count();
        let dirs = clipboard.items.len() - files;
        self.status = format!("{} {}", op_name, selection_summary(files, dirs));
        
        self.file_clipboard = Some(clipboard);
    }

    /// Copy current item's path to system clipboard
    /// Put the rsync command equivalent to the pending transfer into the
    /// system clipboard: same selection roots, same direction, same
    /// flat/tree semantics, same hidden-file setting. Generation only -
    /// the command is never executed by ssh-files.
    pub fn copy_rsync_command(&mut self, source_pane: Pane, preserve: bool) {
        let dest_pane = match source_pane {
            Pane::Local => Pane::Remote,
            Pane::Remote => Pane::Local,
        };
        let (Ok(source), Ok(dest)) = (self.pane_source(source_pane), self.pane_source(dest_pane))
        else {
            self.status = String::from("No source for pane");
            return;
        };
        if source.is_remote() && dest.is_remote() {
            self.status = String::from("rsync has no remote-to-remote mode");
            return;
        }

        let (source_tree, dest_base) = match source_pane {
            Pane::Local => (&self.local_tree, self.remote_tree.root_path.clone()),
            Pane::Remote => (&self.remote_tree, self.local_tree.root_path.clone()),
        };
        let selected = source_tree.get_selected_for_transfer();
        if selected.is_empty() {
            self.status = String::from("No files selected");
            return;
        }
        let selections: Vec<String> =
            selected.iter().map(|n| n.relative_path.clone()).collect();

        let src_ep = RsyncEndpoint {
            prefix: source.rsync_prefix(),
            ssh_command: source.rsync_ssh_command(),
        };
        let dst_ep = RsyncEndpoint {
            prefix: dest.rsync_prefix(),
            ssh_command: dest.rsync_ssh_command(),
        };
        let cmd = build_rsync_command(
            &src_ep,
            &dst_ep,
            &source_tree.root_path,
            &dest_base,
            &selections,
            preserve,
            self.show_hidden,
        );

        let _ = self.system_clipboard.set_text(&cmd);
        self.status = format!(
            "rsync command copied ({}, {} item{})",
            if preserve { "tree" } else { "flat" },
            selections.len(),
            if selections.len() == 1 { "" } else { "s" },
        );
    }

    pub fn copy_path_to_clipboard(&mut self) {
        let tree = match self.focus {
            Pane::Local => &self.local_tree,
            Pane::Remote => &self.remote_tree,
        };

        if let Some(node) = tree.get_cursor_node() {
            let _ = self.system_clipboard.set_text(&node.full_path);
            self.status = format!("Copied path: {}", node.full_path);
        }
    }

    /// Paste from internal clipboard - returns true if paste was initiated
    pub async fn start_paste(&mut self) -> Result<bool> {
        let clipboard = match self.file_clipboard.take() {
            Some(cb) => cb,
            None => {
                self.status = String::from("Nothing to paste");
                return Ok(false);
            }
        };

        if self.transfer.is_some() {
            self.file_clipboard = Some(clipboard);
            self.status = String::from("Transfer already in progress");
            return Ok(false);
        }

        let dest_pane = self.focus;
        let source_pane = clipboard.source_pane;

        match (source_pane, dest_pane) {
            (Pane::Local, Pane::Remote) => {
                // Upload: local -> remote
                self.paste_upload(clipboard).await?;
            }
            (Pane::Remote, Pane::Local) => {
                // Download: remote -> local
                self.paste_download(clipboard).await?;
            }
            (Pane::Local, Pane::Local) => {
                // Local copy/move
                self.paste_local(clipboard)?;
            }
            (Pane::Remote, Pane::Remote) => {
                // Remote copy/move (not implemented yet)
                self.status = String::from("Remote to remote paste not yet supported");
                return Ok(false);
            }
        }

        Ok(true)
    }

    async fn paste_upload(&mut self, clipboard: FileClipboard) -> Result<()> {
        let source = match &self.left_source {
            Some(s) => Arc::clone(s),
            None => {
                self.status = String::from("No local source");
                return Ok(());
            }
        };
        
        let dest = match &self.right_source {
            Some(s) => Arc::clone(s),
            None => {
                self.status = String::from("No remote source");
                return Ok(());
            }
        };

        // Collect files from clipboard items, expanding directories (flat)
        let mut collected = CollectedFiles::new();
        for item in &clipboard.items {
            let entries = source.walk(&item.full_path, self.show_hidden).await?;
            mapping::map_to_destination(false, &item.name, entries, &mut collected);
        }

        if collected.is_empty() {
            self.status = String::from("No files to transfer");
            return Ok(());
        }

        let dest_base = self.remote_tree.root_path.clone();
        let file_count = collected.file_count();
        let note = collected.skipped_note();
        self.spawn_transfer(source, dest, collected, dest_base, TransferDirection::Upload);

        self.status = format!("Transferring {} file(s)...{}", file_count, note);
        Ok(())
    }

    async fn paste_download(&mut self, clipboard: FileClipboard) -> Result<()> {
        let source = match &self.right_source {
            Some(s) => Arc::clone(s),
            None => {
                self.status = String::from("No remote source");
                return Ok(());
            }
        };
        
        let dest = match &self.left_source {
            Some(s) => Arc::clone(s),
            None => {
                self.status = String::from("No local source");
                return Ok(());
            }
        };

        // Collect files from clipboard items, expanding directories (flat)
        let mut collected = CollectedFiles::new();
        for item in &clipboard.items {
            let entries = source.walk(&item.full_path, self.show_hidden).await?;
            mapping::map_to_destination(false, &item.name, entries, &mut collected);
        }

        if collected.is_empty() {
            self.status = String::from("No files to transfer");
            return Ok(());
        }

        let dest_base = self.local_tree.root_path.clone();
        let file_count = collected.file_count();
        let note = collected.skipped_note();
        self.spawn_transfer(source, dest, collected, dest_base, TransferDirection::Download);

        self.status = format!("Transferring {} file(s)...{}", file_count, note);
        Ok(())
    }

    fn paste_local(&mut self, clipboard: FileClipboard) -> Result<()> {
        let dest_dir = PathBuf::from(&self.local_tree.root_path);
        let mut success_count = 0;
        let mut error_count = 0;

        for item in &clipboard.items {
            let dest_path = dest_dir.join(&item.name);
            let source_path = PathBuf::from(&item.full_path);

            // Skip if source == dest
            if source_path == dest_path {
                continue;
            }

            let result = if item.is_dir {
                // Recursively copy/move directory
                match clipboard.operation {
                    ClipboardOp::Copy => Self::copy_dir_recursive(&source_path, &dest_path),
                    ClipboardOp::Cut => std::fs::rename(&source_path, &dest_path).map_err(Into::into),
                }
            } else {
                match clipboard.operation {
                    ClipboardOp::Copy => std::fs::copy(&source_path, &dest_path).map(|_| ()).map_err(Into::into),
                    ClipboardOp::Cut => std::fs::rename(&source_path, &dest_path).map_err(Into::into),
                }
            };

            match result {
                Ok(()) => success_count += 1,
                Err(_) => error_count += 1,
            }
        }

        let op_name = match clipboard.operation {
            ClipboardOp::Copy => "Copied",
            ClipboardOp::Cut => "Moved",
        };

        if error_count > 0 {
            self.status = format!("{} {} item(s), {} failed", op_name, success_count, error_count);
        } else {
            self.status = format!("{} {} item(s)", op_name, success_count);
        }

        Ok(())
    }

    fn copy_dir_recursive(src: &PathBuf, dst: &PathBuf) -> Result<()> {
        std::fs::create_dir_all(dst)?;
        
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let entry_path = entry.path();
            let dest_path = dst.join(entry.file_name());
            
            if entry_path.is_dir() {
                Self::copy_dir_recursive(&entry_path, &dest_path)?;
            } else {
                std::fs::copy(&entry_path, &dest_path)?;
            }
        }
        
        Ok(())
    }

    /// Start a transfer using the new executor system
    /// source_pane: which pane to transfer FROM
    /// preserve_structure: whether to maintain directory structure
    async fn start_transfer_from_pane(
        &mut self,
        source_pane: Pane,
        preserve_structure: bool,
    ) -> Result<()> {
        if self.transfer.is_some() {
            self.status = String::from("Transfer already in progress");
            return Ok(());
        }

        // Get source and destination based on source_pane
        let dest_pane = match source_pane {
            Pane::Local => Pane::Remote,
            Pane::Remote => Pane::Local,
        };
        let source = self.pane_source(source_pane)?;
        let dest = self.pane_source(dest_pane)?;
        let (source_tree, dest_base) = match source_pane {
            Pane::Local => (&self.local_tree, self.remote_tree.root_path.clone()),
            Pane::Remote => (&self.remote_tree, self.local_tree.root_path.clone()),
        };

        // Get selected files from source tree
        let selected = source_tree.get_selected_for_transfer();
        if selected.is_empty() {
            self.status = String::from("No files selected");
            return Ok(());
        }

        // Enumerate each selected subtree, then let the canonical mapping
        // decide every destination path. The anchor is the node's path
        // relative to the source pane root - what the user sees.
        let mut collected = CollectedFiles::new();
        for node in &selected {
            let entries = source.walk(&node.full_path, self.show_hidden).await?;
            mapping::map_to_destination(
                preserve_structure,
                &node.relative_path,
                entries,
                &mut collected,
            );
        }

        if collected.is_empty() {
            self.status = if collected.skipped_symlinks > 0 {
                format!("No files to transfer{}", collected.skipped_note())
            } else {
                String::from("No files found in selection")
            };
            return Ok(());
        }

        // Check for collisions in flat mode
        if !preserve_structure {
            let mut seen: HashSet<String> = HashSet::new();
            let mut duplicates = Vec::new();
            for file in &collected.files {
                if !seen.insert(file.relative_path.clone()) {
                    duplicates.push(file.relative_path.clone());
                }
            }
            if !duplicates.is_empty() {
                self.input.push(InputContext::CollisionWarning { files: duplicates });
                return Ok(());
            }
        }

        let direction = match source_pane {
            Pane::Local => TransferDirection::Upload,
            Pane::Remote => TransferDirection::Download,
        };

        // Overwrite check: find everything that already exists at the
        // destination and ask once for the whole batch - never per file.
        let mut existing = Vec::new();
        for file in &collected.files {
            let dest_path = dest.join_path(&dest_base, &file.relative_path);
            if dest.exists(&dest_path).await.unwrap_or(false) {
                existing.push(file.relative_path.clone());
            }
        }

        let pending = PendingTransfer {
            source,
            dest,
            collected,
            dest_base,
            direction,
        };

        if existing.is_empty() {
            self.launch_transfer(pending);
        } else {
            self.input.push(InputContext::OverwriteConfirm {
                files: existing,
                pending,
            });
        }

        Ok(())
    }

    /// Download from remote to local (flat)
    pub async fn start_download_flat(&mut self) -> Result<()> {
        self.start_transfer_from_pane(Pane::Remote, false).await
    }

    /// Download from remote to local (preserve structure)
    pub async fn start_download_preserve(&mut self) -> Result<()> {
        self.start_transfer_from_pane(Pane::Remote, true).await
    }

    /// Upload from local to remote (flat)
    pub async fn start_upload_flat(&mut self) -> Result<()> {
        self.start_transfer_from_pane(Pane::Local, false).await
    }

    /// Upload from local to remote (preserve structure)
    pub async fn start_upload_preserve(&mut self) -> Result<()> {
        self.start_transfer_from_pane(Pane::Local, true).await
    }
}
