//! Editor shared code: constants, types, GDI drawing helpers
//!
//! Referenced by all editor submodules.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    AlphaBlend, CreateFontW, CreatePen, CreateSolidBrush, CreateCompatibleDC, CreateDIBSection,
    DeleteDC, DeleteObject, DrawTextW, FillRect, GetStockObject, Rectangle, RoundRect, SelectObject,
    SetBkMode, SetTextColor, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, DIB_RGB_COLORS,
    HDC, HFONT, NULL_BRUSH, PEN_STYLE, TRANSPARENT, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DT_LEFT,
};

// ===== Layout constants =====

/// Control row height inside the panel (action button row / toggle row / value row)
pub const SECTION_H: i32 = 34;
pub const PANEL_W: i32 = 460;
pub const TITLE_H: i32 = 42;
pub const FN_BOX_H: i32 = 36;
/// Start y offset of the list area (relative to py): title + filename + mode row + action row + toggle row + value row
pub const LIST_TOP2: i32 =
    TITLE_H + 22 + FN_BOX_H + 8 + SECTION_H + 8 + SECTION_H + 8 + SECTION_H + 8 + SECTION_H + 10;
pub const LIST_ITEM_H: i32 = 42;
pub const HOVER_TIMER_ID: usize = 9001;

/// Value box with up/down arrow buttons: button width (2 character slots)
pub const SPIN_W: i32 = 16;
/// Total width of the toggle value input box (incl. the arrow buttons on the right)
pub const TOG_BOX_W: i32 = 56;

pub const BG_ALPHA: u8 = 76;
pub const UI_ALPHA: u8 = 153;

// Magnifier
pub const MAG_SIZE: i32 = 400;
pub const MAG_CAPTURE: i32 = 200;

// Grid
pub const GRID_SPACING: i32 = 100;

// ===== Shared types =====

#[derive(PartialEq, Clone, Copy)]
pub enum EditorPhase { Selecting, Arranging }

#[derive(PartialEq, Clone, Copy)]
pub enum CursorKind { Cross, SizeAll, Hand, Ibeam, Arrow }

#[derive(PartialEq, Clone, Copy)]
pub enum FieldKind { X, Y, Width, Height }

/// Editable properties of a custom UI element (type-specific + common)
#[derive(PartialEq, Clone, Copy)]
pub enum ElemProp {
    // Common (all UI elements)
    Opacity,
    ZOrder,
    /// Rotation in degrees (degrees clockwise; stored in DisplayRect, editable for all kinds)
    Rotate,
    // Frame
    BorderColor,
    BorderWidth,
    CornerRadius,
    // Background: Color + CornerRadius
    BgColor,
    // Text
    Content,
    FontSize,
    TextColor,
    // Png
    PngPath,
}

impl ElemProp {
    /// Is this a numeric field (digits/minus only); otherwise a text field (all printable chars)
    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            ElemProp::Opacity
                | ElemProp::ZOrder
                | ElemProp::Rotate
                | ElemProp::BorderWidth
                | ElemProp::CornerRadius
                | ElemProp::FontSize
        )
    }
    /// Field label
    pub fn label(self) -> &'static str {
        use crate::i18n::t;
        match self {
            ElemProp::Opacity => t("opacity"),
            ElemProp::ZOrder => t("z_order"),
            ElemProp::Rotate => t("rotate"),
            ElemProp::BorderColor => t("border_color"),
            ElemProp::BorderWidth => t("border_width"),
            ElemProp::CornerRadius => t("corner_radius"),
            ElemProp::BgColor => t("bg_color"),
            ElemProp::Content => t("content"),
            ElemProp::FontSize => t("font_size"),
            ElemProp::TextColor => t("text_color"),
            ElemProp::PngPath => t("png_path"),
        }
    }
}

#[derive(PartialEq, Clone)]
pub enum EditingTarget {
    None,
    RegionField(usize, FieldKind),
    /// Edit a custom UI element property: (element index, property)
    ElemField(usize, ElemProp),
    SnapDistance,
    SnapGap,
    /// Arrow-key nudge step (pixels)
    NudgeStep,
}

