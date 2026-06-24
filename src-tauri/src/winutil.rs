//! Thin Win32 helpers used by the cursor overlay and the text paster.
//!
//! Everything platform-specific is gated behind `cfg(windows)`; the non-Windows
//! stubs let the crate still type-check on other targets even though Orochi only
//! ships on Windows.

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
    use windows::Win32::Graphics::Gdi::{
        CreateBitmap, CreateDIBSection, DeleteObject, GetDC, ReleaseDC, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
    };
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateIconIndirect, SetSystemCursor, SystemParametersInfoW, HCURSOR, ICONINFO,
        SPIF_SENDCHANGE, SPI_SETCURSORS, SYSTEM_CURSOR_ID,
    };

    const CF_UNICODETEXT: u32 = 13;
    const VK_V: u16 = 0x56;

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

    // ---------- System cursor replacement ----------

    // The standard system cursors we override / restore (OCR_* ids).
    const OCR_IDS: [u32; 14] = [
        32512, 32513, 32514, 32515, 32516, 32642, 32643, 32644, 32645, 32646, 32648, 32649, 32650,
        32651,
    ];

    /// Build an alpha-blended HCURSOR from a straight-alpha RGBA buffer (`w`×`h`,
    /// top-down rows) with its hotspot at (`hx`, `hy`). Returns None on any GDI
    /// failure. The caller is responsible for handing the result to a consumer
    /// (e.g. SetSystemCursor) that takes ownership of the handle.
    unsafe fn make_cursor(rgba: &[u8], w: u32, h: u32, hx: u32, hy: u32) -> Option<HCURSOR> {
        if w == 0 || h == 0 || rgba.len() < (w as usize * h as usize * 4) {
            return None;
        }

        // 32bpp top-down DIB (negative height) for the colour plane.
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w as i32;
        bmi.bmiHeader.biHeight = -(h as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0 as u32;

        let hdc = GetDC(None);
        let mut bits: *mut c_void = std::ptr::null_mut();
        let hbm_color = CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok();
        ReleaseDC(None, hdc);
        let hbm_color = hbm_color?;
        if bits.is_null() {
            let _ = DeleteObject(HGDIOBJ(hbm_color.0));
            return None;
        }

        // RGBA -> premultiplied BGRA, which the icon alpha-blend path expects.
        let px = (w as usize) * (h as usize);
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, px * 4);
        for i in 0..px {
            let r = rgba[i * 4] as u32;
            let g = rgba[i * 4 + 1] as u32;
            let b = rgba[i * 4 + 2] as u32;
            let a = rgba[i * 4 + 3] as u32;
            dst[i * 4] = ((b * a) / 255) as u8;
            dst[i * 4 + 1] = ((g * a) / 255) as u8;
            dst[i * 4 + 2] = ((r * a) / 255) as u8;
            dst[i * 4 + 3] = a as u8;
        }

        // Monochrome AND mask, all zero — the colour plane's alpha does the work.
        let hbm_mask = CreateBitmap(w as i32, h as i32, 1, 1, None);

        let ii = ICONINFO {
            fIcon: false.into(), // FALSE => cursor (hotspot honoured)
            xHotspot: hx,
            yHotspot: hy,
            hbmMask: hbm_mask,
            hbmColor: hbm_color,
        };
        let cursor = CreateIconIndirect(&ii).ok();

        // CreateIconIndirect copies the bitmaps; free our originals either way.
        let _ = DeleteObject(HGDIOBJ(hbm_color.0));
        let _ = DeleteObject(HGDIOBJ(hbm_mask.0));

        cursor.map(|c| HCURSOR(c.0))
    }

    /// Replace every standard system cursor with the supplied image. Returns true
    /// if at least one slot was set.
    pub fn set_system_cursor_image(rgba: &[u8], w: u32, h: u32, hx: u32, hy: u32) -> bool {
        unsafe {
            let mut applied = false;
            for id in OCR_IDS {
                // SetSystemCursor destroys the handle it's given, so build a fresh
                // cursor for each slot.
                if let Some(cur) = make_cursor(rgba, w, h, hx, hy) {
                    if SetSystemCursor(cur, SYSTEM_CURSOR_ID(id)).is_ok() {
                        applied = true;
                    }
                }
            }
            applied
        }
    }

    /// Reload every system cursor from the registry defaults.
    pub fn restore_system_cursors() {
        unsafe {
            let _ = SystemParametersInfoW(SPI_SETCURSORS, 0, None, SPIF_SENDCHANGE);
        }
    }
}

#[cfg(windows)]
pub use imp::*;

#[cfg(not(windows))]
mod imp {
    pub fn clipboard_get_text() -> Option<String> {
        None
    }
    pub fn clipboard_set_text(_text: &str) -> bool {
        false
    }
    pub fn send_paste() {}
    pub fn set_system_cursor_image(_rgba: &[u8], _w: u32, _h: u32, _hx: u32, _hy: u32) -> bool {
        false
    }
    pub fn restore_system_cursors() {}
}

#[cfg(not(windows))]
pub use imp::*;
