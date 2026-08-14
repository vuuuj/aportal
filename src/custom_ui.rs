//! Custom UI elements: pre-rendered at startup, alpha-over composited in pure memory per frame
//!
//! Two element kinds:
//! - frame: outlined rounded rectangle
//! - background: filled rounded rectangle

use std::path::Path;

use crate::config::{BackgroundElement, CustomUiElement, FrameElement, ImageElement, TextElement};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC,
    CreateDIBSection, CreateFontW, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteDC,
    DeleteObject, DIB_RGB_COLORS, DrawTextW, GetDC, GetTextExtentPoint32W, ReleaseDC,
    SelectObject, SetBkColor, SetBkMode, SetTextColor, TRANSPARENT,
};

/// Pre-rendered UI element (built at startup, only blitted per frame)
pub struct RenderedUi {
    /// Premultiplied BGRA pixels
    pub pixels: Vec<u8>,
    pub w: u32,
    pub h: u32,
    /// Target position inside the overlay
    pub x: i32,
    pub y: i32,
    pub z_order: i32,
    /// Rotation in degrees clockwise applied at pre-render time (0 = none)
    pub rotate: i32,
    /// true = stencil mode: pixel alpha means "cutout strength" (255 = fully cut away the
    /// layer below, revealing the desktop)
    pub stencil: bool,
}

/// Called at startup: pre-renders all custom_ui elements into pixel buffers, sorted by z_order
/// global_opacity: effective global opacity when an element has no explicit opacity
pub fn prerender_ui(elements: &[CustomUiElement], exe_dir: &Path, global_opacity: f32) -> Vec<RenderedUi> {
    let mut rendered: Vec<RenderedUi> = Vec::new();

    for (i, elem) in elements.iter().enumerate() {
        log::info!("pre-render UI element #{}: {:?}", i, elem);
        log::logger().flush(); // ensure logs are written before a crash
        let result = match elem {
            CustomUiElement::Frame(f) => render_frame_gdi(f, global_opacity),
            CustomUiElement::Background(b) => render_background_gdi(b, global_opacity),
            CustomUiElement::Image(img) => render_image(img, exe_dir, global_opacity),
            CustomUiElement::Text(t) => render_text(t, global_opacity),
        };
        log::logger().flush(); // flush right after rendering
        let result = result.and_then(apply_rotation);
        match result {
            Ok(ui) => {
                log::info!("  -> OK {}x{} @({},{})", ui.w, ui.h, ui.x, ui.y);
                rendered.push(ui);
            }
            Err(e) => log::error!("  -> pre-render failed: {}", e),
        }
        log::logger().flush();
    }

    rendered.sort_by_key(|r| r.z_order);
    rendered
}

