//! Taskbar tray icon + right-click menu
//!
//! Creates the tray icon with Shell_NotifyIconW and receives tray messages through a
//! hidden message-only window. Right-click pops up the menu (fps selection / configs / quit),
//! menu clicks come back through WM_COMMAND, and a shared state carries "what the user picked"
//! to the main loop.

use std::sync::Mutex;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NOTIFYICONDATAW, NOTIFY_ICON_DATA_FLAGS, NOTIFY_ICON_MESSAGE, NIM_ADD, NIM_DELETE,
    NIF_ICON, NIF_MESSAGE, NIF_TIP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DefWindowProcW, DestroyMenu, DispatchMessageW,
    LoadIconW, PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassExW,
    SetForegroundWindow, SystemParametersInfoW, TrackPopupMenu, TranslateMessage, HMENU,
    MENU_ITEM_FLAGS, MSG, PM_REMOVE, TPM_LEFTALIGN, TPM_RIGHTALIGN, TPM_TOPALIGN,
    TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, TPM_VERTICAL, WINDOW_EX_STYLE, WNDCLASSEXW,
    WM_COMMAND, WM_QUIT, WM_RBUTTONUP, SPI_GETWORKAREA, WM_USER, WM_NULL,
};

use crate::config;
use crate::error::{AppError, AppResult};
use crate::i18n::t;

/// Custom tray callback message id (WM_USER + 1)
const WM_TRAYICON: u32 = WM_USER + 1;

/// HWND_MESSAGE: parent handle for creating a message-only window
const HWND_MESSAGE: HWND = HWND((-3isize) as *mut core::ffi::c_void);

/// Menu item ids (must be > 0 and not clash with system commands)
const MENU_FPS_30: usize = 1001;
const MENU_FPS_60: usize = 1002;
const MENU_FPS_120: usize = 1003;
const MENU_FPS_240: usize = 1004;
const MENU_NEW_CONFIG: usize = 1050;
const MENU_OPEN_DIR: usize = 1051;
const MENU_AUTO_SWITCH: usize = 1060;
const MENU_UNLOAD_CONFIG: usize = 1097;
const MENU_RELOAD_CONFIGS: usize = 1098;
const MENU_QUIT: usize = 1099;
/// config file menu item id base (2000, 2001, 2002...)
const MENU_CONFIG_BASE: usize = 2000;
/// edit config file menu item id base (3000, 3001, 3002...)
const MENU_EDIT_CONFIG_BASE: usize = 3000;
/// keyboard group config menu item id base (4000...)
const MENU_KBD_BASE: usize = 4000;
/// pad group config menu item id base (5000...)
const MENU_PAD_BASE: usize = 5000;

/// Input side
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSide {
    Keyboard,
    Controller,
}

/// Commands produced by menu clicks and passed to the main loop
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayCommand {
    SetFps(u32),
    /// create a new config file (enter the editor, blank start)
    EnterEditMode,
    /// edit an existing config file (enter the editor, load the given config)
    EditConfig(String),
    /// toggle a config file on/off (radio: only 1 manual config allowed)
    ToggleConfig(String),
    /// open the exe directory in explorer
    OpenProgramDir,
    /// reload all active configs from disk
    ReloadConfigs,
    /// unload all manually enabled configs (turn off the currently running one; configs are mutually exclusive)
    UnloadConfigs,
    /// master switch for pad/keyboard auto switch (occupies the single config slot, clears the manual set when on)
    ToggleAutoSwitch,
    /// toggle a config check inside one side (keyboard/pad) group
    ToggleInputConfig(InputSide, String),
    Quit,
}

/// Scanned config file list (filled when the menu is built)
static CONFIG_FILES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Shared state: the manual config set (enabled_configs), kept in sync by the main loop.
/// The tray menu reads this instead of re-loading settings.yml so the check marks match
/// the in-memory authority (settings.yml is now only written on exit).
static ENABLED_CONFIGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The main loop pushes the current manual set here whenever enabled_configs changes
pub fn set_manual_enabled_configs(names: Vec<String>) {
    if let Ok(mut g) = ENABLED_CONFIGS.lock() {
        *g = names;
    }
}

