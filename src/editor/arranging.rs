//! Arranging-phase drawing + canvas hit testing (step 2/2)

use std::path::Path;

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, GetStockObject, LineTo,
    MoveToEx, Rectangle, RoundRect, SelectObject, SetBkMode, SetTextColor, NULL_BRUSH, PEN_STYLE,
    TRANSPARENT, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
};

use super::common::*;
use super::state::{EditorState, ElemKind, HDC};
use super::panel::draw_region_panel;

/// Effective size of an element in the editor (canvas display / hit testing / handle):
/// - Image: when width/height is 0 (1:1 original), use the PNG's actual size; 40x20 placeholder if no image
/// - Text: when width/height is 0, estimate from content + font size (no background, size follows text)
/// - Others: use width/height directly
pub fn eff_size(st: &EditorState, i: usize) -> (i32, i32) {
    let e = st.elements.get(i);
    let kind = e.map(|e| e.extra.kind).unwrap_or(ElemKind::Capture);
    let w = st.elements[i].display.width;
    let h = st.elements[i].display.height;
    if kind == ElemKind::Png && (w <= 0 || h <= 0) {
        let path = e.map(|e| e.extra.png_path.as_str()).unwrap_or("");
        if let Some(Some((iw, ih, _))) = st.png_cache.get(path) {
            return (*iw as i32, *ih as i32);
        }
        return (40, 20);
    }
    if kind == ElemKind::Text {
        // Text has no fixed size anymore: always estimate from content + font size
        let ee = st.elements[i].extra.clone();
        return crate::custom_ui::measure_text_size(&ee.content, ee.font_size);
    }
    (w.max(2), h.max(2))
}

/// Decode PNG → premultiplied BGRA (original size) for real-time preview in the editor
fn decode_png_premul(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let file = std::fs::File::open(path).ok()?;
    let mut decoder = png::Decoder::new(file);
    // Expand uniformly: palette/grayscale → RGB(A), 16bit → 8bit (small images like A.png are often palette type)
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let sw = info.width;
    let sh = info.height;
    buf.truncate(info.buffer_size());
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
        _ => return None,
    };
    let mut px = vec![0u8; count * 4];
    for i in 0..count {
        let a = rgba[i][3] as u32;
        px[i * 4] = (rgba[i][2] as u32 * a / 255) as u8;
        px[i * 4 + 1] = (rgba[i][1] as u32 * a / 255) as u8;
        px[i * 4 + 2] = (rgba[i][0] as u32 * a / 255) as u8;
        px[i * 4 + 3] = a as u8;
    }
    Some((sw, sh, px))
}

/// Fetch image (cached). Empty path / missing file → None
fn cached_png(st: &mut EditorState, path: &str) -> Option<(u32, u32, Vec<u8>)> {
    if path.trim().is_empty() {
        return None;
    }
    if let Some(hit) = st.png_cache.get(path) {
        return hit.clone();
    }
    let full = Path::new(&st.exe_dir).join("PNG").join(path);
    let decoded = decode_png_premul(&full);
    let result = decoded.clone();
    st.png_cache.insert(path.to_string(), decoded);
    result
}

