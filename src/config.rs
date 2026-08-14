//! YAML config file loading and validation
//!
//! Two kinds of config files:
//! 1. settings.yml — global settings (FPS memory + per-config toggles)
//! 2. *.yml — region config files (e.g. Default Config 1)
//!
//! Each region config file holds only region data; on/off state lives centrally in settings.yml.

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

/// Global settings (settings.yml) — remembers FPS and per-config toggles
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalSettings {
    /// Global frame rate
    #[serde(default = "default_fps")]
    pub fps: u32,
    /// List of enabled config files
    #[serde(default)]
    pub enabled_configs: Vec<String>,
    /// Editor snap distance (pixels)
    #[serde(default = "default_snap_distance")]
    pub snap_distance: i32,
    /// Editor snap gap (pixels)
    #[serde(default = "default_snap_gap")]
    pub snap_gap: i32,
    /// Editor nudge step (pixels, arrow keys/scroll/up-down buttons)
    #[serde(default = "default_nudge_step")]
    pub nudge_step: i32,
    /// Master toggle for controller/keyboard auto-switch (tray check mark, written back)
    #[serde(default)]
    pub input_auto_switch: bool,
    /// Whether mouse movement counts as "keyboard/mouse activity"
    #[serde(default = "default_input_track_mouse")]
    pub input_track_mouse: bool,
    /// Input polling interval in ms (sensitivity tweak)
    #[serde(default = "default_input_poll_interval_ms")]
    pub input_poll_interval_ms: u64,
    /// Config files enabled in keyboard mode
    #[serde(default)]
    pub keyboard_configs: Vec<String>,
    /// Config files enabled in controller mode
    #[serde(default)]
    pub controller_configs: Vec<String>,
    /// UI language ("zh" default / "en")
    #[serde(default = "default_lang")]
    pub lang: String,
    /// Write log files next to the exe (log.txt / crash.txt); off by default.
    /// Binary switch: false → no log file and no crash file at all.
    #[serde(default)]
    pub log_enabled: bool,
}

fn default_fps() -> u32 {
    60
}

fn default_lang() -> String {
    "zh".to_string()
}

fn default_snap_distance() -> i32 {
    8
}

fn default_snap_gap() -> i32 {
    0
}

fn default_nudge_step() -> i32 {
    1
}

fn default_input_track_mouse() -> bool {
    true
}

fn default_input_poll_interval_ms() -> u64 {
    200
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            fps: 60,
            enabled_configs: Vec::new(),
            snap_distance: 8,
            snap_gap: 0,
            nudge_step: 1,
            input_auto_switch: false,
            input_track_mouse: true,
            input_poll_interval_ms: 200,
            keyboard_configs: Vec::new(),
            controller_configs: Vec::new(),
            lang: "zh".to_string(),
            log_enabled: false,
        }
    }
}

/// Default config filename
/// Default file name used when a new config is saved with an empty name
pub const UNNAMED_CONFIG: &str = "未命名.yml";
/// Global settings filename
pub const SETTINGS_FILE: &str = "settings.yml";
/// Legacy global settings filename (v0.0.4 and earlier); auto-migrated to settings.yml on first load if present
const LEGACY_SETTINGS_FILE: &str = "settings.yaml";

impl GlobalSettings {
    /// Load settings.yml from the exe directory; None when the file doesn't exist (distinguishes first run).
    /// Backwards-compatible with legacy settings.yaml: if present, read and rename to settings.yml (one-time migration).
    pub fn load_optional() -> Option<Self> {
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(dir) = exe_path.parent() {
                // Legacy yaml migration: only when yml is missing but yaml exists
                if !dir.join(SETTINGS_FILE).exists() && dir.join(LEGACY_SETTINGS_FILE).exists() {
                    let _ = std::fs::rename(dir.join(LEGACY_SETTINGS_FILE), dir.join(SETTINGS_FILE));
                    log::info!("Legacy {} migrated to {}", LEGACY_SETTINGS_FILE, SETTINGS_FILE);
                }
                let path = dir.join(SETTINGS_FILE);
                if let Ok(content) = std::fs::read_to_string(&path) {
                    // Guard: users may save with editors that add a BOM; strip it to avoid serde_yml parse failure
                    let content = content.strip_prefix('\u{FEFF}').unwrap_or(&content);
                    if let Ok(mut gs) = serde_yml::from_str::<GlobalSettings>(content) {
                        // Guard: clamp out-of-range fps to avoid a from_secs_f32(inf) panic
                        gs.fps = gs.fps.clamp(1, 240);
                        log::info!("Global settings loaded: fps={}, enabled configs={}",
                            gs.fps, gs.enabled_configs.len());
                        return Some(gs);
                    }
                }
            }
        }
        log::info!("Global settings not found, using defaults");
        None
    }

    /// Load settings.yml from the exe directory; return defaults when missing.
    pub fn load() -> Self {
        Self::load_optional().unwrap_or_default()
    }

    /// Save to settings.yml in the exe directory
    pub fn save(&self) {
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(dir) = exe_path.parent() {
                let path = dir.join(SETTINGS_FILE);
                match serde_yml::to_string(self) {
                    Ok(yaml) => {
                        if let Err(e) = std::fs::write(&path, &yaml) {
                            log::error!("Failed to write global settings: {}", e);
                        } else {
                            log::info!("Global settings saved: fps={}", self.fps);
                        }
                    }
                    Err(e) => log::error!("Failed to serialize global settings: {}", e),
                }
            }
        }
    }

    /// Check whether the given config file is enabled
    pub fn is_enabled(&self, filename: &str) -> bool {
        self.enabled_configs.iter().any(|f| f == filename)
    }

    /// Set the on/off state of the given config file
    pub fn set_enabled(&mut self, filename: &str, enabled: bool) {
        if enabled {
            if !self.is_enabled(filename) {
                self.enabled_configs.push(filename.to_string());
            }
        } else {
            self.enabled_configs.retain(|f| f != filename);
        }
    }

    /// Prune settings references to invalid configs (stale memory after file deletion / legacy .yaml references).
    /// Rule: reference ends with .yaml (yaml support dropped) or file doesn't exist → remove from the enabled set and both input groups.
    /// Returns whether anything was removed (caller saves accordingly).
    pub fn prune_missing_refs(&mut self) -> bool {
        // Resolve the exe directory once (the closure runs per reference)
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let exists_ok = |name: &str| {
            !name.to_ascii_lowercase().ends_with(".yaml")
                && exe_dir
                    .as_ref()
                    .map(|d| d.join(name).exists())
                    .unwrap_or(false)
        };
        let mut changed = false;
        let before = self.enabled_configs.len();
        self.enabled_configs.retain(|f| exists_ok(f));
        changed |= self.enabled_configs.len() != before;
        for list in [&mut self.keyboard_configs, &mut self.controller_configs] {
            let before = list.len();
            list.retain(|f| exists_ok(f));
            changed |= list.len() != before;
        }
        changed
    }
}

