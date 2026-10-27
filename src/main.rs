//! APortal main program
//!
//! After launch (double-click the exe, no black console): a semi-transparent overlay
//! window appears at a given screen position, showing multiple regions captured
//! from the desktop (skill bar re-arrangement); a tray icon appears, and right-clicking
//! the tray switches frame rate (30/60/120/240) or quits.
//!
//! Multiple configs run in parallel: enabled_configs in settings.yml controls each
//! config file's on/off state, toggled via the tray menu; several overlays can run at once.
//!
//! Core design: DXGI Desktop Duplication capture + multi-region cropping + UpdateLayeredWindow true transparency + click-through.

// No console window: double-clicking the exe won't pop a black box.
// For debugging, use `cargo run` to watch stderr, or check the log file.
#![windows_subsystem = "windows"]

mod capture;
mod config;
mod custom_ui;
mod dialog;
mod editor;
mod error;
mod i18n;
mod input;
mod log_init;
mod overlay;
mod single_instance;
mod tray;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use std::hint::spin_loop;

use windows::Win32::Foundation::HWND;

use capture::{DxgiCapture, RegionFrame, RegionRect};
use config::{Config, GlobalSettings};
use custom_ui::{composite_ui, prerender_ui, RenderedUi};
use error::{AppError, AppResult};
use input::{Input, InputMode};
use overlay::OverlayWindow;
use single_instance::InstanceGuard;
use tray::{poll_command, set_active_configs, set_auto_enable, set_current_fps, set_manual_enabled_configs, TrayCommand, TrayIcon};

/// Current target frame rate (variable, switched via tray menu). Atomic so the main loop reads and tray commands write.
static TARGET_FPS: AtomicU32 = AtomicU32::new(30);

/// After this many consecutive capture errors, back off instead of calling the
/// capture API every frame. The program keeps running and retries; a persistent
/// failure is reported to the user once per error streak.
const MAX_CONSECUTIVE_ERRORS: u32 = 30;

/// Persistent DDA rebuild failures (each retry already waits ~1.2s) before telling
/// the user the duplication session cannot be restored right now.
const DDA_REBUILD_NOTICE_AFTER: u32 = 5;

/// A runtime failure must persist this long before a modal box appears, so a
/// transient display-mode switch does not interrupt the user.
const RUNTIME_ERROR_NOTICE_AFTER: Duration = Duration::from_secs(2);

/// Activation failures already shown in this session (filename + error text).
/// Avoids a popup every time an auto-switch group retries a broken config.
static REPORTED_ACTIVATION_ERRORS: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
    std::sync::Mutex::new(None);

/// UI pre-render failures already shown (filename + element error).
static REPORTED_UI_ERRORS: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
    std::sync::Mutex::new(None);

/// One active config instance: config + its overlay window + pre-rendered UI
struct ActiveConfig {
    filename: String,
    cfg: Config,
    win: OverlayWindow,
    ui_elements: Vec<RenderedUi>,
    /// Overlay window size (auto-derived from the content bounding box)
    ow: i32,
    oh: i32,
    /// True until the static (capture-region-free) content has been presented once.
    /// A UI-only config has no capture frames to drive the render path, so it must
    /// be presented explicitly by the main loop.
    needs_static_present: bool,
    /// Start of the current continuous present-failure streak (None = healthy)
    present_fail_since: Option<Instant>,
    /// Whether this streak has already produced a user-visible warning
    present_fail_reported: bool,
}

/// Frame counters printed by the periodic stats
struct FrameStats {
    acquired: u64,
    skipped: u64,
    errors: u64,
    present_errors: u64,
    /// Loop iteration cost (all iterations, ms, cumulative)
    loop_ms: f64,
    /// Longest single iteration in the window
    loop_max_ms: f64,
    /// Number of loop iterations that reached the throttle section
    iter_n: u64,
    /// acquire_regions call cost (all iterations where it ran)
    acquire_ms: f64,
    /// Time spent inside AcquireNextFrame (waiting for a new desktop frame)
    acquire_wait_ms: f64,
    /// Time spent in GPU copy + readback after the frame arrived
    acquire_readback_ms: f64,
    acquire_n: u64,
    /// present() call cost (only frames actually presented)
    present_ms: f64,
    present_n: u64,
    /// DWM present interval (LARGE_INTEGER LastPresentTime delta from AcquireNextFrame) — the
    /// source frame rate; ~4ms on a 240Hz desktop, larger if DWM is the bottleneck
    dwm_gap_ms: f64,
    dwm_gap_n: u64,
}

impl FrameStats {
    fn new() -> Self {
        Self {
            acquired: 0,
            skipped: 0,
            errors: 0,
            present_errors: 0,
            loop_ms: 0.0,
            loop_max_ms: 0.0,
            iter_n: 0,
            acquire_ms: 0.0,
            acquire_wait_ms: 0.0,
            acquire_readback_ms: 0.0,
            acquire_n: 0,
            present_ms: 0.0,
            present_n: 0,
            dwm_gap_ms: 0.0,
            dwm_gap_n: 0,
        }
    }
    fn total_problems(&self) -> u64 {
        self.errors + self.present_errors
    }
}

fn main() {
    if let Err(e) = run() {
        log::error!("Fatal error: {}", e);
        dialog::error(
            HWND(std::ptr::null_mut()),
            i18n::t("startup_failed"),
            &e.to_string(),
        );
    }
}

