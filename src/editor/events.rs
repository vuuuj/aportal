//! Editor event handling: mouse/keyboard event dispatch and state changes
//!
//! Event logic split out of mod.rs.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT,
    VK_UP, VK_Z,
};

use crate::config::{DisplayRect, Rect};

use super::common::*;
use super::state::{do_undo, push_snapshot, push_undo, with_state, EditorPhase, EditingTarget, ElemExtra, ElemKind, Element};
use super::panel::{
    add_button_hit, add_ui_element, apply_spin, commit_editing, hit_test_field,
    hit_test_prop, hit_test_spin_field, hit_test_spin_prop, list_area_rect,
    list_max_scroll, panel_rect, spin_target_at, start_editing_field, start_editing_prop,
    sync_live_edit, toggle_select, AddKind, SB_LANE,
};
use super::snap::{snap_arrangement, snap_resize, snap_selection};
use super::arranging::hit_test;
use super::ui::{
    confirm_box, panel_action_hit, panel_mode_hit, panel_toggle_box_hit, panel_toggle_button_hit,
    panel_toggle_spin_hit, panel_toggle_wheel_hit,
};
use super::request_save_inner;

/// Create an element based on the add-button kind
fn apply_add_kind(st: &mut super::state::EditorState, k: AddKind) {
    match k {
        AddKind::Frame => add_ui_element(st, ElemKind::Frame),
        AddKind::Background => add_ui_element(st, ElemKind::Background),
        AddKind::Png => add_ui_element(st, ElemKind::Png),
        AddKind::Text => add_ui_element(st, ElemKind::Text),
    }
}

/// Abort any in-progress mouse interaction (drag/box-select/resize).
/// Phase switching must call this: leftover state from the old phase would act on the new
/// one (e.g. a Selecting drag box kept alive after Enter leaves a ghost box when switching back).
fn cancel_interactions(st: &mut super::state::EditorState) {
    st.is_dragging = false;
    st.drag_start = None;
    st.drag_current = None;
    st.drag_index = None;
    st.resize_index = None;
    st.resize_start = None;
    st.resize_start_font = 0;
    st.multi_dragging = false;
    st.box_selecting = false;
    st.box_select_start = None;
    st.box_select_current = None;
    st.undo_pending = None;
}

// ===== Mouse down =====

