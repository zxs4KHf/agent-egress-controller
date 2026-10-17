//! Small Win32 helpers: strings, clipboard, opening files, tray icons.

use std::ffi::c_void;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{GetLastError, GlobalFree, HWND};
use windows_sys::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, GetDC, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, RGBQUAD,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, HICON, ICONINFO, SW_SHOWNORMAL,
};

const CF_UNICODETEXT: u32 = 13;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copy `s` into a fixed UTF-16 buffer, truncated and NUL-terminated.
pub fn fill(buf: &mut [u16], s: &str) {
    let units: Vec<u16> = s.encode_utf16().take(buf.len() - 1).collect();
    buf[..units.len()].copy_from_slice(&units);
    buf[units.len()] = 0;
}

/// `owner` must be a window of this thread: with no owner, SetClipboardData always fails.
pub fn set_clipboard(owner: HWND, text: &str) -> Result<(), String> {
    let units = wide(text);
    let bytes = units.len() * 2;
    unsafe {
        if OpenClipboard(owner) == 0 {
            return Err(format!("OpenClipboard failed: error {}", GetLastError()));
        }
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if h.is_null() {
            CloseClipboard();
            return Err("GlobalAlloc failed".into());
        }
        let p = GlobalLock(h) as *mut u16;
        std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
        GlobalUnlock(h);
        // On success the clipboard owns the memory.
        if SetClipboardData(CF_UNICODETEXT, h as _).is_null() {
            let e = GetLastError();
            GlobalFree(h);
            CloseClipboard();
            return Err(format!("SetClipboardData failed: error {e}"));
        }
        CloseClipboard();
    }
    Ok(())
}

/// Open a file, folder or URL with its default program.
pub fn shell_open(target: &str) {
    let (verb, t) = (wide("open"), wide(target));
    unsafe {
        ShellExecuteW(
            null_mut(),
            verb.as_ptr(),
            t.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
}

/// The shield/routes product mark in the current mode colour, with premultiplied alpha.
pub fn dot_icon(rgb: (u8, u8, u8)) -> HICON {
    const N: i32 = 32;
    unsafe {
        let hdc = GetDC(null_mut());
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: N,
                biHeight: -N,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [RGBQUAD {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbReserved: 0,
            }],
        };
        let mut bits: *mut c_void = null_mut();
        let color = CreateDIBSection(hdc, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        ReleaseDC(null_mut(), hdc);
        let px = std::slice::from_raw_parts_mut(bits as *mut u32, (N * N) as usize);
        for (pixel, rgba) in px
            .iter_mut()
            .zip(aec_core::brand::rgba(N as usize, rgb).as_chunks::<4>().0)
        {
            let alpha = u32::from(rgba[3]);
            let pm = |c: u8| u32::from(c) * alpha / 255;
            *pixel = alpha << 24 | pm(rgba[0]) << 16 | pm(rgba[1]) << 8 | pm(rgba[2]);
        }
        let mask = CreateBitmap(N, N, 1, 1, null());
        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info);
        DeleteObject(mask);
        DeleteObject(color);
        icon
    }
}

pub fn message_box(hwnd: HWND, text: &str, title: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK};
    let (t, c) = (wide(text), wide(title));
    unsafe { MessageBoxW(hwnd, t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONWARNING) };
}

pub fn error_box(hwnd: HWND, text: &str, title: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let (t, c) = (wide(text), wide(title));
    unsafe { MessageBoxW(hwnd, t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONERROR) };
}
