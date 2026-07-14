use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    /// Clips shown in the dropdown/HUD (1-9 usable via number keys).
    pub show_count: usize,
    /// History cap, enforced on UNPINNED clips only (Mac lesson: pins must
    /// never brick capture).
    pub history_cap: usize,
    /// Per-format size cap in MB (1-200). A clip with any format over the cap
    /// is not saved, with a notification.
    pub max_format_mb: u32,
    /// After restoring a clip, focus the previous window and send Ctrl+V.
    pub auto_paste: bool,
    /// Restore only plain text (CF_UNICODETEXT) instead of all formats.
    pub plain_text_paste: bool,
    /// Skip copies flagged with clipboard-manager exclusion formats
    /// (password managers).
    pub skip_excluded: bool,
    pub clear_on_quit: bool,
    pub launch_at_login: bool,
    /// Global hotkey, tauri-plugin-global-shortcut syntax.
    pub hotkey: String,
    /// Keep writing the format-survey JSONL (phase 1 diagnostics).
    pub survey_log: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            show_count: 9,
            history_cap: 50,
            max_format_mb: 50,
            auto_paste: true,
            plain_text_paste: false,
            skip_excluded: true,
            clear_on_quit: false,
            launch_at_login: false,
            hotkey: "ctrl+alt+v".into(),
            survey_log: true,
        }
    }
}

impl Settings {
    pub fn load(dir: &PathBuf) -> Self {
        std::fs::read_to_string(dir.join("settings.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &PathBuf) {
        let _ = std::fs::create_dir_all(dir);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(dir.join("settings.json"), json);
        }
    }

    pub fn max_format_bytes(&self) -> usize {
        self.max_format_mb.clamp(1, 200) as usize * 1024 * 1024
    }
}