pub fn on_lbutton_down(x: i32, y: i32) -> bool {
    let (confirm, _screen_w) = with_state(|st| (st.confirm_overwrite, st.screen_w));

    if confirm {
        return handle_confirm_click(x, y);
    }

    // Only mutate state inside the lock; take out the hit action/toggle and call
    // handle_toolbar_action / handle_toggle_click outside the lock (they re-lock internally)
    let (action, toggle) = with_state(|st| {
        st.editing_target = EditingTarget::None;
        st.editing_text.clear();

        // ===== Right panel (all controls: title/filename/action buttons/toggles/values/list) =====
        let (px, py, pw, ph) = panel_rect(st);
        if x >= px && x <= px + pw && y >= py && y <= py + ph {
            // Title bar dragging
            if y <= py + TITLE_H {
                st.drag_panel = true;
                st.panel_drag_offset = (x - px, y - py);
                st.filename_focused = false;
                return (None, None);
            }
            // Mode-switch row (enter arrange / select mode)
            if let Some(act) = panel_mode_hit(st, x, y) {
                st.filename_focused = false;
                return (Some(act), None);
            }
            // Action button row
            if let Some(act) = panel_action_hit(st, x, y) {
                st.filename_focused = false;
                return (Some(act), None);
            }
            // Value box arrow buttons (take priority over whole-box hit)
            if let Some((tk, up)) = panel_toggle_spin_hit(st, x, y) {
                let dir = if up { 1 } else { -1 };
                let target = match tk {
                    ToggleKind::SnapDistance => SpinTarget::SnapDistance,
                    ToggleKind::SnapGap => SpinTarget::SnapGap,
                    ToggleKind::NudgeStep => SpinTarget::NudgeStep,
                    _ => return (None, None),
                };
                apply_spin(st, target, dir);
                return (None, None);
            }
            // Value box click → start editing
            if let Some(tk) = panel_toggle_box_hit(st, x, y) {
                return (None, Some(tk));
            }
            // Toggle row buttons
            if let Some(tk) = panel_toggle_button_hit(st, x, y) {
                return (None, Some(tk));
            }
            // Filename box
            let fx = px + 10;
            let fy = py + TITLE_H + 22;
            let fw = pw - 20;
            let fh = FN_BOX_H;
            if x >= fx && x <= fx + fw && y >= fy && y <= fy + fh {
                st.filename_focused = true;
                return (None, None);
            }
            // Arrow buttons (list fields)
            if let Some((idx, fk, up)) = hit_test_spin_field(st, x, y) {
                let dir = if up { 1 } else { -1 };
                apply_spin(st, SpinTarget::RegionField(idx, fk), dir);
                return (None, None);
            }
            // Arrow buttons (properties)
            if let Some((idx, prop, up)) = hit_test_spin_prop(st, x, y) {
                let dir = if up { 1 } else { -1 };
                apply_spin(st, SpinTarget::ElemProp(idx, prop), dir);
                return (None, None);
            }
            // Property fields in the list
            if let Some((idx, fk)) = hit_test_field(st, x, y) {
                start_editing_field(st, idx, fk);
                return (None, None);
            }
            // Element property fields (colors etc.)
            if let Some((idx, prop)) = hit_test_prop(st, x, y) {
                start_editing_prop(st, idx, prop);
                return (None, None);
            }
            // Add-element button group (arranging only): +frame/bg/image/text — create on press and follow the mouse, place on release
            if st.phase == EditorPhase::Arranging {
                if let Some(kind) = add_button_hit(st, x, y) {
                    // Pre-add snapshot: committed on mouse-up, so "add + drag into place"
                    // is ONE undo step (the push inside add_ui_element is deduped against it)
                    st.undo_pending = Some(st.elements.clone());
                    apply_add_kind(st, kind);
                    let idx = st.elements.len() - 1;
                    let (ew, eh) = (st.elements[idx].display.width, st.elements[idx].display.height);
                    // Element center follows the mouse, then dragged along
                    st.elements[idx].display.x = (x - st.overlay_x - ew / 2).max(0);
                    st.elements[idx].display.y = (y - st.overlay_y - eh / 2).max(0);
                    st.drag_index = Some(idx);
                    st.drag_offset = (ew / 2, eh / 2);
                    st.selected_indices = vec![idx];
                    st.filename_focused = false;
                    return (None, None);
                }
            }
            // Scrollbar: thumb drag / track click to jump
            {
                let (_lsx, lsy, _lsw, lsh) = list_area_rect(st);
                if x >= px + pw - SB_LANE - 2 && x <= px + pw - 2 && y >= lsy && y <= lsy + lsh {
                    let n = st.elements.len() as i32;
                    let vis = (lsh / LIST_ITEM_H).max(1);
                    let max_scroll = (n - vis).max(0);
                    if max_scroll > 0 {
                            let thumb_h = (lsh * vis / n).max(24);
                            let track_h = lsh - thumb_h;
                            let cur_top = lsy + track_h * st.list_scroll.clamp(0, max_scroll) / max_scroll;
                            if y >= cur_top && y <= cur_top + thumb_h {
                                // Thumb hit → drag
                                st.scroll_drag = Some((y, cur_top - lsy));
                            } else {
                                // Track click → jump to the approximate position
                                let frac = ((y - lsy) as f32 - thumb_h as f32 / 2.0) / lsh as f32;
                                st.list_scroll = ((frac * max_scroll as f32).round() as i32).clamp(0, max_scroll);
                            }
                            st.filename_focused = false;
                            return (None, None);
                        }
                    }
            }
            // List item: select element
            if y >= py + LIST_TOP2 {
                let scroll = st.list_scroll.clamp(0, list_max_scroll(st));
                let idx = scroll as usize + ((y - py - LIST_TOP2) / LIST_ITEM_H) as usize;
                if idx < st.elements.len() {
                    let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 };
                    if ctrl {
                        toggle_select(st, idx);
                    } else {
                        st.selected_indices = vec![idx];
                    }
                    st.filename_focused = false;
                    return (None, None);
                }
                // Click on empty list space (not enough items): intercept, don't fall through to canvas
                st.filename_focused = false;
                return (None, None);
            }
            // Any other panel area: intercept, don't fall through to canvas
            st.filename_focused = false;
            return (None, None);
        }

        // ===== Outside the panel: canvas =====
        st.filename_focused = false;
        match st.phase {
            EditorPhase::Selecting => {
                st.is_dragging = true;
                // When tian snap is on: all other snapping (edge/gap) is disabled, only tian snap remains
                let (sx, sy) = if st.snap_on && !st.snap_tian {
                    snap_selection(st, x, y)
                } else {
                    st.snapped_x = None;
                    st.snapped_y = None;
                    (x, y)
                };
                st.drag_start = Some((sx, sy));
                st.drag_current = Some((sx, sy));
            }
            EditorPhase::Arranging => {
                let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 };
                match hit_test(st, x, y) {
                    Some((idx, true)) => {
                        // Snapshot for the undo step; pushed once on mouse-up if the element really changed
                        st.undo_pending = Some(st.elements.clone());
                        st.resize_index = Some(idx);
                        // Text element: record drag start + initial font size for edit-handle drag font scaling
                        let is_text = st.elements.get(idx).map(|e| e.extra.kind == ElemKind::Text).unwrap_or(false);
                        st.resize_start = if is_text { Some((x, y)) } else { None };
                        st.resize_start_font = st.elements.get(idx).map(|e| e.extra.font_size).unwrap_or(0);
                        if !ctrl {
                            st.selected_indices = vec![idx];
                        }
                    }
                    Some((idx, false)) => {
                        // Snapshot for the undo step; pushed once on mouse-up if the element really changed
                        st.undo_pending = Some(st.elements.clone());
                        if ctrl {
                            toggle_select(st, idx);
                        } else if !st.selected_indices.contains(&idx) {
                            st.selected_indices = vec![idx];
                        }
                        // Multi-select drag
                        if st.selected_indices.len() > 1 && st.selected_indices.contains(&idx) {
                            st.multi_dragging = true;
                            st.multi_drag_last = (x, y);
                        } else {
                            st.drag_index = Some(idx);
                            let r = &st.elements[idx].display;
                            st.drag_offset = (x - (st.overlay_x + r.x), y - (st.overlay_y + r.y));
                        }
                    }
                    None => {
                        if !ctrl {
                            st.selected_indices.clear();
                        }
                        st.box_selecting = true;
                        st.box_select_start = Some((x, y));
                        st.box_select_current = Some((x, y));
                    }
                }
            }
        }
        (None, None)
    });

    // Handle outside the lock (re-locks internally)
    if let Some(act) = action {
        return handle_toolbar_action(act);
    }
    if let Some(tk) = toggle {
        return handle_toggle_click(tk);
    }
    false
}

