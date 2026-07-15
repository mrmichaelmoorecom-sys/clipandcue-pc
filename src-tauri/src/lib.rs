mod capture;
mod commands;
mod formats;
mod history;
mod paste;
mod settings;
mod state;

use std::sync::atomic::Ordering;
use std::sync::Mutex;

use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, PhysicalPosition, RunEvent, WebviewWindow, WindowEvent,
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Shortcut, ShortcutState};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE,
    WS_EX_NOACTIVATE,
};

use state::AppState;

pub struct HotkeyState(pub Mutex<Shortcut>);

fn dropdown(app: &AppHandle) -> WebviewWindow {
    app.get_webview_window("dropdown").expect("dropdown window missing")
}

/// Toggle WS_EX_NOACTIVATE so the HUD can show without stealing focus from
/// the window the user is typing in.
fn set_noactivate(window: &WebviewWindow, on: bool) {
    if let Ok(hwnd) = window.hwnd() {
        unsafe {
            let hwnd = windows::Win32::Foundation::HWND(hwnd.0);
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let flag = WS_EX_NOACTIVATE.0 as isize;
            let new = if on { ex | flag } else { ex & !flag };
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new);
        }
    }
}

/// Remember which window a paste should go to. Never our own windows, never
/// the taskbar/desktop shell.
fn update_paste_target(app: &AppHandle) {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return;
        }
        for label in ["dropdown", "prefs"] {
            if let Some(w) = app.get_webview_window(label) {
                if let Ok(own) = w.hwnd() {
                    if own.0 == fg.0 {
                        return;
                    }
                }
            }
        }
        let mut class = [0u16; 64];
        let len = GetClassNameW(fg, &mut class);
        let class = String::from_utf16_lossy(&class[..len.max(0) as usize]);
        if matches!(class.as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Progman" | "WorkerW") {
            return;
        }
        let state = app.state::<AppState>();
        state.paste_target.store(fg.0 as isize, Ordering::SeqCst);
    }
}

