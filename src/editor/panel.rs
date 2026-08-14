//! Right panel: drawing, hit testing, field editing
//!
//! Shared by both phases (selecting/arranging). When a custom UI element is selected,
//! an "element properties" area expands below the list to edit colors/opacity/layer and more in place.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, DrawTextW, SelectObject, SetBkMode, SetTextColor, DT_LEFT, DT_SINGLELINE,
    DT_VCENTER, TRANSPARENT,
};

use super::common::*;
use super::state::{EditorState, EditingTarget, ElemExtra, ElemKind, Element, FieldKind, HDC};
use super::ui::{draw_panel_actions, draw_panel_toggles};
use crate::config::DisplayRect;

/// Height of each property row
const PROP_ROW_H: i32 = 30;
/// Top padding of the property area
const PROP_TOP_PAD: i32 = 8;
/// Divider bar height of the property area
const PROP_SECTION_H: i32 = 22;
/// Add-button row height
const ADD_BTN_H: i32 = 28;
/// Dedicated scrollbar lane width (list content never enters it; scrollbar hugs the list's right edge)
pub const SB_LANE: i32 = 12;
/// Scrollbar width
pub const SB_W: i32 = 7;

// ===== List item geometry =====
/// Width of the "[#index]badge" label area (8 letters wide)
const IDX_W: i32 = 72;
/// X of the X/Y/W/H field column start (relative to px)
const FIELD_BASE_X: i32 = 8 + IDX_W + 10;
/// Field column spacing
const FIELD_STRIDE: i32 = 88;
/// Single field box width (2-char label + 4-digit value + arrow button)
const FIELD_W: i32 = 80;
/// Field label width ("X"/"Y"/"W"/"H")
const FIELD_LABEL_W: i32 = 22;
/// Offset of the field value area start
const FIELD_VAL_X: i32 = FIELD_LABEL_W + 4;
/// Field value area width (excl. the arrow button on the right)
const FIELD_VAL_W: i32 = FIELD_W - FIELD_VAL_X - SPIN_W;

/// X position of a list item's field box
fn field_x(px: i32, col: i32) -> i32 {
    px + FIELD_BASE_X + col * FIELD_STRIDE
}

/// Read a geometry value: source (capture coords) in selecting phase, display (layout coords) in arranging phase
pub fn get_field_coord(st: &EditorState, idx: usize, fk: FieldKind) -> i32 {
    let Some(e) = st.elements.get(idx) else { return 0 };
    if st.phase == EditorPhase::Selecting {
        // UI elements have no source region → 0 (the old zero placeholder rect also displayed 0)
        let Some(s) = &e.source else { return 0 };
        return match fk {
            FieldKind::X => s.x as i32,
            FieldKind::Y => s.y as i32,
            FieldKind::Width => s.width as i32,
            FieldKind::Height => s.height as i32,
        };
    }
    let d = &e.display;
    match fk {
        FieldKind::X => d.x,
        FieldKind::Y => d.y,
        FieldKind::Width => d.width,
        FieldKind::Height => d.height,
    }
}

/// Write a geometry value: source in selecting phase, display in arranging phase (only one changes; no more linkage)
fn set_field_coord(st: &mut EditorState, idx: usize, fk: FieldKind, val: i32) {
    let Some(e) = st.elements.get_mut(idx) else { return };
    if st.phase == EditorPhase::Selecting {
        let Some(s) = &mut e.source else { return };
        match fk {
            FieldKind::X => s.x = val.max(0) as u32,
            FieldKind::Y => s.y = val.max(0) as u32,
            FieldKind::Width => s.width = val.max(10) as u32,
            FieldKind::Height => s.height = val.max(10) as u32,
        }
    } else {
        let d = &mut e.display;
        match fk {
            FieldKind::X => d.x = val.max(0),
            FieldKind::Y => d.y = val.max(0),
            FieldKind::Width => d.width = val.max(10),
            FieldKind::Height => d.height = val.max(10),
        }
    }
}

/// Draw up/down arrow buttons (▲ on top, ▼ below, occupying the SPIN_W area at the right end)
pub unsafe fn draw_spinner_pub(hdc: HDC, x: i32, y: i32, w: i32, h: i32) {
    let half = h / 2;
    fill_rect_solid(hdc, x, y, w, h, rgb(32, 36, 52));
    draw_rect_outline(hdc, x, y, w, h, 1, rgb(52, 56, 76));
    gdi_text(hdc, "▲", x - 2, y, w + 4, half, rgb(190, 220, 120));
    gdi_text(hdc, "▼", x - 2, y + half, w + 4, h - half, rgb(150, 210, 255));
}