fn run() -> AppResult<()> {
    // 0. DPI 感知声明必须先于任何窗口/捕获创建: 否则 DDA 返回缩放后的虚拟分辨率
    // (如 3440x1440@175% -> 1966x823), 源区域越界导致黑框
    let dpi_v2_ok = unsafe {
        windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
        .is_ok()
    };
    // V2 不可用(如旧系统)时回退到 per-monitor V1, 而不是放弃 DPI 感知
    if !dpi_v2_ok {
        unsafe {
            let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwareness(
                windows::Win32::UI::HiDpi::PROCESS_PER_MONITOR_DPI_AWARE,
            );
        }
    }

    // 0.1 Single instance: acquire before touching settings/log/capture. A second
    // copy would stack overlays and overwrite settings.yml on exit. On a duplicate
    // start, read only the language preference (side-effect-free) for the dialog.
    let _instance = match InstanceGuard::acquire() {
        Some(guard) => guard,
        None => {
            i18n::set_lang(&GlobalSettings::load().lang);
            i18n::load_for_exe();
            log::warn!("Another APortal instance is already running; exiting");
            dialog::info(
                HWND(std::ptr::null_mut()),
                i18n::t("already_running"),
                i18n::t("already_running_hint"),
            );
            return Ok(());
        }
    };

    // 0. Global settings (settings.yml: FPS memory + config toggles) — loaded BEFORE log init
    // because the log_enabled switch decides whether any log file is written at all.
    // A broken file is preserved and reported instead of silently reset to defaults.
    let settings_load = GlobalSettings::load_with_status();
    let settings_writable = settings_load.can_write();
    let config::SettingsLoad {
        settings,
        error: settings_error,
        backup: settings_backup,
    } = settings_load;
    let mut global = settings;
    log_init::init(global.log_enabled);
    log::info!("=== APortal v{} started ===", env!("CARGO_PKG_VERSION"));
    log::info!(
        "DPI awareness: {}",
        if dpi_v2_ok { "per-monitor V2" } else { "per-monitor V1 (fallback)" }
    );

    let initial_fps = global.fps;
    TARGET_FPS.store(initial_fps, Ordering::Relaxed);
    log::info!("Global frame rate: {}fps", initial_fps);

    // 0.4 UI language (settings.yml lang field, default zh)
    i18n::set_lang(&global.lang);
    log::info!("UI language: {:?}", i18n::lang());
    // 0.4.1 Load language file (lang\<lang>.yml next to the exe; t() falls back to the English key when missing)
    i18n::load_for_exe();

    // 0.4.2 Report a broken settings.yml now that the language file and log are ready.
    if let Some(err) = settings_error {
        let text = match settings_backup {
            Some(path) => i18n::t("settings_backup_hint")
                .replacen("{}", &path.display().to_string(), 1)
                .replacen("{}", &err, 1),
            None => i18n::t("settings_no_write_hint").replace("{}", &err),
        };
        log::error!("Settings file problem: {}", err);
        dialog::error(
            HWND(std::ptr::null_mut()),
            i18n::t("settings_load_failed"),
            &text,
        );
    }

    // 0.5 Prune settings references to deleted/legacy .yaml configs (prevent stale memory).
    // In-memory only: no disk write here; settings.yml is written once on exit.
    if global.prune_missing_refs() {
        log::info!("Settings had stale config references (file deleted or legacy .yaml), cleaned in memory");
    }

    // 1. Capturer
    let mut cap = DxgiCapture::new()?;
    log::info!("Desktop resolution: {}x{}", cap.width, cap.height);
    let mut screen_w = cap.width;
    let mut screen_h = cap.height;

    // Enable only configs turned on in settings.yml; also runs fine with none (empty overlay)
    let mut actives: Vec<ActiveConfig> = Vec::new();
    let all_configs = config::scan_config_files();
    let mut activation_errors: Vec<(String, String)> = Vec::new();
    for filename in &all_configs {
        if global.is_enabled(filename) {
            match activate_config(filename) {
                Ok(ac) => {
                    log::info!("Config activated: {}", filename);
                    actives.push(ac);
                }
                Err(e) => {
                    log::error!("Failed to activate config {}: {}", filename, e);
                    activation_errors.push((filename.clone(), e.to_string()));
                }
            }
        }
    }
    sync_tray_active_list(&actives);
    log::info!("Active configs: {}", actives.len());
    if !activation_errors.is_empty() {
        // One dialog for all startup failures instead of one modal per config.
        let details: Vec<String> = activation_errors
            .iter()
            .map(|(name, err)| format!("{}: {}", name, err))
            .collect();
        for (name, err) in &activation_errors {
            mark_activation_error(name, err);
        }
        dialog::error(
            HWND(std::ptr::null_mut()),
            i18n::t("config_activate_failed"),
            &format!(
                "{}\n\n{}",
                i18n::t("config_activate_hint"),
                details.join("\n")
            ),
        );
    }

    // 2.5 Push the manual config set so the tray menu check marks match in-memory truth
    // (settings.yml is written once on exit, so the tray must not trust the disk copy)
    set_manual_enabled_configs(global.enabled_configs.clone());

    // 3. Tray icon
    let tray = TrayIcon::new(initial_fps)?;
    set_current_fps(initial_fps);

    // 3.5 Input detector (controller/keyboard auto-switch)
    let mut input_detect = Input::new(
        global.input_poll_interval_ms,
        global.input_track_mouse,
    );
    sync_input_groups_to_tray(&global);
    if global.input_auto_switch && input_detect.poll() {
        // Force a check on the first frame so the config group for the current mode applies at startup
        apply_input_mode(&mut actives, &global, input_detect.mode());
        sync_tray_active_list(&actives);
    }

    // 4. Main loop state
    let mut stats_window_start = Instant::now();
    let mut stats = FrameStats::new();
    let mut total_windows: u64 = 0;
    let mut current_fps = initial_fps;
    let mut frame_duration = fps_to_duration(current_fps);
    let mut consecutive_errors: u32 = 0;
    let mut capture_error_since: Option<Instant> = None;
    let mut capture_error_reported = false;
    // DDA session rebuild failures (ACCESS_LOST) already surfaced to the user
    let mut dda_rebuild_reported = false;
    // DWM present-timestamp tracking (source frame interval diagnostic)
    let mut qpc_freq_raw: i64 = 0;
    unsafe {
        let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(
            &mut qpc_freq_raw,
        );
    }
    let qpc_freq = qpc_freq_raw.max(1) as f64;
    let mut last_present_tick_prev: Option<i64> = None;
    // Periodic settings flush timer (settings.yml is normally written once on exit; this is a
    // safety net so an abnormal termination loses at most SETTINGS_FLUSH_INTERVAL of changes)
    let mut last_settings_save = Instant::now();

    log::info!("--- Entering main loop ---");

    'main: loop {
        // (A) Message pump
        if !tray.process_messages() {
            log::info!("Quit message received");
            break 'main;
        }

        // (B) Handle tray commands
        while let Some(cmd) = poll_command() {
            match cmd {
                TrayCommand::SetFps(new_fps) => {
                    if new_fps != current_fps {
                        log::info!("Frame rate switch: {} -> {}fps", current_fps, new_fps);
                        current_fps = new_fps;
                        frame_duration = fps_to_duration(current_fps);
                        set_current_fps(current_fps);
                        TARGET_FPS.store(current_fps, Ordering::Relaxed);
                        global.fps = current_fps;
                        stats = FrameStats::new();
                        stats_window_start = Instant::now();
                    }
                }
                TrayCommand::EnterEditMode => {
                    log::info!("Creating a new config file");
                    let base_cfg = Config::default();
                    match editor::run_editor(&base_cfg, "") {
                        // run_editor writes the file itself and returns only after a
                        // successful save; a write error keeps the editor open.
                        Ok(Some(filename)) => {
                            finish_editor_save(
                                &mut global,
                                &mut actives,
                                input_detect.mode(),
                                &filename,
                            );
                        }
                        Ok(None) => log::info!("Edit cancelled"),
                        Err(e) => log::error!("Editor error: {}", e),
                    }
                    // The editor stages prefs (snap distance/gap/nudge step) in memory;
                    // apply them so they survive until the single save on exit.
                    apply_editor_prefs(&mut global);
                }
                TrayCommand::EditConfig(filename) => {
                    log::info!("Editing existing config: {}", filename);
                    // Do NOT fall back to a blank Config on load failure: saving that
                    // blank document would overwrite the original (possibly recoverable)
                    // file. Abort the edit and tell the user instead.
                    let base_cfg = match Config::load_from(&filename) {
                        Ok(cfg) => cfg,
                        Err(e) => {
                            log::error!(
                                "Failed to load config {} (editing aborted): {}",
                                filename, e
                            );
                            dialog::error(
                                HWND(std::ptr::null_mut()),
                                i18n::t("load_failed"),
                                &format!("{}\n\n{}", filename, e),
                            );
                            continue 'main;
                        }
                    };
                    // Strip the .yml suffix as the initial filename (yaml support dropped).
                    // Scan accepts .YML too, so strip case-insensitively; is_char_boundary
                    // keeps a malformed non-ASCII name from panicking on slicing.
                    let stem = match filename.len().checked_sub(4) {
                        Some(i)
                            if filename.is_char_boundary(i)
                                && filename[i..].eq_ignore_ascii_case(".yml") =>
                        {
                            &filename[..i]
                        }
                        _ => filename.as_str(),
                    };
                    match editor::run_editor(&base_cfg, stem) {
                        // run_editor writes the file itself and returns only after a
                        // successful save; a write error keeps the editor open.
                        Ok(Some(saved_name)) => {
                            finish_editor_save(
                                &mut global,
                                &mut actives,
                                input_detect.mode(),
                                &saved_name,
                            );
                        }
                        Ok(None) => log::info!("Edit cancelled"),
                        Err(e) => log::error!("Editor error: {}", e),
                    }
                    // The editor stages prefs (snap distance/gap/nudge step) in memory;
                    // apply them so they survive until the single save on exit.
                    apply_editor_prefs(&mut global);
                }
                TrayCommand::ToggleConfig(filename) => {
                    // Single-select model: at most 1 config in the manual set; greyed out while auto-switch is on, so this is unreachable
                    if global.is_enabled(&filename) {
                        // Disable itself: remove from the manual set; if it's still in the current auto group keep the window (defensive, normally unreachable)
                        global.set_enabled(&filename, false);
                        let in_group = global.input_auto_switch
                            && if_auto_group_has(&global, input_detect.mode(), &filename);
                        if !in_group {
                            if let Some(pos) = actives.iter().position(|a| a.filename == filename) {
                                actives.remove(pos); // OverlayWindow Drop
                            }
                        }
                        log::info!("Config removed from the manual set: {}", filename);
                    } else {
                        // Single-select enable: auto-switch yields + the manual set keeps only this config
                        global.enabled_configs = vec![filename.clone()];
                        global.input_auto_switch = false;
                        sync_input_groups_to_tray(&global);
                        apply_input_mode(&mut actives, &global, input_detect.mode());
                        log::info!("Config enabled (single-select): {}", filename);
                    }
                    set_manual_enabled_configs(global.enabled_configs.clone());
                    // Manual toggle = a different config state; stale unload memory must not linger
                    tray::set_last_unloaded(None);
                    sync_tray_active_list(&actives);
                }
                TrayCommand::UnloadConfigs => {
                    // Toggle: unload everything (manual set + pad/keyboard auto-switch) and
                    // remember what was unloaded; when nothing is loaded and a source was
                    // remembered, reload it instead. The menu item label switches between
                    // "unload configs" and "load <name | auto-switch>" accordingly.
                    if !global.enabled_configs.is_empty() || global.input_auto_switch {
                        // Remember the manual config if one was running, otherwise the
                        // auto-switch (which was occupying the only slot)
                        let last = global
                            .enabled_configs
                            .first()
                            .cloned()
                            .map(tray::UnloadSource::Config)
                            .or(Some(tray::UnloadSource::Auto));
                        global.enabled_configs.clear();
                        global.input_auto_switch = false;
                        set_manual_enabled_configs(Vec::new());
                        tray::set_last_unloaded(last);
                        sync_input_groups_to_tray(&global);
                        // apply_input_mode converges actives with the expected set (empty now)
                        apply_input_mode(&mut actives, &global, input_detect.mode());
                        log::info!("All configs unloaded (manual set cleared, auto-switch off)");
                    } else if let Some(src) = tray::last_unloaded_shared() {
                        match src {
                            tray::UnloadSource::Config(name) => {
                                // Reload the config remembered at unload time (single-select model)
                                global.enabled_configs = vec![name.clone()];
                                global.input_auto_switch = false;
                                log::info!("Config reloaded from unload memory: {}", name);
                            }
                            tray::UnloadSource::Auto => {
                                // Reload the auto-switch: takes the only slot, clears the
                                // manual set; the input groups were persisted in settings
                                global.enabled_configs.clear();
                                global.input_auto_switch = true;
                                log::info!("Auto-switch reloaded from unload memory (groups were persisted in settings.yml)");
                            }
                        }
                        set_manual_enabled_configs(global.enabled_configs.clone());
                        tray::set_last_unloaded(None);
                        sync_input_groups_to_tray(&global);
                        apply_input_mode(&mut actives, &global, input_detect.mode());
                    }
                    sync_tray_active_list(&actives);
                }
                TrayCommand::Quit => {
                    log::info!("Quit command received");
                    break 'main;
                }
                TrayCommand::OpenProgramDir => {
                    log::info!("Opening the program directory");
                    if let Ok(exe) = std::env::current_exe() {
                        if let Some(dir) = exe.parent() {
                            let dir_str = dir.to_string_lossy().into_owned();
                            let _ = std::process::Command::new("explorer.exe")
                                .arg(&dir_str)
                                .spawn();
                            log::info!("Opened in Explorer: {}", dir_str);
                        }
                    }
                }
                TrayCommand::ReloadConfigs => {
                    log::info!("Reloading all active configs: {}", actives.len());
                    // Rebuild one by one from new content: unload old windows, reload from the disk configs
                    let names: Vec<String> = actives.iter().map(|a| a.filename.clone()).collect();
                    let mut new_actives: Vec<ActiveConfig> = Vec::with_capacity(names.len());
                    for name in &names {
                        match activate_config(name) {
                            Ok(ac) => {
                                log::info!("Reloaded: {}", name);
                                new_actives.push(ac);
                            }
                            Err(e) => report_activation_failure(name, &e),
                        }
                    }
                    // Old actives drop here (OverlayWindow Drop destroys the window)
                    actives = new_actives;
                    sync_tray_active_list(&actives);
                    log::info!("Config reload done, active: {}", actives.len());
                }
                TrayCommand::ToggleAutoSwitch => {
                    global.input_auto_switch = !global.input_auto_switch;
                    if global.input_auto_switch {
                        // Auto-switch takes the only slot: clear the manual set, keep the group contents
                        global.enabled_configs.clear();
                        set_manual_enabled_configs(Vec::new());
                        tray::set_last_unloaded(None); // stale unload memory must not linger
                        log::info!("Controller/keyboard auto-switch: on (manual set cleared), current mode={:?}", input_detect.mode());
                    } else {
                        log::info!("Controller/keyboard auto-switch: off (auto groups no longer participate, back to the manual set)");
                    }
                    sync_input_groups_to_tray(&global);
                    // Either way the auto group must enter/leave actives (on = merge in, off = remove)
                    apply_input_mode(&mut actives, &global, input_detect.mode());
                    sync_tray_active_list(&actives);
                }
                TrayCommand::ToggleInputConfig(side, filename) => {
                    let track_side = match side {
                        tray::InputSide::Keyboard => &mut global.keyboard_configs,
                        tray::InputSide::Controller => &mut global.controller_configs,
                    };
                    if track_side.contains(&filename) {
                        track_side.retain(|f| f != &filename);
                    } else {
                        track_side.push(filename.clone());
                    }
                    sync_input_groups_to_tray(&global);
                    log::info!("Input group updated {:?}: {} (keyboard:{} controller:{})", side, filename,
                        global.keyboard_configs.len(), global.controller_configs.len());
                    // If auto-switch is on, apply the latest groups immediately
                    if global.input_auto_switch {
                        apply_input_mode(&mut actives, &global, input_detect.mode());
                        sync_tray_active_list(&actives);
                    }
                }
            }
        }

        let frame_start = Instant::now();

        // (B2) Input mode polling: when auto-switch is on, a mode change swaps the config group (settings.yml not written)
        if global.input_auto_switch && input_detect.poll() {
            apply_input_mode(&mut actives, &global, input_detect.mode());
            sync_tray_active_list(&actives);
        }

        // (B3) Desktop resolution change. The DDA session reports the new size after
        // its ACCESS_LOST rebuild. Overlay content uses user-chosen absolute screen
        // coordinates, so rebuild the active windows (do not remap their geometry)
        // and warn once about configs whose capture regions no longer fit.
        if cap.width != screen_w || cap.height != screen_h {
            let old = (screen_w, screen_h);
            screen_w = cap.width;
            screen_h = cap.height;
            handle_resolution_change(&mut actives, old, (screen_w, screen_h));
        }

        // (C) Collect all active configs' source regions; crop and read back on the GPU in one pass
        if actives.is_empty() {
            // With no active config there's nothing to poll at frame rate; sleep longer to save CPU (tray commands still respond promptly)
            std::thread::sleep(Duration::from_millis(100));
        } else {
            // Collect all source regions (flat list, in config order)
            let mut all_regions: Vec<RegionRect> = Vec::new();
            let mut region_offsets: Vec<usize> = Vec::with_capacity(actives.len());
            for ac in &actives {
                region_offsets.push(all_regions.len());
                for r in &ac.cfg.capture_regions {
                    all_regions.push(RegionRect {
                        x: r.source.x,
                        y: r.source.y,
                        w: r.source.width,
                        h: r.source.height,
                    });
                }
            }

            let acquire_t0 = Instant::now();
            match cap.acquire_regions(0, &all_regions) {
                Ok(Some(frames)) => {
                    consecutive_errors = 0;
                    capture_error_since = None;
                    capture_error_reported = false;
                    dda_rebuild_reported = false;
                    stats.acquire_ms += acquire_t0.elapsed().as_secs_f64() * 1000.0;
                    stats.acquire_n += 1;
                    let num_actives = actives.len();
                    for (ac_idx, ac) in actives.iter_mut().enumerate() {
                        let start = region_offsets[ac_idx];
                        let end = if ac_idx + 1 < num_actives {
                            region_offsets[ac_idx + 1]
                        } else {
                            frames.len()
                        };
                        let ac_frames = &frames[start..end];
                        let is_static = ac.cfg.capture_regions.is_empty();

                        // A UI-only config is presented once; capture configs follow
                        // every acquired frame. If this is the first present for a
                        // static config, fall through so its UI actually appears even
                        // when another config supplied the capture frame.
                        if is_static && !ac.needs_static_present {
                            continue;
                        }

                        let present_t0 = Instant::now();
                        match render_config(ac, ac_frames) {
                            Ok(()) => {
                                stats.present_ms += present_t0.elapsed().as_secs_f64() * 1000.0;
                                stats.present_n += 1;
                                stats.acquired += 1;
                                ac.present_fail_since = None;
                                ac.present_fail_reported = false;
                                if is_static {
                                    ac.needs_static_present = false;
                                }
                            }
                            Err(e) => note_present_failure(ac, &mut stats, &e),
                        }
                    }
                    // DWM source interval: delta of the frame's present timestamp vs the previous frame
                    if let Some(t) = cap.last_present_tick {
                        if let Some(prev) = last_present_tick_prev {
                            let ticks = (t - prev) as f64;
                            if ticks > 0.0 {
                                let gap_ms = ticks * 1000.0 / qpc_freq;
                                if gap_ms < 1000.0 {
                                    stats.dwm_gap_ms += gap_ms;
                                    stats.dwm_gap_n += 1;
                                }
                            }
                        }
                        last_present_tick_prev = Some(t);
                    }
                    stats.acquire_wait_ms += cap.last_wait_ms;
                    stats.acquire_readback_ms += cap.last_readback_ms;
                }
                Ok(None) => {
                    stats.skipped += 1;
                    // No capture frame: this is the normal path for a UI-only config
                    // (and for a static desktop). Present its content once instead of
                    // waiting forever for Ok(Some(frames)).
                    consecutive_errors = 0;
                    capture_error_since = None;
                    capture_error_reported = false;
                    // ACCESS_LOST recovery failures also return Ok(None), so the generic
                    // capture-error path never sees them. Report a persistent rebuild
                    // failure once per streak (e.g. a game stuck in exclusive fullscreen).
                    let rebuild_failures = cap.rebuild_failures();
                    if rebuild_failures == 0 {
                        dda_rebuild_reported = false;
                    } else if rebuild_failures >= DDA_REBUILD_NOTICE_AFTER
                        && !dda_rebuild_reported
                    {
                        dda_rebuild_reported = true;
                        log::error!(
                            "DDA session rebuild failed {} times in a row, overlay is frozen",
                            rebuild_failures
                        );
                        dialog::error(
                            HWND(std::ptr::null_mut()),
                            i18n::t("capture_failed"),
                            i18n::t("capture_rebuild_hint"),
                        );
                    }
                    present_static_configs(&mut actives, &mut stats);
                }
                Err(e) => {
                    log::warn!("Frame capture failed: {}", e);
                    stats.errors += 1;
                    consecutive_errors = consecutive_errors.saturating_add(1);
                    let since = *capture_error_since.get_or_insert_with(Instant::now);
                    if !capture_error_reported
                        && since.elapsed() >= RUNTIME_ERROR_NOTICE_AFTER
                    {
                        capture_error_reported = true;
                        log::error!(
                            "Frame capture has failed continuously for {}s: {}",
                            since.elapsed().as_secs(),
                            e
                        );
                        dialog::error(
                            HWND(std::ptr::null_mut()),
                            i18n::t("capture_failed"),
                            &format!("{}\n\n{}", i18n::t("capture_retry_hint"), e),
                        );
                    }
                    // UI-only configs do not depend on capture; keep them visible even
                    // while the duplication session is recovering.
                    present_static_configs(&mut actives, &mut stats);
                    if consecutive_errors == MAX_CONSECUTIVE_ERRORS {
                        log::error!(
                            "{} consecutive frame capture failures, backing off and retrying",
                            consecutive_errors
                        );
                    }
                    if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                        // Never exit on capture errors: a display-mode switch, UAC
                        // desktop or driver reset can take seconds to recover. Keep the
                        // overlay/tray alive and retry at a lower rate.
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        }

        // (D) Per-minute stats (fewer disk writes: 1 line per second -> 1 line per 60 seconds)
        if stats_window_start.elapsed() >= Duration::from_secs(60) {
            total_windows += 1;
            let fmt = |sum: f64, n: u64| {
                if n > 0 { format!("{:.1}", sum / n as f64) } else { "-".to_string() }
            };
            log::info!(
                "[{:>3}m] fps={} active={} acquired={} skipped={} capt_err={} present_err={} \
                 loop={}ms(max {}ms) acquire={}ms(wait {} + readback {}) present={}ms dwm_gap={}ms",
                total_windows, current_fps, actives.len(),
                stats.acquired, stats.skipped, stats.errors, stats.present_errors,
                fmt(stats.loop_ms, stats.iter_n),
                stats.loop_max_ms,
                fmt(stats.acquire_ms, stats.acquire_n),
                fmt(stats.acquire_wait_ms, stats.acquire_n),
                fmt(stats.acquire_readback_ms, stats.acquire_n),
                fmt(stats.present_ms, stats.present_n),
                fmt(stats.dwm_gap_ms, stats.dwm_gap_n)
            );
            if stats.total_problems() > 5 {
                log::warn!("!! {} errors this minute, that's a lot", stats.total_problems());
            }
            stats = FrameStats::new();
            stats_window_start = Instant::now();
        }

        // (D2) Periodic settings flush: write the in-memory settings once every 5 minutes so a
        // crash or force-kill only loses the last ≤5 minutes of changes (normal exit still saves).
        const SETTINGS_FLUSH_INTERVAL: Duration = Duration::from_secs(300);
        if last_settings_save.elapsed() >= SETTINGS_FLUSH_INTERVAL {
            if settings_writable {
                if let Err(e) = global.save() {
                    log::error!("Periodic settings save failed: {}", e);
                }
            } else {
                log::warn!("Skipping periodic settings save: settings.yml was not loaded/preserved");
            }
            last_settings_save = Instant::now();
            log::debug!("Periodic settings flush (every 5 min)");
        }

        // (E) Throttling — pure sleep at low frame rates saves CPU; spin at high frame rates only compensates for sleep granularity
        // Old code spun a fixed ≤1ms per frame: at 120fps that's ≈12% core busy-spin, an obvious waste;
        // At 30/60fps frame intervals are ≥16ms so jitter is irrelevant, just pure sleep; at high frame rates the spin window is halved.
        let elapsed = frame_start.elapsed();
        let loop_ms = elapsed.as_secs_f64() * 1000.0;
        stats.loop_ms += loop_ms;
        stats.loop_max_ms = stats.loop_max_ms.max(loop_ms);
        stats.iter_n += 1;
        if elapsed < frame_duration {
            let remaining = frame_duration - elapsed;
            if current_fps <= 60 {
                std::thread::sleep(remaining);
            } else if remaining > Duration::from_micros(500) {
                std::thread::sleep(remaining - Duration::from_micros(500));
                while frame_start.elapsed() < frame_duration {
                    spin_loop();
                }
            } else {
                while frame_start.elapsed() < frame_duration {
                    spin_loop();
                }
            }
        }
    }

    // Single save on exit: all in-memory mutations (fps/configs/auto-switch/groups/editor prefs)
    // are staged to avoid frequent disk writes (settings.yml is written exactly once here,
    // everything else runs on the in-memory copy).
    if settings_writable {
        if let Err(e) = global.save() {
            log::error!("Failed to save settings on exit: {}", e);
            dialog::error(
                HWND(std::ptr::null_mut()),
                i18n::t("save_failed"),
                &e.to_string(),
            );
        }
    } else {
        log::warn!(
            "Skipping settings save on exit: settings.yml could not be read and was not backed up"
        );
    }
    log::info!("=== Program exiting ===");
    Ok(())
}

// ===== Helper functions =====

fn fps_to_duration(fps: u32) -> Duration {
    // Guard: out-of-range fps yields inf/0 and panics; clamp to 1..=240
    let fps = fps.clamp(1, 240);
    Duration::from_secs_f32(1.0 / fps as f32)
}

/// Record an activation failure in the de-duplication set.
/// Returns true the first time this exact filename+message pair is reported.
fn mark_activation_error(filename: &str, message: &str) -> bool {
    let key = format!("{}\u{1f}{}", filename, message);
    let mut guard = match REPORTED_ACTIVATION_ERRORS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .get_or_insert_with(std::collections::HashSet::new)
        .insert(key)
}

/// Forget past failures for a config that just activated successfully.
fn clear_activation_errors(filename: &str) {
    let prefix = format!("{}\u{1f}", filename);
    let mut guard = match REPORTED_ACTIVATION_ERRORS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(set) = guard.as_mut() {
        set.retain(|key| !key.starts_with(&prefix));
    }
}

/// Tell the user that a config failed to load/activate (once per unique error).
fn report_activation_failure(filename: &str, err: &AppError) {
    log::error!("Failed to activate config {}: {}", filename, err);
    let message = err.to_string();
    if !mark_activation_error(filename, &message) {
        return;
    }
    dialog::error(
        HWND(std::ptr::null_mut()),
        i18n::t("config_activate_failed"),
        &format!(
            "{}\n\n{}: {}",
            i18n::t("config_activate_hint"),
            filename,
            message
        ),
    );
}

/// Report UI pre-render failures for this config (once per filename+message).
fn report_ui_errors(filename: &str, errors: &[String]) {
    let prefix = format!("{}\u{1f}", filename);
    let fresh: Vec<String> = {
        let mut guard = match REPORTED_UI_ERRORS.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let set = guard.get_or_insert_with(std::collections::HashSet::new);
        if errors.is_empty() {
            // The config now renders cleanly: allow a future regression to be reported again.
            set.retain(|key| !key.starts_with(&prefix));
            Vec::new()
        } else {
            errors
                .iter()
                .filter(|error| set.insert(format!("{}{}", prefix, error)))
                .cloned()
                .collect()
        }
    };
    if fresh.is_empty() {
        return;
    }
    log::error!(
        "UI pre-render failed [{}]: {}",
        filename,
        fresh.join("; ")
    );
    dialog::error(
        HWND(std::ptr::null_mut()),
        i18n::t("ui_render_failed"),
        &format!("{}\n\n{}", filename, fresh.join("\n")),
    );
}

/// Count a present failure and warn the user when the streak lasts long enough.
fn note_present_failure(ac: &mut ActiveConfig, stats: &mut FrameStats, err: &AppError) {
    stats.present_errors += 1;
    let now = Instant::now();
    let first = ac.present_fail_since.is_none();
    let since = *ac.present_fail_since.get_or_insert(now);
    if first {
        log::error!("Present failed [{}]: {}", ac.filename, err);
    }
    if !ac.present_fail_reported && now.duration_since(since) >= RUNTIME_ERROR_NOTICE_AFTER {
        ac.present_fail_reported = true;
        log::error!(
            "Present has been failing for {}s [{}]: {}",
            now.duration_since(since).as_secs(),
            ac.filename,
            err
        );
        dialog::error(
            HWND(std::ptr::null_mut()),
            i18n::t("present_failed"),
            &format!(
                "{}\n\n{}",
                i18n::t("present_retry_hint").replace("{}", &ac.filename),
                err
            ),
        );
    }
    // Back off a little; a failing UpdateLayeredWindow should not spin at frame rate.
    std::thread::sleep(Duration::from_millis(50));
}

/// Load a config and create the overlay window + pre-rendered UI
fn activate_config(filename: &str) -> AppResult<ActiveConfig> {
    let mut cfg = Config::load_from(filename)?;
    // v0.0.9: window tight around the content — x/y = content minimum corner, size = the
    // bounding box; translate all content coordinates so each element still lands on the
    // same screen pixel (window area ~20x smaller → per-frame clear/upload/composite cost
    // drops accordingly, measured CPU 0.46% → 0.06% @240fps).
    let (ox, oy, ow, oh) = cfg.tight_bounds();
    if ox != 0 || oy != 0 {
        for r in &mut cfg.capture_regions {
            r.display.x -= ox;
            r.display.y -= oy;
        }
        for ui in &mut cfg.custom_ui {
            ui.shift_xy(ox, oy);
        }
    }
    let win = OverlayWindow::new(ox, oy, ow, oh)?;
    win.show()?;
    // Pre-render top-level custom_ui (already in overlay-absolute coordinates)
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let (mut ui_elements, ui_errors) =
        prerender_ui(&cfg.custom_ui, &exe_dir, cfg.effective_global_opacity());
    ui_elements.sort_by_key(|r| r.z_order);
    report_ui_errors(filename, &ui_errors);
    if !ui_elements.is_empty() {
        log::info!("Config {} pre-rendered {} UI elements", filename, ui_elements.len());
    }
    clear_activation_errors(filename);
    Ok(ActiveConfig {
        filename: filename.to_string(),
        cfg,
        win,
        ui_elements,
        ow,
        oh,
        needs_static_present: true,
        present_fail_since: None,
        present_fail_reported: false,
    })
}

/// Clear, render capture frames + custom UI, and present one overlay.
/// Capture and static-UI configs share this path so a UI-only config cannot
/// silently skip UpdateLayeredWindow just because there is no capture frame.
fn render_config(ac: &ActiveConfig, frames: &[RegionFrame<'_>]) -> AppResult<()> {
    let dib = ac.win.dib_ptr();
    let buf_len = ac.win.buf_len();
    unsafe { std::ptr::write_bytes(dib, 0, buf_len); }
    render_regions(
        frames,
        &ac.cfg,
        dib,
        ac.ow,
        ac.oh,
        ac.cfg.effective_global_opacity(),
    );
    for ui in &ac.ui_elements {
        composite_ui(dib, ac.ow, ac.oh, ui);
    }
    ac.win.present()
}

/// Present configs that have no capture regions (pure frame/background/image/text).
/// They are static, so one successful present is enough; the flag is cleared only
/// after success so a transient present failure is retried.
fn present_static_configs(actives: &mut [ActiveConfig], stats: &mut FrameStats) {
    let no_frames: &[RegionFrame<'_>] = &[];
    for ac in actives.iter_mut() {
        if !ac.cfg.capture_regions.is_empty() || !ac.needs_static_present {
            continue;
        }
        let t0 = Instant::now();
        match render_config(ac, no_frames) {
            Ok(()) => {
                stats.present_ms += t0.elapsed().as_secs_f64() * 1000.0;
                stats.present_n += 1;
                ac.present_fail_since = None;
                ac.present_fail_reported = false;
                ac.needs_static_present = false;
            }
            Err(e) => note_present_failure(ac, stats, &e),
        }
    }
}

/// Sync the active config list to the tray (for menu check marks)
fn sync_tray_active_list(actives: &[ActiveConfig]) {
    let names: Vec<String> = actives.iter().map(|a| a.filename.clone()).collect();
    set_active_configs(names);
}

/// Sync the auto-switch toggle and both input groups to the tray (for menu checks/grey-out)
fn sync_input_groups_to_tray(gs: &GlobalSettings) {
    set_auto_enable(
        gs.input_auto_switch,
        gs.keyboard_configs.clone(),
        gs.controller_configs.clone(),
    );
}

/// Apply the editor prefs staged in memory (snap distance/gap/nudge step). Settings
/// are normally written once on exit, so these must be merged into the live copy.
fn apply_editor_prefs(global: &mut GlobalSettings) {
    if let Some((sd, sg, ns)) = config::take_editor_prefs() {
        global.snap_distance = sd.clamp(1, 100);
        global.snap_gap = sg.clamp(0, 50);
        global.nudge_step = ns.clamp(1, 100);
        log::info!(
            "Editor prefs applied: snap_distance={}, snap_gap={}, nudge_step={}",
            global.snap_distance,
            global.snap_gap,
            global.nudge_step
        );
    }
}

/// Common tail of both editor entry points (new config / edit existing): the saved
/// file becomes the single active config (auto-switch yields), old windows are
/// dropped, the file is reloaded, and the staged editor prefs are applied.
fn finish_editor_save(
    global: &mut GlobalSettings,
    actives: &mut Vec<ActiveConfig>,
    mode: InputMode,
    filename: &str,
) {
    select_single_config(global, filename);
    sync_input_groups_to_tray(global);
    apply_input_mode(actives, global, mode);
    reload_or_activate(actives, filename);
    apply_editor_prefs(global);
    log::info!("Edit done, config: {}", filename);
}

/// Rebuild every active overlay after a desktop resolution change. Content keeps
/// its absolute screen coordinates (user-chosen, not a layout to remap). Configs
/// whose capture source now lies outside the screen are reported once: their
/// out-of-screen part renders black (the capture side clamps it).
fn handle_resolution_change(actives: &mut Vec<ActiveConfig>, old: (u32, u32), new: (u32, u32)) {
    log::info!(
        "Desktop resolution changed: {}x{} -> {}x{}",
        old.0,
        old.1,
        new.0,
        new.1
    );
    let names: Vec<String> = actives.iter().map(|a| a.filename.clone()).collect();
    for name in &names {
        reload_or_activate(actives, name);
    }
    let out_of_bounds: Vec<String> = actives
        .iter()
        .filter(|a| !a.cfg.capture_regions_fit(new.0, new.1))
        .map(|a| a.filename.clone())
        .collect();
    if out_of_bounds.is_empty() {
        return;
    }
    log::warn!(
        "Capture regions outside the {}x{} screen: {}",
        new.0,
        new.1,
        out_of_bounds.join(", ")
    );
    let text = i18n::t("resolution_changed_hint")
        .replace("{old}", &format!("{}x{}", old.0, old.1))
        .replace("{new}", &format!("{}x{}", new.0, new.1))
        .replace("{list}", &out_of_bounds.join("\n"));
    dialog::warning(
        HWND(std::ptr::null_mut()),
        i18n::t("resolution_changed"),
        &text,
    );
}

/// Check whether the given config is in the auto group of the current input mode
fn if_auto_group_has(gs: &GlobalSettings, mode: InputMode, name: &str) -> bool {
    match mode {
        InputMode::Keyboard => gs.keyboard_configs.iter().any(|f| f == name),
        InputMode::Controller => gs.controller_configs.iter().any(|f| f == name),
    }
}

/// Single-select config model: the only slot = 1 manual config or auto-switch (as a whole).
/// Manually selecting a config: write enabled_configs=[name] and turn off auto-switch (auto groups yield).
fn select_single_config(gs: &mut GlobalSettings, name: &str) {
    gs.enabled_configs = vec![name.to_string()];
    gs.input_auto_switch = false;
    set_manual_enabled_configs(gs.enabled_configs.clone());
}

/// Align the active list with the "expected set": expected = manual set (enabled_configs) ∪ current input group (when auto-switch is on).
/// Idempotent: does nothing when there's no difference; doesn't write settings.yml (enabled_configs is maintained by manual actions).
fn apply_input_mode(actives: &mut Vec<ActiveConfig>, gs: &GlobalSettings, mode: InputMode) {
    // Expected set: start with the manual set, then merge in the auto group (dedupe by name)
    let mut target = gs.enabled_configs.clone();
    if gs.input_auto_switch {
        let group = match mode {
            InputMode::Keyboard => &gs.keyboard_configs,
            InputMode::Controller => &gs.controller_configs,
        };
        for f in group {
            if !target.contains(f) {
                target.push(f.clone());
            }
        }
    }
    let current: Vec<String> = actives.iter().map(|a| a.filename.clone()).collect();
    if current == target {
        return;
    }
    log::info!("Input mode {:?} (auto={}): active set {current:?} -> {target:?}",
        mode, gs.input_auto_switch);
    // Close those not in the target set (OverlayWindow Drop → DestroyWindow)
    let mut i = 0;
    while i < actives.len() {
        if !target.contains(&actives[i].filename) {
            actives.remove(i);
        } else {
            i += 1;
        }
    }
    // Open those missing from the target set
    for name in &target {
        if !actives.iter().any(|a| &a.filename == name) {
            match activate_config(name) {
                Ok(ac) => {
                    log::info!("Config activated: {}", name);
                    actives.push(ac);
                }
                Err(e) => report_activation_failure(name, &e),
            }
        }
    }
}

/// Re-activate a config from disk and replace the matching entry in the active list (equivalent to "reload config" for that file).
/// If the filename isn't active yet, add it as a new active config.
fn reload_or_activate(actives: &mut Vec<ActiveConfig>, filename: &str) {
    let pos = actives.iter().position(|a| a.filename == filename);
    match activate_config(filename) {
        Ok(ac) => {
            if let Some(p) = pos {
                actives[p] = ac; // Old OverlayWindow Drop → DestroyWindow
            } else {
                actives.push(ac);
            }
            log::info!("Config reloaded: {}", filename);
        }
        Err(e) => report_activation_failure(filename, &e),
    }
    sync_tray_active_list(actives);
}

/// Multi-region layout + premultiplied alpha.
/// Data is already cropped by the GPU (RegionFrame); here we only lay out, scale, and apply alpha.
/// Optimizations: direct DIB write / 1:1 fast path / bilinear interpolation / skip when alpha=255 /
/// stack-array sort for common small layouts, heap fallback for larger configs
fn render_regions(
    region_frames: &[RegionFrame<'_>],
    cfg: &Config,
    dst: *mut u8,
    dst_w: i32,
    dst_h: i32,
    global_opacity: f32,
) {
    let dst_w_u32 = dst_w as u32;
    let dst_h_i32 = dst_h;
    let dst_total = (dst_w * dst_h * 4) as usize;

    // Sort by z_order. A stack array avoids a per-frame heap allocation for the
    // common small layout; larger configs fall back to a Vec instead of silently
    // dropping regions past the 32nd.
    let n = cfg.capture_regions.len().min(region_frames.len());
    let mut stack_indices = [0usize; 32];
    let mut heap_indices = Vec::new();
    let indices: &mut [usize] = if n <= stack_indices.len() {
        for (i, slot) in stack_indices[..n].iter_mut().enumerate() {
            *slot = i;
        }
        &mut stack_indices[..n]
    } else {
        heap_indices.extend(0..n);
        &mut heap_indices
    };
    indices.sort_by_key(|&i| cfg.capture_regions[i].display.z_order);

    for &i in indices.iter() {
        let region = &cfg.capture_regions[i];
        let rf = &region_frames[i];
        let src_w = rf.w;
        let src_h = rf.h;
        let disp_x = region.display.x;
        let disp_y = region.display.y;
        // Logical (unrotated) display size: explicit display w/h wins; 0 = 1:1 (follow the source size)
        let (lx, ly) = cfg.region_logical_size(region);
        let deg = region.display.rotate.rem_euclid(360);
        // Visible footprint = the rotated bounding box, centered on the logical rect
        let (fx, fy, fw, fh) = crate::config::rotated_footprint(disp_x, disp_y, lx, ly, deg);
        let disp_w = fw as u32;
        let disp_h = fh as u32;

        // Element opacity unset → inherit the global global_opacity (same rule as region display.opacity)
        let combined_opacity = region.display.opacity.unwrap_or(global_opacity).clamp(0.0, 1.0);
        let alpha = (combined_opacity * 255.0).round() as u32;

        if alpha == 0 {
            // Fully transparent region: painting it yields (0,0,0,0) anyway, so skip it to save bandwidth
            continue;
        }

        if disp_w == 0 || disp_h == 0 || src_w == 0 || src_h == 0 {
            continue;
        }

        // ===== Rotation path (any angle): inverse-map each footprint pixel back to source =====
        if deg != 0 {
            // Fast path for axis-aligned 90/180/270: integer transform (no float trig / bilinear),
            // each output pixel maps to exactly one source pixel, so the result equals the slow
            // path with nearest sampling and costs ~1/4 of it.
            if deg == 90 || deg == 180 || deg == 270 {
                let scale_x = src_w as i64 * 0x10000 / lx.max(1) as i64;
                let scale_y = src_h as i64 * 0x10000 / ly.max(1) as i64;
                let alpha_byte = alpha as u8;
                let need_premul = alpha != 255;
                for dy in 0..disp_h {
                    let target_y = fy + dy as i32;
                    if target_y < 0 || target_y >= dst_h_i32 {
                        continue;
                    }
                    for dx in 0..disp_w {
                        let target_x = fx + dx as i32;
                        if target_x < 0 || target_x >= dst_w {
                            continue;
                        }
                        // Inverse-rotate (dx, dy) to logical (unrotated) coordinates,
                        // then scale to the source pixel (16.16 fixed-point, nearest/floor,
                        // identical mapping to the float path's floor((log+0.5)*scale-0.5)).
                        let (lxi, lyi) = match deg {
                            90 => (dy as i64, ly as i64 - 1 - dx as i64),
                            180 => (lx as i64 - 1 - dx as i64, ly as i64 - 1 - dy as i64),
                            270 => (lx as i64 - 1 - dy as i64, dx as i64),
                            _ => unreachable!(),
                        };
                        let sx = ((((lxi + 1) * scale_x - 0x8000) >> 16) as i32)
                            .clamp(0, src_w as i32 - 1) as u32;
                        let sy = ((((lyi + 1) * scale_y - 0x8000) >> 16) as i32)
                            .clamp(0, src_h as i32 - 1) as u32;
                        let si = ((sy * src_w + sx) * 4) as usize;
                        let didx = ((target_y as u32 * dst_w_u32 + target_x as u32) * 4) as usize;
                        if si + 3 >= rf.data.len() || didx + 3 >= dst_total {
                            continue;
                        }
                        let dst_pix = unsafe { dst.add(didx) };
                        for ch in 0..3usize {
                            let v = rf.data[si + ch];
                            unsafe {
                                *dst_pix.add(ch) = if need_premul {
                                    (v as u32 * alpha / 255) as u8
                                } else {
                                    v
                                };
                            }
                        }
                        unsafe { *dst_pix.add(3) = alpha_byte; }
                    }
                }
                continue;
            }
            let rad = (deg as f32).to_radians();
            let (sin_a, cos_a) = rad.sin_cos();
            let scale_x = src_w as f32 / lx as f32;
            let scale_y = src_h as f32 / ly as f32;
            let alpha_byte = alpha as u8;
            let need_premul = alpha != 255;

            for dy in 0..disp_h {
                let target_y = fy + dy as i32;
                if target_y < 0 || target_y >= dst_h_i32 {
                    continue;
                }
                // Destination pixel offset from the footprint center (YX: y down)
                let oy = (dy as f32 + 0.5) - fh as f32 / 2.0;
                for dx in 0..disp_w {
                    let target_x = fx + dx as i32;
                    if target_x < 0 || target_x >= dst_w {
                        continue;
                    }
                    let ox = (dx as f32 + 0.5) - fw as f32 / 2.0;
                    // Inverse-rotate the offset back to the logical rect orientation
                    let ux = ox * cos_a + oy * sin_a;
                    let uy = -ox * sin_a + oy * cos_a;
                    // Logical (unrotated) display pixel
                    let log_x = ux + lx as f32 / 2.0;
                    let log_y = uy + ly as f32 / 2.0;
                    // Map to the source pixel (same sampling as the scale path)
                    let src_fx = (log_x + 0.5) * scale_x - 0.5;
                    let src_fy = (log_y + 0.5) * scale_y - 0.5;
                    let sx0 = src_fx.floor() as i32;
                    let sy0 = src_fy.floor() as i32;
                    let fx_w = src_fx - sx0 as f32;
                    let fy_w = src_fy - sy0 as f32;
                    let cx0 = sx0.clamp(0, src_w as i32 - 1) as u32;
                    let cx1 = (sx0 + 1).clamp(0, src_w as i32 - 1) as u32;
                    let cy0 = sy0.clamp(0, src_h as i32 - 1) as u32;
                    let cy1 = (sy0 + 1).clamp(0, src_h as i32 - 1) as u32;

                    let i00 = ((cy0 * src_w + cx0) * 4) as usize;
                    let i10 = ((cy0 * src_w + cx1) * 4) as usize;
                    let i01 = ((cy1 * src_w + cx0) * 4) as usize;
                    let i11 = ((cy1 * src_w + cx1) * 4) as usize;
                    if i11 + 3 >= rf.data.len() {
                        continue;
                    }
                    // Pixels outside the logical rect (from the bbox corners) stay transparent
                    let in_rect = log_x >= 0.0 && log_x < lx as f32 && log_y >= 0.0 && log_y < ly as f32;
                    if !in_rect && deg % 90 != 0 {
                        continue;
                    }
                    let didx = ((target_y as u32 * dst_w_u32 + target_x as u32) * 4) as usize;
                    if didx + 3 >= dst_total {
                        continue;
                    }
                    let w00 = (1.0 - fx_w) * (1.0 - fy_w);
                    let w10 = fx_w * (1.0 - fy_w);
                    let w01 = (1.0 - fx_w) * fy_w;
                    let w11 = fx_w * fy_w;
                    let dst_pix = unsafe { dst.add(didx) };
                    for ch in 0..3usize {
                        let v = rf.data[i00 + ch] as f32 * w00
                            + rf.data[i10 + ch] as f32 * w10
                            + rf.data[i01 + ch] as f32 * w01
                            + rf.data[i11 + ch] as f32 * w11;
                        let v = v.round() as u32;
                        if need_premul {
                            unsafe { *dst_pix.add(ch) = (v * alpha / 255) as u8; }
                        } else {
                            unsafe { *dst_pix.add(ch) = v as u8; }
                        }
                    }
                    unsafe { *dst_pix.add(3) = alpha_byte; }
                }
            }
            continue;
        }

        // ===== Fast path: 1:1 mapping =====
        if src_w == disp_w && src_h == disp_h {
            let row_bytes = src_w as usize * 4;
            let alpha_byte = alpha as u8;
            let need_premul = alpha != 255;

            for dy in 0..disp_h {
                let target_y = disp_y + dy as i32;
                if target_y < 0 || target_y >= dst_h_i32 {
                    continue;
                }
                // Source data is already cropped, row width = src_w*4, no padding
                let src_row_start = dy as usize * row_bytes;
                let dst_row_start = ((target_y as u32 * dst_w_u32 + disp_x as u32) * 4) as usize;

                if src_row_start + row_bytes > rf.data.len() {
                    continue;
                }
                if dst_row_start + row_bytes > dst_total {
                    continue;
                }

                let src_slice = &rf.data[src_row_start..src_row_start + row_bytes];
                let dst_slice = unsafe {
                    std::slice::from_raw_parts_mut(dst.add(dst_row_start), row_bytes)
                };

                if need_premul {
                    for px in 0..(row_bytes / 4) {
                        let si = px * 4;
                        dst_slice[si] = (src_slice[si] as u32 * alpha / 255) as u8;
                        dst_slice[si + 1] = (src_slice[si + 1] as u32 * alpha / 255) as u8;
                        dst_slice[si + 2] = (src_slice[si + 2] as u32 * alpha / 255) as u8;
                        dst_slice[si + 3] = alpha_byte;
                    }
                } else {
                    dst_slice.copy_from_slice(src_slice);
                    for chunk in dst_slice.chunks_exact_mut(4) {
                        chunk[3] = 255;
                    }
                }
            }
            continue;
        }

        // ===== Scaling path: bilinear interpolation =====
        let scale_x = src_w as f32 / disp_w as f32;
        let scale_y = src_h as f32 / disp_h as f32;
        let alpha_byte = alpha as u8;
        let need_premul = alpha != 255;

        for dy in 0..disp_h {
            let target_y = disp_y + dy as i32;
            if target_y < 0 || target_y >= dst_h_i32 {
                continue;
            }
            let src_fy = (dy as f32 + 0.5) * scale_y - 0.5;
            let sy0 = src_fy.floor() as i32;
            let sy1 = sy0 + 1;
            let fy = src_fy - sy0 as f32;

            for dx in 0..disp_w {
                let target_x = disp_x + dx as i32;
                if target_x < 0 || target_x >= dst_w {
                    continue;
                }
                let src_fx = (dx as f32 + 0.5) * scale_x - 0.5;
                let sx0 = src_fx.floor() as i32;
                let sx1 = sx0 + 1;
                let fx = src_fx - sx0 as f32;

                // Clamp to the region bounds (local coordinates, no need to add src_x/src_y)
                let cx0 = sx0.clamp(0, src_w as i32 - 1) as u32;
                let cx1 = sx1.clamp(0, src_w as i32 - 1) as u32;
                let cy0 = sy0.clamp(0, src_h as i32 - 1) as u32;
                let cy1 = sy1.clamp(0, src_h as i32 - 1) as u32;

                let i00 = ((cy0 * src_w + cx0) * 4) as usize;
                let i10 = ((cy0 * src_w + cx1) * 4) as usize;
                let i01 = ((cy1 * src_w + cx0) * 4) as usize;
                let i11 = ((cy1 * src_w + cx1) * 4) as usize;

                if i11 + 3 >= rf.data.len() {
                    continue;
                }

                let didx = ((target_y as u32 * dst_w_u32 + target_x as u32) * 4) as usize;
                if didx + 3 >= dst_total {
                    continue;
                }

                let w00 = (1.0 - fx) * (1.0 - fy);
                let w10 = fx * (1.0 - fy);
                let w01 = (1.0 - fx) * fy;
                let w11 = fx * fy;

                let dst_pix = unsafe { dst.add(didx) };
                for ch in 0..3usize {
                    let v = rf.data[i00 + ch] as f32 * w00
                        + rf.data[i10 + ch] as f32 * w10
                        + rf.data[i01 + ch] as f32 * w01
                        + rf.data[i11 + ch] as f32 * w11;
                    let v = v.round() as u32;
                    if need_premul {
                        unsafe { *dst_pix.add(ch) = (v * alpha / 255) as u8; }
                    } else {
                        unsafe { *dst_pix.add(ch) = v as u8; }
                    }
                }
                unsafe { *dst_pix.add(3) = alpha_byte; }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CaptureRegion, Config, DisplayRect, Rect};

    /// Build a tiny source frame whose pixel value encodes (x, y): BGRA = (x, y, 127, 255).
    fn frame(w: u32, h: u32) -> Vec<u8> {
        let mut data = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                data[i] = x as u8;
                data[i + 1] = y as u8;
                data[i + 2] = 127;
                data[i + 3] = 255;
            }
        }
        data
    }

    /// Render one 4x3 source rotated by `deg` (display 1:1, no offset) into a dst buffer.
    /// Returns the BGRA pixel at (dx, dy).
    fn rotated_pixel(deg: i32, dx: u32, dy: u32) -> (u8, u8, u8, u8) {
        let (src_w, src_h) = (4u32, 3u32);
        let data = frame(src_w, src_h);
        let rf = RegionFrame { data: &data, w: src_w, h: src_h };
        let cfg = Config {
            capture_regions: vec![CaptureRegion {
                id: "r".into(),
                source: Rect { x: 0, y: 0, width: src_w, height: src_h },
                display: DisplayRect { x: 0, y: 0, width: 0, height: 0, opacity: None, z_order: 0, rotate: deg },
                z_order: None,
            }],
            ..Default::default()
        };
        // 90°/270° → 3x4 footprint; 180° → 4x3
        let (dw, dh) = if deg % 180 == 0 { (src_w, src_h) } else { (src_h, src_w) };
        let mut dst = vec![0u8; (dw * dh * 4) as usize];
        render_regions(&[rf], &cfg, dst.as_mut_ptr(), dw as i32, dh as i32, 1.0);
        let i = ((dy * dw + dx) * 4) as usize;
        (dst[i], dst[i + 1], dst[i + 2], dst[i + 3])
    }

    fn check_axis(deg: i32) {
        let (sw, sh) = (4u32, 3u32);
        // 90°/270° → 3x4 footprint; 180° → 4x3
        let (dw, dh) = if deg % 180 == 0 { (sw, sh) } else { (sh, sw) };
        for dy in 0..dh {
            for dx in 0..dw {
                // Expected inverse mapping (integer axis rotation):
                let (sx, sy) = match deg.rem_euclid(360) {
                    90 => (dy, sh - 1 - dx),
                    180 => (sw - 1 - dx, sh - 1 - dy),
                    270 => (sw - 1 - dy, dx),
                    _ => unreachable!(),
                };
                let p = rotated_pixel(deg, dx, dy);
                assert_eq!((p.0, p.1), (sx as u8, sy as u8),
                    "deg={} dst=({},{}) should sample src=({},{})",
                    deg, dx, dy, sx, sy);
                assert_eq!(p.2, 127);
                assert_eq!(p.3, 255);
            }
        }
    }

    #[test]
    fn axis_rotate_90() { check_axis(90); }
    #[test]
    fn axis_rotate_180() { check_axis(180); }
    #[test]
    fn axis_rotate_270() { check_axis(270); }
    #[test]
    fn axis_rotate_90_plus_360() { check_axis(450); }

    /// Regression: the old renderer capped layouts at 32 regions. Every region
    /// must still be painted, and z_order must work across that boundary.
    #[test]
    fn render_regions_supports_more_than_32_regions() {
        const COUNT: usize = 40;
        // One 1x1 frame per region; the red channel identifies the region.
        let frames: Vec<Vec<u8>> = (0..COUNT)
            .map(|i| vec![(i + 1) as u8, 0, 0, 255])
            .collect();
        let region_frames: Vec<RegionFrame<'_>> = frames
            .iter()
            .map(|data| RegionFrame { data, w: 1, h: 1 })
            .collect();
        let mut cfg = Config {
            capture_regions: (0..COUNT)
                .map(|i| CaptureRegion {
                    id: format!("r{i}"),
                    source: Rect { x: 0, y: 0, width: 1, height: 1 },
                    display: DisplayRect {
                        x: i as i32,
                        y: 0,
                        width: 0,
                        height: 0,
                        opacity: None,
                        z_order: i as i32,
                        rotate: 0,
                    },
                    z_order: None,
                })
                .collect(),
            ..Default::default()
        };

        // Side by side: every column must be painted (no truncation).
        let mut dst = vec![0u8; COUNT * 4];
        render_regions(&region_frames, &cfg, dst.as_mut_ptr(), COUNT as i32, 1, 1.0);
        for (i, px) in dst.chunks_exact(4).enumerate() {
            assert_eq!(px[0], (i + 1) as u8, "region {i} was not rendered");
            assert_eq!((px[1], px[2], px[3]), (0, 0, 255));
        }

        // Stacked at one pixel: the highest z_order (region 39) must win,
        // proving the sort covers indices beyond the old 32-entry array.
        for region in &mut cfg.capture_regions {
            region.display.x = 0;
        }
        let mut dst = vec![0u8; 4];
        render_regions(&region_frames, &cfg, dst.as_mut_ptr(), 1, 1, 1.0);
        assert_eq!(&dst[..], &[COUNT as u8, 0, 0, 255]);
    }
}
