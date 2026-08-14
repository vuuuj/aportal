//! Visual config editor (Layered Window edition) - v4
//!
//! Module layout:
//! - common:    constants, types, GDI drawing helpers
//! - state:     EditorState + global state
//! - snap:      smart snapping (selection/arranging/resize)
//! - visuals:   magnifier, grid, crosshair, XY values
//! - ui:        toolbar, toggle rows, hint bar, confirm popup
//! - panel:     right panel (drawing + field editing)
//! - selecting: selecting-phase drawing
//! - arranging: arranging-phase drawing + canvas hit testing
//! - mod.rs:    entry, rendering, window proc, event dispatch

mod common;
mod state;
mod snap;
mod visuals;
mod ui;
mod panel;
mod selecting;
mod arranging;
mod events;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, BLENDFUNCTION, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject,
    GetDC, ReleaseDC, ScreenToClient, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HDC, HBITMAP, HGDIOBJ,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetSystemMetrics, KillTimer, LoadCursorW, PeekMessageW, RegisterClassExW, SetCursor,
    SetForegroundWindow, SetTimer, ShowWindow, TranslateMessage, UpdateLayeredWindow,
    WaitMessage,
    WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSEXW, WM_CHAR, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, CS_HREDRAW, CS_VREDRAW,
    IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_IBEAM, IDC_SIZEALL, MSG, PM_REMOVE, SM_CXSCREEN,
    SM_CYSCREEN, SW_SHOW, ULW_ALPHA, WS_EX_LAYERED, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use crate::config::{CaptureRegion, Config, DisplayRect, WindowConfig};
use crate::config::GlobalSettings;
use crate::error::{AppError, AppResult};

use common::*;
use state::{set_state, with_state, with_state_ref, ElemExtra, ElemKind, Element, EditorState};
use ui::{
    config_file_exists, draw_confirm_modal, draw_hint_bar, panel_action_hit,
    panel_toggle_box_hit, panel_toggle_button_hit, panel_toggle_spin_hit, resolve_filename,
};
use snap::draw_snap_lines;
use visuals::{draw_crosshair, draw_grid, draw_magnifier, draw_xy_label};
use panel::panel_rect;
use selecting::draw_selecting;
use arranging::draw_arranging;
use events::{on_char, on_keydown, on_lbutton_down, on_lbutton_up, on_mouse_move, on_mouse_wheel, on_rbutton};

// ===== Entry =====

