//! Panel controls, bottom hint bar, overlay confirm popup

use windows::Win32::Foundation::COLORREF;

use super::common::*;
use super::panel::panel_rect;
use super::state::{EditorPhase, EditingTarget, EditorState, HDC};

// ===== Panel action button row =====

pub struct Btn {
    pub x: i32,
    pub w: i32,
    pub label: &'static str,
    pub bg: COLORREF,
    pub fg: COLORREF,
    pub border: COLORREF,
    pub action: BtnAction,
}

/// Mode-switch buttons (own row): enter arrange / enter select mode
pub fn mode_buttons(phase: EditorPhase) -> Vec<Btn> {
    let mut v: Vec<Btn> = Vec::new();
    let mut x = 0;
    let gap = 8;
    macro_rules! push {
        ($w:expr, $label:expr, $bg:expr, $fg:expr, $bd:expr, $act:expr) => {{
            v.push(Btn { x, w: $w, label: $label, bg: $bg, fg: $fg, border: $bd, action: $act });
            x += $w + gap;
        }};
    }
    match phase {
        EditorPhase::Selecting => {
            push!(164, crate::i18n::t("next_step"), rgb(56, 132, 96), rgb(255, 255, 255), rgb(120, 200, 150), BtnAction::NextStep);
        }
        EditorPhase::Arranging => {
            push!(164, crate::i18n::t("back"), rgb(74, 108, 150), rgb(255, 255, 255), rgb(130, 170, 210), BtnAction::BackToSelect);
        }
    }
    let _ = x;
    v
}

/// Action buttons (x relative to the row's left edge)
pub fn toolbar_buttons(phase: EditorPhase) -> Vec<Btn> {
    let mut v: Vec<Btn> = Vec::new();
    let mut x = 0;
    let gap = 8;
    macro_rules! push {
        ($w:expr, $label:expr, $bg:expr, $fg:expr, $bd:expr, $act:expr) => {{
            v.push(Btn { x, w: $w, label: $label, bg: $bg, fg: $fg, border: $bd, action: $act });
            x += $w + gap;
        }};
    }
    match phase {
        EditorPhase::Selecting => {
            push!(120, crate::i18n::t("undo_last"), rgb(60, 66, 92), rgb(220, 225, 240), rgb(90, 96, 130), BtnAction::UndoLast);
            push!(88, crate::i18n::t("cancel"), rgb(150, 64, 64), rgb(255, 255, 255), rgb(200, 120, 120), BtnAction::Cancel);
        }
        EditorPhase::Arranging => {
            push!(120, crate::i18n::t("undo_last"), rgb(60, 66, 92), rgb(220, 225, 240), rgb(90, 96, 130), BtnAction::UndoLast);
            push!(96, crate::i18n::t("delete_selected"), rgb(150, 64, 64), rgb(255, 255, 255), rgb(200, 120, 120), BtnAction::DeleteSelected);
            push!(88, crate::i18n::t("save"), rgb(56, 132, 96), rgb(255, 255, 255), rgb(120, 200, 150), BtnAction::Save);
            push!(76, crate::i18n::t("discard"), rgb(150, 64, 64), rgb(255, 255, 255), rgb(200, 120, 120), BtnAction::Discard);
        }
    }
    let _ = x;
    v
}

/// Hit test for the mode-switch row
pub fn panel_mode_hit(st: &EditorState, x: i32, y: i32) -> Option<BtnAction> {
    let (rx, ry, _rw, rh) = panel_mode_rect(st);
    if y < ry || y > ry + rh {
        return None;
    }
    for btn in mode_buttons(st.phase) {
        if x >= rx + btn.x && x <= rx + btn.x + btn.w {
            return Some(btn.action);
        }
    }
    None
}

/// Hit test for the action button row
pub fn panel_action_hit(st: &EditorState, x: i32, y: i32) -> Option<BtnAction> {
    let (rx, ry, _rw, rh) = panel_action_rect(st);
    if y < ry || y > ry + rh {
        return None;
    }
    for btn in toolbar_buttons(st.phase) {
        if x >= rx + btn.x && x <= rx + btn.x + btn.w {
            return Some(btn.action);
        }
    }
    None
}

// ===== Panel toggle/value rows =====

struct ToggleDef {
    x: i32,
    w: i32,
    label: &'static str,
    kind: ToggleKind,
}

/// Toggle row buttons (x relative to the row's left edge)
fn tog1_defs() -> Vec<ToggleDef> {
    vec![
        ToggleDef { x: 0,   w: 60, label: crate::i18n::t("toggle_magnifier"), kind: ToggleKind::Magnifier },
        ToggleDef { x: 64,  w: 52, label: crate::i18n::t("toggle_grid"),   kind: ToggleKind::Grid },
        ToggleDef { x: 120, w: 60, label: crate::i18n::t("toggle_crosshair"), kind: ToggleKind::Crosshair },
        ToggleDef { x: 184, w: 52, label: crate::i18n::t("toggle_xy"),   kind: ToggleKind::XyLabel },
        ToggleDef { x: 240, w: 48, label: crate::i18n::t("toggle_snap"),   kind: ToggleKind::Snap },
        ToggleDef { x: 292, w: 56, label: crate::i18n::t("toggle_tian"),   kind: ToggleKind::TianSnap },
    ]
}