fn position_near(window: &WebviewWindow, anchor: PhysicalPosition<f64>) {
    // Clamp to the monitor the ANCHOR is on — clamping to the window's
    // previous monitor throws the popup off-screen in multi-monitor setups.
    let monitor = window
        .app_handle()
        .monitor_from_point(anchor.x, anchor.y)
        .ok()
        .flatten()
        .or_else(|| window.current_monitor().ok().flatten());
    if let (Ok(size), Some(monitor)) = (window.outer_size(), monitor) {
        let mon_pos = monitor.position();
        let mon_size = monitor.size();
        let x = (anchor.x - size.width as f64 / 2.0).clamp(
            mon_pos.x as f64,
            (mon_pos.x + mon_size.width.saturating_sub(size.width) as i32) as f64,
        );
        let y = (anchor.y - size.height as f64 - 8.0).max(mon_pos.y as f64);
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
}

pub fn hide_dropdown(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.hud_mode.swap(false, Ordering::SeqCst) {
        capture::post_unregister_digits();
    }
    let _ = dropdown(app).hide();
}

/// Show as a normal focused dropdown (tray click).
fn show_dropdown_focused(app: &AppHandle, anchor: Option<PhysicalPosition<f64>>) {
    update_paste_target(app);
    let window = dropdown(app);
    set_noactivate(&window, false);
    if let Some(a) = anchor {
        position_near(&window, a);
    }
    let _ = window.show();
    let _ = window.set_focus();
    let _ = app.emit("dropdown-shown", false);
}

/// Show as a non-activating HUD (hotkey): focus stays in the target app,
/// digits 1-9 paste, Escape dismisses.
fn show_hud(app: &AppHandle) {
    update_paste_target(app);
    let window = dropdown(app);
    set_noactivate(&window, true);
    if let Ok(pos) = app.cursor_position() {
        position_near(&window, pos);
    }
    let state = app.state::<AppState>();
    if !state.hud_mode.swap(true, Ordering::SeqCst) {
        let count = state.settings.lock().unwrap().show_count;
        capture::post_register_digits(count);
    }
    let _ = window.show();
    let _ = app.emit("dropdown-shown", true);
}

fn toggle_hud(app: &AppHandle) {
    let window = dropdown(app);
    if window.is_visible().unwrap_or(false) {
        hide_dropdown(app);
    } else {
        show_hud(app);
    }
}

pub(crate) fn paste_nth(app: &AppHandle, n: usize) {
    let state = app.state::<AppState>();
    let id = {
        let h = state.history.lock().unwrap();
        h.view().get(n).map(|c| c.id.clone())
    };
    let Some(id) = id else {
        return;
    };
    hide_dropdown(app);
    // Delivery blocks (group sequences run seconds); keep the capture
    // thread's message loop free.
    let app = app.clone();
    std::thread::spawn(move || paste::deliver_clip(&app, &id));
}

pub fn apply_hotkey(app: &AppHandle, old: &str, new: &str) -> Result<(), String> {
    let new_shortcut: Shortcut = new.parse().map_err(|e| format!("bad hotkey: {e:?}"))?;
    if let Ok(old_shortcut) = old.parse::<Shortcut>() {
        let _ = app.global_shortcut().unregister(old_shortcut);
    }
    app.global_shortcut()
        .register(new_shortcut)
        .map_err(|e| e.to_string())?;
    *app.state::<HotkeyState>().0.lock().unwrap() = new_shortcut;
    Ok(())
}

pub fn rebuild_tray_menu(app: &AppHandle) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        let state = app.state::<AppState>();
        let (plain, auto) = {
            let s = state.settings.lock().unwrap();
            (s.plain_text_paste, s.auto_paste)
        };
        let build = || -> tauri::Result<Menu<tauri::Wry>> {
            let show = MenuItem::with_id(&app, "show", "Show clips", true, None::<&str>)?;
            let plain_item = CheckMenuItem::with_id(&app, "plain_text", "Plain-text paste", true, plain, None::<&str>)?;
            let auto_item = CheckMenuItem::with_id(&app, "auto_paste", "Auto-paste on click", true, auto, None::<&str>)?;
            let clear = MenuItem::with_id(&app, "clear", "Clear history", true, None::<&str>)?;
            let prefs = MenuItem::with_id(&app, "prefs", "Preferences…", true, None::<&str>)?;
            let quit = MenuItem::with_id(&app, "quit", "Quit clipandcue", true, None::<&str>)?;
            let sep1 = PredefinedMenuItem::separator(&app)?;
            let sep2 = PredefinedMenuItem::separator(&app)?;
            Menu::with_items(&app, &[&show, &sep1, &plain_item, &auto_item, &clear, &prefs, &sep2, &quit])
        };
        if let (Ok(menu), Some(tray)) = (build(), app.tray_by_id("main")) {
            let _ = tray.set_menu(Some(menu));
        }
    });
}

