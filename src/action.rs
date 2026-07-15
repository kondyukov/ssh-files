use crate::app::Pane;
use crate::transfer::TransferDirection;

/// How transferred files are placed at the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferMode {
    /// All items go to the target root under their own name.
    Flat,
    /// Items keep their relative path from the source root.
    Preserve,
}

/// A semantic operation on the application, decoupled from the input
/// (key, mouse, context menu entry) that triggered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    // Navigation
    MoveUp,
    MoveDown,
    PageUp,
    PageDown,
    GoToTop,
    GoToBottom,
    EnterDir,
    GoUp,
    ToggleExpand,
    FocusPane(Pane),
    // Selection
    ToggleSelect,
    SelectAll,
    DeselectAll,
    // Transfers
    Transfer {
        direction: TransferDirection,
        mode: TransferMode,
    },
    /// Copy the rsync command equivalent to the transfer the current
    /// selection would perform into the system clipboard. Text only:
    /// nothing is ever executed on the user's behalf.
    CopyRsync {
        direction: TransferDirection,
        mode: TransferMode,
    },
    // Clipboard (copy/cut/paste) is deferred past this release: the
    // variants and their dispatch machinery stay as disabled stubs, but
    // nothing constructs them - the menu entries and keymap registrations
    // are commented out until the feature lands completely and cleanly.
    #[allow(dead_code)]
    ClipboardCopy,
    #[allow(dead_code)]
    ClipboardCut,
    #[allow(dead_code)]
    Paste,
    CopyPath,
    // Modals
    StartRename,
    ConfirmRename,
    CancelRename,
    StartDelete,
    ConfirmDelete,
    CancelDelete,
    ToggleHelp,
    /// Open the context menu anchored at the focused pane's cursor row
    /// (the keyboard counterpart of a right-click).
    OpenContextMenu,
    // Misc
    ToggleHidden,
    Refresh,
    Quit,
}