/// Rotate a premultiplied BGRA pixel buffer by `deg` degrees clockwise around its center.
/// Returns (new_pixels, new_w, new_h). Renders into the rotated bounding box, keeping the
/// rotated content centered; out-of-bounds samples (bbox corners) stay transparent.
fn rotate_pixels(src: &[u8], w: u32, h: u32, deg: i32) -> Result<(Vec<u8>, u32, u32), String> {
    let deg = deg.rem_euclid(360);
    if deg == 0 {
        return Ok((src.to_vec(), w, h));
    }
    let (bw, bh) = crate::config::rotated_bbox((w as i32, h as i32), deg);
    let bw = bw as u32;
    let bh = bh as u32;
    if deg == 180 {
        let mut out = vec![0u8; bw as usize * bh as usize * 4];
        for dy in 0..h {
            for dx in 0..w {
                let src_i = (dy * w + dx) as usize * 4;
                let odx = w - 1 - dx;
                let ody = h - 1 - dy;
                let dst_i = (ody * bw + odx) as usize * 4;
                out[dst_i..dst_i + 4].copy_from_slice(&src[src_i..src_i + 4]);
            }
        }
        return Ok((out, bw, bh));
    }
    if deg == 90 || deg == 270 {
        let mut out = vec![0u8; bw as usize * bh as usize * 4];
        for dy in 0..h {
            for dx in 0..w {
                let src_i = (dy * w + dx) as usize * 4;
                // 90° clockwise: (x,y) → (h-1-y, x); 270° = 90° counter-clockwise
                let (odx, ody) = if deg == 90 {
                    (h - 1 - dy, dx)
                } else {
                    (dy, w - 1 - dx)
                };
                let dst_i = (ody * bw + odx) as usize * 4;
                out[dst_i..dst_i + 4].copy_from_slice(&src[src_i..src_i + 4]);
            }
        }
        return Ok((out, bw, bh));
    }
    // General angle: inverse-map each bbox pixel into the source, nearest-neighbor sampling
    let rad = (deg as f32).to_radians();
    let (sin_a, cos_a) = rad.sin_cos();
    let mut out = vec![0u8; bw as usize * bh as usize * 4];
    for dy in 0..bh {
        let oy = (dy as f32 + 0.5) - bh as f32 / 2.0;
        for dx in 0..bw {
            let ox = (dx as f32 + 0.5) - bw as f32 / 2.0;
            let ux = ox * cos_a + oy * sin_a + w as f32 / 2.0;
            let uy = -ox * sin_a + oy * cos_a + h as f32 / 2.0;
            if ux < 0.0 || ux >= w as f32 || uy < 0.0 || uy >= h as f32 {
                continue;
            }
            let sxi = ux.floor() as u32;
            let syi = uy.floor() as u32;
            let src_i = (syi * w + sxi) as usize * 4;
            let dst_i = (dy * bw + dx) as usize * 4;
            out[dst_i..dst_i + 4].copy_from_slice(&src[src_i..src_i + 4]);
        }
    }
    Ok((out, bw, bh))
}

/// Apply rotation to a pre-rendered element: rotates the buffer, swaps w/h to the bbox,
/// and shifts x/y so the rotated content stays centered on the logical rect.
fn apply_rotation(ui: RenderedUi) -> Result<RenderedUi, String> {
    if ui.rotate == 0 {
        return Ok(ui);
    }
    let (pixels, nw, nh) = rotate_pixels(&ui.pixels, ui.w, ui.h, ui.rotate)?;
    let (x, y) = (
        ui.x + (ui.w as i32 - nw as i32) / 2,
        ui.y + (ui.h as i32 - nh as i32) / 2,
    );
    Ok(RenderedUi { pixels, w: nw, h: nh, x, y, ..ui })
}