// ===== Confirm popup click =====

fn handle_confirm_click(x: i32, y: i32) -> bool {
    with_state(|st| {
        let (cx, cy, _cw, _ch) = confirm_box(st.screen_w, st.screen_h);
        let yes = (cx + 60, cy + 100, 150, 40);
        let no = (cx + 250, cy + 100, 150, 40);
        if x >= yes.0 && x <= yes.0 + yes.2 && y >= yes.1 && y <= yes.1 + yes.3 {
            st.saved = true;
            st.confirm_overwrite = false;
            true
        } else if x >= no.0 && x <= no.0 + no.2 && y >= no.1 && y <= no.1 + no.3 {
            st.confirm_overwrite = false;
            false
        } else {
            false
        }
    })
}

// ===== Toggle row click =====

fn handle_toggle_click(tk: ToggleKind) -> bool {
    with_state(|st| {
        match tk {
            ToggleKind::Magnifier => st.magnifier_on = !st.magnifier_on,
            ToggleKind::Grid => st.grid_on = !st.grid_on,
            ToggleKind::Crosshair => st.crosshair_on = !st.crosshair_on,
            ToggleKind::XyLabel => st.xy_label_on = !st.xy_label_on,
            ToggleKind::Snap => {
                st.snap_on = !st.snap_on;
                // Mutually exclusive: turning on XY snap disables tian snap
                if st.snap_on {
                    st.snap_tian = false;
                }
                st.snapped_x = None;
                st.snapped_y = None;
            }
            ToggleKind::TianSnap => {
                st.snap_tian = !st.snap_tian;
                // Mutually exclusive: turning on tian snap disables XY snap so the two don't interfere
                if st.snap_tian {
                    st.snap_on = false;
                    st.snapped_x = None;
                    st.snapped_y = None;
                }
            }
            ToggleKind::SnapDistance => {
                st.editing_target = EditingTarget::SnapDistance;
                st.editing_text = st.snap_distance.to_string();
                st.filename_focused = false;
            }
            ToggleKind::SnapGap => {
                st.editing_target = EditingTarget::SnapGap;
                st.editing_text = st.snap_gap.to_string();
                st.filename_focused = false;
            }
            ToggleKind::NudgeStep => {
                st.editing_target = EditingTarget::NudgeStep;
                st.editing_text = st.nudge_step.to_string();
                st.filename_focused = false;
            }
        }
        false
    })
}

