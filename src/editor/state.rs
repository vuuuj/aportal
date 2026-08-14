//! Editor state: EditorState struct + unified element model + global state access
//!
//! Referenced by all editor submodules.
//!
//! Unified element model: capture regions and custom UI elements (frame/bg) live in one list,
//! sharing display geometry data, so drag/resize/multi-select/box-select logic is all reused.

use std::sync::Mutex;
use std::collections::HashMap;

// Re-exports: submodules reference these types via super::state::
pub use windows::Win32::Graphics::Gdi::HDC;
pub use super::common::{EditingTarget, EditorPhase, FieldKind};

use crate::config::{DisplayRect, Rect};

/// Unified element: capture regions and custom UI share one list; the index is the element number.
/// Previously three parallel Vecs (source/display/extra) were kept aligned by convention —
/// any missed push/pop/remove would silently misalign; now a single array, guaranteed complete by the compiler.
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    /// Source rect (only capture elements have one; UI elements are None, no zero placeholder rect stored)
    pub source: Option<Rect>,
    /// Display geometry (overlay-relative coords, shared by all elements)
    pub display: DisplayRect,
    /// Type-specific data
    pub extra: ElemExtra,
}

impl Element {
    /// Create a capture-region element
    pub fn new_capture(source: Rect, display: DisplayRect, extra: ElemExtra) -> Self {
        Self { source: Some(source), display, extra }
    }
    /// Create a UI element (no source rect)
    pub fn new_ui(display: DisplayRect, extra: ElemExtra) -> Self {
        Self { source: None, display, extra }
    }
    /// Get the source rect of a UI element; a zero rect when there is none.
    /// Callers should prefer `if let Some(src) = &e.source` to detect capture elements.
    pub fn source_rect(&self) -> Rect {
        self.source.clone().unwrap_or(Rect { x: 0, y: 0, width: 0, height: 0 })
    }
}

/// Element kinds: capture region + 4 custom UI types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElemKind {
    Capture,    // screen capture projection
    Frame,      // frame: outlined rounded rect
    Background, // background: filled rounded rect
    Png,        // image: PNG picture
    Text,       // text: rounded background + text
}

impl ElemKind {
    /// Type badge shown in the list
    pub fn badge(&self) -> &'static str {
        use crate::i18n::t;
        match self {
            ElemKind::Capture => t("badge_capture"),
            ElemKind::Frame => t("badge_frame"),
            ElemKind::Background => t("badge_bg"),
            ElemKind::Png => t("badge_png"),
            ElemKind::Text => t("badge_text"),
        }
    }
}

/// Type-specific element data (combined with display geometry into one Element; single-array storage)
#[derive(Debug, Clone, PartialEq)]
pub struct ElemExtra {
    pub kind: ElemKind,
    /// Background: fill color
    pub color: String,
    /// Text: content
    pub content: String,
    /// Text: font size
    pub font_size: u32,
    /// Text: text color
    pub text_color: String,
    /// Png: image path
    pub png_path: String,
    /// Frame: border color
    pub border_color: String,
    /// Frame: border width
    pub border_width: u32,
    /// Corner radius (Frame/Background)
    pub corner_radius: u32,
    /// Element opacity (0.0~1.0) — value displayed/edited in the editor
    pub opacity: f32,
    /// Whether the user explicitly changed opacity (true → written to yml; false → inherits global_opacity, no entry)
    pub opacity_explicit: bool,
    /// Render layer
    pub z_order: i32,
}

impl ElemExtra {
    /// Create a capture-region element
    pub fn new_capture() -> Self {
        Self {
            kind: ElemKind::Capture,
            content: String::new(),
            font_size: 14,
            text_color: "#FFFFFF".to_string(),
            color: "#D3CFC9".to_string(),
            png_path: String::new(),
            border_color: "#FFDC3C".to_string(),
            border_width: 2,
            corner_radius: 4,
            opacity: 1.0,
            opacity_explicit: false,
            z_order: 0,
        }
    }

    /// Create a UI element of the given kind
    pub fn new_ui(kind: ElemKind) -> Self {
        let mut e = Self::new_capture();
        e.kind = kind;
        e.opacity = 1.0;
        e.opacity_explicit = false;
        e.z_order = 10;
        if kind == ElemKind::Text {
            // Text: white text by default (color editable, no background)
            e.color = String::new();
            e.text_color = "#000000".to_string();
        }
        e
    }
}