/// Software-composite premultiplied BGRA pixels (bilinear scaling + alpha-over) into the editor DIB buffer.
/// Not using AlphaBlend: AC_SRC_ALPHA blits between memory DCs are unreliable in this editor (image not shown).
/// Matches the runtime custom_ui compositing, so WYSIWYG is guaranteed.
#[allow(clippy::too_many_arguments)]
fn composite_png(
    buf: &mut [u8],
    sw: i32,
    sh: i32,
    px: &[u8],
    iw: u32,
    ih: u32,
    dx: i32,
    dy: i32,
    dw: i32,
    dh: i32,
) {
    if iw == 0 || ih == 0 || dw <= 0 || dh <= 0 {
        return;
    }
    let dst_x0 = dx.max(0);
    let dst_y0 = dy.max(0);
    let dst_x1 = (dx + dw).min(sw);
    let dst_y1 = (dy + dh).min(sh);
    if dst_x1 <= dst_x0 || dst_y1 <= dst_y0 {
        return;
    }
    let scale_x = iw as f32 / dw as f32;
    let scale_y = ih as f32 / dh as f32;
    let iw_i = iw as i32;
    let ih_i = ih as i32;

    for ty in dst_y0..dst_y1 {
        let sy = ((ty - dy) as f32 + 0.5) * scale_y - 0.5;
        let fy = sy.fract();
        let cy0 = (sy.floor() as i32).clamp(0, ih_i - 1) as u32;
        let cy1 = ((sy.floor() as i32 + 1) as u32).min(ih_i as u32 - 1);
        for tx in dst_x0..dst_x1 {
            let sx = ((tx - dx) as f32 + 0.5) * scale_x - 0.5;
            let fx = sx.fract();
            let cx0 = (sx.floor() as i32).clamp(0, iw_i - 1) as u32;
            let cx1 = ((sx.floor() as i32 + 1) as u32).min(iw_i as u32 - 1);
            let i00 = ((cy0 * iw + cx0) * 4) as usize;
            let i10 = ((cy0 * iw + cx1) * 4) as usize;
            let i01 = ((cy1 * iw + cx0) * 4) as usize;
            let i11 = ((cy1 * iw + cx1) * 4) as usize;
            // Bilinear interpolation (per channel)
            let idx = ((ty as u32 * sw as u32 + tx as u32) * 4) as usize;
            let dst = &mut buf[idx..idx + 4];
            let channel = |off: usize, fx: f32, fy: f32| {
                (px[i00 + off] as f32 * (1.0 - fx) * (1.0 - fy)
                    + px[i10 + off] as f32 * fx * (1.0 - fy)
                    + px[i01 + off] as f32 * (1.0 - fx) * fy
                    + px[i11 + off] as f32 * fx * fy)
                    .round() as u8
            };
            let a = channel(3, fx, fy);
            if a == 0 {
                continue;
            }
            if a == 255 {
                dst[0] = channel(0, fx, fy);
                dst[1] = channel(1, fx, fy);
                dst[2] = channel(2, fx, fy);
                dst[3] = 255;
            } else {
                let inv = 255 - a;
                // Source is premultiplied BGRA, add directly
                dst[0] = (channel(0, fx, fy) as u32 + dst[0] as u32 * inv as u32 / 255) as u8;
                dst[1] = (channel(1, fx, fy) as u32 + dst[1] as u32 * inv as u32 / 255) as u8;
                dst[2] = (channel(2, fx, fy) as u32 + dst[2] as u32 * inv as u32 / 255) as u8;
                dst[3] = dst[3].saturating_add(a);
            }
        }
    }
}

/// Parse "#RRGGBB" into a COLORREF
fn hex_color(hex: &str) -> windows::Win32::Foundation::COLORREF {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 {
        return rgb(128, 128, 128);
    }
    let r = u8::from_str_radix(&h[0..2], 16).unwrap_or(128);
    let g = u8::from_str_radix(&h[2..4], 16).unwrap_or(128);
    let b = u8::from_str_radix(&h[4..6], 16).unwrap_or(128);
    rgb(r, g, b)
}

