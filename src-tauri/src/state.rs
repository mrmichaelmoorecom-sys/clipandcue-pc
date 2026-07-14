use std::sync::atomic::{AtomicBool, AtomicIsize};
use std::sync::Mutex;

use crate::history::History;
use crate::settings::Settings;

pub struct AppState {
    pub history: Mutex<History>,
    pub settings: Mutex<Settings>,
    /// Clipboard sequence-number ranges produced by our own writes:
    /// (seq_before_write, seq_after_write]. Updates inside a range are ours.
    pub suppress: Mutex<Vec<(u32, u32)>>,
    /// HWND (as isize) of the window a paste should target — the foreground
    /// window at the moment the dropdown/HUD was summoned.
    pub paste_target: AtomicIsize,
    /// Dropdown is currently shown in non-activating HUD mode with digit
    /// shortcuts registered.
    pub hud_mode: AtomicBool,
    /// Sequence number of the last capture we processed (dup-event guard).
    pub last_seq: AtomicIsize,
}
