use crate::app::Pane;

/// Clipboard operation type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardOp {
    Copy,
    Cut,
}

/// Item in the internal file clipboard
#[derive(Debug, Clone)]
pub struct ClipboardItem {
    pub full_path: String,
    pub name: String,
    pub is_dir: bool,
}

/// Internal file clipboard state
#[derive(Debug, Clone)]
pub struct FileClipboard {
    pub operation: ClipboardOp,
    pub source_pane: Pane,
    pub items: Vec<ClipboardItem>,
}

impl FileClipboard {
    pub fn new(
        operation: ClipboardOp,
        source_pane: Pane,
        items: Vec<ClipboardItem>,
    ) -> Self {
        Self {
            operation,
            source_pane,
            items,
        }
    }

    /// Get paths as newline-separated text
    pub fn paths_as_text(&self) -> String {
        self.items
            .iter()
            .map(|item| item.full_path.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}
