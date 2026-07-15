//! Paste-back: restore every captured format to the clipboard, suppress our
//! own write via sequence numbers, then optionally focus the previous window
//! and synthesize Ctrl+V.

use std::sync::atomic::Ordering;

use tauri::{AppHandle, Manager};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::{IsWindow, SetForegroundWindow};

use crate::formats::open_clipboard_polite;
use crate::state::AppState;

const CF_UNICODETEXT: u32 = 13;
const CF_LOCALE: u32 = 16;

/// OLE bookkeeping formats. Captured for the record, but restoring stale
/// copies poisons OLE's view of the clipboard: consumers that paste via
/// OleGetClipboard (Office, Adobe, .NET apps) then see almost no formats.
/// Verified empirically — with these restored, WinForms GetFormats() saw 1
/// of 6 formats; without them, all content formats.
const OLE_PLUMBING: &[&str] = &["DataObject", "Ole Private Data", "OwnerLink", "ObjectLink"];

fn alloc_hglobal(bytes: &[u8]) -> Option<HGLOBAL> {
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)).ok()?;
        let ptr = GlobalLock(h) as *mut u8;
        if ptr.is_null() {
            return None;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        let _ = GlobalUnlock(h);
        Some(h)
    }
}

/// Restore a clip's stored formats to the clipboard. Returns true on success.
pub fn restore_clip(app: &AppHandle, clip_id: &str, plain_text: bool) -> bool {
    let state = app.state::<AppState>();

    let formats: Vec<(u32, Vec<u8>)> = {
        let history = state.history.lock().unwrap();
        let Some(meta) = history.find(clip_id) else {
            return false;
        };
        meta.formats
            .iter()
            .filter(|f| !OLE_PLUMBING.contains(&f.name.as_str()))
            .filter(|f| !plain_text || f.id == CF_UNICODETEXT || f.id == CF_LOCALE)
            .filter_map(|f| history.load_blob(clip_id, f.id).map(|b| (f.id, b)))
            .collect()
    };
    if formats.is_empty() {
        return false;
    }

    unsafe {
        if !open_clipboard_polite(None, 0) {
            return false;
        }
        let seq_before = GetClipboardSequenceNumber();
        if EmptyClipboard().is_err() {
            let _ = CloseClipboard();
            return false;
        }
        let mut wrote = 0usize;
        for (id, bytes) in &formats {
            if let Some(h) = alloc_hglobal(bytes) {
                if SetClipboardData(*id, Some(HANDLE(h.0))).is_err() {
                    // Ownership stays with us on failure; free it.
                    let _ = windows::Win32::Foundation::GlobalFree(Some(h));
                } else {
                    wrote += 1;
                }
            }
        }
        let _ = CloseClipboard();
        let seq_after = GetClipboardSequenceNumber();

        // Every sequence bump in (before, after] is ours — ignore those updates.
        state.suppress.lock().unwrap().push((seq_before, seq_after));

        wrote > 0
    }
}

/// Focus the recorded paste target window.
fn focus_target(app: &AppHandle) {
    let state = app.state::<AppState>();
    let hwnd = HWND(state.paste_target.load(Ordering::SeqCst) as *mut _);
    unsafe {
        if !hwnd.0.is_null() && IsWindow(Some(hwnd)).as_bool() {
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

fn send_ctrl_v() {
    unsafe {
        let key = |vk, up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() },
                    ..Default::default()
                },
            },
        };
        let inputs = [
            key(VK_CONTROL, false),
            key(VK_V, false),
            key(VK_V, true),
            key(VK_CONTROL, true),
        ];
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// Deliver a clip: single clips restore + (optionally) auto-paste; groups
/// paste every child sequentially, Mac-style — write child, Ctrl+V, wait.
/// Illustrator needs the ~500ms between cycles (Mac lesson). Blocking —
/// call from a dedicated thread, never the UI or capture thread.
pub fn deliver_clip(app: &AppHandle, id: &str) {
    let state = app.state::<AppState>();
    let (plain, auto) = {
        let s = state.settings.lock().unwrap();
        (s.plain_text_paste, s.auto_paste)
    };
    let child_ids: Vec<String> = {
        let h = state.history.lock().unwrap();
        h.get(id)
            .filter(|c| c.kind == crate::history::ClipKind::Group)
            .map(|g| g.children.iter().map(|k| k.id.clone()).collect())
            .unwrap_or_default()
    };

    if child_ids.is_empty() {
        if restore_clip(app, id, plain) && auto {
            focus_target(app);
            std::thread::sleep(std::time::Duration::from_millis(80));
            send_ctrl_v();
        }
        return;
    }

    if !auto {
        // Without auto-paste there is no sequence to run: put the first
        // child on the clipboard so a manual Ctrl+V gets something usable.
        let _ = restore_clip(app, &child_ids[0], plain);
        return;
    }

    focus_target(app);
    std::thread::sleep(std::time::Duration::from_millis(180));
    for kid in child_ids {
        if restore_clip(app, &kid, plain) {
            send_ctrl_v();
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
}