// ===== Toolbar buttons =====

/// Remove all selected elements (sorted descending so earlier removals don't shift indices)
fn delete_selected(st: &mut super::state::EditorState) {
    let mut idxs = st.selected_indices.clone();
    idxs.sort_unstable();
    idxs.dedup();
    for &i in idxs.iter().rev() {
        if i < st.elements.len() {
            st.elements.remove(i);
        }
    }
    st.selected_indices.clear();
    // Deleting mid-drag/resize (mouse still captured): the interaction indices now dangle
    // and the next WM_MOUSEMOVE would index out of bounds. Cancel the interaction, like do_undo.
    st.drag_index = None;
    st.resize_index = None;
    st.resize_start = None;
    st.multi_dragging = false;
    st.undo_pending = None;
}

fn handle_toolbar_action(action: BtnAction) -> bool {
    with_state(|st| {
        st.editing_target = EditingTarget::None;
        st.editing_text.clear();
        match action {
            BtnAction::UndoLast => {
                // Generic undo of the last operation (drag/add/delete/spin/nudge/field commit)
                do_undo(st);
                false
            }
            BtnAction::NextStep => {
                if st.phase == EditorPhase::Selecting && !st.elements.is_empty() {
                    cancel_interactions(st);
                    st.phase = EditorPhase::Arranging;
                    st.selected_indices.clear();
                }
                false
            }
            BtnAction::BackToSelect => {
                if st.phase == EditorPhase::Arranging {
                    cancel_interactions(st);
                    st.phase = EditorPhase::Selecting;
                    st.selected_indices.clear();
                }
                false
            }
            BtnAction::Cancel => {
                st.saved = false;
                true
            }
            BtnAction::DeleteSelected => {
                // No-op when nothing is selected (the old version wrongly deleted the last list element)
                if st.phase == EditorPhase::Arranging && !st.selected_indices.is_empty() {
                    push_undo(st);
                    delete_selected(st);
                }
                false
            }
            BtnAction::Save => request_save_inner(st),
            BtnAction::Discard => {
                st.saved = false;
                true
            }
        }
    })
}

// ===== Mouse move =====

