mod types;

#[cfg(windows)]
mod windows;

#[cfg(all(not(windows), feature = "clipboard"))]
mod fallback;

pub use types::*;

use anyhow::Result;

/// Platform-agnostic clipboard provider
pub trait SystemClipboard: Send {
    /// Set plain text to clipboard
    fn set_text(&mut self, text: &str) -> Result<()>;
}

/// Create platform-appropriate clipboard provider
pub fn create_clipboard() -> Box<dyn SystemClipboard> {
    #[cfg(windows)]
    {
        Box::new(windows::WindowsClipboard::new())
    }

    #[cfg(all(not(windows), feature = "clipboard"))]
    {
        Box::new(fallback::FallbackClipboard::new())
    }

    #[cfg(all(not(windows), not(feature = "clipboard")))]
    {
        Box::new(NoopClipboard)
    }
}

/// No-op clipboard for when clipboard feature is disabled
#[cfg(all(not(windows), not(feature = "clipboard")))]
struct NoopClipboard;

#[cfg(all(not(windows), not(feature = "clipboard")))]
impl SystemClipboard for NoopClipboard {
    fn set_text(&mut self, _text: &str) -> Result<()> {
        Ok(()) // Silently do nothing
    }
}
