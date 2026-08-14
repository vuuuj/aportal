//! Selecting-phase drawing (step 1/2)

use super::common::*;
use super::state::{EditorState, HDC};
use super::panel::draw_region_panel;

/// Draw the selecting phase: selected regions + the dragging selection box + right panel
pub unsafe fn draw_selecting(hdc: HDC, st: &EditorState) {
    for (i, e) in st.elements.iter().enumerate() {
        // Only draw source rects of capture elements (UI elements have none; the old zero rect drew a 0x0 marker)
        let Some(src) = &e.source else { continue };
        let (x, y, w, h) = (src.x as i32, src.y as i32, src.width as i32, src.height as i32);
        // Translucent fill (element area visible) + outline, real screen still shows through
        fill_rect_alpha(hdc, x, y, w, h, rgb(40, 190, 110), 110);
        draw_rect_outline(hdc, x, y, w, h, 2, rgb(80, 200, 120));
        fill_rect_solid(hdc, x, y, 38, 22, rgb(255, 220, 60));
        gdi_text(hdc, &format!("#{}", i + 1), x, y, 38, 22, rgb(30, 30, 40));
        // Show X/Y/W/H
        let info = format!("X:{} Y:{} W:{} H:{}", src.x, src.y, src.width, src.height);
        let info_w = 220;
        // For narrow selections the label hugs the right edge of the selection (the old x.min(x+w-220) threw it to the far left of the screen)
        let info_x = if w > info_w {
            x.min(x + w - info_w).max(0)
        } else {
            (x + w - info_w).max(0)
        };
        let info_y = (y + h).min(st.screen_h - 24);
        fill_rect_solid(hdc, info_x, info_y, info_w, 22, rgb(20, 40, 24));
        gdi_text(hdc, &info, info_x, info_y, info_w, 22, rgb(120, 230, 140));
        // List-selection highlight (yellow outline)
        if st.selected_indices.contains(&i) {
            draw_rect_outline(hdc, x - 2, y - 2, w + 4, h + 4, 2, rgb(255, 220, 60));
        }
    }
    if st.is_dragging {
        if let (Some((sx, sy)), Some((cx, cy))) = (st.drag_start, st.drag_current) {
            draw_rect_outline(
                hdc,
                sx.min(cx),
                sy.min(cy),
                (cx - sx).abs(),
                (cy - sy).abs(),
                2,
                rgb(80, 220, 120),
            );
            let info = format!("{} \u{00D7} {}", (cx - sx).abs(), (cy - sy).abs());
            let lx = sx.min(cx);
            let ly = (sy.min(cy) - 24).max(2);
            fill_rect_solid(hdc, lx, ly, 120, 22, rgb(20, 40, 24));
            gdi_text(hdc, &info, lx, ly, 120, 22, rgb(120, 230, 140));
        }
    }
    // Right panel (selecting-phase list)
    draw_region_panel(hdc, st);
}