/// Field values use a smaller font so 4 digits fit
unsafe fn gdi_field_value(hdc: HDC, text: &str, x: i32, y: i32, w: i32, h: i32, color: COLORREF) {
    use super::common::create_ui_font;
    if text.is_empty() {
        return;
    }
    let _ = SetBkMode(hdc, TRANSPARENT);
    let _ = SetTextColor(hdc, color);
    let rect = RECT { left: x, top: y, right: x + w, bottom: y + h };
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    let font = create_ui_font(14);
    let old = SelectObject(hdc, font);
    let _ = DrawTextW(hdc, &mut utf16, &rect as *const RECT as *mut _, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    SelectObject(hdc, old);
    let _ = DeleteObject(font);
}

/// Field labels (X/Y/W/H etc.) use a small font so they stay inside the label area
unsafe fn gdi_field_label(hdc: HDC, text: &str, x: i32, y: i32, w: i32, h: i32, color: COLORREF) {
    use super::common::create_ui_font;
    if text.is_empty() {
        return;
    }
    let _ = SetBkMode(hdc, TRANSPARENT);
    let _ = SetTextColor(hdc, color);
    let rect = RECT { left: x, top: y, right: x + w, bottom: y + h };
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    let font = create_ui_font(11);
    let old = SelectObject(hdc, font);
    let _ = DrawTextW(hdc, &mut utf16, &rect as *const RECT as *mut _, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    SelectObject(hdc, old);
    let _ = DeleteObject(font);
}

/// Editable property list for an element kind (draw order)
fn props_for_kind(kind: ElemKind) -> &'static [ElemProp] {
    match kind {
        ElemKind::Capture => &[ElemProp::Rotate],
        ElemKind::Frame => &[
            ElemProp::Rotate,
            ElemProp::BorderColor,
            ElemProp::BorderWidth,
            ElemProp::CornerRadius,
            ElemProp::Opacity,
            ElemProp::ZOrder,
        ],
        ElemKind::Background => &[
            ElemProp::Rotate,
            ElemProp::BgColor,
            ElemProp::CornerRadius,
            ElemProp::Opacity,
            ElemProp::ZOrder,
        ],
        ElemKind::Png => &[
            ElemProp::Rotate,
            ElemProp::PngPath,
            ElemProp::Opacity,
            ElemProp::ZOrder,
        ],
        ElemKind::Text => &[
            ElemProp::Rotate,
            ElemProp::Content,
            ElemProp::FontSize,
            ElemProp::TextColor,
            ElemProp::Opacity,
            ElemProp::ZOrder,
        ],
    }
}

/// Read the current string value of a property.
/// `rotate` lives in DisplayRect (not ElemExtra), so it is read here via `st`.
fn read_prop(st: &EditorState, idx: usize, extra: &ElemExtra, prop: ElemProp) -> String {
    match prop {
        ElemProp::Opacity => format!("{:.2}", extra.opacity),
        ElemProp::ZOrder => extra.z_order.to_string(),
        ElemProp::Rotate => st
            .elements
            .get(idx)
            .map(|e| e.display.rotate.rem_euclid(360).to_string())
            .unwrap_or_else(|| "0".to_string()),
        ElemProp::BorderColor => extra.border_color.clone(),
        ElemProp::BorderWidth => extra.border_width.to_string(),
        ElemProp::CornerRadius => extra.corner_radius.to_string(),
        ElemProp::BgColor => extra.color.clone(),
        ElemProp::Content => extra.content.clone(),
        ElemProp::FontSize => extra.font_size.to_string(),
        ElemProp::TextColor => extra.text_color.clone(),
        ElemProp::PngPath => extra.png_path.clone(),
    }
}

/// Height needed by the property area (incl. title + rows)
fn props_section_height(st: &EditorState) -> i32 {
    let idx = match st.selected_indices.last() {
        Some(&i) => i,
        None => return 0,
    };
    let kind = match st.elements.get(idx).map(|e| e.extra.kind) {
        Some(k) => k,
        None => return 0,
    };
    // Capture rotation only makes sense in the arranging phase (display-side geometry)
    if kind == ElemKind::Capture && st.phase != EditorPhase::Arranging {
        return 0;
    }
    let n = props_for_kind(kind).len() as i32;
    if n == 0 {
        0
    } else {
        PROP_TOP_PAD + PROP_SECTION_H + n * PROP_ROW_H + 6
    }
}

/// Compute the panel rect (px, py, pw, ph)
pub fn panel_rect(st: &EditorState) -> (i32, i32, i32, i32) {
    let px = st.panel_x;
    let py = st.panel_y;
    let n = st.elements.len() as i32;
    // Base height: list + add-button row (kept only in the arranging phase)
    let add_row = if st.phase == EditorPhase::Arranging { ADD_BTN_H + 12 } else { 12 };
    let mut ph = LIST_TOP2 + n * LIST_ITEM_H + add_row;
    // Append the property area when a UI element is selected
    ph += props_section_height(st);
    let max_ph = st.screen_h - py - 48;
    ph = ph.min(max_ph).max(LIST_TOP2 + add_row + 68);
    (px, py, PANEL_W, ph)
}

/// Bottom edge of the list area (property area and add buttons sit below it)
fn list_area_bottom(st: &EditorState, py: i32, ph: i32) -> i32 {
    let add_top = py + ph - ADD_BTN_H - 8;
    if st.phase == EditorPhase::Arranging {
        add_top
    } else {
        // Selecting phase has no add-button row; the list extends to the panel bottom with margin
        add_top + ADD_BTN_H + 4
    }
}

/// List area rect (screen coords): clipped to the visible region
pub fn list_area_rect(st: &EditorState) -> (i32, i32, i32, i32) {
    let (px, py, pw, ph) = panel_rect(st);
    let props_h = props_section_height(st);
    let add_top = py + ph - ADD_BTN_H - 8;
    let props_top = add_top - props_h;
    let bottom = if props_h > 0 { props_top } else { list_area_bottom(st, py, ph) };
    (px, py + LIST_TOP2, pw, bottom - (py + LIST_TOP2))
}

/// Number of visible list items (at least 1)
pub fn list_visible_count(st: &EditorState) -> i32 {
    let (_lx, _ly, _lw, lsh) = list_area_rect(st);
    (lsh / LIST_ITEM_H).max(1)
}

/// Maximum list scroll amount
pub fn list_max_scroll(st: &EditorState) -> i32 {
    (st.elements.len() as i32 - list_visible_count(st)).max(0)
}

/// List scroll amount (clamped)
pub fn list_scroll_clamped(st: &EditorState) -> i32 {
    st.list_scroll.clamp(0, list_max_scroll(st))
}

/// Draw the right panel (userscript-plugin style: dark background + rounded corners + separators)
pub unsafe fn draw_region_panel(hdc: HDC, st: &EditorState) {
    let (px, py, pw, ph) = panel_rect(st);
    let props_h = props_section_height(st);
    let add_top = py + ph - ADD_BTN_H - 8;
    let props_top = add_top - props_h;
    let (_lx, lsy, _lw, lsh) = list_area_rect(st);
    let list_bottom = lsy + lsh;

    // Background: dark black + rounded border
    fill_rect_solid(hdc, px, py, pw, ph, rgb(22, 22, 22));
    draw_rounded_border(hdc, px, py, pw, ph, rgb(80, 80, 80));

    // Title bar (#222 background)
    fill_rect_solid(hdc, px + 1, py + 1, pw - 2, TITLE_H - 1, rgb(34, 34, 34));
    gdi_text_left(hdc, crate::i18n::t("region_list"), px + 10, py, 100, TITLE_H, rgb(220, 220, 220));
    let count_label = format!("{}{}", st.elements.len(), crate::i18n::t("count_suffix"));
    gdi_text_left(hdc, &count_label, px + 100, py, 50, TITLE_H, rgb(150, 150, 150));
    gdi_text(hdc, crate::i18n::t("drag_hint"), px + pw - 70, py, 60, TITLE_H, rgb(100, 100, 100));
    fill_rect_solid(hdc, px, py + TITLE_H, pw, 1, rgb(42, 42, 42));

    // Filename input box
    gdi_text_left(hdc, crate::i18n::t("filename_label"), px + 10, py + TITLE_H + 4, pw - 20, 16, rgb(140, 140, 140));
    let fx = px + 10;
    let fy = py + TITLE_H + 22;
    let fw = pw - 20;
    let fh = FN_BOX_H;
    fill_rect_solid(hdc, fx, fy, fw, fh, rgb(14, 14, 14));
    let bcol = if st.filename_focused { rgb(255, 204, 0) } else { rgb(68, 68, 68) };
    draw_rect_outline(hdc, fx, fy, fw, fh, 1, bcol);
    if st.save_filename.is_empty() {
        if st.filename_focused {
            gdi_text_left(hdc, "|", fx + 8, fy, fw - 16, fh, rgb(255, 255, 255));
        } else {
            gdi_text_left(hdc, crate::i18n::t("filename_hint"), fx + 8, fy, fw - 16, fh, rgb(100, 100, 100));
        }
    } else {
        let disp = if st.filename_focused {
            format!("{}|", st.save_filename)
        } else {
            st.save_filename.clone()
        };
        gdi_text_left(hdc, &disp, fx + 8, fy, fw - 16, fh, rgb(255, 255, 255));
    }

    // Action button row + toggle/value rows (moved into the panel from the top)
    draw_panel_actions(hdc, st);
    draw_panel_toggles(hdc, st);
    fill_rect_solid(hdc, px, py + LIST_TOP2 - 10, pw, 1, rgb(42, 42, 42));

    // List items (start at the scroll offset, clipped to the list area)
    let scroll = list_scroll_clamped(st);
    let list_top = py + LIST_TOP2;
    for si in scroll as usize..st.elements.len() {
        let i = si;
        let iy = list_top + (si as i32 - scroll) * LIST_ITEM_H;
        if iy + LIST_ITEM_H > list_bottom {
            break;
        }
        let is_sel = st.selected_indices.contains(&i);
        let item_right = px + pw - SB_LANE - 4;

        if is_sel {
            fill_rect_solid(hdc, px + 4, iy, item_right - px - 8, LIST_ITEM_H - 2, rgb(38, 44, 66));
            fill_rect_solid(hdc, px + 4, iy, 3, LIST_ITEM_H - 2, rgb(80, 160, 255));
        }
        if i < st.elements.len() - 1 {
            fill_rect_solid(hdc, px + 8, iy + LIST_ITEM_H - 2, item_right - px - 16, 1, rgb(42, 42, 42));
        }

        let badge = st.elements.get(i).map(|e| e.extra.kind.badge()).unwrap_or("?");
        let idx_label = format!("#{}[{}]", i + 1, badge);
        let idx_col = if is_sel { rgb(150, 210, 255) } else { rgb(200, 200, 200) };
        gdi_text_left(hdc, &idx_label, px + 8, iy, IDX_W, LIST_ITEM_H - 2, idx_col);

        // Selecting phase: source coords (capture position); arranging phase: display coords (layout position).
        // Text has no fixed size (auto-fit) → show only X/Y, no W/H
        let is_text = st.elements.get(i).map(|e| e.extra.kind == ElemKind::Text).unwrap_or(false);
        let mut fields: Vec<(&str, i32, FieldKind, i32)> = vec![
            ("X", get_field_coord(st, i, FieldKind::X), FieldKind::X, 0),
            ("Y", get_field_coord(st, i, FieldKind::Y), FieldKind::Y, 1),
        ];
        if !is_text {
            fields.push(("W", get_field_coord(st, i, FieldKind::Width), FieldKind::Width, 2));
            fields.push(("H", get_field_coord(st, i, FieldKind::Height), FieldKind::Height, 3));
        }
        for (label, val, fk, col) in &fields {
            let fx2 = field_x(px, *col);
            let fw2 = FIELD_W;
            let fh2 = LIST_ITEM_H - 8;
            let fy2 = iy + 4;
            let is_editing = matches!(
                &st.editing_target,
                EditingTarget::RegionField(idx, fk2) if *idx == i && *fk2 == *fk
            );
            let bg = if is_editing { rgb(50, 56, 80) } else { rgb(14, 14, 14) };
            fill_rect_solid(hdc, fx2, fy2, fw2, fh2, bg);
            draw_rect_outline(hdc, fx2, fy2, fw2, fh2, 1, rgb(60, 60, 60));
            gdi_field_label(hdc, label, fx2 + 3, fy2, FIELD_LABEL_W, fh2, rgb(120, 120, 120));
            let val_str = if is_editing {
                format!("{}|", st.editing_text)
            } else {
                val.to_string()
            };
            // Value area + arrow button on the right
            gdi_field_value(hdc, &val_str, fx2 + FIELD_VAL_X, fy2, FIELD_VAL_W, fh2, rgb(150, 220, 160));
            draw_spinner_pub(hdc, fx2 + FIELD_W - SPIN_W, fy2, SPIN_W, fh2);
        }

        // Delete button (x) - arranging phase only
        if st.phase == EditorPhase::Arranging {
            let has_sb = st.elements.len() as i32 > list_visible_count(st);
            let del_x = px + pw - if has_sb { 36 } else { 24 };
            let del_y = iy + (LIST_ITEM_H - 18) / 2;
            gdi_text(hdc, "✕", del_x, del_y, 18, 18, rgb(180, 90, 90));
        }
    }

    // ===== Scrollbar (dedicated lane on the list's right edge, does not affect list content) =====
    let n = st.elements.len() as i32;
    let vis = list_visible_count(st);
    if n > vis {
        let sb_x = px + pw - SB_LANE;
        let thumb_h = (lsh * vis / n).max(24);
        let track_h = lsh - thumb_h;
        let thumb_y = lsy + track_h * scroll / (n - vis);
        fill_rect_solid(hdc, sb_x, lsy, SB_W, lsh, rgb(26, 26, 32));
        fill_rect_solid(hdc, sb_x, thumb_y, SB_W, thumb_h, rgb(72, 76, 92));
        draw_rect_outline(hdc, sb_x, thumb_y, SB_W, thumb_h, 1, rgb(100, 110, 140));
    }

    // ===== Element property area (when an element is selected) =====
    if props_h > 0 {
        if let Some(&idx) = st.selected_indices.last() {
            if let Some(extra) = st.elements.get(idx).map(|e| &e.extra) {
                draw_props_section(hdc, st, px, props_top, pw, props_h, idx, extra);
            }
        }
    }

    // Add-element button group: frame/bg/image/text (arranging phase only)
    if st.phase == EditorPhase::Arranging {
        draw_add_buttons(hdc, px, add_top + 4, pw);
    }
}

/// Draw the element property edit area
#[allow(clippy::too_many_arguments)]
unsafe fn draw_props_section(
    hdc: HDC,
    st: &EditorState,
    px: i32,
    top: i32,
    pw: i32,
    _h: i32,
    idx: usize,
    extra: &ElemExtra,
) {
    // Divider + title
    fill_rect_solid(hdc, px + 4, top, pw - 8, 1, rgb(58, 58, 72));
    let title_y = top + 4;
    gdi_text_left(
        hdc,
        crate::i18n::t("elem_props"),
        px + 8,
        title_y,
        pw - 16,
        PROP_SECTION_H - 4,
        rgb(150, 210, 255),
    );

    let props = props_for_kind(extra.kind);
    let label_w = 64i32;
    let value_x = px + 8 + label_w;
    let value_w = pw - 16 - label_w - 4;

    for (row, &prop) in props.iter().enumerate() {
        let ry = top + PROP_TOP_PAD + PROP_SECTION_H + row as i32 * PROP_ROW_H;
        // Label
        gdi_text_left(hdc, prop.label(), px + 8, ry, label_w, PROP_ROW_H - 6, rgb(150, 155, 170));

        // Value input box
        let is_editing = matches!(
            &st.editing_target,
            EditingTarget::ElemField(i, p) if *i == idx && *p == prop
        );
        let box_y = ry + 2;
        let box_h = PROP_ROW_H - 8;
        let bg = if is_editing { rgb(50, 56, 80) } else { rgb(14, 14, 14) };
        fill_rect_solid(hdc, value_x, box_y, value_w, box_h, bg);
        let border = if is_editing { rgb(255, 204, 0) } else { rgb(60, 60, 60) };
        draw_rect_outline(hdc, value_x, box_y, value_w, box_h, 1, border);

        let val_str = if is_editing {
            format!("{}|", st.editing_text)
        } else {
            read_prop(st, idx, extra, prop)
        };
        let fg = match prop {
            ElemProp::BorderColor | ElemProp::BgColor | ElemProp::TextColor => rgb(180, 220, 255),
            ElemProp::Content | ElemProp::PngPath => rgb(255, 255, 255),
            _ => rgb(150, 220, 160),
        };
        if prop.is_numeric() {
            let val_w = value_w - SPIN_W;
            gdi_field_value(hdc, &val_str, value_x + 6, box_y, val_w - 10, box_h, fg);
            draw_spinner_pub(hdc, value_x + value_w - SPIN_W, box_y, SPIN_W, box_h);
        } else {
            gdi_text_left(hdc, &val_str, value_x + 6, box_y, value_w - 10, box_h, fg);
        }
    }
}

/// Draw a rounded border
unsafe fn draw_rounded_border(hdc: HDC, x: i32, y: i32, w: i32, h: i32, color: COLORREF) {
    use windows::Win32::Graphics::Gdi::{CreatePen, SelectObject, RoundRect, DeleteObject, GetStockObject, NULL_BRUSH, PEN_STYLE};
    let pen = CreatePen(PEN_STYLE(0), 1, color);
    let old_pen = SelectObject(hdc, pen);
    let old_br = SelectObject(hdc, GetStockObject(NULL_BRUSH));
    let _ = RoundRect(hdc, x, y, x + w, y + h, 6, 6);
    SelectObject(hdc, old_pen);
    SelectObject(hdc, old_br);
    let _ = DeleteObject(pen);
}

/// Hit test: X/Y/W/H fields in the list
pub fn hit_test_field(st: &EditorState, x: i32, y: i32) -> Option<(usize, FieldKind)> {
    let (px, py, pw, ph) = panel_rect(st);
    if x < px || x > px + pw {
        return None;
    }
    let add_top = py + ph - ADD_BTN_H - 8;
    let props_h = props_section_height(st);
    let props_top = add_top - props_h;
    let list_bottom = if props_h > 0 { props_top } else { list_area_bottom(st, py, ph) };

    let list_top = py + LIST_TOP2;
    let scroll = list_scroll_clamped(st);
    for si in scroll as usize..st.elements.len() {
        let i = si;
        let iy = list_top + (si as i32 - scroll) * LIST_ITEM_H;
        if iy + LIST_ITEM_H > list_bottom {
            break;
        }
        if y < iy || y > iy + LIST_ITEM_H {
            continue;
        }
        // Text has no fixed size (auto-fit) → no W/H fields
        let is_text = st.elements.get(i).map(|e| e.extra.kind == ElemKind::Text).unwrap_or(false);
        for (col, fk) in [FieldKind::X, FieldKind::Y, FieldKind::Width, FieldKind::Height]
            .iter()
            .enumerate()
        {
            if is_text && (matches!(fk, FieldKind::Width | FieldKind::Height)) {
                continue;
            }
            let fx2 = field_x(px, col as i32);
            // Value area: from after the label to before the arrow button
            if x >= fx2 + FIELD_VAL_X && x <= fx2 + FIELD_W - SPIN_W {
                return Some((i, *fk));
            }
        }
    }
    None
}

/// Hit test: X/Y/W/H arrow buttons in the list. Returns (index, field, is-up)
pub fn hit_test_spin_field(st: &EditorState, x: i32, y: i32) -> Option<(usize, FieldKind, bool)> {
    let (px, py, pw, ph) = panel_rect(st);
    if x < px || x > px + pw {
        return None;
    }
    let add_top = py + ph - ADD_BTN_H - 8;
    let props_h = props_section_height(st);
    let props_top = add_top - props_h;
    let list_bottom = if props_h > 0 { props_top } else { list_area_bottom(st, py, ph) };

    let list_top = py + LIST_TOP2;
    let scroll = list_scroll_clamped(st);
    for si in scroll as usize..st.elements.len() {
        let i = si;
        let iy = list_top + (si as i32 - scroll) * LIST_ITEM_H;
        if iy + LIST_ITEM_H > list_bottom {
            break;
        }
        if y < iy || y > iy + LIST_ITEM_H {
            continue;
        }
        // Text has no fixed size (auto-fit) → no W/H spin buttons
        let is_text = st.elements.get(i).map(|e| e.extra.kind == ElemKind::Text).unwrap_or(false);
        for (col, fk) in [FieldKind::X, FieldKind::Y, FieldKind::Width, FieldKind::Height]
            .iter()
            .enumerate()
        {
            if is_text && (matches!(fk, FieldKind::Width | FieldKind::Height)) {
                continue;
            }
            let fx2 = field_x(px, col as i32);
            let fh2 = LIST_ITEM_H - 8;
            let fy2 = iy + 4;
            if x >= fx2 + FIELD_W - SPIN_W && x <= fx2 + FIELD_W && y >= fy2 && y <= fy2 + fh2 {
                return Some((i, *fk, y <= fy2 + fh2 / 2));
            }
        }
    }
    None
}

/// Hit test: fields in the element property area. Returns (element index, property)
pub fn hit_test_prop(st: &EditorState, x: i32, y: i32) -> Option<(usize, ElemProp)> {
    let (px, py, pw, ph) = panel_rect(st);
    if x < px || x > px + pw {
        return None;
    }
    let props_h = props_section_height(st);
    if props_h == 0 {
        return None;
    }
    let add_top = py + ph - ADD_BTN_H - 8;
    let props_top = add_top - props_h;
    if y < props_top || y > add_top {
        return None;
    }

    let idx = st.selected_indices.last().copied()?;
    let extra = st.elements.get(idx).map(|e| &e.extra)?;

    let label_w = 64i32;
    let value_x = px + 8 + label_w;
    let value_w = pw - 16 - label_w - 4;
    if x < value_x || x > value_x + value_w {
        return None;
    }

    let props = props_for_kind(extra.kind);
    for (row, &prop) in props.iter().enumerate() {
        let ry = props_top + PROP_TOP_PAD + PROP_SECTION_H + row as i32 * PROP_ROW_H;
        if y >= ry && y <= ry + PROP_ROW_H {
            return Some((idx, prop));
        }
    }
    None
}

/// Hit test: arrow buttons of numeric fields in the element property area. Returns (index, property, is-up)
pub fn hit_test_spin_prop(st: &EditorState, x: i32, y: i32) -> Option<(usize, ElemProp, bool)> {
    let (px, py, pw, ph) = panel_rect(st);
    if x < px || x > px + pw {
        return None;
    }
    let props_h = props_section_height(st);
    if props_h == 0 {
        return None;
    }
    let add_top = py + ph - ADD_BTN_H - 8;
    let props_top = add_top - props_h;
    if y < props_top || y > add_top {
        return None;
    }

    let idx = st.selected_indices.last().copied()?;
    let extra = st.elements.get(idx).map(|e| &e.extra)?;

    let label_w = 64i32;
    let value_x = px + 8 + label_w;
    let value_w = pw - 16 - label_w - 4;
    let sx = value_x + value_w - SPIN_W;
    if x < sx || x > value_x + value_w {
        return None;
    }

    let props = props_for_kind(extra.kind);
    for (row, &prop) in props.iter().enumerate() {
        if !prop.is_numeric() {
            continue;
        }
        let ry = props_top + PROP_TOP_PAD + PROP_SECTION_H + row as i32 * PROP_ROW_H;
        let box_y = ry + 2;
        let box_h = PROP_ROW_H - 8;
        if y >= box_y && y <= box_y + box_h {
            return Some((idx, prop, y <= box_y + box_h / 2));
        }
    }
    None
}

/// Wheel hit test: returns a SpinTarget for value nudging
pub fn spin_target_at(st: &EditorState, x: i32, y: i32) -> Option<SpinTarget> {
    if let Some((i, fk)) = hit_test_field(st, x, y) {
        return Some(SpinTarget::RegionField(i, fk));
    }
    if let Some((i, fk, _up)) = hit_test_spin_field(st, x, y) {
        return Some(SpinTarget::RegionField(i, fk));
    }
    if let Some((i, prop)) = hit_test_prop(st, x, y) {
        if prop.is_numeric() {
            return Some(SpinTarget::ElemProp(i, prop));
        }
    }
    if let Some((i, prop, _up)) = hit_test_spin_prop(st, x, y) {
        return Some(SpinTarget::ElemProp(i, prop));
    }
    None
}

/// Compute the placement for a new element: opposite side of the panel, stacked vertically
/// Returns overlay-relative coords (x, y)
fn next_element_pos(st: &EditorState, elem_w: i32, _elem_h: i32) -> (i32, i32) {
    let panel_center = st.panel_x + PANEL_W / 2;
    let screen_center = st.screen_w / 2;
    let is_panel_right = panel_center > screen_center;

    // Target screen x: the panel's other side
    let screen_x = if is_panel_right {
        (st.panel_x - elem_w - 10).max(4)
    } else {
        st.panel_x + PANEL_W + 10
    };
    // Convert to overlay-relative coords and clamp inside the overlay to stay visible
    let x = (screen_x - st.overlay_x).clamp(0, (st.overlay_w - elem_w).max(0));

    // Vertical stacking: base_y aligns with the panel list area (overlay-relative coords)
    let base_y = (st.panel_y + LIST_TOP2 - st.overlay_y).max(0);
    let prev_bottom = st
        .elements
        .last()
        .map(|p| p.display.y + p.display.height + 10)
        .unwrap_or(base_y);
    let y = prev_bottom.max(base_y);
    let y = y.clamp(0, (st.overlay_h - _elem_h).max(0));

    (x, y)
}

/// Create a new UI element of the given kind (default 40x20; images default to 0x0 = 1:1 original, shown as a 40x20 placeholder)
pub fn add_ui_element(st: &mut EditorState, kind: ElemKind) {
    super::state::push_undo(st); // one undo step per added element
    let (x, y) = next_element_pos(st, 40, 20);
    // Image/text: width/height 0 (auto-sized), no width/height entries written on save
    let (w, h) = if kind == ElemKind::Png || kind == ElemKind::Text { (0, 0) } else { (40, 20) };
    let mut extra = ElemExtra::new_ui(kind);
    extra.opacity = st.global_opacity; // display inherits global; still not written to the entry on save
    st.elements.push(Element::new_ui(
        DisplayRect { x, y, width: w, height: h, z_order: 0, opacity: None, rotate: 0 },
        extra,
    ));
}

// ===== Add-element button group =====

/// Add-button kind
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddKind {
    Frame,
    Background,
    Png,
    Text,
}

const ADD_LABELS: [(AddKind, &str); 4] = [
    (AddKind::Frame, "add_frame"),
    (AddKind::Background, "add_bg"),
    (AddKind::Png, "add_png"),
    (AddKind::Text, "add_text"),
];

/// Draw the add-button group (4 buttons in a row)
unsafe fn draw_add_buttons(hdc: HDC, px: i32, y: i32, pw: i32) {
    let h = 22;
    let gap = 4;
    let total_gap = gap * (ADD_LABELS.len() as i32 - 1);
    let btn_w = (pw - 16 - total_gap) / ADD_LABELS.len() as i32;
    for (i, (_, label)) in ADD_LABELS.iter().enumerate() {
        let label = crate::i18n::t(label);
        let bx = px + 8 + i as i32 * (btn_w + gap);
        // Same style as the other buttons; no emphasis on the "frame" button
        let (bg, border, fg) = (rgb(74, 63, 45), rgb(106, 90, 63), rgb(243, 232, 216));
        fill_rect_solid(hdc, bx, y, btn_w, h, bg);
        draw_rect_outline(hdc, bx, y, btn_w, h, 1, border);
        gdi_text(hdc, label, bx, y, btn_w, h, fg);
    }
}

/// Hit test: add-button group
pub fn add_button_hit(st: &EditorState, x: i32, y: i32) -> Option<AddKind> {
    let (px, py, pw, ph) = panel_rect(st);
    let btn_y = py + ph - ADD_BTN_H - 4;
    let h = 22;
    if y < btn_y || y > btn_y + h {
        return None;
    }
    let gap = 4;
    let total_gap = gap * (ADD_LABELS.len() as i32 - 1);
    let btn_w = (pw - 16 - total_gap) / ADD_LABELS.len() as i32;
    for (i, (kind, _)) in ADD_LABELS.iter().enumerate() {
        let bx = px + 8 + i as i32 * (btn_w + gap);
        if x >= bx && x <= bx + btn_w {
            return Some(*kind);
        }
    }
    None
}

/// Start editing an X/Y/W/H field
pub fn start_editing_field(st: &mut EditorState, idx: usize, fk: FieldKind) {
    let val = get_field_coord(st, idx, fk);
    st.editing_target = EditingTarget::RegionField(idx, fk);
    st.editing_text = val.to_string();
    st.filename_focused = false;
}

/// Start editing an element property field
pub fn start_editing_prop(st: &mut EditorState, idx: usize, prop: ElemProp) {
    let val = st
        .elements
        .get(idx)
        .map(|e| read_prop(st, idx, &e.extra, prop))
        .unwrap_or_default();
    st.editing_target = EditingTarget::ElemField(idx, prop);
    st.editing_text = val;
    st.filename_focused = false;
}

/// Commit editing: write editing_text back to the field
pub fn commit_editing(st: &mut EditorState) {
    let before = st.elements.clone();
    match &st.editing_target {
        EditingTarget::RegionField(idx, fk) => {
            if let Ok(val) = st.editing_text.parse::<i32>() {
                let val = val.max(0);
                set_field_coord(st, *idx, *fk, val);
            }
        }
        EditingTarget::ElemField(idx, prop) => {
            apply_prop(st, *idx, *prop, &st.editing_text.clone());
        }
        EditingTarget::SnapDistance => {
            if let Ok(val) = st.editing_text.parse::<i32>() {
                st.snap_distance = val.clamp(1, 100);
                crate::config::save_editor_prefs(st.snap_distance, st.snap_gap, st.nudge_step);
            }
        }
        EditingTarget::SnapGap => {
            if let Ok(val) = st.editing_text.parse::<i32>() {
                st.snap_gap = val.clamp(0, 50);
                crate::config::save_editor_prefs(st.snap_distance, st.snap_gap, st.nudge_step);
            }
        }
        EditingTarget::NudgeStep => {
            if let Ok(val) = st.editing_text.parse::<i32>() {
                st.nudge_step = val.clamp(1, 100);
                crate::config::save_editor_prefs(st.snap_distance, st.snap_gap, st.nudge_step);
            }
        }
        EditingTarget::None => {}
    }
    // One undo step when the commit actually changed something (dedup via the stack top)
    if st.elements != before {
        super::state::push_snapshot(st, before);
    }
    st.editing_target = EditingTarget::None;
    st.editing_text.clear();
}

/// Increment/decrement per SpinTarget. dir: +1 up, -1 down (one unit per operation).
/// List X/Y/W/H fields: each "unit" moves by the nudge step; arrow keys/wheel/buttons behave identically.
pub fn apply_spin(st: &mut EditorState, target: SpinTarget, dir: i32) {
    let before = st.elements.clone();
    match target {
        SpinTarget::RegionField(idx, fk) => {
            let step = st.nudge_step.max(1);
            let cur = get_field_coord(st, idx, fk);
            set_field_coord(st, idx, fk, cur + dir * step);
        }
        SpinTarget::ElemProp(idx, prop) => {
            let Some(e) = st.elements.get_mut(idx) else { return };
            let extra = &mut e.extra;
            match prop {
                // Opacity steps by 0.1
                ElemProp::Opacity => {
                    let step = 0.1 * dir as f32;
                    extra.opacity = (extra.opacity + step).clamp(0.0, 1.0);
                    extra.opacity_explicit = true;
                    e.display.opacity = Some(extra.opacity);
                }
                ElemProp::ZOrder => extra.z_order += dir,
                ElemProp::Rotate => {
                    // display.rotate is a disjoint field from extra (borrowed above) → allowed
                    e.display.rotate = (e.display.rotate + dir).rem_euclid(360);
                }
                ElemProp::BorderWidth => {
                    extra.border_width = ((extra.border_width as i32 + dir).max(0)) as u32
                }
                ElemProp::CornerRadius => {
                    extra.corner_radius = ((extra.corner_radius as i32 + dir).max(0)) as u32
                }
                ElemProp::FontSize => {
                    extra.font_size = ((extra.font_size as i32 + dir).max(1)) as u32
                }
                _ => {}
            }
        }
        SpinTarget::SnapDistance => {
            st.snap_distance = (st.snap_distance + dir).clamp(1, 100);
            crate::config::save_editor_prefs(st.snap_distance, st.snap_gap, st.nudge_step);
        }
        SpinTarget::SnapGap => {
            st.snap_gap = (st.snap_gap + dir).clamp(0, 50);
            crate::config::save_editor_prefs(st.snap_distance, st.snap_gap, st.nudge_step);
        }
        SpinTarget::NudgeStep => {
            st.nudge_step = (st.nudge_step + dir).clamp(1, 100);
            crate::config::save_editor_prefs(st.snap_distance, st.snap_gap, st.nudge_step);
        }
    }
    // One undo step per spin tick when the value actually changed (dedup via the stack top)
    if st.elements != before {
        super::state::push_snapshot(st, before);
    }
}

/// Write the text being edited back to the field in real time (for WYSIWYG fields like colors)
pub fn sync_live_edit(st: &mut EditorState) {
    if let EditingTarget::ElemField(idx, prop) = &st.editing_target {
        let text = st.editing_text.clone();
        apply_prop(st, *idx, *prop, &text);
    }
}

/// Apply a string value to an element property (with clamp / color normalization)
fn apply_prop(st: &mut EditorState, idx: usize, prop: ElemProp, text: &str) {
    let Some(e) = st.elements.get_mut(idx) else { return };
    let extra = &mut e.extra;
    match prop {
        ElemProp::Opacity => {
            if let Ok(v) = text.parse::<f32>() {
                extra.opacity = v.clamp(0.0, 1.0);
                extra.opacity_explicit = true;
            }
        }
        ElemProp::ZOrder => {
            if let Ok(v) = text.parse::<i32>() {
                extra.z_order = v;
            }
        }
        ElemProp::Rotate => {
            if let Ok(v) = text.parse::<i32>() {
                e.display.rotate = v.rem_euclid(360);
            }
        }
        ElemProp::BorderColor => extra.border_color = normalize_color(text),
        ElemProp::BorderWidth => {
            if let Ok(v) = text.parse::<u32>() {
                extra.border_width = v.min(50);
            }
        }
        ElemProp::CornerRadius => {
            if let Ok(v) = text.parse::<u32>() {
                extra.corner_radius = v.min(200);
            }
        }
        ElemProp::BgColor => extra.color = normalize_color(text),
        ElemProp::Content => extra.content = text.to_string(),
        ElemProp::FontSize => {
            if let Ok(v) = text.parse::<u32>() {
                extra.font_size = v;
            }
        }
        ElemProp::TextColor => extra.text_color = normalize_color(text),
        ElemProp::PngPath => extra.png_path = text.to_string(),
    }
    // Sync the display opacity (UI elements use it for geometric opacity too)
    if prop == ElemProp::Opacity {
        e.display.opacity = Some(extra.opacity);
    }
}

/// Normalize a color input to "#RRGGBB" (lenient).
/// Empty → return empty string (means "no color specified"; runtime treats it as transparent/default)
fn normalize_color(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() {
        return String::new();
    }
    let t = t.trim_start_matches('#');
    if t.len() == 6 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("#{}", t.to_uppercase())
    } else {
        text.to_string()
    }
}

/// Toggle selection (Ctrl+click multi-select)
pub fn toggle_select(st: &mut EditorState, idx: usize) {
    if let Some(pos) = st.selected_indices.iter().position(|&i| i == idx) {
        st.selected_indices.remove(pos);
    } else {
        st.selected_indices.push(idx);
    }
}
