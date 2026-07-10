use anyhow::Result;

use super::SystemClipboard;

/// Fallback clipboard implementation using arboard (text-only for non-Windows)
pub struct FallbackClipboard {
    clipboard: Option<arboard::Clipboard>,
}

impl FallbackClipboard {
    pub fn new() -> Self {
        Self {
            clipboard: arboard::Clipboard::new().ok(),
        }
    }
}

impl SystemClipboard for FallbackClipboard {
    fn set_text(&mut self, text: &str) -> Result<()> {
        if let Some(ref mut cb) = self.clipboard {
            cb.set_text(text)?;
        }
        Ok(())
    }
}