/// Value row: (toggle kind, label x, label width, label text) relative to the row's left edge
fn tog2_defs() -> Vec<(ToggleKind, i32, i32, &'static str)> {
    vec![
        (ToggleKind::SnapDistance, 0, 92, crate::i18n::t("snap_dist")),
        (ToggleKind::SnapGap, 184, 48, crate::i18n::t("snap_gap")),
        (ToggleKind::NudgeStep, 308, 48, crate::i18n::t("nudge_step")),
    ]
}

/// Panel section rect (screen coords): row 0=mode 1=action 2=toggles 3=values
fn panel_section_rect(st: &EditorState, row: i32) -> (i32, i32, i32, i32) {
    let (px, py, pw, _ph) = panel_rect(st);
    let y = py + TITLE_H + 22 + FN_BOX_H + 8 + row * (SECTION_H + 8);
    (px + 8, y, pw - 16, SECTION_H)
}

pub fn panel_mode_rect(st: &EditorState) -> (i32, i32, i32, i32) {
    panel_section_rect(st, 0)
}

pub fn panel_action_rect(st: &EditorState) -> (i32, i32, i32, i32) {
    panel_section_rect(st, 1)
}

pub fn panel_tog1_rect(st: &EditorState) -> (i32, i32, i32, i32) {
    panel_section_rect(st, 2)
}

pub fn panel_tog2_rect(st: &EditorState) -> (i32, i32, i32, i32) {
    panel_section_rect(st, 3)
}

/// Input box rect of a toggle in the value row (screen coords)
fn tog2_box_rect(st: &EditorState, kind: ToggleKind) -> (i32, i32, i32, i32) {
    let (vx, vy, _vw, vh) = panel_tog2_rect(st);
    for (k, lx, lw, _label) in tog2_defs() {
        if k == kind {
            return (vx + lx + lw + 8, vy, TOG_BOX_W, vh);
        }
    }
    (0, 0, 0, 0)
}

/// Hit test for the toggle row buttons
pub fn panel_toggle_button_hit(st: &EditorState, x: i32, y: i32) -> Option<ToggleKind> {
    let (rx, ry, _rw, rh) = panel_tog1_rect(st);
    if y < ry || y > ry + rh {
        return None;
    }
    for td in tog1_defs() {
        if x >= rx + td.x && x <= rx + td.x + td.w {
            return Some(td.kind);
        }
    }
    None
}

/// Hit test for the whole value box (incl. arrow area)
pub fn panel_toggle_box_hit(st: &EditorState, x: i32, y: i32) -> Option<ToggleKind> {
    for (kind, _lx, _lw, _label) in tog2_defs() {
        let (bx, by, bw, bh) = tog2_box_rect(st, kind);
        if x >= bx && x <= bx + bw && y >= by && y <= by + bh {
            return Some(kind);
        }
    }
    None
}

/// Hit test for value box arrow buttons: returns (toggle kind, is-up)
pub fn panel_toggle_spin_hit(st: &EditorState, x: i32, y: i32) -> Option<(ToggleKind, bool)> {
    for (kind, _lx, _lw, _label) in tog2_defs() {
        let (bx, by, bw, bh) = tog2_box_rect(st, kind);
        let sx = bx + bw - SPIN_W;
        if x >= sx && x <= bx + bw && y >= by && y <= by + bh {
            return Some((kind, y <= by + bh / 2));
        }
    }
    None
}

/// Wheel hit test: the whole value box
pub fn panel_toggle_wheel_hit(st: &EditorState, x: i32, y: i32) -> Option<ToggleKind> {
    panel_toggle_box_hit(st, x, y)
}

// ===== Panel drawing =====

pub unsafe fn draw_panel_actions(hdc: HDC, st: &EditorState) {
    // Mode-switch row
    let (mx, my, _mw, mh) = panel_mode_rect(st);
    for btn in mode_buttons(st.phase) {
        draw_button(hdc, mx + btn.x, my, btn.w, mh, btn.label, btn.bg, btn.fg, btn.border);
    }
    // Action row
    let (rx, ry, _rw, rh) = panel_action_rect(st);
    for btn in toolbar_buttons(st.phase) {
        draw_button(hdc, rx + btn.x, ry, btn.w, rh, btn.label, btn.bg, btn.fg, btn.border);
    }
}