/// Editor snap distance/gap/nudge step, staged in a shared slot instead of writing to disk.
/// The main loop polls these and applies them to its in-memory GlobalSettings, which is
/// written to settings.yml only once on exit (avoids frequent disk writes while tweaking).
static PENDING_PREFS: std::sync::Mutex<Option<(i32, i32, i32)>> = std::sync::Mutex::new(None);

/// Called by the editor when snap prefs change: stage them (no disk I/O here)
pub fn save_editor_prefs(snap_distance: i32, snap_gap: i32, nudge_step: i32) {
    if let Ok(mut g) = PENDING_PREFS.lock() {
        *g = Some((snap_distance, snap_gap, nudge_step));
    }
}

/// Called by the main loop (after the editor closes) to grab staged prefs and apply them in memory
pub fn take_editor_prefs() -> Option<(i32, i32, i32)> {
    if let Ok(mut g) = PENDING_PREFS.lock() {
        g.take()
    } else {
        None
    }
}

/// Top-level config struct (one region config file)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct Config {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub capture_regions: Vec<CaptureRegion>,
    /// Custom UI elements (parallel to capture_regions, overlay-absolute coordinates)
    #[serde(default)]
    pub custom_ui: Vec<CustomUiElement>,
}


/// Global settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct Settings {
    /// Global opacity 0.0~1.0 (defaults to 1.0 when unset; elements without opacity inherit it)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_opacity: Option<f32>,
    /// Overlay window anchor: the origin of the content coordinate system lands at screen (x, y). Size is no longer hand-written; it's auto-fitted to the content bounding box.
    /// Defaults to (0,0) when unset (content coordinates equal screen coordinates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowConfig>,
}

/// Overlay window anchor (right/bottom sizes deprecated, only x/y are read; width/height kept for legacy files)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WindowConfig {
    pub x: i32,
    pub y: i32,
    #[serde(rename = "w", alias = "width", default, skip_serializing_if = "is_zero_i32")]
    pub width: i32,
    #[serde(rename = "h", alias = "height", default, skip_serializing_if = "is_zero_i32")]
    pub height: i32,
}

fn is_zero_i32(v: &i32) -> bool {
    *v == 0
}

/// Lightweight PNG size read (IHDR chunk only, no full decode)
fn png_intrinsic_size(path: &std::path::Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut sig = [0u8; 8];
    f.read_exact(&mut sig).ok()?;
    if sig != [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'] {
        return None;
    }
    let mut hdr = [0u8; 16];
    f.read_exact(&mut hdr).ok()?;
    if &hdr[4..8] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
    let h = u32::from_be_bytes([hdr[12], hdr[13], hdr[14], hdr[15]]);
    Some((w, h))
}


/// Screen capture region
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureRegion {
    /// Unique id
    pub id: String,
    /// Source region (screen-absolute coordinates)
    pub source: Rect,
    /// Display position (overlay window coordinates)
    pub display: DisplayRect,
    /// Render order; higher numbers are on top (legacy top-level position, merged into display.z on load)
    #[serde(default, rename = "z", alias = "z_order", skip_serializing_if = "Option::is_none")]
    pub z_order: Option<i32>,
}

/// Rectangle (source coordinates)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    #[serde(rename = "w", alias = "width")]
    pub width: u32,
    #[serde(rename = "h", alias = "height")]
    pub height: u32,
}

/// Display rectangle (with opacity and render order)
/// width/height of 0 or missing = display at 1:1 (source size)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayRect {
    pub x: i32,
    pub y: i32,
    #[serde(rename = "w", alias = "width", default, skip_serializing_if = "is_zero")]
    pub width: i32,
    #[serde(rename = "h", alias = "height", default, skip_serializing_if = "is_zero")]
    pub height: i32,
    /// Render order; higher numbers are on top (moved here from the region top level)
    #[serde(default, rename = "z", alias = "z_order")]
    pub z_order: i32,
    /// This element's opacity 0.0~1.0 (inherits global_opacity when unset; 1.0 when there's no global_opacity)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    /// Rotation in degrees clockwise (0/90/180/270 fast paths, any angle supported):
    /// rotates the content around the display center; w/h describe the unrotated
    /// (logical) size and the visible footprint becomes the rotated bounding box
    /// (e.g. a 60x100 source at 1:1 rotated 90° occupies a 100x60 footprint)
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rotate: i32,
}

// ===== Default value functions =====

fn default_corner_radius() -> u32 {
    4
}
fn default_border_width() -> u32 {
    2
}
fn default_border_color() -> String {
    "#FFDC3C".to_string()
}
fn default_fill_color() -> String {
    "#2A2A3A".to_string()
}
fn default_font_size() -> u32 {
    14
}
fn default_text_color() -> String {
    String::new()
}
// Missing width/height default to 0: images display 1:1, other elements render as a minimal edit point
fn default_ui_w() -> i32 {
    0
}
fn default_ui_h() -> i32 {
    0
}

// ===== Custom UI elements (parallel to capture_regions) =====

/// Custom UI element (types distinguished by a serde tag). Each kind carries geometry (x/y/width/height) for dragging/resizing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CustomUiElement {
    /// Frame: outlined rounded rectangle
    #[serde(rename = "frame")]
    Frame(FrameElement),
    /// Background: filled rounded rectangle
    #[serde(rename = "background")]
    Background(BackgroundElement),
    /// Image: PNG picture
    #[serde(rename = "image")]
    Image(ImageElement),
    /// Text: rounded background + text
    #[serde(rename = "text")]
    Text(TextElement),
}