pub fn run_editor(
    current_config: &Config,
    initial_filename: &str,
) -> AppResult<Option<(Config, String)>> {
    unsafe {
        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);

        // Load existing config: capture regions + custom UI elements → unified element list
        let mut elements: Vec<Element> = Vec::new();
        // Capture regions
        for region in &current_config.capture_regions {
            let mut extra = ElemExtra::new_capture();
            extra.z_order = region.display.z_order;
            extra.opacity = region.display.opacity.unwrap_or(current_config.effective_global_opacity());
            extra.opacity_explicit = region.display.opacity.is_some();
            // In-editor working size: display 0 (1:1) falls back to the source size so the canvas,
            // hit-testing and snap all see the real footprint
            let mut display = region.display.clone();
            if display.width <= 0 || display.height <= 0 {
                display.width = region.source.width as i32;
                display.height = region.source.height as i32;
            }
            elements.push(Element::new_capture(
                region.source.clone(),
                display,
                extra,
            ));
        }
        // Custom UI elements (default width/height from PNG intrinsic size/text measurement, so tian-center snap etc. use real sizes)
        for ui in &current_config.custom_ui {
            let (x, y, w, h) = current_config.element_footprint(ui);
            elements.push(Element::new_ui(
                DisplayRect { x, y, width: w, height: h, z_order: 0, opacity: ui.opacity(), rotate: ui.rotate() },
                ui_elem_to_extra(ui, current_config),
            ));
        }

        // Load editor prefs (snap distance/gap/nudge step, remembered from settings.yml)
        let gs = GlobalSettings::load();
        let pref_snap_distance = gs.snap_distance;
        let pref_snap_gap = gs.snap_gap;
        let pref_nudge_step = gs.nudge_step;

        let (ov_x, ov_y, ov_w, ov_h) = current_config.overlay_bounds();
        let st = EditorState {
            phase: if elements.is_empty() {
                EditorPhase::Selecting
            } else {
                EditorPhase::Arranging
            },
            hwnd: 0,
            screen_w: sw,
            screen_h: sh,
            drag_start: None,
            drag_current: None,
            is_dragging: false,
            elements,
            overlay_x: ov_x,
            overlay_y: ov_y,
            overlay_w: ov_w,
            overlay_h: ov_h,
            drag_index: None,
            drag_offset: (0, 0),
            resize_index: None,
            resize_start: None,
            resize_start_font: 0,
            selected_indices: Vec::new(),
            box_select_start: None,
            box_select_current: None,
            box_selecting: false,
            multi_dragging: false,
            multi_drag_last: (0, 0),
            filename_focused: false,
            save_filename: if initial_filename.is_empty() {
                String::new()
            } else {
                initial_filename.to_string()
            },
            confirm_overwrite: false,
            panel_x: sw - PANEL_W - 12,
            panel_y: 12,
            drag_panel: false,
            panel_drag_offset: (0, 0),
            over_title: false,
            tooltip_show: false,
            hint_top: false,
            close_requested: false,
            saved: false,
            magnifier_on: true,
            grid_on: true,
            crosshair_on: true,
            xy_label_on: true,
            snap_on: true,
            snap_tian: false,
            snap_distance: pref_snap_distance,
            snap_gap: pref_snap_gap,
            nudge_step: pref_nudge_step,
            list_scroll: 0,
            wheel_acc: 0,
            scroll_drag: None,
            global_opacity: current_config.effective_global_opacity(),
            mouse_x: 0,
            mouse_y: 0,
            snapped_x: None,
            snapped_y: None,
            editing_target: EditingTarget::None,
            editing_text: String::new(),
            png_cache: std::collections::HashMap::new(),
            undo_stack: Vec::new(),
            undo_pending: None,
            exe_dir: std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_string_lossy().to_string()))
                .unwrap_or_default(),
        };
        set_state(st);

        // Register the window class
        let class_name = w!("APortalEditor");
        let title_utf16: Vec<u16> = crate::i18n::t("editor_title")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let title_name = windows::core::PCWSTR::from_raw(title_utf16.as_ptr());
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(editor_wnd_proc),
            hCursor: LoadCursorW(None, IDC_CROSS).map_err(|e| AppError::windows("LoadCursor", e))?,
            lpszClassName: class_name,
            style: CS_HREDRAW | CS_VREDRAW,
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_LAYERED.0 | WS_EX_TOPMOST.0),
            class_name,
            title_name,
            WINDOW_STYLE(WS_POPUP.0 | WS_VISIBLE.0),
            0,
            0,
            sw,
            sh,
            None,
            None,
            None,
            None,
        )
        .map_err(|e| AppError::windows("CreateWindowExW editor", e))?;

        with_state(|s| s.hwnd = hwnd.0 as usize);

        let _ = SetForegroundWindow(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);

        render();

        let mut msg = MSG::default();
        loop {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).into() {
                let _ = TranslateMessage(&msg);
                if DispatchMessageW(&msg) == LRESULT(-1) {
                    // DispatchMessageW returns -1 with GetLastError=0x0118 for WM_SYSTIMER
                    // (0x0118, internal system timer message); that is normal and can be ignored.
                    // Other errors are logged and processing continues without breaking the message loop.
                    let e = std::io::Error::last_os_error();
                    if e.raw_os_error() != Some(0x0118) {
                        log::error!("DispatchMessageW failed: {}", e);
                    }
                }
            }
            if with_state(|s| s.close_requested) {
                break;
            }
            // Suspend when idle (WM_TIMER/mouse/keyboard messages wake it up), avoiding busy polling
            let _ = WaitMessage();
        }

        let hwnd_raw = with_state(|s| s.hwnd);
        let _ = DestroyWindow(HWND(hwnd_raw as *mut std::ffi::c_void));

        let result = with_state_ref(|st| {
            if st.saved && !st.elements.is_empty() {
                let mut new_cfg = current_config.clone();
                // Record the current overlay anchor; size is still auto-determined by content
                new_cfg.settings.window = Some(WindowConfig {
                    x: st.overlay_x,
                    y: st.overlay_y,
                    width: st.overlay_w,
                    height: st.overlay_h,
                });
                new_cfg.capture_regions.clear();
                new_cfg.custom_ui.clear();
                let mut capture_count = 0usize;
                for e in &st.elements {
                    let extra = &e.extra;
                    let dr = &e.display;
                    match extra.kind {
                        ElemKind::Capture => {
                            capture_count += 1;
                            let mut display = dr.clone();
                            // User never explicitly set opacity → don't write the entry (inherits global_opacity)
                            display.opacity = if extra.opacity_explicit { Some(extra.opacity) } else { None };
                            // Render order lives on display.z; the legacy top-level z is no longer written (still read on load)
                            display.z_order = extra.z_order;
                            let src = e.source_rect();
                            // 1:1 display (editor size equals the source) → write 0 so the entry stays
                            // compact and follows the source size at runtime
                            if dr.width == src.width as i32 && dr.height == src.height as i32 {
                                display.width = 0;
                                display.height = 0;
                            }
                            new_cfg.capture_regions.push(CaptureRegion {
                                id: format!("region_{}", capture_count),
                                source: src,
                                display,
                                z_order: None,
                            });
                        }
                        _ => {
                            new_cfg.custom_ui.push(extra_to_ui_elem(extra, dr));
                        }
                    }
                }
                let filename = resolve_filename(&st.save_filename);
                log::info!("Editor: saved config {} ({} elements)", filename, st.elements.len());
                Some((new_cfg, filename))
            } else {
                None
            }
        });
        Ok(result)
    }
}