fn manual_enabled_shared() -> Vec<String> {
    ENABLED_CONFIGS.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Config name remembered at unload time; the unload menu item toggles into "load <name>"
/// while this is set and nothing is currently loaded. The load source is either a real
/// config file (UnloadSource::Config) or the pad/keyboard auto-switch (UnloadSource::Auto),
/// both treated as "one group of config".
static LAST_UNLOADED: Mutex<Option<UnloadSource>> = Mutex::new(None);

/// What was unloaded last: a config file, or the pad/keyboard auto-switch (as a whole)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnloadSource {
    Config(String),
    Auto,
}

pub fn set_last_unloaded(src: Option<UnloadSource>) {
    if let Ok(mut g) = LAST_UNLOADED.lock() {
        *g = src;
    }
}

pub fn last_unloaded_shared() -> Option<UnloadSource> {
    LAST_UNLOADED.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Shared state: the main loop polls this and takes pending commands
static PENDING_COMMAND: Mutex<Option<TrayCommand>> = Mutex::new(None);

/// Push a command into the shared state (called by the tray window's WndProc)
fn push_command(cmd: TrayCommand) {
    if let Ok(mut guard) = PENDING_COMMAND.lock() {
        *guard = Some(cmd);
    }
}

/// Called by the main loop: take the pending command (if any), then clear it
pub fn poll_command() -> Option<TrayCommand> {
    if let Ok(mut guard) = PENDING_COMMAND.lock() {
        guard.take()
    } else {
        None
    }
}

/// Tray controller. The icon is removed automatically on drop.
pub struct TrayIcon {
    hwnd: HWND,
    #[allow(dead_code)]
    menu: HMENU,
}

impl TrayIcon {
    /// Create the tray icon + hidden message-only window.
    /// `current_fps` is used to check the current entry in the menu.
    pub fn new(current_fps: u32) -> AppResult<Self> {
        unsafe {
            // 1. register and create a hidden message-only window (receives tray callbacks)
            let class_name = w!("APortalTray");
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(tray_wnd_proc),
                lpszClassName: class_name,
                ..Default::default()
            };
            RegisterClassExW(&wc);

            // create a message-only window with HWND_MESSAGE (invisible, receives no input)
            let hwnd = windows::Win32::UI::WindowsAndMessaging::CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name,
                w!(""),
                windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(0),
                0, 0, 0, 0,
                HWND_MESSAGE, None, None, None,
            )
            .map_err(|e| AppError::windows("CreateWindowExW tray", e))?;

            // 2. build the menu
            let menu = build_menu(current_fps)?;

            // 3. add the tray icon
            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: 1,
                uFlags: NOTIFY_ICON_DATA_FLAGS(NIF_MESSAGE.0 | NIF_ICON.0 | NIF_TIP.0),
                uCallbackMessage: WM_TRAYICON,
                hIcon: LoadIconW(
                    GetModuleHandleW(None).unwrap_or_default(),
                    // MAKEINTRESOURCE(1): 1 is the program icon resource id, not a dangling pointer
                    #[allow(clippy::manual_dangling_ptr)]
                    PCWSTR::from_raw(1usize as *const u16),
                )
                .map_err(|e| AppError::windows("LoadIconW", e))?,
                ..Default::default()
            };
            let tip: Vec<u16> = "APortal\0".encode_utf16().collect();
            for (i, &c) in tip.iter().enumerate().take(127) {
                nid.szTip[i] = c;
            }

            let ok = windows::Win32::UI::Shell::Shell_NotifyIconW(
                NOTIFY_ICON_MESSAGE(NIM_ADD.0), &nid,
            );
            if !ok.as_bool() {
                return Err(AppError::other("Shell_NotifyIconW NIM_ADD failed"));
            }

            log::info!("tray icon added");
            Ok(Self { hwnd, menu })
        }
    }

    /// Process the tray window's messages (including menu command dispatch). Returns false on quit.
    pub fn process_messages(&self) -> bool {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).into() {
                if msg.message == WM_QUIT {
                    return false;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            true
        }
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        unsafe {
            let nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd, uID: 1,
                uFlags: NOTIFY_ICON_DATA_FLAGS(NIF_MESSAGE.0 | NIF_ICON.0 | NIF_TIP.0),
                uCallbackMessage: WM_TRAYICON,
                ..Default::default()
            };
            let _ = windows::Win32::UI::Shell::Shell_NotifyIconW(
                NOTIFY_ICON_MESSAGE(NIM_DELETE.0), &nid,
            );
            let _ = DestroyMenu(self.menu);
            log::info!("tray icon removed");
        }
    }
}