/// Per-frame composite: alpha-over blend a pre-rendered UI element onto a DIB buffer
pub fn composite_ui(dst: *mut u8, dst_w: i32, dst_h: i32, ui: &RenderedUi) {
    let dst_w_u = dst_w as u32;
    let dst_total = (dst_w * dst_h * 4) as usize;

    // Compute the actual draw area (clipped to the overlay bounds)
    let start_x = ui.x.max(0) as u32;
    let start_y = ui.y.max(0) as u32;
    let ui_right = (ui.x + ui.w as i32).min(dst_w) as u32;
    let ui_bottom = (ui.y + ui.h as i32).min(dst_h) as u32;

    if start_x >= ui_right || start_y >= ui_bottom {
        return;
    }

    let copy_w = (ui_right - start_x) as usize;
    let src_offset_x = (start_x as i32 - ui.x) as u32;
    let src_offset_y = (start_y as i32 - ui.y) as u32;

    for row in 0..(ui_bottom - start_y) {
        let dst_y = start_y + row;
        let dst_row_start = ((dst_y * dst_w_u + start_x) * 4) as usize;
        let src_row_start = (((src_offset_y + row) * ui.w + src_offset_x) * 4) as usize;
        let row_bytes = copy_w * 4;

        if dst_row_start + row_bytes > dst_total {
            break;
        }
        if src_row_start + row_bytes > ui.pixels.len() {
            break;
        }

        let src_slice = &ui.pixels[src_row_start..src_row_start + row_bytes];
        let dst_slice = unsafe {
            std::slice::from_raw_parts_mut(dst.add(dst_row_start), row_bytes)
        };

        // alpha-over: out = src + dst * (1 - src_alpha)
        for px in 0..copy_w {
            let si = px * 4;
            let sa = src_slice[si + 3] as u32;
            if sa == 0 {
                continue; // fully transparent, skip
            }
            if ui.stencil {
                // Stencil: lower the layer below by the cutout strength, revealing the desktop
                let inv = 255 - sa;
                dst_slice[si + 3] = (dst_slice[si + 3] as u32 * inv / 255) as u8;
                dst_slice[si] = (dst_slice[si] as u32 * inv / 255) as u8;
                dst_slice[si + 1] = (dst_slice[si + 1] as u32 * inv / 255) as u8;
                dst_slice[si + 2] = (dst_slice[si + 2] as u32 * inv / 255) as u8;
                continue;
            }
            if sa == 255 {
                // fully opaque, overwrite directly
                dst_slice[si] = src_slice[si];
                dst_slice[si + 1] = src_slice[si + 1];
                dst_slice[si + 2] = src_slice[si + 2];
                dst_slice[si + 3] = 255;
            } else {
                let inv = 255 - sa;
                dst_slice[si] = (src_slice[si] as u32 + dst_slice[si] as u32 * inv / 255) as u8;
                dst_slice[si + 1] = (src_slice[si + 1] as u32 + dst_slice[si + 1] as u32 * inv / 255) as u8;
                dst_slice[si + 2] = (src_slice[si + 2] as u32 + dst_slice[si + 2] as u32 * inv / 255) as u8;
                dst_slice[si + 3] = (sa + dst_slice[si + 3] as u32 * inv / 255) as u8;
            }
        }
    }
}

/// Measure the pixel size of text (exactly matching render_text layout):
/// Microsoft YaHei UI, weight 700. Returns (w, h) = text bounding box size.
pub fn measure_text_size(content: &str, font_size: u32) -> (i32, i32) {
    if content.is_empty() {
        return (2, 2);
    }
    unsafe {
        let hdc_screen = GetDC(None);
        if hdc_screen.is_invalid() {
            return (2, 2);
        }
        let mem_dc = CreateCompatibleDC(hdc_screen);
        if mem_dc.is_invalid() {
            ReleaseDC(None, hdc_screen);
            return (2, 2);
        }
        let font = CreateFontW(
            font_size as i32,
            0,
            0,
            0,
            700,
            0,
            0,
            0,
            1,
            0,
            0,
            0,
            0,
            w!("Microsoft YaHei UI"),
        );
        if font.is_invalid() {
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(None, hdc_screen);
            return (2, 2);
        }
        let mut utf16: Vec<u16> = content.encode_utf16().collect();
        utf16.push(0);
        let text_len = utf16.len() - 1;
        let old_font = SelectObject(mem_dc, font);
        let mut extent = SIZE::default();
        let _ = GetTextExtentPoint32W(mem_dc, &utf16[..text_len], &mut extent);
        SelectObject(mem_dc, old_font);
        let _ = DeleteObject(font);
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(None, hdc_screen);
        (extent.cx + 4, extent.cy + 4)
    }
}

/// Parse "#RRGGBB" into (r, g, b)
pub fn parse_color(hex: &str) -> (u8, u8, u8) {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return (0, 0, 0);
    }
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
    (r, g, b)
}

