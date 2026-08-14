//! Visual helpers: magnifier, grid, crosshair, XY values

use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC,
};

use super::common::*;
use super::state::EditorState;

// ===== Magnifier =====

pub unsafe fn draw_magnifier(hdc: HDC, hdc_screen: HDC, st: &EditorState) {
    let mag_y = 16;
    // When the mouse is in the top-left corner, place the magnifier top-right; otherwise top-left
    let mag_x = if st.mouse_x < st.screen_w / 3 && st.mouse_y < st.screen_h / 3 {
        st.screen_w - MAG_SIZE - 20
    } else {
        20
    };

    let cap_x = (st.mouse_x - MAG_CAPTURE / 2).clamp(0, st.screen_w.saturating_sub(MAG_CAPTURE));
    let cap_y = (st.mouse_y - MAG_CAPTURE / 2).clamp(0, st.screen_h.saturating_sub(MAG_CAPTURE));

    let temp_dc = CreateCompatibleDC(hdc_screen);
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: MAG_CAPTURE, biHeight: -MAG_CAPTURE, biPlanes: 1, biBitCount: 32,
            biCompression: BI_RGB.0, ..Default::default()
        }, ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let cap_bmp = match CreateDIBSection(temp_dc, &bi, DIB_RGB_COLORS, &mut bits, None, 0) {
        Ok(b) => b,
        Err(_) => { let _ = DeleteDC(temp_dc); return; }
    };
    let old_cap = SelectObject(temp_dc, cap_bmp);

    #[link(name = "gdi32")]
    extern "system" {
        fn BitBlt(hdc: isize, x: i32, y: i32, cx: i32, cy: i32, hdcsrc: isize, x1: i32, y1: i32, rop: u32) -> i32;
        fn StretchBlt(hdcdst: isize, xdst: i32, ydst: i32, wdst: i32, hdst: i32, hdcsrc: isize, xsrc: i32, ysrc: i32, wsrc: i32, hsrc: i32, rop: u32) -> i32;
    }
    const SRCCOPY: u32 = 0x00CC0020;

    let _ = BitBlt(temp_dc.0 as isize, 0, 0, MAG_CAPTURE, MAG_CAPTURE, hdc_screen.0 as isize, cap_x, cap_y, SRCCOPY);
    let _ = StretchBlt(hdc.0 as isize, mag_x, mag_y, MAG_SIZE, MAG_SIZE, temp_dc.0 as isize, 0, 0, MAG_CAPTURE, MAG_CAPTURE, SRCCOPY);

    SelectObject(temp_dc, old_cap);
    let _ = DeleteObject(cap_bmp);
    let _ = DeleteDC(temp_dc);

    // Border
    draw_rect_outline(hdc, mag_x, mag_y, MAG_SIZE, MAG_SIZE, 2, rgb(255, 220, 60));
    // Center cross
    let cx = mag_x + MAG_SIZE / 2;
    let cy = mag_y + MAG_SIZE / 2;
    fill_rect_solid(hdc, cx - 6, cy - 1, 12, 2, rgb(255, 80, 80));
    fill_rect_solid(hdc, cx - 1, cy - 6, 2, 12, rgb(255, 80, 80));
}

// ===== Grid =====

pub unsafe fn draw_grid(hdc: HDC, st: &EditorState) {
    // Edge tick color (position only; no more full-screen 100px guide lines)
    let tick_color = rgb(46, 52, 70);
    let label_color = rgb(95, 105, 128);

    // ===== Resolution quartering guide lines =====
    // 1/2 line: most visible (2px); 1/4 lines: less; 1/8, 1/16 lines: weakest
    let (sw, sh) = (st.screen_w, st.screen_h);
    let weak_color = tick_color;

    // 1/16 lines (skip positions where 1/4 and 1/2 lines run)
    for k in 1..16 {
        if k % 4 == 0 {
            continue;
        }
        let lx = sw * k / 16;
        fill_rect_solid(hdc, lx, 0, 1, sh, weak_color);
    }
    for k in 1..16 {
        if k % 4 == 0 {
            continue;
        }
        let ly = sh * k / 16;
        fill_rect_solid(hdc, 0, ly, sw, 1, weak_color);
    }

    // 1/4 lines
    let quarter_color = rgb(0x5a, 0x64, 0x8c);
    for k in 1..4 {
        let lx = sw * k / 4;
        fill_rect_solid(hdc, lx, 0, 1, sh, quarter_color);
    }
    for k in 1..4 {
        let ly = sh * k / 4;
        fill_rect_solid(hdc, 0, ly, sw, 1, quarter_color);
    }

    // 1/2 line (most visible, 2px)
    let center_color = rgb(0x96, 0xa0, 0xd7);
    let cx = sw / 2;
    let cy = sh / 2;
    fill_rect_solid(hdc, cx - 1, 0, 2, sh, center_color);
    fill_rect_solid(hdc, 0, cy - 1, sw, 2, center_color);

    // ===== Screen edge ticks (every 100, 25px tick + number only) =====
    const EDGE_TICK: i32 = 25;
    let mut x = GRID_SPACING;
    while x < st.screen_w {
        fill_rect_solid(hdc, x, 0, 1, EDGE_TICK, tick_color);
        gdi_text(hdc, &x.to_string(), x - 20, 4, 40, 18, label_color);
        x += GRID_SPACING;
    }
    let mut y = GRID_SPACING;
    while y < st.screen_h {
        fill_rect_solid(hdc, 0, y, EDGE_TICK, 1, tick_color);
        gdi_text_left(hdc, &y.to_string(), 4, y - 9, 40, 18, label_color);
        y += GRID_SPACING;
    }
}

// ===== Crosshair =====

pub unsafe fn draw_crosshair(hdc: HDC, st: &EditorState) {
    let color = rgb(80, 200, 120);
    fill_rect_solid(hdc, st.mouse_x, 0, 1, st.screen_h, color);
    fill_rect_solid(hdc, 0, st.mouse_y, st.screen_w, 1, color);
}

// ===== XY values =====

pub unsafe fn draw_xy_label(hdc: HDC, st: &EditorState) {
    let text = format!("X:{} Y:{}", st.mouse_x, st.mouse_y);
    let lx = (st.mouse_x + 20).clamp(0, st.screen_w.saturating_sub(160));
    let ly = (st.mouse_y - 30).max(0);
    fill_rect_solid(hdc, lx, ly, 140, 22, rgb(20, 30, 20));
    draw_rect_outline(hdc, lx, ly, 140, 22, 1, rgb(60, 120, 60));
    gdi_text(hdc, &text, lx, ly, 140, 22, rgb(120, 230, 140));
}