/// config's CustomUiElement → editor ElemExtra
fn ui_elem_to_extra(ui: &crate::config::CustomUiElement, cfg: &Config) -> ElemExtra {
    use crate::config::CustomUiElement as U;
    let _ = cfg;
    let read_opacity = |raw: Option<f32>, extra: &mut ElemExtra| {
        extra.opacity = raw.unwrap_or(cfg.effective_global_opacity());
        extra.opacity_explicit = raw.is_some();
    };
    match ui {
        U::Frame(e) => {
            let mut x = ElemExtra::new_ui(ElemKind::Frame);
            x.border_color = e.border_color.clone();
            x.border_width = e.border_width;
            x.corner_radius = e.corner_radius;
            read_opacity(e.opacity, &mut x);
            x.z_order = e.z_order;
            x
        }
        U::Background(e) => {
            let mut x = ElemExtra::new_ui(ElemKind::Background);
            x.color = e.color.clone();
            x.corner_radius = e.corner_radius;
            read_opacity(e.opacity, &mut x);
            x.z_order = e.z_order;
            x
        }
        U::Image(e) => {
            let mut x = ElemExtra::new_ui(ElemKind::Png);
            x.png_path = e.path.clone();
            read_opacity(e.opacity, &mut x);
            x.z_order = e.z_order;
            x
        }
        U::Text(e) => {
            let mut x = ElemExtra::new_ui(ElemKind::Text);
            x.content = e.content.clone();
            x.font_size = e.font_size;
            x.text_color = e.text_color.clone();
            read_opacity(e.opacity, &mut x);
            x.z_order = e.z_order;
            x
        }
    }
}