/// Whether a pixel lies inside a rounded rectangle
fn is_in_rounded_rect(px: i32, py: i32, w: i32, h: i32, r: i32) -> bool {
    if r <= 0 {
        return true;
    }
    // corner circle centers
    let corners = [
        (r, r),           // top-left
        (w - r - 1, r),   // top-right
        (r, h - r - 1),   // bottom-left
        (w - r - 1, h - r - 1), // bottom-right
    ];
    // in the middle band (not near corners) it is always inside
    if px >= r && px < w - r {
        return true;
    }
    if py >= r && py < h - r {
        return true;
    }
    // near a corner: check distance to that corner's center
    for &(cx, cy) in &corners {
        let dx = px - cx;
        let dy = py - cy;
        // only check the matching quadrant
        let in_quadrant = match (px < r, py < r) {
            (true, true) => cx == r && cy == r,
            (false, true) => cx == w - r - 1 && cy == r,
            (true, false) => cx == r && cy == h - r - 1,
            (false, false) => cx == w - r - 1 && cy == h - r - 1,
        };
        if in_quadrant {
            return dx * dx + dy * dy <= r * r;
        }
    }
    true
}

// ===== frame / background rendering (software rasterized rounded rectangles) =====

/// Render a background element: filled rounded rectangle
fn render_background_gdi(b: &BackgroundElement, global_opacity: f32) -> Result<RenderedUi, String> {
    let w = b.width.max(2);
    let h = b.height.max(2);
    let (r, g, bl) = parse_color(&b.color);
    let alpha = (b.opacity.unwrap_or(global_opacity).clamp(0.0, 1.0) * 255.0) as u32;
    let cr = b.corner_radius as i32;

    let pixel_count = (w * h) as usize;
    let mut pixels = vec![0u8; pixel_count * 4];
    for i in 0..pixel_count {
        let px = (i as i32) % w;
        let py = (i as i32) / w;
        if is_in_rounded_rect(px, py, w, h, cr) {
            let si = i * 4;
            pixels[si] = (bl as u32 * alpha / 255) as u8;     // B
            pixels[si + 1] = (g as u32 * alpha / 255) as u8;  // G
            pixels[si + 2] = (r as u32 * alpha / 255) as u8;  // R
            pixels[si + 3] = alpha as u8;
        }
    }
    Ok(RenderedUi { pixels, w: w as u32, h: h as u32, x: b.x, y: b.y, z_order: b.z_order, rotate: b.rotate, stencil: false })
}

/// Render a frame element: outlined rounded rectangle (inside transparent)
fn render_frame_gdi(f: &FrameElement, global_opacity: f32) -> Result<RenderedUi, String> {
    let w = f.width.max(2);
    let h = f.height.max(2);
    let (r, g, bl) = parse_color(&f.border_color);
    let alpha = (f.opacity.unwrap_or(global_opacity).clamp(0.0, 1.0) * 255.0) as u32;
    let cr = f.corner_radius as i32;
    let bw = f.border_width.max(1) as i32;

    let pixel_count = (w * h) as usize;
    let mut pixels = vec![0u8; pixel_count * 4];
    for i in 0..pixel_count {
        let px = (i as i32) % w;
        let py = (i as i32) / w;
        let in_outer = is_in_rounded_rect(px, py, w, h, cr);
        // inner rounded rectangle (shrunk inward by bw)
        let in_inner = px >= bw && py >= bw && px < w - bw && py < h - bw
            && is_in_rounded_rect(px - bw, py - bw, w - bw * 2, h - bw * 2, cr.max(0));
        if in_outer && !in_inner {
            let si = i * 4;
            pixels[si] = (bl as u32 * alpha / 255) as u8;
            pixels[si + 1] = (g as u32 * alpha / 255) as u8;
            pixels[si + 2] = (r as u32 * alpha / 255) as u8;
            pixels[si + 3] = alpha as u8;
        }
    }
    Ok(RenderedUi { pixels, w: w as u32, h: h as u32, x: f.x, y: f.y, z_order: f.z_order, rotate: f.rotate, stencil: false })
}

// ===== image / text rendering =====