pub fn on_mouse_move(x: i32, y: i32) -> bool {
    with_state(|st| {
        if st.confirm_overwrite {
            return false;
        }
        if st.drag_panel {
            st.panel_x = (x - st.panel_drag_offset.0).clamp(4, st.screen_w - PANEL_W - 4);
            st.panel_y = (y - st.panel_drag_offset.1).clamp(4, st.screen_h - TITLE_H - 60);
            return true;
        }
        // Scrollbar drag
        if let Some((_down_y, thumb_off)) = st.scroll_drag {
            let (_lsx, lsy, _lsw, lsh) = list_area_rect(st);
            let n = st.elements.len() as i32;
            let vis = (lsh / LIST_ITEM_H).max(1);
            let max_scroll = (n - vis).max(0);
            if max_scroll > 0 {
                let thumb_h = (lsh * vis / n).max(24);
                let track_h = lsh - thumb_h;
                let frac = (y - lsy - thumb_off) as f32 / track_h as f32;
                st.list_scroll = ((frac * max_scroll as f32).round() as i32).clamp(0, max_scroll);
            }
            return true;
        }
        match st.phase {
            EditorPhase::Selecting => {
                if st.is_dragging {
                    let (snap_x, snap_y) = if st.snap_on && !st.snap_tian {
                        snap_selection(st, x, y)
                    } else {
                        (x, y)
                    };
                    st.drag_current = Some((snap_x, snap_y));
                    true
                } else {
                    false
                }
            }
            EditorPhase::Arranging => {
                // Multi-select drag
                if st.multi_dragging {
                    let dx = x - st.multi_drag_last.0;
                    let dy = y - st.multi_drag_last.1;
                    st.multi_drag_last = (x, y);
                    for &idx in &st.selected_indices.clone() {
                        st.elements[idx].display.x = (st.elements[idx].display.x + dx).max(0);
                        st.elements[idx].display.y = (st.elements[idx].display.y + dy).max(0);
                    }
                    return true;
                }
                // Box select
                if st.box_selecting {
                    st.box_select_current = Some((x, y));
                    return true;
                }
                if let Some(idx) = st.drag_index {
                    let new_x = (x - st.overlay_x - st.drag_offset.0).max(0);
                    let new_y = (y - st.overlay_y - st.drag_offset.1).max(0);
                    let (snap_x, snap_y) = if st.snap_on || st.snap_tian {
                        snap_arrangement(st, idx, new_x, new_y)
                    } else {
                        (new_x, new_y)
                    };
                    st.elements[idx].display.x = snap_x;
                    st.elements[idx].display.y = snap_y;
                    true
                } else if let Some(idx) = st.resize_index {
                    // Text element: edit-handle drag = font scaling (width/height unchanged)
                    if st.elements.get(idx).map(|e| e.extra.kind == ElemKind::Text).unwrap_or(false) {
                        if let (Some((sx, sy)), start_font) = (st.resize_start, st.resize_start_font) {
                            let r = &st.elements[idx].display;
                            let rx = st.overlay_x + r.x;
                            let ry = st.overlay_y + r.y;
                            // Top-left point as the anchor; drag distance → font size
                            let start_dist = ((sx - rx).max(1) as f32).hypot((sy - ry) as f32).max(1.0);
                            let cur_dist = ((x - rx).max(1) as f32).hypot((y - ry) as f32).max(1.0);
                            let new_font = (start_font as f32 * cur_dist / start_dist).round() as u32;
                            if let Some(e) = st.elements.get_mut(idx) {
                                e.extra.font_size = new_font.clamp(6, 300);
                            }
                        }
                        return true;
                    }
                    let new_w;
                    let new_h;
                    {
                        let r = &st.elements[idx].display;
                        new_w = (x - st.overlay_x - r.x).max(20);
                        new_h = (y - st.overlay_y - r.y).max(20);
                    }
                    let snap_on = st.snap_on;
                    let snap_dist = st.snap_distance;
                    // When tian snap is on: no edge snapping
                    let (snap_w, snap_h) = if snap_on && !st.snap_tian {
                        snap_resize(&st.elements, idx, new_w, new_h, snap_dist)
                    } else {
                        (new_w, new_h)
                    };
                    st.elements[idx].display.width = snap_w;
                    st.elements[idx].display.height = snap_h;
                    true
                } else {
                    false
                }
            }
        }
    })
}

// ===== Mouse release =====