impl CustomUiElement {
    /// Get geometry (x, y, w, h). Text has no fixed size (auto-fit) → (x, y, 0, 0).
    pub fn geometry(&self) -> (i32, i32, i32, i32) {
        match self {
            CustomUiElement::Frame(e) => (e.x, e.y, e.width, e.height),
            CustomUiElement::Background(e) => (e.x, e.y, e.width, e.height),
            CustomUiElement::Image(e) => (e.x, e.y, e.width, e.height),
            CustomUiElement::Text(e) => (e.x, e.y, 0, 0),
        }
    }
    /// Shift the element position by (dx, dy) — used when the overlay window is
    /// moved to the tight content bounds: content translates so screen pixels don't move.
    pub fn shift_xy(&mut self, dx: i32, dy: i32) {
        match self {
            CustomUiElement::Frame(e) => { e.x -= dx; e.y -= dy; }
            CustomUiElement::Background(e) => { e.x -= dx; e.y -= dy; }
            CustomUiElement::Image(e) => { e.x -= dx; e.y -= dy; }
            CustomUiElement::Text(e) => { e.x -= dx; e.y -= dy; }
        }
    }
    /// Element opacity (None = not explicitly set)
    pub fn opacity(&self) -> Option<f32> {
        match self {
            CustomUiElement::Frame(e) => e.opacity,
            CustomUiElement::Background(e) => e.opacity,
            CustomUiElement::Image(e) => e.opacity,
            CustomUiElement::Text(e) => e.opacity,
        }
    }
    /// Rotation in degrees clockwise (0 = none)
    pub fn rotate(&self) -> i32 {
        match self {
            CustomUiElement::Frame(e) => e.rotate,
            CustomUiElement::Background(e) => e.rotate,
            CustomUiElement::Image(e) => e.rotate,
            CustomUiElement::Text(e) => e.rotate,
        }
    }
}

/// Frame element: outlined rounded rectangle
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameElement {
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default = "default_ui_w", rename = "w", alias = "width")]
    pub width: i32,
    #[serde(default = "default_ui_h", rename = "h", alias = "height")]
    pub height: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    #[serde(default, rename = "z", alias = "z_order")]
    pub z_order: i32,
    /// Border color
    #[serde(default = "default_border_color")]
    pub border_color: String,
    /// Border width
    #[serde(default = "default_border_width")]
    pub border_width: u32,
    #[serde(default = "default_corner_radius")]
    pub corner_radius: u32,
    /// Rotation in degrees clockwise (any angle; the footprint becomes the rotated bounding box)
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rotate: i32,
}

/// Background element: filled rounded rectangle
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundElement {
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default = "default_ui_w", rename = "w", alias = "width")]
    pub width: i32,
    #[serde(default = "default_ui_h", rename = "h", alias = "height")]
    pub height: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    #[serde(default, rename = "z", alias = "z_order")]
    pub z_order: i32,
    /// Fill color
    #[serde(default = "default_fill_color")]
    pub color: String,
    #[serde(default = "default_corner_radius")]
    pub corner_radius: u32,
    /// Rotation in degrees clockwise (any angle; the footprint becomes the rotated bounding box)
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rotate: i32,
}

/// Image element: PNG picture
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageElement {
    /// Image file name (read from the PNG\ subfolder of the exe directory)
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    /// Width (0 or missing = show the image at native 1:1 size)
    #[serde(default, skip_serializing_if = "is_zero", rename = "w", alias = "width")]
    pub width: i32,
    /// Height (0 or missing = show the image at native 1:1 size)
    #[serde(default, skip_serializing_if = "is_zero", rename = "h", alias = "height")]
    pub height: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    #[serde(default, rename = "z", alias = "z_order")]
    pub z_order: i32,
    /// Rotation in degrees clockwise (any angle; the footprint becomes the rotated bounding box)
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rotate: i32,
}

fn is_zero(v: &i32) -> bool {
    *v == 0
}

/// Tolerate bare scalars like `content: 13` in yaml: numbers/booleans and other scalars are all coerced to strings
fn de_string_or_number<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v: serde_yml::Value = serde::Deserialize::deserialize(deserializer)?;
    match v {
        serde_yml::Value::String(s) => Ok(s),
        serde_yml::Value::Number(n) => Ok(n.to_string()),
        serde_yml::Value::Bool(b) => Ok(b.to_string()),
        serde_yml::Value::Null => Ok(String::new()),
        other => serde_yml::from_value::<String>(other).map_err(D::Error::custom),
    }
}

/// Text element: plain text (no background; size always auto-derived from content + font size)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextElement {
    /// Text content
    #[serde(default, deserialize_with = "de_string_or_number")]
    pub content: String,
    /// Font size
    #[serde(default = "default_font_size")]
    pub font_size: u32,
    /// Text color "#RRGGBB"
    #[serde(default = "default_text_color")]
    pub text_color: String,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    #[serde(default, rename = "z", alias = "z_order")]
    pub z_order: i32,
    /// Rotation in degrees clockwise (any angle; the footprint becomes the rotated bounding box)
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rotate: i32,
}

// ===== Compact YAML output =====
//
// Geometry rects (source/display/window) collapse into single-line flow mappings:
//   display: {x: 1360, 'y': 475, w: 230, h: 176}
// Image/text geometry (x/'y'/z) also folds into one line under the geometry key:
//   - type: image
//     path: X.png
//     geometry: {x: 0, 'y': 8, z: 5}
// Short names w/h/z are emitted via serde rename; width/height/z_order still parse (legacy files).
// A bare `{...}` line is invalid YAML (a block-map value needs a key), hence the geometry: key.

/// Geometry keys folded into a single-line flow (element geometry; text has no w/h)
const ELEM_GEOMETRY_KEYS: [&str; 8] = ["x", "y", "'y'", "w", "h", "z", "opacity", "rotate"];