/// Editor ElemExtra + geometry → config's CustomUiElement
fn extra_to_ui_elem(extra: &ElemExtra, dr: &DisplayRect) -> crate::config::CustomUiElement {
    use crate::config::{BackgroundElement, CustomUiElement, FrameElement, ImageElement, TextElement};
    // User never explicitly set opacity → don't write the entry (inherits global_opacity)
    let op = if extra.opacity_explicit { Some(extra.opacity) } else { None };
    match extra.kind {
        ElemKind::Frame => CustomUiElement::Frame(FrameElement {
            x: dr.x, y: dr.y, width: dr.width, height: dr.height,
            opacity: op, z_order: extra.z_order,
            border_color: extra.border_color.clone(),
            border_width: extra.border_width,
            corner_radius: extra.corner_radius,
            rotate: dr.rotate,
        }),
        ElemKind::Background => CustomUiElement::Background(BackgroundElement {
            x: dr.x, y: dr.y, width: dr.width, height: dr.height,
            opacity: op, z_order: extra.z_order,
            color: extra.color.clone(),
            corner_radius: extra.corner_radius,
            rotate: dr.rotate,
        }),
        ElemKind::Png => CustomUiElement::Image(ImageElement {
            path: extra.png_path.clone(),
            x: dr.x, y: dr.y, width: dr.width, height: dr.height,
            opacity: op, z_order: extra.z_order,
            rotate: dr.rotate,
        }),
        ElemKind::Text => CustomUiElement::Text(TextElement {
            content: extra.content.clone(),
            // size is always auto-derived from content + font size (no width/height entries)
            x: dr.x,
            y: dr.y,
            font_size: extra.font_size,
            text_color: extra.text_color.clone(),
            opacity: op,
            z_order: extra.z_order,
            rotate: dr.rotate,
        }),
        ElemKind::Capture => unreachable!("Capture does not belong to custom_ui"),
    }
}

pub(crate) fn request_save_inner(st: &mut EditorState) -> bool {
    let f = resolve_filename(&st.save_filename);
    if config_file_exists(&f) {
        st.confirm_overwrite = true;
        false
    } else {
        st.saved = true;
        true
    }
}

// ===== Rendering =====

/// Reusable off-screen drawing surface. The editor renders every frame (mouse move,
/// hover timer, ...); creating/tearing down a DIB section + DC per frame was wasteful.
/// Keyed by screen size: rebuilt only when the resolution changes.
struct DibSurface {
    sw: i32,
    sh: i32,
    hdc_screen: HDC,
    mem_dc: HDC,
    bmp: HBITMAP,
    old_bmp: HGDIOBJ,
    bits: *mut u8,
}

impl Drop for DibSurface {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.mem_dc, self.old_bmp);
            let _ = DeleteObject(self.bmp);
            let _ = DeleteDC(self.mem_dc);
            let _ = ReleaseDC(None, self.hdc_screen);
        }
    }
}

thread_local! {
    static DIB_SURFACE: std::cell::RefCell<Option<DibSurface>> = const { std::cell::RefCell::new(None) };
}

unsafe fn get_dib_surface(sw: i32, sh: i32) -> Option<DibSurface> {
    let cache = DIB_SURFACE.with(|c| c.borrow_mut().take());
    if cache.as_ref().is_some_and(|s| s.sw == sw && s.sh == sh) {
        return cache; // Reuse: same resolution
    }
    // (Re)build: drop the stale surface and create a fresh one
    drop(cache);
    let hdc_screen = GetDC(None);
    if hdc_screen.is_invalid() {
        return None;
    }
    let mem_dc = CreateCompatibleDC(hdc_screen);
    if mem_dc.is_invalid() {
        let _ = ReleaseDC(None, hdc_screen);
        return None;
    }
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: sw,
            biHeight: -sh,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let bmp = match CreateDIBSection(mem_dc, &bi, DIB_RGB_COLORS, &mut bits, None, 0) {
        Ok(b) => b,
        Err(_) => {
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(None, hdc_screen);
            return None;
        }
    };
    let old_bmp = SelectObject(mem_dc, bmp);
    Some(DibSurface {
        sw,
        sh,
        hdc_screen,
        mem_dc,
        bmp,
        old_bmp,
        bits: bits as *mut u8,
    })
}