// ===== shared state =====

static CURRENT_FPS: Mutex<u32> = Mutex::new(30);
static ACTIVE_CONFIGS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static AUTO_SWITCH_ON: Mutex<bool> = Mutex::new(false);
static KBD_CONFIGS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static PAD_CONFIGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn current_fps_shared() -> u32 { CURRENT_FPS.lock().map(|g| *g).unwrap_or(30) }
fn active_configs_shared() -> Vec<String> { ACTIVE_CONFIGS.lock().map(|g| g.clone()).unwrap_or_default() }
fn auto_switch_shared() -> bool { AUTO_SWITCH_ON.lock().map(|g| *g).unwrap_or(false) }
fn kbd_configs_shared() -> Vec<String> { KBD_CONFIGS.lock().map(|g| g.clone()).unwrap_or_default() }
fn pad_configs_shared() -> Vec<String> { PAD_CONFIGS.lock().map(|g| g.clone()).unwrap_or_default() }

pub fn set_current_fps(fps: u32) {
    if let Ok(mut g) = CURRENT_FPS.lock() { *g = fps; }
}
pub fn set_active_configs(configs: Vec<String>) {
    if let Ok(mut g) = ACTIVE_CONFIGS.lock() { *g = configs; }
}
/// Sync the auto-switch state and both input group lists (called at main loop start / settings change)
pub fn set_auto_enable(auto_on: bool, kbd: Vec<String>, pad: Vec<String>) {
    if let Ok(mut g) = AUTO_SWITCH_ON.lock() { *g = auto_on; }
    if let Ok(mut g) = KBD_CONFIGS.lock() { *g = kbd; }
    if let Ok(mut g) = PAD_CONFIGS.lock() { *g = pad; }
}

// ===== menu building =====

/// Build the right-click menu.
/// Layout: refresh rate (submenu) | new | edit existing (submenu) | config toggles | quit
/// On any failure the already-built menu (including submenus) is destroyed to avoid handle leaks
fn build_menu(current_fps: u32) -> AppResult<HMENU> {
    unsafe {
        let menu = CreatePopupMenu().map_err(|e| AppError::windows("CreatePopupMenu", e))?;
        let ret = build_menu_inner(menu, current_fps);
        if ret.is_err() {
            // submenus hang under `menu`, DestroyMenu recursively destroys them
            let _ = DestroyMenu(menu);
        }
        ret
    }
}

