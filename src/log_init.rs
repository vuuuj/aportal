//! Log initialization: write files to the log\ folder next to the exe (log.txt / crash.txt)
//!
//! With no console, file logs are the only way to diagnose (especially after a crash).
//! stderr output is kept too (visible with cargo run).
//!
//! Encoding strategy: log.txt / crash.txt are always **UTF-8 with BOM** (EF BB BF).
//! BOM-less UTF-8 would be misread as ANSI by GBK tools (CMD type / old Notepad / some editors),
//! garbling all CJK text — this decouples display from storage: any viewer reads them as UTF-8.

use std::io;
use std::io::Write as _;

use crate::error::{AppError, AppResult};

/// UTF-8 BOM bytes (at the file start, marking the file as UTF-8 encoded)
const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// Get the log\ folder next to the exe, creating it if missing.
pub fn exe_log_dir() -> AppResult<std::path::PathBuf> {
    let exe_path = std::env::current_exe()
        .map_err(|e| AppError::other(format!("failed to get exe path: {}", e)))?;
    let exe_dir = exe_path
        .parent()
        .ok_or_else(|| AppError::other("exe path has no parent directory"))?;
    let dir = exe_dir.join("log");
    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::other(format!("failed to create directory {}: {}", dir.display(), e)))?;
    }
    Ok(dir)
}

/// Initialize logging: write to file + stderr.
/// File mode is append, convenient for reviewing history across runs.
///
/// `log_enabled=false` (settings.yml) → fully disabled: no log.txt, no crash.txt,
/// no log folder, no panic hook. Binary switch for everyday use; enable only when debugging.
pub fn init(log_enabled: bool) {
    if !log_enabled {
        return;
    }
    let level = env_or("RUST_LOG", "info");

    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(&level),
    );
    builder
        .format_timestamp_millis()
        .format(|buf, record| {
            // Compact format: [time level module] message
            writeln!(
                buf,
                "[{} {:>5} {}] {}",
                buf.timestamp_millis().to_string().trim_matches('"'),
                record.level(),
                record.module_path().unwrap_or("?"),
                record.args()
            )
        });

    // Try file output; fall back to stderr-only on failure
    match exe_log_dir() {
        Ok(dir) => {
            let log_path = dir.join("log.txt");
            // Normalize on startup: add missing BOM + trim when over the line limit to prevent unbounded growth
            normalize_log_file(&log_path);
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                Ok(file) => {
                    // Encoding marker: write the BOM when the file is new or empty after trimming, so any tool opens it as UTF-8
                    ensure_utf8_bom(&file);
                    // Separator: one line per startup to tell runs apart
                    let mut sep_file = file.try_clone().unwrap_or_else(|_| {
                        std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&log_path)
                            .expect("failed to reopen the log file")
                    });
                    let _ = writeln!(
                        io::Write::by_ref(&mut sep_file),
                        "\n========== APortal started {} ==========",
                        now_string()
                    );
                    // Mirror the startup separator to stderr too
                    eprintln!("\n=== APortal started, log file: {} ===", log_path.display());

                    builder.target(env_logger::Target::Pipe(Box::new(file))).init();
                    log::info!("log file: {}", log_path.display());

                    // Register a panic hook: write crash info to the log file and flush
                    let crash_path = dir.join("crash.txt");
                    let crash_path_for_hook = crash_path.clone();
                    std::panic::set_hook(Box::new(move |info| {
                        // Write crash.txt directly (not relying on the env_logger buffer)
                        let msg = format!(
                            "========== CRASH ==========\nTime: {}\nPanic: {}\n\n",
                            now_string(),
                            info
                        );
                        let mut bytes = Vec::with_capacity(UTF8_BOM.len() + msg.len());
                        bytes.extend_from_slice(&UTF8_BOM);
                        bytes.extend_from_slice(msg.as_bytes());
                        let _ = std::fs::write(&crash_path_for_hook, &bytes);
                        // Also try to flush the log
                        log::error!("PANIC: {}", info);
                        log::logger().flush();
                    }));
                    log::info!("panic hook registered; crash info will be written to {}", crash_path.display());
                }
                Err(e) => {
                    builder.init();
                    log::error!("failed to open log file {}: {}", log_path.display(), e);
                }
            }
        }
        Err(e) => {
            builder.init();
            log::error!("failed to locate the log folder next to the exe: {}", e);
        }
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Maximum lines kept in the log file (covers the last few runs)
const LOG_MAX_LINES: usize = 1000;

/// Normalize log.txt on startup: ① add a missing UTF-8 BOM (old files without BOM get misread by GBK tools);
/// ② keep only the last LOG_MAX_LINES lines when the limit is exceeded.
/// The old logic also required the file to be ≥512KB before trimming — files over the line limit but under
/// 512KB (short lines) never got trimmed; measured 4172 lines / 487KB sitting right below the threshold.
/// Trimmed by line count alone now: the one-time startup read cost is negligible.
fn normalize_log_file(path: &std::path::Path) {
    let Ok(orig) = std::fs::read_to_string(path) else { return };
    let had_bom = orig.starts_with('\u{FEFF}');
    let content = orig.trim_start_matches('\u{FEFF}');
    let total = content.lines().count();
    let trimmed = total > LOG_MAX_LINES;
    if had_bom && !trimmed {
        return;
    }
    let final_content = if trimmed {
        content
            .lines()
            .skip(total - LOG_MAX_LINES)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    } else {
        content.to_string()
    };
    let mut bytes = Vec::with_capacity(UTF8_BOM.len() + final_content.len());
    bytes.extend_from_slice(&UTF8_BOM);
    bytes.extend_from_slice(final_content.as_bytes());
    let _ = std::fs::write(path, bytes);
}

/// If the file is empty (new or just trimmed), write the UTF-8 BOM first.
/// std implements Write for `&File`, so no mutable handle is needed.
fn ensure_utf8_bom(mut file: &std::fs::File) {
    let is_empty = file.metadata().map(|m| m.len() == 0).unwrap_or(false);
    if is_empty {
        let _ = file.write_all(&UTF8_BOM);
    }
}

fn now_string() -> String {
    // Simple timestamp: SystemTime → seconds, avoiding a chrono dependency
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix={}", secs)
}