/// Draw the arranging phase: region rects + right panel
/// Selected elements are drawn last (frontmost, higher layer) so they are not covered by other elements/coordinate labels while nudging.
pub unsafe fn draw_arranging(hdc: HDC, st: &mut EditorState, buf: &mut [u8], sw: i32, sh: i32) {
    // Draw in ascending z_order (lower first, higher on top); selected ones last (topmost)
    let colors = [
        rgb(180, 60, 60),
        rgb(60, 160, 60),
        rgb(60, 60, 180),
        rgb(180, 140, 40),
        rgb(140, 60, 160),
        rgb(60, 160, 160),
    ];
    let mut order: Vec<usize> = (0..st.elements.len()).collect();
    order.sort_by_key(|&i| {
        (
            st.selected_indices.contains(&i),
            st.elements.get(i).map(|e| e.extra.z_order).unwrap_or(0),
        )
    });

    for &i in &order {
        let (drx, dry, drw, drh) = {
            let d = &st.elements[i].display;
            (st.overlay_x + d.x, st.overlay_y + d.y, d.width, d.height)
        };
        let rx = drx;
        let ry = dry;
        let is_sel = st.selected_indices.contains(&i);
        let kind = st.elements.get(i).map(|e| e.extra.kind).unwrap_or(ElemKind::Capture);
        let extra = st.elements.get(i).map(|e| &e.extra);
        let corner = extra.map(|e| e.corner_radius as i32).unwrap_or(8)
            .min(drw / 2).min(drh / 2).max(0);

        // Selection outline pen
        let sel_pen = if is_sel {
            CreatePen(PEN_STYLE(0), 3, rgb(255, 220, 60))
        } else {
            CreatePen(PEN_STYLE(0), 1, rgb(255, 255, 255))
        };

        match kind {
            ElemKind::Capture => {
                // Capture region: translucent colored fill (element area visible) + colored outline, real screen still shows through
                fill_rect_alpha(hdc, rx, ry, drw, drh, colors[i % colors.len()], 90);
                let cap_pen = CreatePen(PEN_STYLE(0), if is_sel { 3 } else { 2 }, colors[i % colors.len()]);
                let op = SelectObject(hdc, cap_pen);
                let ob = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                let _ = RoundRect(hdc, rx, ry, rx + drw, ry + drh, 8, 8);
                SelectObject(hdc, op);
                SelectObject(hdc, ob);
                let _ = DeleteObject(cap_pen);
            }
            ElemKind::Frame => {
                // Frame: outlined rounded rect (no fill), configurable width/color/radius
                let bc = extra.map(|e| hex_color(&e.border_color)).unwrap_or(rgb(255, 220, 60));
                let bw = extra.map(|e| e.border_width.max(1) as i32).unwrap_or(2);
                let frame_pen = CreatePen(PEN_STYLE(0), bw, bc);
                let op = SelectObject(hdc, frame_pen);
                let ob = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                let _ = RoundRect(hdc, rx, ry, rx + drw, ry + drh, corner * 2, corner * 2);
                SelectObject(hdc, op);
                SelectObject(hdc, ob);
                let _ = DeleteObject(frame_pen);
            }
            ElemKind::Background => {
                // Background: filled rounded rect
                let fc = extra.map(|e| hex_color(&e.color)).unwrap_or(rgb(42, 42, 58));
                let brush = CreateSolidBrush(fc);
                let op = SelectObject(hdc, sel_pen);
                let ob = SelectObject(hdc, brush);
                let _ = RoundRect(hdc, rx, ry, rx + drw, ry + drh, corner * 2, corner * 2);
                SelectObject(hdc, op);
                SelectObject(hdc, ob);
                let _ = DeleteObject(brush);
            }
            ElemKind::Png => {
                // Image: software-composite the actual picture (moves in real time while dragging/nudging), placeholder when no image
                let (ew, eh) = eff_size(st, i);
                let path = extra.map(|e| e.png_path.as_str()).unwrap_or("").to_string();
                if let Some((iw, ih, px)) = cached_png(st, &path) {
                    composite_png(buf, sw, sh, &px, iw, ih, rx, ry, ew, eh);
                    if is_sel {
                        draw_rect_outline(hdc, rx, ry, ew, eh, 1, rgb(255, 220, 60));
                    }
                } else {
                    let brush = CreateSolidBrush(rgb(40, 60, 80));
                    let op = SelectObject(hdc, sel_pen);
                    let ob = SelectObject(hdc, brush);
                    let _ = RoundRect(hdc, rx, ry, rx + ew, ry + eh, corner * 2, corner * 2);
                    SelectObject(hdc, op);
                    SelectObject(hdc, ob);
                    let _ = DeleteObject(brush);
                    if !path.is_empty() {
                        gdi_text(hdc, &path, rx + 2, ry + eh / 2 - 8, ew - 4, 16, rgb(200, 220, 255));
                    } else {
                        gdi_text(hdc, crate::i18n::t("no_image"), rx + 2, ry + eh / 2 - 8, ew - 4, 16, rgb(200, 100, 100));
                    }
                }
            }
            ElemKind::Text => {
                // Text: plain text (no background), real-time preview in the editor. Size follows content + font size.
                let (ew, eh) = eff_size(st, i);
                let has_text_color = extra.map(|e| !e.text_color.trim().is_empty()).unwrap_or(false);
                let text_c = if has_text_color {
                    extra.map(|e| hex_color(&e.text_color)).unwrap_or(rgb(255, 255, 255))
                } else {
                    rgb(255, 255, 255)
                };
                let content = extra.map(|e| e.content.as_str()).unwrap_or("");
                let font_size = extra.map(|e| e.font_size).unwrap_or(14);
                if !content.is_empty() {
                    // Create a font with the given font_size
                    let font_face: Vec<u16> = "Microsoft YaHei UI\0".encode_utf16().collect();
                    let font = CreateFontW(
                        font_size as i32, 0, 0, 0, 700, 0, 0, 0, 1, 0, 0, 0, 0,
                        windows::core::PCWSTR::from_raw(font_face.as_ptr()),
                    );
                    let old_font = SelectObject(hdc, font);
                    let _ = SetBkMode(hdc, TRANSPARENT);
                    let _ = SetTextColor(hdc, text_c);
                    let mut rect = RECT { left: rx, top: ry, right: rx + ew, bottom: ry + eh };
                    let mut utf16: Vec<u16> = content.encode_utf16().collect();
                    let _ = DrawTextW(hdc, &mut utf16, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                    SelectObject(hdc, old_font);
                    let _ = DeleteObject(font);
                }
                if is_sel {
                    draw_rect_outline(hdc, rx, ry, ew, eh, 1, rgb(255, 220, 60));
                }
            }
        }
        let _ = DeleteObject(sel_pen);

        // Rotated elements: show the rotated footprint (bounding box) + a diagonal hint from the
        // logical top-left corner toward its rotated position, so the angle reads at a glance
        let rot = st.elements.get(i).map(|e| e.display.rotate).unwrap_or(0);
        if rot.rem_euclid(360) != 0 {
            let (ew, eh) = eff_size(st, i);
            let (bx, by, bw, bh) = crate::config::rotated_footprint(
                st.overlay_x + st.elements[i].display.x,
                st.overlay_y + st.elements[i].display.y,
                ew,
                eh,
                rot,
            );
            let rot_pen = CreatePen(PEN_STYLE(0), 1, rgb(255, 220, 60));
            let op = SelectObject(hdc, rot_pen);
            let ob = SelectObject(hdc, GetStockObject(NULL_BRUSH));
            let _ = Rectangle(hdc, bx, by, bx + bw, by + bh);
            SelectObject(hdc, op);
            SelectObject(hdc, ob);
            let _ = DeleteObject(rot_pen);
            // Diagonal hint: logical top-left → rotated top-left corner
            let (sx, sy, ex_c, ey_c) = rotated_top_left(
                rx, ry, ew, eh, rot,
            );
            let hint_pen = CreatePen(PEN_STYLE(0), 1, rgb(160, 160, 255));
            let op = SelectObject(hdc, hint_pen);
            let _ = MoveToEx(hdc, sx, sy, None);
            let _ = LineTo(hdc, ex_c, ey_c);
            SelectObject(hdc, op);
            let _ = DeleteObject(hint_pen);
        }

        // Index badge (uses effective size for image/text)
        let (ew, eh) = eff_size(st, i);
        let badge = st.elements.get(i).map(|e| e.extra.kind.badge()).unwrap_or("?");
        gdi_text(hdc, &format!("#{}{}", i + 1, badge), rx, ry - 18, ew.max(40), 16, rgb(255, 255, 255));
        draw_handle(hdc, rx + ew - 12, ry + eh - 12);
        // Show X/Y/W/H (effective size for text/image, so the numbers match the real footprint)
        let info = format!("X:{} Y:{} W:{} H:{}", drx - st.overlay_x, dry - st.overlay_y, ew, eh);
        let info_w = 220;
        let info_x = (rx + ew / 2 - info_w / 2).max(0);
        let info_y = (ry + eh).min(st.screen_h - 24);
        fill_rect_solid(hdc, info_x, info_y, info_w, 20, rgb(20, 40, 24));
        gdi_text(hdc, &info, info_x, info_y, info_w, 20, rgb(120, 230, 140));
    }

    // Right panel
    draw_region_panel(hdc, st);

    // Box-select dashed rectangle
    if st.box_selecting {
        if let (Some((sx, sy)), Some((ex, ey))) = (st.box_select_start, st.box_select_current) {
            let rx = sx.min(ex);
            let ry = sy.min(ey);
            let rw = (ex - sx).abs();
            let rh = (ey - sy).abs();
            if rw > 2 && rh > 2 {
                // Draw the box-select rect with a thin line (light color simulates dashes)
                draw_rect_outline(hdc, rx, ry, rw, rh, 1, rgb(255, 220, 100));
                // Translucent fill effect (approximated with a light border)
                draw_rect_outline(hdc, rx + 1, ry + 1, rw - 2, rh - 2, 1, rgb(255, 220, 100));
            }
        }
    }
}

/// Rotated top-left corner of a w×h rect at `deg` degrees (clockwise), centered on (x, y)-(x+w, y+h).
fn rotated_top_left(x: i32, y: i32, w: i32, h: i32, deg: i32) -> (i32, i32, i32, i32) {
    let rad = (deg.rem_euclid(360) as f32).to_radians();
    let (s, c) = rad.sin_cos();
    let cx = x as f32 + w as f32 / 2.0;
    let cy = y as f32 + h as f32 / 2.0;
    // Logical top-left corner offset from center, rotated clockwise
    let ox = -w as f32 / 2.0;
    let oy = -h as f32 / 2.0;
    let rx = ox * c - oy * s;
    let ry = ox * s + oy * c;
    (x, y, (cx + rx).round() as i32, (cy + ry).round() as i32)
}

/// Canvas hit test: returns (element index, whether the resize handle is hit).
/// Tests in descending z_order; the topmost element wins.
pub fn hit_test(st: &EditorState, mx: i32, my: i32) -> Option<(usize, bool)> {
    let mut order: Vec<usize> = (0..st.elements.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(st.elements.get(i).map(|e| e.extra.z_order).unwrap_or(0)));
    for i in order {
        let r = &st.elements[i].display;
        let (ew, eh) = eff_size(st, i);
        let rx = st.overlay_x + r.x;
        let ry = st.overlay_y + r.y;
        if mx >= rx + ew - 12
            && mx <= rx + ew
            && my >= ry + eh - 12
            && my <= ry + eh
        {
            return Some((i, true));
        }
        if mx >= rx && mx <= rx + ew && my >= ry && my <= ry + eh {
            return Some((i, false));
        }
    }
    None
}