/// Collapse serde_yml's block-form geometry rects into single-line flow mappings
fn collapse_inline_rects(yaml: &str) -> String {
    fn strip_indent(line: &str) -> (usize, &str) {
        let trimmed = line.trim_start();
        (line.len() - trimmed.len(), trimmed)
    }
    /// Parse "key: value" where the value is a plain numeric scalar
    fn split_scalar(line: &str) -> Option<(String, String)> {
        let (k, v) = line.split_once(':')?;
        let key = k.trim();
        if key.is_empty() {
            return None;
        }
        let val = v.trim();
        if val.is_empty() || !val.chars().all(|c| {
            c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')
        }) {
            return None;
        }
        Some((key.to_string(), val.to_string()))
    }
    fn is_elem_geometry_key(key: &str) -> bool {
        ELEM_GEOMETRY_KEYS.contains(&key)
    }

    let lines: Vec<&str> = yaml.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let (indent, content) = strip_indent(line);
        let parent = content
            .strip_suffix(':')
            .map(str::trim_end)
            .unwrap_or("");
        if matches!(parent, "source" | "display" | "window") {
            // Collect deeper child lines
            let mut j = i + 1;
            let mut entries: Vec<(String, String)> = Vec::new();
            while j < lines.len() {
                let (n_indent, n_content) = strip_indent(lines[j]);
                if n_indent <= indent || n_content.is_empty() {
                    break;
                }
                match split_scalar(n_content) {
                    Some(e) => {
                        entries.push(e);
                        j += 1;
                    }
                    None => break,
                }
            }
            let has = |k: &str| entries.iter().any(|(ek, _)| ek == k);
            let is_num = |k: &str| entries.iter().any(|(ek, v)| ek == k && !v.is_empty());
            // display may omit w/h (1:1 follows the source size) → fold with just x,y(,z,opacity);
            // source/window are full rects and always carry w/h
            let fold_ok = if parent == "display" {
                entries.len() >= 2 && has("x") && (has("y") || has("'y'"))
            } else {
                entries.len() >= 4
                    && has("x")
                    && (has("y") || has("'y'"))
                    && is_num("w")
                    && is_num("h")
            };
            if fold_ok {
                let items: Vec<String> = entries
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, v))
                    .collect();
                out.push(format!(
                    "{}: {{{}}}",
                    " ".repeat(indent) + parent,
                    items.join(", ")
                ));
                i = j;
                continue;
            }
        } else if content.trim_start().starts_with("- type:")
            && matches!(content.trim_start().trim_start_matches("- type:").trim(), "image" | "text")
        {
            // Element block: fold the trailing geometry keys (x/'y'/w/h/z/opacity) into one flow line.
            // Field order puts the geometry at the END of the block (path/content/color come first).
            let mut j = i + 1;
            let mut pre_lines: Vec<&str> = Vec::new();
            let mut pending: Vec<(String, String)> = Vec::new();
            let mut folded_indent: usize = 0;
            while j < lines.len() {
                let (n_indent, n_content) = strip_indent(lines[j]);
                if n_indent <= indent || n_content.is_empty() {
                    break;
                }
                match split_scalar(n_content) {
                    Some((k, v)) if is_elem_geometry_key(&k) => {
                        pending.push((k, v));
                        folded_indent = n_indent;
                        j += 1;
                    }
                    _ => {
                        // non-geometry line: geometry is only foldable when trailing, so reset
                        pending.clear();
                        pre_lines.push(lines[j]);
                        j += 1;
                    }
                }
            }
            let has = |k: &str| pending.iter().any(|(ek, _)| ek == k);
            if pending.len() >= 2 && has("x") && (has("y") || has("'y'")) {
                let items: Vec<String> = pending
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, v))
                    .collect();
                let mut block: Vec<String> = Vec::with_capacity(pre_lines.len() + 1);
                block.extend(pre_lines.iter().map(|pl| (*pl).to_string()));
                // A bare `{...}` line is invalid YAML (a block-map value needs a key):
                // the folded geometry goes under the geometry: key, mirroring display: {...}
                block.push(format!(
                    "{}geometry: {{{}}}",
                    " ".repeat(folded_indent),
                    items.join(", ")
                ));
                out.push(format!("{}{}", " ".repeat(indent), content));
                out.extend(block);
                i = j;
                continue;
            }
        }
        out.push(line.to_string());
        i += 1;
    }
    out.join("\n")
}

// ===== Loading logic =====

/// Parse config content, filtering out unsupported custom_ui element types,
/// so unknown variants don't trip up the serde enum and fail parsing.
fn parse_config_filtered(content: &str) -> AppResult<Config> {
    let mut value: serde_yml::Value = serde_yml::from_str(content)
        .map_err(|e| AppError::other(format!("Failed to parse config file: {}", e)))?;

    if let Some(seq) = value.get_mut("custom_ui").and_then(|v| v.as_sequence_mut()) {
        seq.retain(|item| {
            match item.get("type").and_then(|t| t.as_str()) {
                Some("frame") | Some("background") | Some("image") | Some("text") => true,
                Some(other) => {
                    log::warn!("Skipping unsupported custom_ui element type: {}", other);
                    false
                }
                None => {
                    log::warn!("Skipping a custom_ui element missing the type field");
                    false
                }
            }
        });
        // Unnest the compact geometry: {x, 'y', z} mapping back to flat keys,
        // so both the new geometry: {...} form and legacy flat form parse identically
        for item in seq.iter_mut() {
            let Some(map) = item.as_mapping_mut() else { continue };
            let Some(geom) = map.remove("geometry") else { continue };
            let Some(gmap) = geom.as_mapping() else { continue };
            for (k, v) in gmap {
                map.insert(k.clone(), v.clone());
            }
        }
    }

    serde_yml::from_value(value)
        .map_err(|e| AppError::other(format!("Failed to parse config file: {}", e)))
}

impl Config {
    /// Write the config to the given filename in the exe directory
    pub fn save_as(&self, filename: &str) -> AppResult<()> {
        let exe_path = std::env::current_exe()
            .map_err(|e| AppError::other(format!("Failed to get the exe path: {}", e)))?;
        let exe_dir = exe_path
            .parent()
            .ok_or_else(|| AppError::other("Failed to get the exe directory"))?;
        let config_path = exe_dir.join(filename);
        let yaml = self
            .to_compact_yaml()
            .map_err(|e| AppError::other(format!("Failed to serialize config: {}", e)))?;
        std::fs::write(&config_path, &yaml)
            .map_err(|e| AppError::other(format!("Failed to write config file: {}", e)))?;
        log::info!("Config saved: {}", config_path.display());
        Ok(())
    }

    /// Serialize to compact YAML (single-line geometry rects + short w/h/z fields)
    pub fn to_compact_yaml(&self) -> AppResult<String> {
        let yaml = serde_yml::to_string(self)
            .map_err(|e| AppError::other(format!("Failed to serialize config: {}", e)))?;
        Ok(collapse_inline_rects(&yaml))
    }

    /// Load a config from the given file in the exe directory
    pub fn load_from(filename: &str) -> AppResult<Self> {
        let exe_path = std::env::current_exe()
            .map_err(|e| AppError::other(format!("Failed to get the exe path: {}", e)))?;
        let exe_dir = exe_path
            .parent()
            .ok_or_else(|| AppError::other("Failed to get the exe directory"))?;
        let config_path = exe_dir.join(filename);

        let content = std::fs::read_to_string(&config_path)
            .map_err(|e| AppError::other(format!("Failed to read config file {}: {}", config_path.display(), e)))?;

        let mut config: Config = parse_config_filtered(&content)?;

        config.validate();
        log::info!("Config loaded: {}", config_path.display());
        Ok(config)
    }

    /// Effective global opacity: 1.0 when global_opacity is unset
    pub fn effective_global_opacity(&self) -> f32 {
        self.settings.global_opacity.unwrap_or(1.0)
    }