fn render() {
    unsafe {
        let (hwnd_raw, sw, sh) = with_state(|st| (st.hwnd, st.screen_w, st.screen_h));
        let hwnd = HWND(hwnd_raw as *mut std::ffi::c_void);

        let Some(dib) = get_dib_surface(sw, sh) else { return; };
        let hdc_screen = dib.hdc_screen;
        let mem_dc = dib.mem_dc;
        let bits = dib.bits;
        DIB_SURFACE.with(|c| *c.borrow_mut() = Some(dib));

        let buf = std::slice::from_raw_parts_mut(bits, (sw * sh * 4) as usize);

        with_state(|st| {
            // 1. Background
            for i in 0..(sw * sh) as usize {
                let si = i * 4;
                buf[si] = 0;
                buf[si + 1] = 0;
                buf[si + 2] = 0;
                buf[si + 3] = BG_ALPHA;
            }

            // 2. Grid (drawn first, at the bottom)
            if st.grid_on {
                draw_grid(mem_dc, st);
            }

            // 3. Phase drawing
            match st.phase {
                EditorPhase::Selecting => draw_selecting(mem_dc, st),
                EditorPhase::Arranging => draw_arranging(mem_dc, st, buf, st.screen_w, st.screen_h),
            }

            // 4. Snap guide lines
            draw_snap_lines(mem_dc, st);

            // 5. Crosshair
            if st.crosshair_on {
                draw_crosshair(mem_dc, st);
            }

            // 6. Magnifier
            if st.magnifier_on {
                draw_magnifier(mem_dc, hdc_screen, st);
            }

            // 7. XY values
            if st.xy_label_on {
                draw_xy_label(mem_dc, st);
            }

            // 8. UI (panel controls drawn inside draw_region_panel)
            draw_hint_bar(mem_dc, st);
            if st.confirm_overwrite {
                draw_confirm_modal(mem_dc, st);
            }

            // 9. Alpha correction
            for i in 0..(sw * sh) as usize {
                let si = i * 4;
                let b = buf[si];
                let g = buf[si + 1];
                let r = buf[si + 2];
                if b != 0 || g != 0 || r != 0 {
                    buf[si] = (b as u16 * UI_ALPHA as u16 / 255) as u8;
                    buf[si + 1] = (g as u16 * UI_ALPHA as u16 / 255) as u8;
                    buf[si + 2] = (r as u16 * UI_ALPHA as u16 / 255) as u8;
                    buf[si + 3] = UI_ALPHA;
                } else {
                    buf[si + 3] = BG_ALPHA;
                }
            }

            // 9.5 The confirm popup must be fully opaque (otherwise the underlying overlay shows through as ghosting)
            if st.confirm_overwrite {
                let (cx, cy, cw, ch) = ui::confirm_box(st.screen_w, st.screen_h);
                for py in cy..cy + ch {
                    for px in cx..cx + cw {
                        let si = ((py * st.screen_w + px) as usize) * 4;
                        buf[si + 3] = 255;
                    }
                }
            }
        });

        let pt_src = POINT { x: 0, y: 0 };
        let size = SIZE { cx: sw, cy: sh };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_ALPHA as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = UpdateLayeredWindow(
            hwnd,
            hdc_screen,
            None,
            Some(&size),
            mem_dc,
            Some(&pt_src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        // Surface is cached in DIB_SURFACE (reused next frame, released on thread exit via Drop)
    }
}

// ===== Window proc =====

unsafe extern "system" fn editor_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_SETCURSOR => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = ScreenToClient(hwnd, &mut pt);
            let kind = cursor_kind_for(pt.x, pt.y);
            let id = match kind {
                CursorKind::Cross => IDC_CROSS,
                CursorKind::SizeAll => IDC_SIZEALL,
                CursorKind::Hand => IDC_HAND,
                CursorKind::Ibeam => IDC_IBEAM,
                CursorKind::Arrow => IDC_ARROW,
            };
            if let Ok(hc) = LoadCursorW(None, id) {
                let _ = SetCursor(hc);
            }
            LRESULT(1)
        }
        WM_TIMER => {
            if wparam.0 == HOVER_TIMER_ID {
                let _ = KillTimer(hwnd, HOVER_TIMER_ID);
                let need = with_state(|st| {
                    if st.over_title {
                        st.tooltip_show = true;
                        true
                    } else {
                        false
                    }
                });
                if need {
                    render();
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            let _ = SetCapture(hwnd);
            let close = on_lbutton_down(x, y);
            render();
            if close {
                with_state(|st| st.close_requested = true);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;

            with_state(|st| {
                st.mouse_x = x;
                st.mouse_y = y;
            });

            // Hint bar dodges the mouse: approaching the edge it currently sits on
            // flips it to the opposite edge (bottom <-> top)
            let hint_flipped = with_state(|st| {
                let zone = 60;
                let near = if st.hint_top {
                    y < zone
                } else {
                    y > st.screen_h - zone
                };
                if near && !st.confirm_overwrite {
                    st.hint_top = !st.hint_top;
                    true
                } else {
                    false
                }
            });

            let now = is_over_title(x, y);
            let tooltip_hidden = with_state(|st| {
                let prev = st.over_title;
                st.over_title = now;
                if now && !prev {
                    let _ = SetTimer(hwnd, HOVER_TIMER_ID, 400, None);
                    false
                } else if !now && prev {
                    let _ = KillTimer(hwnd, HOVER_TIMER_ID);
                    let was = st.tooltip_show;
                    st.tooltip_show = false;
                    was
                } else {
                    false
                }
            });
            let mut need = tooltip_hidden;
            if on_mouse_move(x, y) {
                need = true;
            }
            // Visual helpers (crosshair/magnifier/XY label) follow the mouse and need continuous redraws
            let visual_needs = with_state_ref(|st| {
                st.crosshair_on || st.magnifier_on || st.xy_label_on
            });
            if need || visual_needs || hint_flipped {
                render();
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            let _ = ReleaseCapture();
            on_lbutton_up(x, y);
            render();
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // lparam is in screen coords; the window is fullscreen at the origin, so use directly
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            // High 16 bits hold the signed scroll increment (120 = one notch)
            let delta = (((wparam.0 >> 16) as u16) as i16) as i32;
            let changed = on_mouse_wheel(x, y, delta);
            if changed {
                render();
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let close = on_rbutton();
            render();
            if close {
                with_state(|st| st.close_requested = true);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let close = on_keydown(wparam.0 as u32);
            render();
            if close {
                with_state(|st| st.close_requested = true);
            }
            LRESULT(0)
        }
        WM_CHAR => {
            // wparam is a UTF-16 code unit (BMP chars incl. CJK fit in a single u16)
            // Surrogate pairs come as two WM_CHAR messages; only BMP is handled here
            let ch_u16 = wparam.0 as u16;
            if let Some(ch) = char::from_u32(ch_u16 as u32) {
                if on_char(ch) {
                    render();
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ===== Mouse cursor selection =====

fn is_over_title(x: i32, y: i32) -> bool {
    with_state(|st| {
        let (px, py, pw, _ph) = panel_rect(st);
        x >= px && x <= px + pw && y >= py && y <= py + TITLE_H
    })
}

fn cursor_kind_for(x: i32, y: i32) -> CursorKind {
    with_state(|st| {
        if st.confirm_overwrite {
            return CursorKind::Arrow;
        }
        let (px, py, pw, ph) = panel_rect(st);
        if x >= px && x <= px + pw && y >= py && y <= py + ph {
            // Title bar
            if y <= py + TITLE_H {
                return CursorKind::SizeAll;
            }
            // Action row / toggle row / value row
            if panel_action_hit(st, x, y).is_some()
                || panel_toggle_button_hit(st, x, y).is_some()
                || panel_toggle_box_hit(st, x, y).is_some()
                || panel_toggle_spin_hit(st, x, y).is_some()
            {
                return CursorKind::Hand;
            }
            // Filename box
            let fx = px + 10;
            let fy = py + TITLE_H + 22;
            let fw = pw - 20;
            let fh = FN_BOX_H;
            if x >= fx && x <= fx + fw && y >= fy && y <= fy + fh {
                return CursorKind::Ibeam;
            }
            // List / property area
            if y >= py + LIST_TOP2 {
                return CursorKind::Hand;
            }
            return CursorKind::Arrow;
        }
        CursorKind::Cross
    })
}