/// Text -> NUL-terminated UTF-16 (AppendMenuW copies the text synchronously, local borrow is safe)
fn text_utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn build_menu_inner(menu: HMENU, current_fps: u32) -> AppResult<HMENU> {
    // refresh rate submenu (title shows the current fps)
        let fps_submenu = CreatePopupMenu()
            .map_err(|e| AppError::windows("CreatePopupMenu fps_submenu", e))?;
        append_fps_item(fps_submenu, "30 FPS", MENU_FPS_30, current_fps == 30)?;
        append_fps_item(fps_submenu, "60 FPS", MENU_FPS_60, current_fps == 60)?;
        append_fps_item(fps_submenu, "120 FPS", MENU_FPS_120, current_fps == 120)?;
        append_fps_item(fps_submenu, "240 FPS", MENU_FPS_240, current_fps == 240)?;
        // MF_POPUP = 0x10; title dynamically shows the current fps
        let fps_title = format!("{}: {} FPS\0", t("refresh_rate"), current_fps);
        let fps_title_utf16: Vec<u16> = fps_title.encode_utf16().collect();
        let fps_pcstr = windows::core::PCWSTR::from_raw(fps_title_utf16.as_ptr());
        AppendMenuW(menu, MENU_ITEM_FLAGS(0x10), fps_submenu.0 as usize, fps_pcstr)?;

        // separator + new config
        AppendMenuW(menu, MENU_ITEM_FLAGS(0x800), 0, w!("\u{0}"))?; // MF_SEPARATOR
        let new_cfg = text_utf16(t("new_config"));
        AppendMenuW(menu, MENU_ITEM_FLAGS(0), MENU_NEW_CONFIG, PCWSTR::from_raw(new_cfg.as_ptr()))?;

        // config file list
        let config_files = scan_config_files();
        if !config_files.is_empty() {
            let active = active_configs_shared();
            // edit existing config — submenu
            let edit_submenu = CreatePopupMenu()
                .map_err(|e| AppError::windows("CreatePopupMenu edit_submenu", e))?;
            for (i, name) in config_files.iter().enumerate() {
                let mut label: Vec<u16> = name.encode_utf16().collect();
                label.push(0);
                let pcstr = windows::core::PCWSTR::from_raw(label.as_ptr());
                // check configs in use (MF_CHECKED)
                let flags = if active.contains(name) { 0x8u32 } else { 0u32 };
                let _ = AppendMenuW(edit_submenu, MENU_ITEM_FLAGS(flags), MENU_EDIT_CONFIG_BASE + i, pcstr);
            }
            let edit_label = text_utf16(t("edit_existing"));
            AppendMenuW(menu, MENU_ITEM_FLAGS(0x10), edit_submenu.0 as usize, PCWSTR::from_raw(edit_label.as_ptr()))?;
            // open the program folder (right below edit existing)
            let open_dir = text_utf16(t("open_folder"));
            AppendMenuW(menu, MENU_ITEM_FLAGS(0), MENU_OPEN_DIR, PCWSTR::from_raw(open_dir.as_ptr()))?;

            // separator + config toggle list
            AppendMenuW(menu, MENU_ITEM_FLAGS(0x800), 0, w!("\u{0}"))?; // MF_SEPARATOR
            // manual config area: radio (only 1 allowed); clicking while auto switch is on gives up the slot
            // Check marks come from the in-memory set (synced by the main loop), NOT a fresh disk read:
            // settings.yml is only written on exit, so the disk copy is the previous session's state.
            let auto_on = auto_switch_shared();
            let manual = manual_enabled_shared();
            for (i, name) in config_files.iter().enumerate() {
                let mut label: Vec<u16> = name.encode_utf16().collect();
                label.push(0);
                let pcstr = windows::core::PCWSTR::from_raw(label.as_ptr());
                let mut flags = 0u32;
                if manual.iter().any(|f| f == name) { flags |= 0x8u32; } // MF_CHECKED
                let _ = AppendMenuW(menu, MENU_ITEM_FLAGS(flags), MENU_CONFIG_BASE + i, pcstr);
            }

            // separator + pad/keyboard auto switch area
            AppendMenuW(menu, MENU_ITEM_FLAGS(0x800), 0, w!("\u{0}"))?;
            let auto_flags = if auto_on { 0x8u32 } else { 0u32 }; // MF_CHECKED
            let auto_switch_label = text_utf16(t("auto_switch"));
            AppendMenuW(menu, MENU_ITEM_FLAGS(auto_flags), MENU_AUTO_SWITCH, PCWSTR::from_raw(auto_switch_label.as_ptr()))?;

            let kbd = kbd_configs_shared();
            let pad = pad_configs_shared();
            // tree prefix: ├/└ mark these as sub-options of "auto switch"; pad first, keyboard second
            append_input_group_menu(menu, &format!("├ {}", t("pad")), &config_files, &pad, MENU_PAD_BASE, auto_on)?;
            append_input_group_menu(menu, &format!("└ {}", t("keyboard")), &config_files, &kbd, MENU_KBD_BASE, auto_on)?;

            // store for wnd_proc to query
            if let Ok(mut guard) = CONFIG_FILES.lock() {
                *guard = config_files;
            }
        }

        // separator + reload configs (its own group)
        AppendMenuW(menu, MENU_ITEM_FLAGS(0x800), 0, w!("\u{0}"))?; // MF_SEPARATOR
        let reload_label = text_utf16(t("reload_configs"));
        AppendMenuW(menu, MENU_ITEM_FLAGS(0), MENU_RELOAD_CONFIGS, PCWSTR::from_raw(reload_label.as_ptr()))?;

        // unload/reload toggle: "unload configs" while something is loaded; switches to
        // "load <last unloaded>" when nothing is loaded and a config was unloaded before
        // (fixed menu position, no need to find the previously enabled config in the list)
        let manual = manual_enabled_shared();
        let auto_on = auto_switch_shared();
        let last = last_unloaded_shared();
        let (unload_label, unload_flags): (String, u32) =
            if !manual.is_empty() || auto_on {
                (t("unload_config").to_string(), 0)
            } else if let Some(src) = &last {
                let target = match src {
                    UnloadSource::Config(name) => name.clone(),
                    UnloadSource::Auto => t("auto_switch").to_string(),
                };
                (format!("{} {target}", t("load_config")), 0)
            } else {
                (t("unload_config").to_string(), 0x1) // MF_GRAYED
            };
        let unload_label = text_utf16(&unload_label);
        AppendMenuW(menu, MENU_ITEM_FLAGS(unload_flags), MENU_UNLOAD_CONFIG, PCWSTR::from_raw(unload_label.as_ptr()))?;

        // separator + quit
        AppendMenuW(menu, MENU_ITEM_FLAGS(0x800), 0, w!("\u{0}"))?; // MF_SEPARATOR
        let quit_label = text_utf16(t("quit"));
        AppendMenuW(menu, MENU_ITEM_FLAGS(0), MENU_QUIT, PCWSTR::from_raw(quit_label.as_ptr()))?;

        Ok(menu)
}