    /// Resolve "element opacity (possibly None)" to an effective value:
    /// element set → use it; unset → use global_opacity; neither set → 1.0
    #[allow(dead_code)]
    pub fn effective_opacity(&self, elem: Option<f32>) -> f32 {
        elem.unwrap_or(self.effective_global_opacity())
    }

    /// Compute the overlay window rect (position + auto size).
    /// Position = settings.window anchor (x, y), (0,0) when unset; content coordinates are window coordinates, presented as-is.
    /// Size = max right/bottom extent of the content (window.width/height no longer used).
    /// When image/text lack width/height, use their PNG intrinsic size / measured text size so the window bottom isn't truncated.
    /// Returns (x, y, w, h). Returns 200x150 when there's no content.
    /// NOTE (v0.0.9): the runtime overlay window no longer uses this function (tight_bounds
    /// is used instead); this one is kept for the editor's canvas viewport.
    pub fn overlay_bounds(&self) -> (i32, i32, i32, i32) {
        let (wx, wy) = self
            .settings
            .window
            .as_ref()
            .map(|w| (w.x, w.y))
            .unwrap_or((0, 0));
        let mut max_x = 0i32;
        let mut max_y = 0i32;

        for r in &self.capture_regions {
            let d = &r.display;
            let (dw, dh) = self.region_logical_size(r);
            let (fx, fy, fw, fh) = rotated_footprint(d.x, d.y, dw, dh, d.rotate);
            max_x = max_x.max(fx + fw);
            max_y = max_y.max(fy + fh);
        }
        for ui in &self.custom_ui {
            let (x, y, w, h) = self.element_footprint(ui);
            let (fx, fy, fw, fh) = rotated_footprint(x, y, w, h, ui.rotate());
            max_x = max_x.max(fx + fw);
            max_y = max_y.max(fy + fh);
        }

        if max_x == 0 && max_y == 0 {
            return (wx, wy, 200, 150);
        }
        (wx, wy, max_x.max(1), max_y.max(1))
    }

    /// Window rect tight around the content (v0.0.9): x/y = content's minimum corner,
    /// size = the content bounding box (max - min). All content stays inside the window
    /// (no left/top empty band, negative coordinates are included). This makes the
    /// overlay window as small as possible: per-frame DIB clearing + UpdateLayeredWindow
    /// upload + DWM composition all scale with the window area (~20x smaller in typical
    /// configs, measured CPU 0.46% → 0.06% @240fps).
    /// The content coordinates are NOT the screen position anymore: activate_config
    /// translates all content by (-x, -y) so screen pixels stay put.
    /// Returns (x, y, w, h). Returns (0, 0, 200, 150) when there's no content.
    pub fn tight_bounds(&self) -> (i32, i32, i32, i32) {
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;

        for r in &self.capture_regions {
            let d = &r.display;
            let (dw, dh) = self.region_logical_size(r);
            let (fx, fy, fw, fh) = rotated_footprint(d.x, d.y, dw, dh, d.rotate);
            min_x = min_x.min(fx);
            min_y = min_y.min(fy);
            max_x = max_x.max(fx + fw);
            max_y = max_y.max(fy + fh);
        }
        for ui in &self.custom_ui {
            let (x, y, w, h) = self.element_footprint(ui);
            let (fx, fy, fw, fh) = rotated_footprint(x, y, w, h, ui.rotate());
            min_x = min_x.min(fx);
            min_y = min_y.min(fy);
            max_x = max_x.max(fx + fw);
            max_y = max_y.max(fy + fh);
        }

        if max_x == i32::MIN {
            return (0, 0, 200, 150);
        }
        (min_x, min_y, (max_x - min_x).max(1), (max_y - min_y).max(1))
    }

    /// Compute the actual footprint rect (x, y, w, h) of a single custom_ui element.
    /// image without width/height → read the PNG intrinsic size; text without width/height → use the measured text size.
    pub fn element_footprint(&self, ui: &CustomUiElement) -> (i32, i32, i32, i32) {
        match ui {
            CustomUiElement::Image(e) => {
                let (x, y, w, h) = (e.x, e.y, e.width, e.height);
                if w > 0 && h > 0 {
                    return (x, y, w, h);
                }
                // PNG intrinsic size (1:1 display when width/height are unset)
                let png = Self::exe_png_dir().join(&e.path);
                let (iw, ih) = png_intrinsic_size(&png).unwrap_or((0, 0));
                (x, y, if w > 0 { w } else { iw as i32 }, if h > 0 { h } else { ih as i32 })
            }
            CustomUiElement::Text(t) => {
                // Text has no fixed size: always auto-fit to content + font size
                let (tw, th) = crate::custom_ui::measure_text_size(&t.content, t.font_size);
                (t.x, t.y, tw, th)
            }
            _ => ui.geometry(),
        }
    }

    /// PNG asset directory next to the exe (falls back to the current directory when missing)
    fn exe_png_dir() -> std::path::PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("PNG")))
            .unwrap_or_else(|| std::path::PathBuf::from("./PNG"))
    }

    /// Field validation and clamps
    fn validate(&mut self) {
        if let Some(o) = self.settings.global_opacity.as_mut() {
            *o = o.clamp(0.0, 1.0);
        }

        for region in &mut self.capture_regions {
            if let Some(o) = region.display.opacity.as_mut() {
                *o = o.clamp(0.0, 1.0);
            }
            if region.source.width == 0 {
                region.source.width = 60;
            }
            if region.source.height == 0 {
                region.source.height = 60;
            }
            // Migrate the legacy top-level z into display.z (0 also fine as the default order)
            if let Some(z) = region.z_order.take() {
                region.display.z_order = z;
            }
            // display width/height of 0 stays 0: it means "1:1, follow the source size"
        }
    }

    /// Logical (unrotated) display size of a region: explicit display w/h wins; 0 = 1:1 (source size)
    pub fn region_logical_size(&self, r: &CaptureRegion) -> (i32, i32) {
        let (dw, dh) = (r.display.width, r.display.height);
        if dw > 0 && dh > 0 {
            (dw, dh)
        } else {
            (r.source.width as i32, r.source.height as i32)
        }
    }

}

