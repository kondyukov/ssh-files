use anyhow::{anyhow, Result};

use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
};
use windows_sys::Win32::Foundation::HANDLE;

use super::SystemClipboard;

const CF_UNICODETEXT: u32 = 13;

pub struct WindowsClipboard;

impl WindowsClipboard {
    pub fn new() -> Self {
        Self
    }

    fn open_clipboard(&self) -> Result<ClipboardGuard> {
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return Err(anyhow!("Failed to open clipboard"));
            }
        }
        Ok(ClipboardGuard)
    }
}

/// RAII guard for clipboard - closes on drop
struct ClipboardGuard;

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}

impl SystemClipboard for WindowsClipboard {
    fn set_text(&mut self, text: &str) -> Result<()> {
        let _guard = self.open_clipboard()?;

        unsafe {
            EmptyClipboard();
        }

        // Convert to wide string with null terminator
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let size = wide.len() * 2;

        unsafe {
            let hmem = GlobalAlloc(GMEM_MOVEABLE, size);
            if hmem.is_null() {
                return Err(anyhow!("GlobalAlloc failed"));
            }

            let ptr = GlobalLock(hmem);
            if ptr.is_null() {
                return Err(anyhow!("GlobalLock failed"));
            }

            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr as *mut u16, wide.len());
            GlobalUnlock(hmem);

            if SetClipboardData(CF_UNICODETEXT, hmem as HANDLE).is_null() {
                return Err(anyhow!("SetClipboardData failed"));
            }
        }

        Ok(())
    }
}

impl Default for WindowsClipboard {
    fn default() -> Self {
        Self::new()
    }
}