/// Target of a numeric spinner (arrow buttons / wheel)
#[derive(PartialEq, Clone, Copy)]
pub enum SpinTarget {
    RegionField(usize, FieldKind),
    ElemProp(usize, ElemProp),
    SnapDistance,
    SnapGap,
    NudgeStep,
}

#[derive(PartialEq, Clone, Copy)]
pub enum ToggleKind {
    Magnifier,
    Grid,
    Crosshair,
    XyLabel,
    Snap,
    TianSnap,
    SnapDistance,
    SnapGap,
    NudgeStep,
}

#[derive(PartialEq, Clone, Copy)]
pub enum BtnAction {
    UndoLast,
    NextStep,
    BackToSelect,
    Cancel,
    DeleteSelected,
    Save,
    Discard,
}

// ===== Color helpers =====

pub fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

// ===== Fonts =====

/// Create a rounded UI font (Microsoft YaHei UI, ClearType smoothing)
/// pixel_height: character pixel height (positive; negated internally to specify pixel height)
pub unsafe fn create_ui_font(pixel_height: i32) -> HFONT {
    let face_name: Vec<u16> = "Microsoft YaHei UI\0".encode_utf16().collect();
    CreateFontW(
        -pixel_height,  // negative = character pixel height
        0,          // cWidth
        0,          // cEscapement
        0,          // cOrientation
        400,        // cWeight = FW_NORMAL
        0,          // bItalic
        0,          // bUnderline
        0,          // bStrikeOut
        1,          // iCharSet = DEFAULT_CHARSET
        0,          // iOutPrecision = OUT_DEFAULT_PRECIS
        0,          // iClipPrecision = CLIP_DEFAULT_PRECIS
        5,          // iQuality = CLEARTYPE_QUALITY
        0,          // iPitchAndFamily = DEFAULT_PITCH | FF_DONTCARE
        windows::core::PCWSTR::from_raw(face_name.as_ptr()),
    )
}

// ===== GDI drawing helpers =====
// Note: DrawTextW uses the slice length as cchText, no null terminator needed.
// A null terminator before caused U+0000 to render as a small dot (deg symbol).