/// Bounding box of a (w, h) rect rotated by `deg` degrees clockwise around its center.
/// 0°/180° → (w, h); 90°/270° → (h, w); other angles → the general |...|·cos+|...|·sin formula.
pub fn rotated_bbox((w, h): (i32, i32), deg: i32) -> (i32, i32) {
    let deg = deg.rem_euclid(360);
    if deg == 0 || deg == 180 {
        return (w, h);
    }
    if deg == 90 || deg == 270 {
        return (h, w);
    }
    let rad = (deg as f32).to_radians();
    let (s, c) = rad.sin_cos();
    let bw = (w as f32 * c.abs() + h as f32 * s.abs()).round() as i32;
    let bh = (w as f32 * s.abs() + h as f32 * c.abs()).round() as i32;
    (bw.max(1), bh.max(1))
}

/// The visible footprint rect of a (x, y, w, h) rect rotated by deg around its center:
/// top-left moves to keep the bbox centered, size becomes the rotated bbox.
pub fn rotated_footprint(x: i32, y: i32, w: i32, h: i32, deg: i32) -> (i32, i32, i32, i32) {
    let (bw, bh) = rotated_bbox((w, h), deg);
    let (fx, fy) = (x + (w - bw) / 2, y + (h - bh) / 2);
    (fx, fy, bw, bh)
}

// ===== Config file scanning =====