/// Editor state
pub struct EditorState {
    pub phase: EditorPhase,
    pub hwnd: usize,
    pub screen_w: i32,
    pub screen_h: i32,
    // Box select (selecting phase)
    pub drag_start: Option<(i32, i32)>,
    pub drag_current: Option<(i32, i32)>,
    pub is_dragging: bool,
    // Unified element data (single array, capture + UI elements)
    pub elements: Vec<Element>,
    // Overlay preview
    pub overlay_x: i32,
    pub overlay_y: i32,
    pub overlay_w: i32,
    pub overlay_h: i32,
    // Global opacity (new elements in the editor inherit it by default, but it is not written on save)
    pub global_opacity: f32,
    // Arranging drag
    pub drag_index: Option<usize>,
    pub drag_offset: (i32, i32),
    pub resize_index: Option<usize>,
    // Text font scaling: drag start (screen coords) and start font size
    pub resize_start: Option<(i32, i32)>,
    pub resize_start_font: u32,
    // Multi-select
    pub selected_indices: Vec<usize>,
    // Box select (arranging phase)
    pub box_select_start: Option<(i32, i32)>,
    pub box_select_current: Option<(i32, i32)>,
    pub box_selecting: bool,
    // Multi-drag
    pub multi_dragging: bool,
    pub multi_drag_last: (i32, i32),
    // Filename input (resident in the panel)
    pub filename_focused: bool,
    pub save_filename: String,
    // Overwrite-confirm popup
    pub confirm_overwrite: bool,
    // Right panel position/dragging
    pub panel_x: i32,
    pub panel_y: i32,
    pub drag_panel: bool,
    pub panel_drag_offset: (i32, i32),
    // Title bar hover tooltip
    pub over_title: bool,
    pub tooltip_show: bool,
    // Bottom hint bar dodges the mouse: flips to the opposite edge when the mouse
    // approaches its current edge (bottom → top, top → bottom)
    pub hint_top: bool,
    // Exit
    pub close_requested: bool,
    pub saved: bool,
    // Visual helper toggles (all on by default)
    pub magnifier_on: bool,
    pub grid_on: bool,
    pub crosshair_on: bool,
    pub xy_label_on: bool,
    pub snap_on: bool,
    /// Tian snap (mutually exclusive with XY snap: when on, the moving element's center snaps to other elements' 3x3 intersections)
    pub snap_tian: bool,
    // Snap parameters
    pub snap_distance: i32,
    pub snap_gap: i32,
    // Arrow-key nudge step (pixels)
    pub nudge_step: i32,
    // List scrolling (index of the first visible element)
    pub list_scroll: i32,
    // Wheel delta accumulation (smooth wheels/small-delta devices; scroll 1 item per 120)
    pub wheel_acc: i32,
    // Scrollbar drag: (mouse y at press, thumb top at press)
    pub scroll_drag: Option<(i32, i32)>,
    // Mouse position (for guides/magnifier)
    pub mouse_x: i32,
    pub mouse_y: i32,
    // Snap state (hysteresis)
    pub snapped_x: Option<i32>,
    pub snapped_y: Option<i32>,
    // Property being edited
    pub editing_target: EditingTarget,
    pub editing_text: String,
    // PNG cache (path → premultiplied BGRA raw pixels)
    pub png_cache: HashMap<String, Option<(u32, u32, Vec<u8>)>>,
    /// Directory of the exe (for loading images from the PNG\ subfolder)
    pub exe_dir: String,
    /// Undo stack: element-list snapshots taken at operation commit points (newest last)
    pub undo_stack: Vec<Vec<Element>>,
    /// Snapshot taken at drag/resize start; pushed onto the undo stack on mouse-up
    /// when the element actually moved (one drag = one undo step, not per-pixel)
    pub undo_pending: Option<Vec<Element>>,
}

/// Push the current element list onto the undo stack as one undo step.
/// Deduplicates against the top of the stack (a no-op click must not create an empty layer).
pub fn push_undo(st: &mut EditorState) {
    push_snapshot(st, st.elements.clone());
}

/// Push an explicit snapshot (e.g. the drag-start state, taken at mouse-down)
pub fn push_snapshot(st: &mut EditorState, snap: Vec<Element>) {
    const UNDO_LIMIT: usize = 50;
    let dup = st.undo_stack.last().is_some_and(|top| *top == snap);
    if !dup {
        st.undo_stack.push(snap);
        if st.undo_stack.len() > UNDO_LIMIT {
            st.undo_stack.remove(0);
        }
    }
}

/// Undo the last operation: restore the previous element snapshot.
/// Returns true when an undo actually happened.
pub fn do_undo(st: &mut EditorState) -> bool {
    // Abandon any in-progress drag: its pending snapshot is stale after the restore
    st.undo_pending = None;
    st.is_dragging = false;
    st.drag_index = None;
    st.resize_index = None;
    st.multi_dragging = false;
    if let Some(prev) = st.undo_stack.pop() {
        st.elements = prev;
        st.selected_indices.clear();
        true
    } else {
        false
    }
}

static STATE: Mutex<Option<EditorState>> = Mutex::new(None);

/// Initialize the state
pub fn set_state(st: EditorState) {
    *STATE.lock().unwrap() = Some(st);
}

/// Mutable access
pub fn with_state<F, R>(f: F) -> R
where
    F: FnOnce(&mut EditorState) -> R,
{
    let mut guard = STATE.lock().unwrap();
    f(guard.as_mut().expect("editor state"))
}

/// Read-only access
pub fn with_state_ref<F, R>(f: F) -> R
where
    F: FnOnce(&EditorState) -> R,
{
    let guard = STATE.lock().unwrap();
    f(guard.as_ref().expect("editor state"))
}
