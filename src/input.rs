//! Pad/keyboard auto switching - input mode detection
//!
//! Anti-cheat safety: only polls public global read APIs (XInputGetState / GetCursorPos),
//! no injection, no hooking, no game memory writes - consistent with "read screen pixels only".
//!
//! Decision rules (latest poll wins, no debounce, no rollback):
//! - Pad active: pad connected and (button pressed / trigger>8 / stick out of dead zone>4000)
//! - Keyboard active: mouse moved > 2px (can be disabled in settings)
//! - Priority within one poll: pad > keyboard (pad wins on conflict)

use std::time::{Duration, Instant};

/// Stick dead zone (XInput axis range +-32768, static drift measured about +-320)
const STICK_DEAD_ZONE: i16 = 4000;
/// Trigger threshold (range 0..=255, light presses count as activity too)
const TRIGGER_THRESHOLD: u8 = 8;
/// Mouse move threshold (pixels, relative to the previous poll)
const MOUSE_MOVE_THRESHOLD: i32 = 2;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct XinputGamepad {
    w_buttons: u16,
    b_left_trigger: u8,
    b_right_trigger: u8,
    s_thumb_lx: i16,
    s_thumb_ly: i16,
    s_thumb_rx: i16,
    s_thumb_ry: i16,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct XinputState {
    dw_packet_number: u32,
    gamepad: XinputGamepad,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct CursorPos {
    x: i32,
    y: i32,
}

#[link(name = "xinput")]
#[link(name = "user32")]
extern "system" {
    fn XInputGetState(dw_user_index: u32, p_state: *mut XinputState) -> u32;
    fn GetCursorPos(lp_point: *mut CursorPos) -> i32;
}

/// Input mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Keyboard,
    Controller,
}

impl InputMode {
    fn label(self) -> &'static str {
        match self {
            InputMode::Keyboard => "keyboard",
            InputMode::Controller => "pad",
        }
    }
}

/// Input detector: polls once per fixed interval, returns true on mode change.
pub struct Input {
    /// Poll interval (ms, from settings input_poll_interval_ms)
    interval: u64,
    /// Whether mouse movement counts as keyboard activity
    track_mouse: bool,
    last_poll: Instant,
    last_cursor: CursorPos,
    /// Current mode (starts as keyboard; first poll forces one decision)
    mode: InputMode,
    first_poll: bool,
}

impl Input {
    pub fn new(interval_ms: u64, track_mouse: bool) -> Self {
        let mut last_cursor = CursorPos::default();
        unsafe { GetCursorPos(&mut last_cursor) };
        Self {
            interval: interval_ms.clamp(1, 10_000),
            track_mouse,
            last_poll: Instant::now(),
            last_cursor,
            mode: InputMode::Keyboard,
            first_poll: true,
        }
    }

    pub fn mode(&self) -> InputMode { self.mode }

    /// Poll if the interval elapsed; if the mode changed, log it and return true
    pub fn poll(&mut self) -> bool {
        if !self.first_poll && self.last_poll.elapsed().as_millis() < self.interval as u128 {
            return false;
        }
        self.first_poll = false;
        self.last_poll = Instant::now();

        let mut cursor = CursorPos::default();
        unsafe { GetCursorPos(&mut cursor) };
        let dx = cursor.x - self.last_cursor.x;
        let dy = cursor.y - self.last_cursor.y;
        let mouse_active = self.track_mouse
            && (dx.abs() > MOUSE_MOVE_THRESHOLD || dy.abs() > MOUSE_MOVE_THRESHOLD);
        self.last_cursor = cursor;

        let pad = pad_activity();

        // Mutually exclusive: pad > keyboard
        let new_mode = match pad {
            PadState::Active => InputMode::Controller,
            PadState::Idle => {
                // Connected but idle: keyboard activity decides
                if mouse_active { InputMode::Keyboard } else { self.mode }
            }
            PadState::NotConnected => {
                // XInputGetState on an unplugged controller costs ~10ms per call
                // (it enumerates the USB bus): polling at 100ms would burn ~10% of
                // one core. Back off to ~1s while disconnected; the first poll after
                // a reconnect catches the pad again.
                self.last_poll += Duration::from_millis(900);
                if mouse_active { InputMode::Keyboard } else { self.mode }
            }
        };

        if new_mode != self.mode {
            self.mode = new_mode;
            log::info!("input mode switched: {}", new_mode.label());
            true
        } else {
            false
        }
    }
}

/// Stick/trigger out of dead zone (drift 320 is far below the threshold, no false positives)
fn pad_activity() -> PadState {
    let mut st = XinputState::default();
    let hr = unsafe { XInputGetState(0, &mut st) };
    if hr != 0 {
        return PadState::NotConnected;
    }
    let g = &st.gamepad;
    if g.w_buttons != 0
        || g.b_left_trigger > TRIGGER_THRESHOLD
        || g.b_right_trigger > TRIGGER_THRESHOLD
        || g.s_thumb_lx.abs() > STICK_DEAD_ZONE
        || g.s_thumb_ly.abs() > STICK_DEAD_ZONE
        || g.s_thumb_rx.abs() > STICK_DEAD_ZONE
        || g.s_thumb_ry.abs() > STICK_DEAD_ZONE
    {
        PadState::Active
    } else {
        PadState::Idle
    }
}

/// Pad poll result: Active = input detected, Idle = connected but idle,
/// NotConnected = no controller (XInputGetState costs ~10ms per call when unplugged)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PadState {
    Active,
    Idle,
    NotConnected,
}