pub fn on_lbutton_up(x: i32, y: i32) {
    with_state(|st| {
        // Commit the drag/resize snapshot taken at mouse-down as ONE undo step
        // (dedup makes a plain click without movement a no-op)
        if let Some(pending) = st.undo_pending.take() {
            push_snapshot(st, pending);
        }
        st.snapped_x = None;
        st.snapped_y = None;
        // Panel dragging: stops on release (common to both phases)
        st.drag_panel = false;
        st.scroll_drag = None;
        match st.phase {
            EditorPhase::Selecting => {
                if st.is_dragging {
                    st.is_dragging = false;
                    let (cx, cy) = st.drag_current.unwrap_or((x, y));
                    if let Some((sx, sy)) = st.drag_start {
                        let rw = (cx - sx).abs();
                        let rh = (cy - sy).abs();
                        if rw > 10 && rh > 10 {
                            // Clamp to >=0: with SetCapture the mouse can drag past the primary
                            // monitor's left/top edge onto a negative-coordinate monitor, and a
                            // negative value here would wrap through `as u32` into a huge source coord
                            let rx = sx.min(cx).max(0);
                            let ry = sy.min(cy).max(0);
                            // Element stays where it was selected (screen coords → overlay-relative coords),
                            // so the arranging phase shows the real captured position instead of stacking them.
                            let dx = rx - st.overlay_x;
                            let dy = ry - st.overlay_y;
                            push_undo(st); // one undo step per added capture element
                            let mut extra = ElemExtra::new_capture();
                            extra.opacity = st.global_opacity; // display inherits global; still not written to the entry on save
                            st.elements.push(Element::new_capture(
                                Rect {
                                    x: rx as u32,
                                    y: ry as u32,
                                    width: rw as u32,
                                    height: rh as u32,
                                },
                                DisplayRect {
                                    x: dx,
                                    y: dy,
                                    width: rw,
                                    height: rh,
                                    z_order: 0,
                                    opacity: None,
                                    rotate: 0,
                                },
                                extra,
                            ));
                        }
                    }
                    st.drag_start = None;
                    st.drag_current = None;
                    st.snapped_x = None;
                    st.snapped_y = None;
                }
            }
            EditorPhase::Arranging => {
                st.multi_dragging = false;
                if st.box_selecting {
                    st.box_selecting = false;
                    if let (Some((sx, sy)), Some((ex, ey))) = (st.box_select_start, st.box_select_current) {
                        let bx0 = sx.min(ex);
                        let by0 = sy.min(ey);
                        let bx1 = sx.max(ex);
                        let by1 = sy.max(ey);
                        if bx1 - bx0 > 5 && by1 - by0 > 5 {
                            let mut hits = Vec::new();
                            for (i, e) in st.elements.iter().enumerate() {
                                let r = &e.display;
                                let rx0 = st.overlay_x + r.x;
                                let ry0 = st.overlay_y + r.y;
                                let rx1 = rx0 + r.width;
                                let ry1 = ry0 + r.height;
                                if rx0 < bx1 && rx1 > bx0 && ry0 < by1 && ry1 > by0 {
                                    hits.push(i);
                                }
                            }
                            st.selected_indices = hits;
                        }
                    }
                    st.box_select_start = None;
                    st.box_select_current = None;
                }
                st.drag_index = None;
                st.resize_index = None;
                st.resize_start = None;
                st.resize_start_font = 0;
                st.drag_panel = false;
                st.snapped_x = None;
                st.snapped_y = None;
            }
        }
    });
}

// ===== Right click =====

pub fn on_rbutton() -> bool {
    with_state(|st| {
        st.editing_target = EditingTarget::None;
        st.editing_text.clear();
        match st.phase {
            EditorPhase::Selecting => {
                if !st.elements.is_empty() {
                    push_undo(st);
                    st.elements.pop();
                    // The popped element may be selected: indices would then exceed len
                    st.selected_indices.clear();
                }
                false
            }
            EditorPhase::Arranging => {
                do_undo(st);
                false
            }
        }
    })
}

