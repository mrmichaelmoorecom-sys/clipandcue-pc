mod survey;

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, PhysicalPosition, WebviewWindow, WindowEvent,
};
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut, ShortcutState};

fn dropdown(app: &AppHandle) -> WebviewWindow {
    app.get_webview_window("dropdown")
        .expect("dropdown window missing")
}

fn hide_dropdown(window: &WebviewWindow) {
    let _ = window.hide();
}

fn show_dropdown_at(window: &WebviewWindow, anchor: Option<PhysicalPosition<f64>>) {
    if let Some(pos) = anchor {
        if let (Ok(size), Ok(Some(monitor))) = (window.outer_size(), window.current_monitor()) {
            let work = monitor.size();
            // Anchor the dropdown near the click, clamped on-screen and above the taskbar.
            let x = (pos.x - size.width as f64 / 2.0)
                .clamp(0.0, (work.width.saturating_sub(size.width)) as f64);
            let y = (pos.y - size.height as f64 - 8.0).max(0.0);
            let _ = window.set_position(PhysicalPosition::new(x, y));
        }
    }
    let _ = window.show();
    let _ = window.set_focus();
}

fn toggle_dropdown(window: &WebviewWindow, anchor: Option<PhysicalPosition<f64>>) {
    if window.is_visible().unwrap_or(false) {
        hide_dropdown(window);
    } else {
        show_dropdown_at(window, anchor);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let hotkey = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyV);

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_shortcut(hotkey)
                .expect("failed to parse hotkey")
                .with_handler(move |app, shortcut, event| {
                    if *shortcut == hotkey && event.state() == ShortcutState::Pressed {
                        toggle_dropdown(&dropdown(app), None);
                    }
                })
                .build(),
        )
        .setup(|app| {
            let show = MenuItem::with_id(app, "show", "Show clips", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit clipandcue", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;

            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("clipandcue")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_dropdown_at(&dropdown(app), None),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        position,
                        ..
                    } = event
                    {
                        toggle_dropdown(&dropdown(tray.app_handle()), Some(position));
                    }
                })
                .build(app)?;

            survey::start(app.handle().clone());

            Ok(())
        })
        .on_window_event(|window, event| {
            // Dropdown behaves like a menu: clicking elsewhere dismisses it.
            if window.label() == "dropdown" {
                if let WindowEvent::Focused(false) = event {
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
