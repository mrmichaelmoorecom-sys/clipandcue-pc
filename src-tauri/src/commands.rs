use base64::Engine;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::capture::emit_history;
use crate::history::ClipMeta;
use crate::settings::Settings;
use crate::state::AppState;
use crate::{apply_hotkey, hide_dropdown, rebuild_tray_menu};

#[tauri::command]
pub async fn list_clips(state: State<'_, AppState>) -> Result<Vec<ClipMeta>, String> {
    Ok(state.history.lock().unwrap().view())
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    Ok(state.settings.lock().unwrap().clone())
}

#[tauri::command]
pub async fn set_settings(app: AppHandle, state: State<'_, AppState>, settings: Settings) -> Result<(), String> {
    let old = {
        let mut s = state.settings.lock().unwrap();
        let old = s.clone();
        *s = settings.clone();
        s.save(&app.path().app_data_dir().map_err(|e| e.to_string())?);
        old
    };
    if old.hotkey != settings.hotkey {
        apply_hotkey(&app, &old.hotkey, &settings.hotkey)?;
    }
    if old.launch_at_login != settings.launch_at_login {
        set_launch_at_login(settings.launch_at_login);
    }
    rebuild_tray_menu(&app);
    Ok(())
}

#[tauri::command]
pub async fn paste_clip(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    let (plain, auto) = {
        let s = state.settings.lock().unwrap();
        (s.plain_text_paste, s.auto_paste)
    };
    hide_dropdown(&app);
    if !crate::paste::restore_clip(&app, &id, plain) {
        return Err("clip could not be restored".into());
    }
    if auto {
        crate::paste::auto_paste(&app);
    }
    Ok(())
}

#[tauri::command]
pub async fn toggle_pin(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    let pinned = {
        let mut h = state.history.lock().unwrap();
        let Some(clip) = h.get(&id) else {
            return Err("no such clip".into());
        };
        let target = !clip.pinned;
        h.set_pinned(&id, target);
        target
    };
    let _ = pinned;
    emit_history(&app);
    Ok(())
}

#[tauri::command]
pub async fn reorder_clip(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    index: usize,
) -> Result<(), String> {
    if !state.history.lock().unwrap().reorder(&id, index) {
        return Err("no such clip".into());
    }
    emit_history(&app);
    Ok(())
}

#[tauri::command]
pub async fn delete_clip(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.history.lock().unwrap().delete(&id);
    emit_history(&app);
    Ok(())
}

#[tauri::command]
pub async fn clear_history(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    state.history.lock().unwrap().clear(true);
    emit_history(&app);
    Ok(())
}

#[derive(Serialize)]
pub struct Preview {
    pub mime: String,
    pub b64: String,
}

#[tauri::command]
pub async fn get_preview(state: State<'_, AppState>, id: String) -> Result<Option<Preview>, String> {
    let path = {
        let history = state.history.lock().unwrap();
        history.preview_path(&id)
    };
    let Some(path) = path else {
        return Ok(None);
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(None);
    };
    let mime = if path.extension().is_some_and(|e| e == "png") {
        "image/png"
    } else {
        "image/bmp"
    };
    Ok(Some(Preview {
        mime: mime.into(),
        b64: base64::engine::general_purpose::STANDARD.encode(bytes),
    }))
}

#[tauri::command]
pub async fn hide_window(app: AppHandle) {
    hide_dropdown(&app);
}

#[tauri::command]
pub async fn open_prefs(app: AppHandle) {
    hide_dropdown(&app);
    if let Some(w) = app.get_webview_window("prefs") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

pub fn set_launch_at_login(enable: bool) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let mut cmd = std::process::Command::new("reg");
    if enable {
        let exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        cmd.args(["add", key, "/v", "clipandcue", "/t", "REG_SZ", "/d", &exe, "/f"]);
    } else {
        cmd.args(["delete", key, "/v", "clipandcue", "/f"]);
    }
    let _ = cmd.creation_flags(CREATE_NO_WINDOW).output();
}