/// Append a menu item with a check state
unsafe fn append_fps_item(menu: HMENU, label: &str, id: usize, checked: bool) -> AppResult<()> {
    let mut flags = 0u32;
    if checked { flags |= 0x8; } // MF_CHECKED
    let mut label_utf16: Vec<u16> = label.encode_utf16().collect();
    label_utf16.push(0);
    let pcstr = windows::core::PCWSTR::from_raw(label_utf16.as_ptr());
    AppendMenuW(menu, MENU_ITEM_FLAGS(flags), id, pcstr)?;
    Ok(())
}

/// Append a "side-config group" submenu: 「Keyboard: config1, config2 ▶」(whole row grayed when auto switch is off)
unsafe fn append_input_group_menu(
    menu: HMENU,
    side_name: &str,
    config_files: &[String],
    members: &[String],
    id_base: usize,
    auto_on: bool,
) -> AppResult<()> {
    let submenu = CreatePopupMenu()
        .map_err(|e| AppError::windows("CreatePopupMenu input_group_submenu", e))?;
    for (i, name) in config_files.iter().enumerate() {
        let mut label: Vec<u16> = name.encode_utf16().collect();
        label.push(0);
        let pcstr = windows::core::PCWSTR::from_raw(label.as_ptr());
        let flags = if members.contains(name) { 0x8u32 } else { 0u32 }; // MF_CHECKED
        let _ = AppendMenuW(submenu, MENU_ITEM_FLAGS(flags), id_base + i, pcstr);
    }
    let suffix = if members.is_empty() {
        t("unset").to_string()
    } else if members.len() <= 3 {
        members.join(", ")
    } else {
        t("n_more_groups")
            .replace("{}", &members[..3].join(", "))
            .replace("{}", &members.len().to_string())
    };
    let title = format!("{side_name}: {suffix}\0");
    let mut title_utf16: Vec<u16> = title.encode_utf16().collect();
    title_utf16.push(0);
    let pcstr = windows::core::PCWSTR::from_raw(title_utf16.as_ptr());
    // MF_POPUP(0x10) + (auto_on ? 0 : MF_GRAYED)
    let flags = if auto_on { 0x10u32 } else { 0x11u32 };
    AppendMenuW(menu, MENU_ITEM_FLAGS(flags), submenu.0 as usize, pcstr)?;
    Ok(())
}

