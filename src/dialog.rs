//! Native message boxes for user-visible messages and errors.
//!
//! The release build has no console, so an error that only reaches the log is
//! effectively invisible. Callers pass already-localized title/text.

use windows::core::HSTRING;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK,
    MB_SETFOREGROUND, MB_YESNO,
};

/// Show a modal information box (non-error).
pub fn info(parent: HWND, title: &str, text: &str) {
    let title = HSTRING::from(title);
    let text = HSTRING::from(text);
    unsafe {
        let _ = MessageBoxW(
            parent,
            &text,
            &title,
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
        );
    }
}

/// Show a modal error box, owned by `parent` when it is a real window.
pub fn error(parent: HWND, title: &str, text: &str) {
    let title = HSTRING::from(title);
    let text = HSTRING::from(text);
    unsafe {
        let _ = MessageBoxW(
            parent,
            &text,
            &title,
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

/// Show a modal warning box (non-fatal).
pub fn warning(parent: HWND, title: &str, text: &str) {
    let title = HSTRING::from(title);
    let text = HSTRING::from(text);
    unsafe {
        let _ = MessageBoxW(
            parent,
            &text,
            &title,
            MB_OK | MB_ICONWARNING | MB_SETFOREGROUND,
        );
    }
}

/// Ask a Yes/No question; No is the default so Enter does not confirm a risky action.
pub fn confirm(parent: HWND, title: &str, text: &str) -> bool {
    let title = HSTRING::from(title);
    let text = HSTRING::from(text);
    unsafe {
        MessageBoxW(
            parent,
            &text,
            &title,
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND,
        ) == IDYES
    }
}
