//! Single-instance guard: only one APortal may run per logon session.
//!
//! Two copies would stack identical topmost overlays and both write settings.yml
//! on exit (the later exit silently overwrites the earlier one's changes). A named
//! mutex is the cheapest reliable cross-process lock, and the OS releases it when
//! the process exits, including a crash.
//!
//! The `Local\` namespace keeps the name per logon session, so different Windows
//! users may each run their own copy. The mutex is held for the whole process
//! lifetime; ownership itself is irrelevant, only the object's existence matters.

use windows::core::w;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE,
};
use windows::Win32::System::Threading::CreateMutexW;

/// Keeps the named mutex alive until the process exits.
pub struct InstanceGuard {
    /// `None` when the mutex could not be created and the program runs unprotected.
    handle: Option<HANDLE>,
}

impl InstanceGuard {
    /// Try to become the only running instance.
    ///
    /// Returns `None` when another instance already owns the mutex. If the mutex
    /// itself cannot be created (rare: access denied), the program continues
    /// unprotected instead of refusing to start.
    pub fn acquire() -> Option<Self> {
        let name = w!("Local\\APortal_SingleInstance");
        unsafe {
            match CreateMutexW(None, true, name) {
                Ok(handle) => {
                    // CreateMutexW succeeds even when the name already exists;
                    // the last-error value is the only way to tell us apart.
                    if GetLastError() == ERROR_ALREADY_EXISTS {
                        let _ = CloseHandle(handle);
                        None
                    } else {
                        Some(Self {
                            handle: Some(handle),
                        })
                    }
                }
                Err(e) => {
                    log::warn!(
                        "single-instance mutex unavailable ({}); continuing without it",
                        e
                    );
                    Some(Self { handle: None })
                }
            }
        }
    }
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
    }
}