/// Scan config files next to the exe (excluding settings.yml)
fn scan_config_files() -> Vec<String> {
    config::scan_config_files()
}

// ===== menu popup position (avoid the taskbar) =====

/// Get the work area (screen area minus the taskbar)
unsafe fn get_work_area() -> RECT {
    let mut rc = RECT::default();
    let _ = SystemParametersInfoW(
        SPI_GETWORKAREA, 0,
        Some(&mut rc as *mut RECT as *mut core::ffi::c_void),
        windows::Win32::UI::WindowsAndMessaging::SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
    );
    rc
}

/// Compute the menu popup position and alignment flags (u32), making sure it is not hidden
/// behind the taskbar. Returns (x, y, flags_u32) — the caller wraps flags into TRACK_POPUP_MENU_FLAGS.
unsafe fn compute_popup_pos(pt: &POINT) -> (i32, i32, u32) {
    let wa = get_work_area();
    let screen_w = windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
        windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN,
    );
    let screen_h = windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
        windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN,
    );

    // find which side the taskbar is on (via the work area vs screen difference)
    let taskbar_bottom = wa.bottom < screen_h && wa.top == 0;
    let taskbar_top = wa.top > 0 && wa.bottom == screen_h;
    let _taskbar_left = wa.left > 0 && wa.right == screen_w;
    let _taskbar_right = wa.right < screen_w && wa.left == 0;

    // X: clamp inside the work area, align left/right by position
    let pop_x = pt.x.clamp(wa.left, wa.right - 1);
    let h_flag = if pop_x > (wa.left + wa.right) / 2 {
        TPM_RIGHTALIGN.0 // mouse in the right half, menu pops to the left
    } else {
        TPM_LEFTALIGN.0
    };

    // Y: clamp inside the work area, choose up/down by taskbar position
    let pop_y = pt.y.clamp(wa.top, wa.bottom - 1);
    let v_flag = if taskbar_top {
        TPM_TOPALIGN.0 // taskbar on top, menu pops down
    } else if taskbar_bottom {
        TPM_BOTTOMALIGN.0 // taskbar at the bottom, menu pops up
    } else if pop_y < (wa.top + wa.bottom) / 2 {
        TPM_TOPALIGN.0
    } else {
        TPM_BOTTOMALIGN.0
    };

    (pop_x, pop_y, h_flag | v_flag)
}

// ===== window procedure =====