// ===== Keyboard =====

pub fn on_keydown(vk: u32) -> bool {
    with_state(|st| {
        if st.confirm_overwrite {
            match vk {
                k if k == VK_ESCAPE.0 as u32 => {
                    st.confirm_overwrite = false;
                    return false;
                }
                k if k == VK_RETURN.0 as u32 => {
                    st.saved = true;
                    st.confirm_overwrite = false;
                    return true;
                }
                _ => return false,
            }
        }

        // Editing property fields (X/Y/W/H or element props)
        if !matches!(st.editing_target, EditingTarget::None) {
            match vk {
                k if k == VK_BACK.0 as u32 => {
                    st.editing_text.pop();
                    sync_live_edit(st);
                    return false;
                }
                k if k == VK_ESCAPE.0 as u32 => {
                    st.editing_target = EditingTarget::None;
                    st.editing_text.clear();
                    return false;
                }
                k if k == VK_RETURN.0 as u32 => {
                    commit_editing(st);
                    return false;
                }
                // Typing chars (digits/letters etc.): keep the editor open
                _ => return false,
            }
        }

        // Filename input focused
        if st.filename_focused {
            match vk {
                k if k == VK_BACK.0 as u32 => {
                    st.save_filename.pop();
                    return false;
                }
                k if k == VK_ESCAPE.0 as u32 => {
                    st.filename_focused = false;
                    return false;
                }
                k if k == VK_RETURN.0 as u32 => {
                    st.filename_focused = false;
                    return false;
                }
                _ => return false,
            }
        }

        // Ctrl+Z: undo the last operation (not while editing a text field — handled above)
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 };
        if ctrl && vk == VK_Z.0 as u32 {
            do_undo(st);
            return false;
        }

        match vk {
            // Arrow keys nudge: move selected element(s) by nudge_step pixels (single/multi select)
            k if k == VK_LEFT.0 as u32
                || k == VK_RIGHT.0 as u32
                || k == VK_UP.0 as u32
                || k == VK_DOWN.0 as u32 =>
            {
                let step = st.nudge_step.max(1);
                let (dx, dy) = match vk {
                    k if k == VK_LEFT.0 as u32 => (-step, 0),
                    k if k == VK_RIGHT.0 as u32 => (step, 0),
                    k if k == VK_UP.0 as u32 => (0, -step),
                    _ => (0, step),
                };
                if st.selected_indices.is_empty() {
                    return false;
                }
                // Only push an undo layer when something can actually move: stale indices
                // (deleted elements) or source-less UI elements selected in Selecting would
                // otherwise create an empty "undo that changes nothing" step
                let has_valid_target = st.selected_indices.iter().any(|&i| match st.phase {
                    EditorPhase::Selecting => {
                        st.elements.get(i).map(|e| e.source.is_some()).unwrap_or(false)
                    }
                    EditorPhase::Arranging => i < st.elements.len(),
                });
                if !has_valid_target {
                    return false;
                }
                push_undo(st); // one undo step per arrow-key nudge
                // Selecting phase: move the source rect; arranging phase: move the display rect
                match st.phase {
                    EditorPhase::Selecting => {
                        for &i in st.selected_indices.clone().iter() {
                            if let Some(e) = st.elements.get_mut(i) {
                                if let Some(s) = &mut e.source {
                                    s.x = ((s.x as i32) + dx).max(0) as u32;
                                    s.y = ((s.y as i32) + dy).max(0) as u32;
                                }
                            }
                        }
                    }
                    EditorPhase::Arranging => {
                        for &i in st.selected_indices.clone().iter() {
                            if let Some(e) = st.elements.get_mut(i) {
                                e.display.x = (e.display.x + dx).max(0);
                                e.display.y = (e.display.y + dy).max(0);
                            }
                        }
                    }
                }
                false
            }
            k if k == VK_ESCAPE.0 as u32 => {
                st.saved = false;
                true
            }
            k if k == VK_RETURN.0 as u32 => match st.phase {
                EditorPhase::Selecting => {
                    if st.elements.is_empty() {
                        st.saved = false;
                        true
                    } else {
                        cancel_interactions(st);
                        st.phase = EditorPhase::Arranging;
                        st.selected_indices.clear();
                        false
                    }
                }
                EditorPhase::Arranging => request_save_inner(st),
            },
            k if k == VK_DELETE.0 as u32 => {
                if st.phase == EditorPhase::Arranging && !st.selected_indices.is_empty() {
                    push_undo(st);
                    delete_selected(st);
                }
                false
            }
            _ => false,
        }
    })
}

