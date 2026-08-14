//! Transparent always-on-top overlay window
//!
//! UpdateLayeredWindow + premultiplied alpha BGRA gives true per-pixel transparency (no color-key holes).
//! Click-through: WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE.
//! This is the classic window signature of an "overlay addon" (see design doc §3.2 / §10 R5) and is the intentional core approach.

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, BLENDFUNCTION, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject,
    DIB_RGB_COLORS, GetDC, HDC, HBITMAP, HGDIOBJ, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, LoadCursorW,
    RegisterClassExW, ShowWindow, UpdateLayeredWindow,
    WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSEXW, CS_HREDRAW, CS_VREDRAW,
    IDC_ARROW, SW_SHOWNOACTIVATE, ULW_ALPHA, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::error::{AppError, AppResult};

/// Transparent overlay window
#[allow(dead_code)]
pub struct OverlayWindow {
    pub hwnd: HWND,
    pub width: i32,
    pub height: i32,
    /// Cached DIB section (avoids per-frame CreateDIBSection/DeleteObject)
    dib_hbmp: HBITMAP,
    /// DIB pixel data pointer (written directly, no intermediate buffer copy)
    dib_bits: *mut u8,
    /// Cached compatible DC (bitmap already selected in; avoids per-frame CreateCompatibleDC/DeleteDC)
    mem_dc: HDC,
    /// Cached screen DC (avoids per-frame GetDC/ReleaseDC)
    screen_dc: HDC,
    /// Old object returned by SelectObject (restored on Drop)
    old_bmp: HGDIOBJ,
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        unsafe {
            // Restore the old bitmap, then clean up the cached GDI resources
            SelectObject(self.mem_dc, self.old_bmp);
            let _ = DeleteObject(self.dib_hbmp);
            let _ = DeleteDC(self.mem_dc);
            let _ = ReleaseDC(None, self.screen_dc);
            let _ = DestroyWindow(self.hwnd);
            log::info!("overlay window destroyed");
        }
    }
}

impl OverlayWindow {
    /// Create a transparent always-on-top click-through window
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> AppResult<Self> {
        unsafe {
            let class_name = w!("APortalOverlayClass");

            // Register the window class
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(def_window_proc),
                hCursor: LoadCursorW(None, IDC_ARROW)
                    .map_err(|e| AppError::windows("LoadCursorW", e))?,
                lpszClassName: class_name,
                style: CS_HREDRAW | CS_VREDRAW,
                ..Default::default()
            };
            RegisterClassExW(&wc);

            // Click-through + always-on-top + no focus steal + layered (per-pixel alpha)
            let ex_style = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE;

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(ex_style.0),
                class_name,
                w!("APortal"),
                WINDOW_STYLE(WS_POPUP.0),
                x,
                y,
                width,
                height,
                None,
                None,
                None,
                None,
            )
            .map_err(|e| AppError::windows("CreateWindowExW", e))?;

            log::info!("overlay window created: {}x{} @ ({},{})", width, height, x, y);

            // Pre-create the DIB section + compatible DC (cached, reused every frame)
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // negative = top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    biSizeImage: (width * height * 4) as u32,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                ..Default::default()
            };
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let dib_hbmp: HBITMAP = CreateDIBSection(
                HDC::default(),
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                None,
                0,
            )
            .map_err(|e| AppError::windows("CreateDIBSection (cached)", e))?;
            if bits.is_null() {
                return Err(AppError::other("CreateDIBSection bits is null"));
            }
            let dib_bits = bits as *mut u8;

            let hdc_screen = GetDC(None);
            let mem_dc = CreateCompatibleDC(hdc_screen);
            let old_bmp = SelectObject(mem_dc, dib_hbmp);

            Ok(Self {
                hwnd,
                width,
                height,
                dib_hbmp,
                dib_bits,
                mem_dc,
                screen_dc: hdc_screen,
                old_bmp,
            })
        }
    }

    /// Show the window (without stealing focus)
    pub fn show(&self) -> AppResult<()> {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        Ok(())
    }

    /// Raw pointer to the DIB pixel buffer (for render_regions to write directly, skipping an intermediate buffer)
    pub fn dib_ptr(&self) -> *mut u8 {
        self.dib_bits
    }

    /// Total byte size of the DIB buffer (width * height * 4)
    pub fn buf_len(&self) -> usize {
        (self.width * self.height * 4) as usize
    }

    /// Present the cached DIB pixels to the window (zero-copy, direct UpdateLayeredWindow).
    /// Make sure dib_bits holds premultiplied BGRA data before calling.
    pub fn present(&self) -> AppResult<()> {
        unsafe {
            let pt_zero = POINT { x: 0, y: 0 };
            let size = SIZE {
                cx: self.width,
                cy: self.height,
            };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_ALPHA as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255, // use per-pixel alpha
                AlphaFormat: AC_SRC_ALPHA as u8,
            };

            let result = UpdateLayeredWindow(
                self.hwnd,
                self.screen_dc,
                None,          // pptdst: window position unchanged
                Some(&size),   // psize
                self.mem_dc,   // cached DC (bitmap already selected in)
                Some(&pt_zero),// pptsrc
                COLORREF(0),
                Some(&blend),  // pblend
                ULW_ALPHA,
            );

            result.map_err(|e| AppError::windows("UpdateLayeredWindow", e))
        }
    }
}

/// Default window proc
extern "system" fn def_window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
