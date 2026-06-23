//! Thin Win32 helpers used by the cursor overlay and the text paster.
//!
//! Everything platform-specific is gated behind `cfg(windows)`; the non-Windows
//! stubs let the crate still type-check on other targets even though Orochi only
//! ships on Windows.

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, POINT};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateCursor, GetCursorPos, SetSystemCursor, SystemParametersInfoW, SPIF_SENDCHANGE,
        SPI_SETCURSORS, SYSTEM_CURSOR_ID,
    };

    const CF_UNICODETEXT: u32 = 13;
    const VK_V: u16 = 0x56;

    /// Current mouse position in physical screen pixels (virtual-desktop coords).
    pub fn cursor_pos() -> Option<(i32, i32)> {
        let mut p = POINT::default();
        unsafe { GetCursorPos(&mut p).ok()? };
        Some((p.x, p.y))
    }

    // ---------- Clipboard ----------

    fn open_clipboard() -> bool {
        unsafe { OpenClipboard(Some(HWND(std::ptr::null_mut()))).is_ok() }
    }

    pub fn clipboard_get_text() -> Option<String> {
        if !open_clipboard() {
            return None;
        }
        let text = unsafe {
            GetClipboardData(CF_UNICODETEXT).ok().and_then(|h| {
                let ptr = GlobalLock(HGLOBAL(h.0)) as *const u16;
                if ptr.is_null() {
                    return None;
                }
                // Read until the terminating NUL.
                let mut len = 0usize;
                while *ptr.add(len) != 0 {
                    len += 1;
                }
                let slice = std::slice::from_raw_parts(ptr, len);
                let s = String::from_utf16_lossy(slice);
                let _ = GlobalUnlock(HGLOBAL(h.0));
                Some(s)
            })
        };
        unsafe { let _ = CloseClipboard(); }
        text
    }

    pub fn clipboard_set_text(text: &str) -> bool {
        let mut utf16: Vec<u16> = text.encode_utf16().collect();
        utf16.push(0);
        let bytes = utf16.len() * std::mem::size_of::<u16>();

        if !open_clipboard() {
            return false;
        }
        let ok = unsafe {
            let _ = EmptyClipboard();
            match GlobalAlloc(GMEM_MOVEABLE, bytes) {
                Ok(hmem) => {
                    let dst = GlobalLock(hmem) as *mut u16;
                    if dst.is_null() {
                        false
                    } else {
                        std::ptr::copy_nonoverlapping(utf16.as_ptr(), dst, utf16.len());
                        let _ = GlobalUnlock(hmem);
                        // Ownership of hmem transfers to the system on success.
                        SetClipboardData(CF_UNICODETEXT, Some(HANDLE(hmem.0))).is_ok()
                    }
                }
                Err(_) => false,
            }
        };
        unsafe { let _ = CloseClipboard(); }
        ok
    }

    // ---------- Synthesized input ----------

    fn key_event(vk: u16, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn send(inputs: &[INPUT]) {
        unsafe {
            SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
        }
    }

    /// Release any modifier keys the user may still be holding from the triggering
    /// hotkey, then synthesize a clean Ctrl+V.
    pub fn send_paste() {
        for vk in [
            VK_CONTROL.0,
            VK_MENU.0,
            VK_SHIFT.0,
            VK_LWIN.0,
            VK_RWIN.0,
        ] {
            send(&[key_event(vk, true)]);
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
        send(&[
            key_event(VK_CONTROL.0, false),
            key_event(VK_V, false),
            key_event(VK_V, true),
            key_event(VK_CONTROL.0, true),
        ]);
    }

    // ---------- System cursor hiding ----------

    // The standard system cursors we blank out / restore (OCR_* ids).
    const OCR_IDS: [u32; 14] = [
        32512, 32513, 32514, 32515, 32516, 32642, 32643, 32644, 32645, 32646, 32648, 32649, 32650,
        32651,
    ];

    fn make_blank_cursor() -> Option<windows::Win32::UI::WindowsAndMessaging::HCURSOR> {
        // 32x32: AND mask all 1s, XOR mask all 0s => fully transparent.
        let and_mask = [0xFFu8; 128];
        let xor_mask = [0x00u8; 128];
        unsafe {
            CreateCursor(
                None,
                0,
                0,
                32,
                32,
                and_mask.as_ptr() as *const c_void,
                xor_mask.as_ptr() as *const c_void,
            )
            .ok()
        }
    }

    pub fn set_system_cursor_hidden(hide: bool) {
        unsafe {
            if hide {
                for id in OCR_IDS {
                    // SetSystemCursor destroys the handle it's given, so make a
                    // fresh blank cursor for each slot.
                    if let Some(cur) = make_blank_cursor() {
                        let _ = SetSystemCursor(cur, SYSTEM_CURSOR_ID(id));
                    }
                }
            } else {
                // Reload every system cursor from the registry defaults.
                let _ = SystemParametersInfoW(SPI_SETCURSORS, 0, None, SPIF_SENDCHANGE);
            }
        }
    }
}

#[cfg(windows)]
pub use imp::*;

#[cfg(not(windows))]
mod imp {
    pub fn cursor_pos() -> Option<(i32, i32)> {
        None
    }
    pub fn clipboard_get_text() -> Option<String> {
        None
    }
    pub fn clipboard_set_text(_text: &str) -> bool {
        false
    }
    pub fn send_paste() {}
    pub fn set_system_cursor_hidden(_hide: bool) {}
}

#[cfg(not(windows))]
pub use imp::*;