pub unsafe fn gdi_text(hdc: HDC, text: &str, x: i32, y: i32, w: i32, h: i32, color: COLORREF) {
    // Skip empty strings: DrawTextW reads up to the null terminator when cchText=0,
    // and an empty Vec's pointer is dangling, which would hit an invalid address and hard-crash.
    if text.is_empty() {
        return;
    }
    let _ = SetBkMode(hdc, TRANSPARENT);
    let _ = SetTextColor(hdc, color);
    let rect = RECT { left: x, top: y, right: x + w, bottom: y + h };
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    let font_h = (h * 3 / 5).clamp(11, 22);
    let font = create_ui_font(font_h);
    let old = SelectObject(hdc, font);
    let _ = DrawTextW(hdc, &mut utf16, &rect as *const RECT as *mut _, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
    SelectObject(hdc, old);
    let _ = DeleteObject(font);
}

pub unsafe fn gdi_text_left(hdc: HDC, text: &str, x: i32, y: i32, w: i32, h: i32, color: COLORREF) {
    // Same as gdi_text: skip empty strings to avoid a DrawTextW crash.
    if text.is_empty() {
        return;
    }
    let _ = SetBkMode(hdc, TRANSPARENT);
    let _ = SetTextColor(hdc, color);
    let rect = RECT { left: x, top: y, right: x + w, bottom: y + h };
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    let font_h = (h * 3 / 5).clamp(11, 22);
    let font = create_ui_font(font_h);
    let old = SelectObject(hdc, font);
    let _ = DrawTextW(hdc, &mut utf16, &rect as *const RECT as *mut _, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    SelectObject(hdc, old);
    let _ = DeleteObject(font);
}

pub unsafe fn fill_rect_solid(hdc: HDC, x: i32, y: i32, w: i32, h: i32, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    let rect = RECT { left: x, top: y, right: x + w, bottom: y + h };
    let _ = FillRect(hdc, &rect, brush);
    let _ = DeleteObject(brush);
}

/// Translucent fill: draw a solid color block with AlphaBlend. The real screen below stays visible in the editor.
pub unsafe fn fill_rect_alpha(hdc: HDC, x: i32, y: i32, w: i32, h: i32, color: COLORREF, alpha: u8) {
    if w <= 0 || h <= 0 {
        return;
    }
    let hdc_src = CreateCompatibleDC(hdc);
    if hdc_src.is_invalid() {
        return;
    }
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: 1,
            biHeight: -1,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let hbmp = match CreateDIBSection(hdc_src, &bi, DIB_RGB_COLORS, &mut bits, None, 0) {
        Ok(b) => b,
        Err(_) => {
            let _ = DeleteDC(hdc_src);
            return;
        }
    };
    if bits.is_null() {
        let _ = DeleteObject(hbmp);
        let _ = DeleteDC(hdc_src);
        return;
    }
    // COLORREF's low byte is R; DIB memory byte order is B, G, R
    let p = bits as *mut u8;
    *p = ((color.0 >> 16) & 0xFF) as u8;
    *p.add(1) = ((color.0 >> 8) & 0xFF) as u8;
    *p.add(2) = (color.0 & 0xFF) as u8;
    *p.add(3) = 0;
    let old = SelectObject(hdc_src, hbmp);
    let blend = BLENDFUNCTION {
        BlendOp: 0, // AC_SRC_OVER
        BlendFlags: 0,
        SourceConstantAlpha: alpha,
        AlphaFormat: 0, // only SourceConstantAlpha; source 32bpp alpha ignored
    };
    let _ = AlphaBlend(hdc, x, y, w, h, hdc_src, 0, 0, 1, 1, blend);
    SelectObject(hdc_src, old);
    let _ = DeleteObject(hbmp);
    let _ = DeleteDC(hdc_src);
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn draw_button(hdc: HDC, x: i32, y: i32, w: i32, h: i32, label: &str, bg: COLORREF, fg: COLORREF, border: COLORREF) {
    let brush = CreateSolidBrush(bg);
    let pen = CreatePen(PEN_STYLE(0), 1, border);
    let old_pen = SelectObject(hdc, pen);
    let old_br = SelectObject(hdc, brush);
    let _ = RoundRect(hdc, x, y, x + w, y + h, 6, 6);
    SelectObject(hdc, old_pen);
    SelectObject(hdc, old_br);
    let _ = DeleteObject(brush);
    let _ = DeleteObject(pen);
    gdi_text(hdc, label, x, y, w, h, fg);
}

pub unsafe fn draw_rect_outline(hdc: HDC, x: i32, y: i32, w: i32, h: i32, pen_w: i32, color: COLORREF) {
    let pen = CreatePen(PEN_STYLE(0), pen_w, color);
    let old_pen = SelectObject(hdc, pen);
    let old_br = SelectObject(hdc, GetStockObject(NULL_BRUSH));
    let _ = Rectangle(hdc, x, y, x + w, y + h);
    SelectObject(hdc, old_pen);
    SelectObject(hdc, old_br);
    let _ = DeleteObject(pen);
}

pub unsafe fn draw_handle(hdc: HDC, x: i32, y: i32) {
    let brush = CreateSolidBrush(rgb(255, 220, 60));
    let pen = CreatePen(PEN_STYLE(0), 1, rgb(30, 30, 30));
    let old_pen = SelectObject(hdc, pen);
    let old_br = SelectObject(hdc, brush);
    let _ = Rectangle(hdc, x, y, x + 12, y + 12);
    SelectObject(hdc, old_pen);
    SelectObject(hdc, old_br);
    let _ = DeleteObject(brush);
    let _ = DeleteObject(pen);
}