/// Render an image element: PNG decode + bilinear scale + premultiplied BGRA, with opacity
fn render_image(img: &ImageElement, exe_dir: &Path, global_opacity: f32) -> Result<RenderedUi, String> {
    let path = exe_dir.join("PNG").join(&img.path);
    if !path.exists() {
        log::warn!("image file not found: {}", path.display());
        return Err(format!("image file not found: {}", path.display()));
    }

    let file = std::fs::File::open(&path)
        .map_err(|e| format!("failed to open image: {}", e))?;
    let mut decoder = png::Decoder::new(file);
    // expand palette/gray -> RGB(A), 16bit -> 8bit (small images like A.png are often palette)
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("PNG decode failed: {}", e))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("PNG frame read failed: {}", e))?;
    let sw = info.width;
    let sh = info.height;
    buf.truncate(info.buffer_size());

    // decode to RGBA uniformly
    let count = (sw * sh) as usize;
    let rgba: Vec<[u8; 4]> = match info.color_type {
        png::ColorType::Rgba => (0..count)
            .map(|i| {
                let s = i * 4;
                [buf[s], buf[s + 1], buf[s + 2], buf[s + 3]]
            })
            .collect(),
        png::ColorType::Rgb => (0..count)
            .map(|i| {
                let s = i * 3;
                [buf[s], buf[s + 1], buf[s + 2], 255]
            })
            .collect(),
        _ => return Err(format!("unsupported PNG color type: {:?}", info.color_type)),
    };

    // width/height of 0 means use the PNG's original size (1:1), otherwise scale
    let dw = if img.width > 0 { img.width as u32 } else { sw };
    let dh = if img.height > 0 { img.height as u32 } else { sh };
    let mut pixels = vec![0u8; (dw * dh * 4) as usize];
    let swi = sw as i32;
    let shi = sh as i32;

    // 1:1 fast path (no scaling) or bilinear scaling -> premultiplied BGRA
    let is_1to1 = dw == sw && dh == sh;
    for dy in 0..dh {
        for dx in 0..dw {
            // 1:1 takes the source pixel directly, avoiding float interpolation
            let (r, g, b, a) = if is_1to1 {
                let p = rgba[(dy * sw + dx) as usize];
                (p[0], p[1], p[2], p[3])
            } else {
            let sx = (dx as f32 + 0.5) * sw as f32 / dw as f32 - 0.5;
            let sy = (dy as f32 + 0.5) * sh as f32 / dh as f32 - 0.5;
            let x0 = sx.floor() as i32;
            let y0 = sy.floor() as i32;
            let fx = sx - x0 as f32;
            let fy = sy - y0 as f32;
            let x0c = x0.clamp(0, swi - 1) as u32;
            let y0c = y0.clamp(0, shi - 1) as u32;
            let x1c = (x0 + 1).clamp(0, swi - 1) as u32;
            let y1c = (y0 + 1).clamp(0, shi - 1) as u32;
            let p00 = rgba[(y0c * sw + x0c) as usize];
            let p10 = rgba[(y0c * sw + x1c) as usize];
            let p01 = rgba[(y1c * sw + x0c) as usize];
            let p11 = rgba[(y1c * sw + x1c) as usize];
            let lerp = |a: u8, b: u8, t: f32| (a as f32 * (1.0 - t) + b as f32 * t).round() as u8;
            let r = lerp(lerp(p00[0], p10[0], fx), lerp(p01[0], p11[0], fx), fy);
            let g = lerp(lerp(p00[1], p10[1], fx), lerp(p01[1], p11[1], fx), fy);
            let b = lerp(lerp(p00[2], p10[2], fx), lerp(p01[2], p11[2], fx), fy);
            let a = lerp(lerp(p00[3], p10[3], fx), lerp(p01[3], p11[3], fx), fy);
            (r, g, b, a)
            };
            let di = (dy * dw + dx) as usize * 4;
            pixels[di] = (b as u32 * a as u32 / 255) as u8;
            pixels[di + 1] = (g as u32 * a as u32 / 255) as u8;
            pixels[di + 2] = (r as u32 * a as u32 / 255) as u8;
            pixels[di + 3] = a;
        }
    }

    // apply opacity to the alpha channel (premultiplied BGRA: color channels scale together)
    let opacity_alpha = (img.opacity.unwrap_or(global_opacity).clamp(0.0, 1.0) * 255.0) as u32;
    for i in 0..(dw * dh) as usize {
        let si = i * 4;
        let a = pixels[si + 3] as u32;
        if a == 0 {
            continue;
        }
        pixels[si] = (pixels[si] as u32 * opacity_alpha / 255) as u8;
        pixels[si + 1] = (pixels[si + 1] as u32 * opacity_alpha / 255) as u8;
        pixels[si + 2] = (pixels[si + 2] as u32 * opacity_alpha / 255) as u8;
        pixels[si + 3] = (a * opacity_alpha / 255) as u8;
    }