// ===== Character input =====

pub fn on_char(ch: char) -> bool {
    with_state(|st| {
        // Editing an element property field
        if let EditingTarget::ElemField(_, prop) = &st.editing_target {
            // Numeric fields accept only digits/dot/minus
            if prop.is_numeric() {
                if ch.is_ascii_digit() || ch == '.' || ch == '-' {
                    st.editing_text.push(ch);
                    sync_live_edit(st);
                    return true;
                }
                return false;
            }
            // Text fields accept all graphic chars (incl. CJK/letters/symbols/#) and spaces
            if ch.is_ascii_graphic() || ch == ' ' || !ch.is_ascii() {
                st.editing_text.push(ch);
                sync_live_edit(st);
                return true;
            }
            return false;
        }

        // X/Y/W/H fields accept only digits and minus
        if matches!(st.editing_target, EditingTarget::RegionField(..)) {
            if ch.is_ascii_digit() || ch == '-' {
                st.editing_text.push(ch);
                return true;
            }
            return false;
        }

        // Snap distance / snap gap / nudge step fields accept only digits and minus
        if matches!(
            st.editing_target,
            EditingTarget::SnapDistance | EditingTarget::SnapGap | EditingTarget::NudgeStep
        ) {
            if ch.is_ascii_digit() || ch == '-' {
                st.editing_text.push(ch);
                return true;
            }
            return false;
        }

        if st.filename_focused && !st.confirm_overwrite && (ch.is_ascii_graphic() || !ch.is_ascii()) {
            st.save_filename.push(ch);
            true
        } else {
            false
        }
    })
}

// ===== Mouse wheel =====
// delta: signed scroll increment (120 = one notch). When a value box is hit, adjust by the default step.

pub fn on_mouse_wheel(x: i32, y: i32, delta: i32) -> bool {
    if delta == 0 {
        return false;
    }
    with_state(|st| {
        // Modal: block wheel adjustments while the overwrite-confirm popup is open
        // (consistent with on_mouse_move / on_keydown)
        if st.confirm_overwrite {
            return false;
        }
        // Panel value row (snap distance / gap / nudge)
        if let Some(tk) = panel_toggle_wheel_hit(st, x, y) {
            let target = match tk {
                ToggleKind::SnapDistance => SpinTarget::SnapDistance,
                ToggleKind::SnapGap => SpinTarget::SnapGap,
                ToggleKind::NudgeStep => SpinTarget::NudgeStep,
                _ => return false,
            };
            let steps = delta / 120;
            if steps == 0 {
                return false;
            }
            apply_spin(st, target, steps);
            return true;
        }
        // List XYWH fields / element numeric prop fields (take priority over list scrolling)
        if let Some(target) = spin_target_at(st, x, y) {
            let steps = delta / 120;
            if steps == 0 {
                return false;
            }
            apply_spin(st, target, steps);
            return true;
        }
        // List scrolling (only inside the list rect, on empty space/index area/scrollbar not hit by fields)
        {
            let (lx, ly, lw, lh) = list_area_rect(st);
            if x >= lx && x <= lx + lw && y >= ly && y <= ly + lh {
                st.wheel_acc = st.wheel_acc.saturating_add(delta);
                if st.wheel_acc.abs() >= 120 {
                    let n = st.wheel_acc / 120;
                    st.wheel_acc -= n * 120;
                    st.list_scroll = (st.list_scroll - n).clamp(0, list_max_scroll(st));
                }
                return true;
            }
        }
        false
    })
}