unsafe extern "system" fn tray_wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        m if m == WM_TRAYICON => {
            let mouse_msg = (lparam.0 & 0xffff) as u32;
            if mouse_msg == WM_RBUTTONUP {
                show_menu_inline(hwnd);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let menu_id = wparam.0 & 0xffff;
            match menu_id {
                MENU_FPS_30 => push_command(TrayCommand::SetFps(30)),
                MENU_FPS_60 => push_command(TrayCommand::SetFps(60)),
                MENU_FPS_120 => push_command(TrayCommand::SetFps(120)),
                MENU_FPS_240 => push_command(TrayCommand::SetFps(240)),
                MENU_NEW_CONFIG => {
                    log::info!("tray menu: user chose new config");
                    push_command(TrayCommand::EnterEditMode);
                }
                MENU_OPEN_DIR => {
                    log::info!("tray menu: open program folder");
                    push_command(TrayCommand::OpenProgramDir);
                }
                MENU_RELOAD_CONFIGS => {
                    log::info!("tray menu: reload configs");
                    push_command(TrayCommand::ReloadConfigs);
                }
                MENU_UNLOAD_CONFIG => {
                    log::info!("tray menu: unload configs");
                    push_command(TrayCommand::UnloadConfigs);
                }
                MENU_AUTO_SWITCH => {
                    log::info!("tray menu: toggle pad/keyboard auto switch");
                    push_command(TrayCommand::ToggleAutoSwitch);
                }
                MENU_QUIT => {
                    log::info!("tray menu: user chose quit");
                    push_command(TrayCommand::Quit);
                    PostQuitMessage(0);
                }
                id if (MENU_EDIT_CONFIG_BASE..MENU_EDIT_CONFIG_BASE + 1000).contains(&id) => {
                    let idx = id - MENU_EDIT_CONFIG_BASE;
                    if let Ok(guard) = CONFIG_FILES.lock() {
                        if let Some(filename) = guard.get(idx) {
                            log::info!("tray menu: edit config {}", filename);
                            push_command(TrayCommand::EditConfig(filename.clone()));
                        }
                    }
                }
                id if (MENU_CONFIG_BASE..MENU_CONFIG_BASE + 1000).contains(&id) => {
                    let idx = id - MENU_CONFIG_BASE;
                    if let Ok(guard) = CONFIG_FILES.lock() {
                        if let Some(filename) = guard.get(idx) {
                            log::info!("tray menu: toggle config {}", filename);
                            push_command(TrayCommand::ToggleConfig(filename.clone()));
                        }
                    }
                }
                id if (MENU_KBD_BASE..MENU_KBD_BASE + 1000).contains(&id) => {
                    let idx = id - MENU_KBD_BASE;
                    if let Ok(guard) = CONFIG_FILES.lock() {
                        if let Some(filename) = guard.get(idx) {
                            log::info!("tray menu: toggle keyboard group {}", filename);
                            push_command(TrayCommand::ToggleInputConfig(InputSide::Keyboard, filename.clone()));
                        }
                    }
                }
                id if (MENU_PAD_BASE..MENU_PAD_BASE + 1000).contains(&id) => {
                    let idx = id - MENU_PAD_BASE;
                    if let Ok(guard) = CONFIG_FILES.lock() {
                        if let Some(filename) = guard.get(idx) {
                            log::info!("tray menu: toggle pad group {}", filename);
                            push_command(TrayCommand::ToggleInputConfig(InputSide::Controller, filename.clone()));
                        }
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Pop the menu: detect the taskbar position and pop the opposite way to avoid being hidden
unsafe fn show_menu_inline(hwnd: HWND) {
    let mut pt = POINT { x: 0, y: 0 };
    let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
    let fps = current_fps_shared();
    match build_menu(fps) {
        Ok(menu) => {
            let activated = SetForegroundWindow(hwnd);
            if !activated.as_bool() {
                // common workarea trick: when foreground activation is refused, fake one
                // WM_NULL interaction so Windows thinks we have user interaction,
                // otherwise TrackPopupMenu closes immediately
                let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
            }
            let (pop_x, pop_y, flags_u32) = compute_popup_pos(&pt);
            let all_flags = flags_u32 | TPM_RIGHTBUTTON.0 | TPM_VERTICAL.0;
            let _ = TrackPopupMenu(
                menu,
                windows::Win32::UI::WindowsAndMessaging::TRACK_POPUP_MENU_FLAGS(all_flags),
                pop_x, pop_y, 0,
                hwnd, None,
            );
            let _ = DestroyMenu(menu);
        }
        Err(e) => log::error!("failed to build menu: {}", e),
    }
}