Ok(RenderedUi {
        pixels,
        w: dw,
        h: dh,
        x: img.x,
        y: img.y,
        z_order: img.z_order,
        rotate: img.rotate,
        stencil: false,
    })
}

/// Render a text element: software rasterized rounded background + GDI DrawTextW text, with opacity
/// Render a text element: plain text only (no background, size follows content + font size)
fn render_text(t: &TextElement, global_opacity: f32) -> Result<RenderedUi, String> {
    log::info!("render_text start: content='{}' font={} color='{}'", t.content, t.font_size, t.text_color);

    // empty color -> stencil text (drawn with a black brush, then cut out to transparent)
    let has_text_color = !t.text_color.trim().is_empty();
    let (tx_r, tx_g, tx_b) = if has_text_color {
        parse_color(&t.text_color)
    } else {
        (0, 0, 0)
    };

    unsafe {
        let hdc_screen = GetDC(None);
        if hdc_screen.is_invalid() {
            return Err("GetDC failed".to_string());
        }
        let mem_dc = CreateCompatibleDC(hdc_screen);
        if mem_dc.is_invalid() {
            ReleaseDC(None, hdc_screen);
            return Err("CreateCompatibleDC failed".to_string());
        }

        // create a font at the given size and measure the actual text size (no background, size follows text)
        let measure_font = CreateFontW(
            t.font_size as i32,
            0,
            0,
            0,
            700,
            0,
            0,
            0,
            1,
            0,
            0,
            0,
            0,
            w!("Microsoft YaHei UI"),
        );
        if measure_font.is_invalid() {
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(None, hdc_screen);
            return Err("CreateFontW failed".to_string());
        }
        let mut utf16: Vec<u16> = t.content.encode_utf16().collect();
        utf16.push(0); // null terminator
        let text_len = utf16.len() - 1;
        let old_mfont = SelectObject(mem_dc, measure_font);
        let mut extent = SIZE::default();
        let _ = GetTextExtentPoint32W(mem_dc, &utf16[..text_len], &mut extent);
        SelectObject(mem_dc, old_mfont);
        let _ = DeleteObject(measure_font);

        // Text always auto-fits to the measured size (no width/height entries exist anymore; 4px margin)
        let w = (extent.cx + 4).max(2);
        let h = (extent.cy + 4).max(2);

        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let hbmp = CreateDIBSection(mem_dc, &bi, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| format!("CreateDIBSection failed: {}", e))?;
        if bits.is_null() {
            let _ = DeleteObject(hbmp);
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(None, hdc_screen);
            return Err("CreateDIBSection returned null bits".to_string());
        }
        let old_bmp = SelectObject(mem_dc, hbmp);
        let dib = std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize);
        // clear to fully transparent (no background)
        for i in 0..(w * h) as usize {
            let si = i * 4;
            dib[si] = 0;
            dib[si + 1] = 0;
            dib[si + 2] = 0;
            dib[si + 3] = 0;
        }

        log::info!("render_text transparent base cleared, preparing to draw text");

        // GDI draws the text (transparent background mode, centered)
        // note: append a null terminator for GDI compatibility
        // key point: GDI drawing into a 32-bit DIB only writes RGB, alpha stays 0,
        // so reading alpha directly would make everything invisible. Hence we draw
        // white text on black and later extract coverage from the brightness (R channel).
        let mut utf16: Vec<u16> = t.content.encode_utf16().collect();
        utf16.push(0); // null terminator
        if utf16.len() > 1 {
            SetBkMode(mem_dc, TRANSPARENT);
            SetBkColor(mem_dc, COLORREF(0));
            SetTextColor(mem_dc, COLORREF(0xFFFFFF));
            let font = CreateFontW(
                t.font_size as i32,
                0,
                0,
                0,
                700,
                0,
                0,
                0,
                1,
                0,
                0,
                0,
                0,
                w!("Microsoft YaHei UI"),
            );
            if font.is_invalid() {
                log::warn!("CreateFontW failed, skipping text drawing");
            } else {
                let old_font = SelectObject(mem_dc, font);
                let mut rect = RECT {
                    left: 0,
                    top: 0,
                    right: w,
                    bottom: h,
                };
                // pass the null-terminated slice (length excludes the trailing 0)
                let text_len = utf16.len() - 1;
                let text_slice = &mut utf16[..text_len];
                let _ = DrawTextW(
                    mem_dc,
                    text_slice,
                    &mut rect,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                );
                SelectObject(mem_dc, old_font);
                let _ = DeleteObject(font);
            }
        }

        log::info!("render_text text drawn, preparing to read pixels");

        // white text on black (background cleared to 0): R=G=B=coverage.
        // with a text color -> extract alpha and premultiply-shade it; without (stencil text) ->
        // strokes output cutout strength (alpha=coverage, RGB=0), and compositing carves away
        // whatever is below, revealing the desktop.
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        if has_text_color {
            for i in 0..(w * h) as usize {
                let si = i * 4;
                let cov = dib[si] as u32; // R channel = coverage 0~255
                if cov == 0 {
                    continue;
                }
                pixels[si] = (tx_b as u32 * cov / 255) as u8;
                pixels[si + 1] = (tx_g as u32 * cov / 255) as u8;
                pixels[si + 2] = (tx_r as u32 * cov / 255) as u8;
                pixels[si + 3] = cov as u8;
            }
        } else {
            for i in 0..(w * h) as usize {
                let si = i * 4;
                let cov = dib[si] as u32; // R channel = coverage 0~255
                if cov == 0 {
                    continue;
                }
                pixels[si + 3] = cov as u8; // RGB stays 0, compositing only cuts out
            }
        }

        // cleanup GDI
        SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(hbmp);
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(None, hdc_screen);

        log::info!("render_text GDI cleanup done");

        // apply opacity to the alpha channel (premultiplied BGRA: stencil mode only scales
        // cutout strength, RGB=0 so scaling has no side effect)
        let opacity_alpha = (t.opacity.unwrap_or(global_opacity).clamp(0.0, 1.0) * 255.0) as u32;
        for i in 0..(w * h) as usize {
            let si = i * 4;
            let a = pixels[si + 3] as u32;
            if a == 0 {
                continue;
            }
            pixels[si] = (pixels[si] as u32 * opacity_alpha / 255) as u8;
            pixels[si + 1] = (pixels[si + 1] as u32 * opacity_alpha / 255) as u8;
            pixels[si + 2] = (pixels[si + 2] as u32 * opacity_alpha / 255) as u8;
            pixels[si + 3] = (a * opacity_alpha / 255) as u8;
        }

        log::info!("render_text done, stencil={}", !has_text_color);
        Ok(RenderedUi {
            pixels,
            w: w as u32,
            h: h as u32,
            x: t.x,
            y: t.y,
            z_order: t.z_order,
            rotate: t.rotate,
            stencil: !has_text_color,
        })
    }
}