pub unsafe fn draw_panel_toggles(hdc: HDC, st: &EditorState) {
    // Toggle row
    let (rx, ry, _rw, rh) = panel_tog1_rect(st);
    for td in tog1_defs() {
        let on = match td.kind {
            ToggleKind::Magnifier => st.magnifier_on,
            ToggleKind::Grid => st.grid_on,
            ToggleKind::Crosshair => st.crosshair_on,
            ToggleKind::XyLabel => st.xy_label_on,
            ToggleKind::Snap => st.snap_on,
            ToggleKind::TianSnap => st.snap_tian,
            _ => false,
        };
        let bg = if on { rgb(56, 132, 96) } else { rgb(50, 50, 60) };
        let fg = if on { rgb(255, 255, 255) } else { rgb(120, 120, 130) };
        let border = if on { rgb(120, 200, 150) } else { rgb(70, 70, 80) };
        draw_button(hdc, rx + td.x, ry, td.w, rh, td.label, bg, fg, border);
    }
    // Value row
    let (vx, vy, _vw, vh) = panel_tog2_rect(st);
    for (kind, lx, lw, label) in tog2_defs() {
        let (val, is_editing) = match kind {
            ToggleKind::SnapDistance => (
                st.snap_distance.to_string(),
                matches!(st.editing_target, EditingTarget::SnapDistance),
            ),
            ToggleKind::SnapGap => (
                st.snap_gap.to_string(),
                matches!(st.editing_target, EditingTarget::SnapGap),
            ),
            ToggleKind::NudgeStep => (
                st.nudge_step.to_string(),
                matches!(st.editing_target, EditingTarget::NudgeStep),
            ),
            _ => continue,
        };
        gdi_text_left(hdc, label, vx + lx, vy, lw, vh, rgb(160, 165, 180));
        let (bx, by, bw, bh) = (vx + lx + lw + 8, vy, TOG_BOX_W, vh);
        let bg = if is_editing { rgb(60, 70, 100) } else { rgb(30, 34, 50) };
        fill_rect_solid(hdc, bx, by, bw, bh, bg);
        draw_rect_outline(hdc, bx, by, bw, bh, 1, rgb(80, 90, 130));
        let txt = if is_editing { format!("{}|", st.editing_text) } else { val };
        gdi_text(hdc, &txt, bx + 2, by, bw - SPIN_W - 4, bh, rgb(120, 230, 140));
        use super::panel::draw_spinner_pub;
        draw_spinner_pub(hdc, bx + bw - SPIN_W, by, SPIN_W, bh);
    }
}

pub unsafe fn draw_hint_bar(hdc: HDC, st: &EditorState) {
    // Hint bar dodges the mouse: normally at the bottom, jumps to the top when the
    // mouse approaches the bottom edge (and back when it approaches the top edge)
    let hy = if st.hint_top { 0 } else { st.screen_h - 40 };
    fill_rect_solid(hdc, 0, hy, st.screen_w, 40, rgb(22, 24, 38));
    fill_rect_solid(hdc, 0, hy, st.screen_w, 1, rgb(60, 66, 92));
    let hint = match st.phase {
        EditorPhase::Selecting => crate::i18n::t("hint_selecting"),
        EditorPhase::Arranging => crate::i18n::t("hint_arranging"),
    };
    gdi_text(hdc, hint, 16, hy, st.screen_w - 32, 40, rgb(200, 205, 220));
}

// ===== Overlay confirm popup =====

pub fn confirm_box(sw: i32, sh: i32) -> (i32, i32, i32, i32) {
    let cw = 460; let ch = 156;
    ((sw - cw) / 2, (sh - ch) / 2, cw, ch)
}

pub fn resolve_filename(s: &str) -> String {
    if s.is_empty() {
        crate::config::UNNAMED_CONFIG.to_string()
    } else {
        // YAML support dropped: strip a trailing .yaml/.yml from the input, always append .yml
        let stem = s.trim_end_matches(".yaml").trim_end_matches(".yml");
        format!("{stem}.yml")
    }
}

pub fn config_file_exists(filename: &str) -> bool {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join(filename).exists();
        }
    }
    false
}

pub unsafe fn draw_confirm_modal(hdc: HDC, st: &EditorState) {
    let (cx, cy, cw, ch) = confirm_box(st.screen_w, st.screen_h);
    fill_rect_solid(hdc, cx, cy, cw, ch, rgb(28, 30, 48));
    draw_rect_outline(hdc, cx, cy, cw, ch, 2, rgb(220, 170, 60));

    let fname = resolve_filename(&st.save_filename);
    gdi_text(hdc, crate::i18n::t("cfg_exists"), cx, cy + 18, cw, 28, rgb(255, 220, 120));
    gdi_text(hdc, &format!("\u{201C}{}\u{201D} {}", fname, crate::i18n::t("overwrite_ask")), cx, cy + 52, cw, 28, rgb(210, 215, 230));

    draw_button(hdc, cx + 60, cy + 100, 150, 40, crate::i18n::t("yes_overwrite"), rgb(150, 64, 64), rgb(255, 255, 255), rgb(210, 110, 110));
    draw_button(hdc, cx + 250, cy + 100, 150, 40, crate::i18n::t("no"), rgb(60, 66, 92), rgb(220, 225, 240), rgb(90, 96, 130));
}