/// Scan all region config files (*.yml, yaml support dropped) in the exe directory,
/// excluding settings.yml (the global settings file).
/// Returns the sorted list of filenames.
pub fn scan_config_files() -> Vec<String> {
    let mut files = Vec::new();
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(dir) = exe_path.parent() {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    if let Some(ext) = entry.path().extension() {
                        if ext == "yml" {
                            if let Some(name) = entry.file_name().to_str() {
                                // Exclude the global settings file
                                if name != SETTINGS_FILE {
                                    files.push(name.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    files.sort();
    files
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_content_accepts_bare_number() {
        let yaml = r#"
custom_ui:
  - type: text
    x: 10
    y: 20
    width: 120
    height: 40
    content: 13
"#;
        let cfg: Config = serde_yml::from_str(yaml).expect("bare number content should parse");
        let elems = cfg.custom_ui;
        let elem = elems.first().expect("should have one element");
        match elem {
            CustomUiElement::Text(t) => assert_eq!(t.content, "13"),
            other => panic!("expected a text element, got {:?}", other),
        }
    }

    #[test]
    fn text_content_accepts_string() {
        let yaml = r#"
custom_ui:
  - type: text
    x: 10
    y: 20
    width: 120
    height: 40
    content: hello
"#;
        let mut cfg: Config = serde_yml::from_str(yaml).expect("string content should parse");
        let elem = cfg.custom_ui.remove(0);
        match elem {
            CustomUiElement::Text(t) => assert_eq!(t.content, "hello"),
            other => panic!("expected a text element, got {:?}", other),
        }
    }

    #[test]
    fn opacity_inheritance_rules() {
        // 1) No opacity anywhere in the file → elements default to 1.0
        let cfg: Config = serde_yml::from_str("custom_ui:\n  - type: image\n    path: a.png\n").unwrap();
        assert_eq!(cfg.effective_global_opacity(), 1.0);
        match &cfg.custom_ui[0] {
            CustomUiElement::Image(i) => assert_eq!(cfg.effective_opacity(i.opacity), 1.0),
            _ => panic!(),
        }

        // 2) global_opacity set, element unset → use the global value
        let cfg: Config = serde_yml::from_str(
            "settings:\n  global_opacity: 0.7\ncustom_ui:\n  - type: image\n    path: a.png\n",
        )
        .unwrap();
        assert_eq!(cfg.effective_global_opacity(), 0.7);
        match &cfg.custom_ui[0] {
            CustomUiElement::Image(i) => assert_eq!(cfg.effective_opacity(i.opacity), 0.7),
            _ => panic!(),
        }

        // 3) Element has its own opacity → overrides the global value
        let cfg: Config = serde_yml::from_str(
            "settings:\n  global_opacity: 0.7\ncustom_ui:\n  - type: image\n    path: a.png\n    opacity: 0.3\n",
        )
        .unwrap();
        match &cfg.custom_ui[0] {
            CustomUiElement::Image(i) => assert_eq!(cfg.effective_opacity(i.opacity), 0.3),
            _ => panic!(),
        }

        // 4) Same rule for capture regions
        let cfg: Config = serde_yml::from_str(
            "settings:\n  global_opacity: 0.6\ncapture_regions:\n  - id: a\n    source: {x: 0, y: 0, width: 10, height: 10}\n    display: {x: 0, y: 0, width: 10, height: 10}\n",
        )
        .unwrap();
        assert_eq!(cfg.effective_opacity(cfg.capture_regions[0].display.opacity), 0.6);

// 5) image without width/height isn't serialized (native 1:1 image)
        let yaml = serde_yml::to_string(&Config {
            settings: Settings::default(),
            capture_regions: Vec::new(),
            custom_ui: vec![CustomUiElement::Image(ImageElement {
                path: "a.png".to_string(),
                x: 1,
                y: 2,
                width: 0,
                height: 0,
                opacity: None,
                z_order: 0,
                rotate: 0,
            })],
        })
        .unwrap();
        // Look only at the image element section (width/height under settings.window are window sizes, unrelated to elements)
        let img_section = yaml
            .split("custom_ui:")
            .nth(1)
            .unwrap_or_default()
            .to_string();
        assert!(!img_section.contains("width"), "image with default size must not write width: {}", yaml);
        assert!(!img_section.contains("height"), "image with default size must not write height: {}", yaml);
        assert!(!img_section.contains("opacity"), "no explicit opacity must not write the entry: {}", yaml);
    }

    #[test]
    fn compact_yaml_reads_short_names_and_flow_rect() {
        // New format: single-line flow rects + short w/h/z fields
        let yaml = r#"
settings:
  global_opacity: 0.65
  window: {x: 20, 'y': 20, w: 1972, h: 1187}
capture_regions:
- id: region_1
  source: {x: 1492, 'y': 461, w: 230, h: 176}
  display: {x: 1360, 'y': 475, w: 230, h: 176, opacity: 0.95}
  z: 3
custom_ui:
- type: text
  content: 13
  x: 1022
  'y': 1012
  w: 40
  h: 20
  font_size: 25
  z: 10
"#;
        let cfg: Config = serde_yml::from_str(yaml).expect("new format should parse");
        let reg = &cfg.capture_regions[0];
        assert_eq!(reg.source.width, 230);
        assert_eq!(reg.display.x, 1360);
        assert_eq!(reg.display.width, 230);
        assert_eq!(reg.z_order, Some(3), "legacy top-level z should still be read at parse time");
        match &cfg.custom_ui[0] {
            CustomUiElement::Text(t) => {
                assert_eq!(t.content, "13");
                assert_eq!(t.x, 1022);
                assert_eq!(t.y, 1012);
                assert_eq!(t.z_order, 10);
            }
            other => panic!("expected a text element, got {:?}", other),
        }
    }

    #[test]
    fn compact_yaml_writer_uses_short_names_and_inline_rect() {
        let cfg: Config = serde_yml::from_str(
            "settings:\n  global_opacity: 0.65\n  window: {x: 20, y: 20, w: 1972, h: 1187}\ncapture_regions:\n- id: a\n  source: {x: 0, y: 0, w: 10, h: 10}\n  display: {x: 1360, y: 475, w: 230, h: 176}\n  z: 3\n",
        )
        .unwrap();
        let yaml = cfg.to_compact_yaml().expect("compact serialization");
        // window anchor x/y must be kept (window position is auto-derived from the content bounding box; sizes are ignored)
        assert!(yaml.contains("window: {x: 20, 'y': 20"), "should output the window anchor: {}", yaml);
        // Geometry rects collapse to single-line flow (z always written: 0 → explicit level control)
        assert!(yaml.contains("source: {x: 0, 'y': 0, w: 10, h: 10}"), "{}", yaml);
        assert!(yaml.contains("display: {x: 1360, 'y': 475, w: 230, h: 176, z: 0}"), "{}", yaml);
        // Uncollapsed fields use short names
        assert!(yaml.contains("\n  z: 3"), "z_order should be output as the short name z: {}", yaml);
        // The output must still parse itself (round-trip)
        let cfg2: Config = serde_yml::from_str(&yaml).expect("compact output should parse back");
        assert_eq!(cfg2.capture_regions[0].display.x, cfg.capture_regions[0].display.x);
        assert_eq!(cfg2.capture_regions[0].display.y, cfg.capture_regions[0].display.y);
        assert_eq!(cfg2.capture_regions[0].display.width, cfg.capture_regions[0].display.width);
        assert_eq!(cfg2.capture_regions[0].display.height, cfg.capture_regions[0].display.height);
        assert_eq!(cfg2.capture_regions[0].z_order, cfg.capture_regions[0].z_order);
    }

    #[test]
    fn old_file_with_width_height_z_order_still_loads() {
        // Old format: block-form rects + width/height/z_order entries
        let yaml = r#"
settings:
  global_opacity: 1.0
  window:
    x: 20
    y: 20
    width: 100
    height: 200
capture_regions:
- id: old_1
  source:
    x: 1
    y: 2
    width: 10
    height: 11
  display:
    x: 3
    y: 4
    width: 12
    height: 13
  z_order: 5
"#;
        let cfg: Config = serde_yml::from_str(yaml).expect("old format should parse (alias compatibility)");
        assert_eq!(cfg.capture_regions[0].source.width, 10);
        assert_eq!(cfg.capture_regions[0].display.width, 12);
        assert_eq!(cfg.capture_regions[0].z_order, Some(5), "legacy top-level z_order read into the Option field");
        // Position = window anchor (20,20); size = max content extent (display 3,4,12,13 → 15x17), ignoring width/height
        assert_eq!(cfg.overlay_bounds(), (20, 20, 15, 17));
    }

    #[test]
    fn display_without_size_is_1x1_and_z_migrates_to_display_on_load() {
        // New format: a region may omit display w/h (1:1, follows the source size) and
        // carries z on display; the legacy top-level z must be merged into display.z by validate()
        let yaml = r#"
settings:
  global_opacity: 1.0
capture_regions:
- id: r1
  source: {x: 0, y: 0, w: 640, h: 360}
  display: {x: 30, y: 40, z: 7}
- id: r2
  source: {x: 0, y: 0, w: 32, h: 32}
  display: {x: 100, y: 100}
  z: 2
"#;
        let mut cfg: Config = serde_yml::from_str(yaml).expect("1:1 + z-on-display format should parse");
        // Before validate: display size stays 0, legacy z still on the Option field
        assert_eq!(cfg.capture_regions[0].display.width, 0);
        assert_eq!(cfg.capture_regions[0].display.z_order, 7);
        assert_eq!(cfg.capture_regions[1].display.z_order, 0);
        assert_eq!(cfg.capture_regions[1].z_order, Some(2));

        cfg.validate();
        // Legacy top-level z merged into display.z, Option cleared (never serialized again)
        assert_eq!(cfg.capture_regions[1].display.z_order, 2);
        assert_eq!(cfg.capture_regions[1].z_order, None);
        // 1:1 regions take their effective display size from the source
        assert_eq!(cfg.region_logical_size(&cfg.capture_regions[0]), (640, 360));
        assert_eq!(cfg.region_logical_size(&cfg.capture_regions[1]), (32, 32));
    }

    #[test]
    fn compact_writer_skips_zero_size_and_write_z_inside_display() {
        let mut cfg: Config = serde_yml::from_str(
            "settings:\n  global_opacity: 0.65\ncapture_regions:\n- id: a\n  source: {x: 0, y: 0, w: 640, h: 360}\n  display: {x: 30, y: 40, z: 7}\n",
        )
        .unwrap();
        cfg.validate();
        let yaml = cfg.to_compact_yaml().expect("compact serialization");
        // display 1:1 (0 size) → no w/h written; z stays inside the display flow
        assert!(yaml.contains("display: {x: 30, 'y': 40, z: 7}"), "1:1 display should omit w/h and keep z inside: {}", yaml);
        // No stray top-level z on the region (only inside the display flow)
        let region_section = yaml.split("capture_regions:").nth(1).unwrap_or_default().to_string();
        let region_block = region_section.split("custom_ui:").next().unwrap_or_default();
        assert!(!region_block.contains("\n  z: 7"), "z must live on display, not the region top level: {}", yaml);
    }

    #[test]
    fn rotate_folds_into_display_and_element_geometry() {
        // New format: element geometry (x, 'y', z) folds into one line with braces
        let mut cfg: Config = serde_yml::from_str(
            "settings:\n  global_opacity: 1.0\ncapture_regions:\n- id: a\n  source: {x: 0, y: 0, w: 640, h: 360}\n  display: {x: 30, y: 40, w: 230, h: 176, rotate: 90}\ncustom_ui:\n- type: image\n  path: X.png\n  x: 5\n  'y': 6\n  rotate: 45\n",
        )
        .unwrap();
        cfg.validate();
        // Effective display footprint = the rotated bounding box; logical size stays 230x176
        assert_eq!(crate::config::rotated_bbox(cfg.region_logical_size(&cfg.capture_regions[0]), cfg.capture_regions[0].display.rotate), (176, 230));
        assert_eq!(cfg.region_logical_size(&cfg.capture_regions[0]), (230, 176));
        let yaml = cfg.to_compact_yaml().expect("compact serialization");
        // rotate folds into the display flow, after w/h/z keys
        assert!(yaml.contains("display: {x: 30, 'y': 40, w: 230, h: 176, z: 0, rotate: 90}"), "{}", yaml);
        assert!(yaml.contains("  path: X.png\n  geometry: {x: 5, 'y': 6, z: 0, rotate: 45}"), "{}", yaml);
        // Round-trip through the real loader (geometry: {...} unnests back to flat keys)
        let cfg2: Config = parse_config_filtered(&yaml).expect("flow output should parse back");
        assert_eq!(cfg2.capture_regions[0].display.rotate, 90);
        match &cfg2.custom_ui[0] {
            CustomUiElement::Image(i) => assert_eq!(i.rotate, 45),
            other => panic!("expected an image element, got {:?}", other),
        }
    }

    #[test]
    fn text_and_image_geometry_fold_into_flow_with_braces() {
        // New format: text/image geometry (x, 'y', z) folds into one line with braces
        let mut cfg: Config = serde_yml::from_str(
            "settings:\n  global_opacity: 1.0\ncustom_ui:\n- type: text\n  content: '1'\n  font_size: 30\n  text_color: '#FFFFFF'\n  x: 12\n  'y': 34\n  z: 20\n- type: image\n  path: X.png\n  x: 5\n  'y': 6\n  z: 9\n",
        )
        .unwrap();
        cfg.validate();
        let yaml = cfg.to_compact_yaml().expect("compact serialization");
        // text: content/font_size/color stay on their own lines, geometry folds to one line under geometry:
        assert!(yaml.contains("  content: '1'\n  font_size: 30\n  text_color: '#FFFFFF'\n  geometry: {x: 12, 'y': 34, z: 20}"),
            "{}", yaml);
        // image: path stays, geometry folds to one line
        assert!(yaml.contains("  path: X.png\n  geometry: {x: 5, 'y': 6, z: 9}"), "{}", yaml);
        // Round-trip: the flow output must parse back identically (through the real loader,
        // which unnests geometry: {...} back to flat keys)
        let cfg2: Config = parse_config_filtered(&yaml).expect("flow output should parse back");
        match &cfg2.custom_ui[0] {
            CustomUiElement::Text(t) => {
                assert_eq!(t.x, 12);
                assert_eq!(t.y, 34);
                assert_eq!(t.z_order, 20);
            }
            other => panic!("expected a text element, got {:?}", other),
        }
        match &cfg2.custom_ui[1] {
            CustomUiElement::Image(i) => {
                assert_eq!(i.x, 5);
                assert_eq!(i.y, 6);
                assert_eq!(i.z_order, 9);
            }
            other => panic!("expected an image element, got {:?}", other),
        }
    }

    #[test]
    fn text_has_no_width_height_entries() {
        // Text is fully auto-sized: no w/h keys may appear in the serialized output
        let mut cfg: Config = serde_yml::from_str(
            "custom_ui:\n- type: text\n  content: hello\n  font_size: 30\n  x: 12\n  'y': 34\n",
        )
        .unwrap();
        cfg.validate();
        let yaml = cfg.to_compact_yaml().expect("compact serialization");
        let elem_section = yaml.split("custom_ui:").nth(1).unwrap_or_default().to_string();
        assert!(!elem_section.contains("width"), "text must not write width: {}", yaml);
        assert!(!elem_section.contains("height"), "text must not write height: {}", yaml);
        assert!(!elem_section.contains("w:"), "text must not write w: {}", yaml);
        assert!(!elem_section.contains("h:"), "text must not write h: {}", yaml);
    }

    #[test]
    fn tight_bounds_single_region_equals_footprint() {
        // One region at (100,100) 10x10 → the window is exactly the footprint
        let cfg: Config = serde_yml::from_str(
            "capture_regions:\n- id: a\n  source: {x: 0, y: 0, w: 10, h: 10}\n  display: {x: 100, y: 100, w: 10, h: 10}\n",
        )
        .unwrap();
        assert_eq!(cfg.tight_bounds(), (100, 100, 10, 10));
    }

    #[test]
    fn tight_bounds_spans_min_to_max_of_all_content() {
        // Regions at different corners + a UI element: window = min corner + bounding box
        let cfg: Config = serde_yml::from_str(
            "capture_regions:\n\
             - id: a\n  source: {x: 0, y: 0, w: 76, h: 92}\n  display: {x: 1597, y: 804, w: 76, h: 92}\n\
             - id: b\n  source: {x: 0, y: 0, w: 60, h: 60}\n  display: {x: 1863, y: 875, w: 60, h: 60}\n\
             custom_ui:\n\
             - type: image\n  path: X.png\n  x: 1617\n  'y': 878\n  w: 36\n  h: 36\n",
        )
        .unwrap();
        // min corner = (1597, 804); max = (1923, 935) → size = (326, 131)
        assert_eq!(cfg.tight_bounds(), (1597, 804, 326, 131));
    }

    #[test]
    fn tight_bounds_covers_negative_coordinates() {
        // Content with negative coordinates: the window must include them (no clipping)
        let cfg: Config = serde_yml::from_str(
            "capture_regions:\n- id: a\n  source: {x: 0, y: 0, w: 50, h: 50}\n  display: {x: -100, y: -200, w: 50, h: 50}\n",
        )
        .unwrap();
        assert_eq!(cfg.tight_bounds(), (-100, -200, 50, 50));
    }

    #[test]
    fn tight_bounds_empty_content_falls_back() {
        let cfg = Config::default();
        assert_eq!(cfg.tight_bounds(), (0, 0, 200, 150));
    }

    #[test]
    fn shift_xy_moves_all_element_kinds() {
        let mut cfg: Config = serde_yml::from_str(
            "custom_ui:\n\
             - type: frame\n  x: 100\n  'y': 100\n  w: 10\n  h: 10\n\
             - type: image\n  path: X.png\n  x: 200\n  'y': 300\n  w: 20\n  h: 20\n",
        )
        .unwrap();
        for ui in &mut cfg.custom_ui {
            ui.shift_xy(50, 25);
        }
        let geom: Vec<(i32, i32)> = cfg.custom_ui.iter().map(|u| {
            let (x, y, _, _) = u.geometry();
            (x, y)
        }).collect();
        assert_eq!(geom, vec![(50, 75), (150, 275)]);
    }
}