fn on_menu_event(app: &AppHandle, id: &str) {
    match id {
        "show" => {
            let anchor = app.cursor_position().ok();
            show_dropdown_focused(app, anchor);
        }
        "plain_text" | "auto_paste" => {
            {
                let state = app.state::<AppState>();
                let mut s = state.settings.lock().unwrap();
                if id == "plain_text" {
                    s.plain_text_paste = !s.plain_text_paste;
                } else {
                    s.auto_paste = !s.auto_paste;
                }
                if let Ok(dir) = app.path().app_data_dir() {
                    s.save(&dir);
                }
            }
            rebuild_tray_menu(app);
        }
        "clear" => {
            let state = app.state::<AppState>();
            state.history.lock().unwrap().clear(true);
            capture::emit_history(app);
        }
        "prefs" => {
            if let Some(w) = app.get_webview_window("prefs") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }
        "quit" => app.exit(0),
        _ => {}
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    // Keep this handler trivial and NEVER touch shortcut
                    // registration in here — that deadlocks the dispatch.
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    let main_hotkey = *app.state::<HotkeyState>().0.lock().unwrap();
                    if *shortcut == main_hotkey {
                        toggle_hud(app);
                    }
                })
                .build(),
        )
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let settings = settings::Settings::load(&data_dir);
            let history = history::History::load(data_dir.clone());
            let hotkey: Shortcut = settings
                .hotkey
                .parse()
                .unwrap_or_else(|_| Shortcut::new(Some(tauri_plugin_global_shortcut::Modifiers::CONTROL | tauri_plugin_global_shortcut::Modifiers::ALT), Code::KeyV));

            app.manage(AppState {
                history: Mutex::new(history),
                settings: Mutex::new(settings.clone()),
                suppress: Mutex::new(Vec::new()),
                paste_target: std::sync::atomic::AtomicIsize::new(0),
                hud_mode: std::sync::atomic::AtomicBool::new(false),
                last_seq: std::sync::atomic::AtomicIsize::new(-1),
            });
            app.manage(HotkeyState(Mutex::new(hotkey)));

            app.global_shortcut().register(hotkey)?;

            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("clipandcue")
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| on_menu_event(app, event.id.as_ref()))
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        position,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        let window = dropdown(app);
                        if window.is_visible().unwrap_or(false) {
                            hide_dropdown(app);
                        } else {
                            show_dropdown_focused(app, Some(position));
                        }
                    }
                })
                .build(app)?;

            rebuild_tray_menu(app.handle());
            capture::start(app.handle().clone());

            // Debug-only file trigger so restore can be exercised from test
            // scripts without desktop input: write "restore:<clip-id>" to
            // <app-data>/debug-cmd.txt; result lands in debug-cmd-result.txt.
            #[cfg(debug_assertions)]
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    let Ok(dir) = handle.path().app_data_dir() else {
                        continue;
                    };
                    let f = dir.join("debug-cmd.txt");
                    let Ok(cmd) = std::fs::read_to_string(&f) else {
                        continue;
                    };
                    let _ = std::fs::remove_file(&f);
                    let cmd = cmd.trim();
                    let ok = if let Some(id) = cmd.strip_prefix("restore:") {
                        paste::restore_clip(&handle, id.trim(), false)
                    } else if let Some(rest) = cmd.strip_prefix("group:") {
                        let mut parts = rest.split(':');
                        let (Some(dropped), Some(target)) = (parts.next(), parts.next()) else {
                            continue;
                        };
                        let ok = handle
                            .state::<AppState>()
                            .history
                            .lock()
                            .unwrap()
                            .group(dropped.trim(), target.trim());
                        capture::emit_history(&handle);
                        ok
                    } else if let Some(id) = cmd.strip_prefix("ungroup:") {
                        let ok = handle
                            .state::<AppState>()
                            .history
                            .lock()
                            .unwrap()
                            .ungroup(id.trim());
                        capture::emit_history(&handle);
                        ok
                    } else {
                        false
                    };
                    let _ = std::fs::write(
                        dir.join("debug-cmd-result.txt"),
                        if ok { "ok" } else { "fail" },
                    );
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| match (window.label(), event) {
            ("dropdown", WindowEvent::Focused(false)) => {
                let app = window.app_handle();
                if !app.state::<AppState>().hud_mode.load(Ordering::SeqCst) {
                    hide_dropdown(app);
                }
            }
            ("prefs", WindowEvent::CloseRequested { api, .. }) => {
                api.prevent_close();
                let _ = window.hide();
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_clips,
            commands::get_settings,
            commands::set_settings,
            commands::paste_clip,
            commands::toggle_pin,
            commands::reorder_clip,
            commands::group_clips,
            commands::ungroup_clip,
            commands::delete_clip,
            commands::clear_history,
            commands::get_preview,
            commands::hide_window,
            commands::open_prefs,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                let state = app.state::<AppState>();
                let clear = state.settings.lock().unwrap().clear_on_quit;
                if clear {
                    state.history.lock().unwrap().clear(false);
                }
            }
        });
}
