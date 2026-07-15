//! Event-driven clipboard capture engine.
//!
//! A message-only window listens via AddClipboardFormatListener. On each
//! update (after self-write suppression and duplicate-event dedupe) it
//! snapshots every HGLOBAL format on the clipboard, applies privacy and size
//! policies, and persists the clip. Rules baked in from the Mac version and
//! the phase-1 format survey — see docs/clipboard-format-survey.md.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::Ordering;
use std::sync::OnceLock;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostMessageW, RegisterClassW,
    TranslateMessage, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
    WM_CLIPBOARDUPDATE, WM_HOTKEY, WNDCLASSW,
};

use crate::formats::{
    clipboard_source_exe, exclusion_formats, format_name, hglobal_bytes, is_gdi_handle,
    is_owner_display, now_ms, open_clipboard_polite,
};
use crate::history::{dib_to_bmp, file_name_of, hdrop_paths, ClipKind, ClipMeta, FormatMeta, RawFormat};
use crate::state::AppState;

static APP: OnceLock<AppHandle> = OnceLock::new();
static CAPTURE_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// HUD digit hotkeys live on THIS thread's message loop (plain RegisterHotKey),
/// never on the global-shortcut plugin: registering/unregistering shortcuts
/// from inside that plugin's own handler dispatch deadlocks the main thread.
const MSG_REGISTER_DIGITS: u32 = WM_APP + 1;
const MSG_UNREGISTER_DIGITS: u32 = WM_APP + 2;
const HOTKEY_ESCAPE_ID: i32 = 10;

pub fn post_register_digits(count: usize) {
    let hwnd = CAPTURE_HWND.load(std::sync::atomic::Ordering::SeqCst);
    if hwnd != 0 {
        let _ = unsafe {
            PostMessageW(
                Some(HWND(hwnd as *mut _)),
                MSG_REGISTER_DIGITS,
                WPARAM(count.clamp(1, 9)),
                LPARAM(0),
            )
        };
    }
}

pub fn post_unregister_digits() {
    let hwnd = CAPTURE_HWND.load(std::sync::atomic::Ordering::SeqCst);
    if hwnd != 0 {
        let _ = unsafe {
            PostMessageW(Some(HWND(hwnd as *mut _)), MSG_UNREGISTER_DIGITS, WPARAM(0), LPARAM(0))
        };
    }
}

const CF_TEXT: u32 = 1;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const CF_DIB: u32 = 8;
const CF_DIBV5: u32 = 17;

enum Snapshot {
    Clip(Vec<RawFormat>, u32),
    /// Excluded by a privacy format.
    Excluded,
    /// A format exceeded the size cap (name, size).
    TooBig(String, u64),
    Empty,
    Failed,
}

fn snapshot(hwnd: HWND, skip_excluded: bool, max_bytes: usize) -> Snapshot {
    unsafe {
        // Back off before opening: opening too soon makes the WRITER's own
        // follow-up operations fail (survey finding, reproduced twice).
        if !open_clipboard_polite(Some(hwnd), 150) {
            return Snapshot::Failed;
        }
        let seq = GetClipboardSequenceNumber();
        let excl = exclusion_formats();

        let mut format_ids = Vec::new();
        let mut fmt = 0u32;
        loop {
            fmt = EnumClipboardFormats(fmt);
            if fmt == 0 {
                break;
            }
            format_ids.push(fmt);
        }

        if format_ids.is_empty() {
            let _ = CloseClipboard();
            return Snapshot::Empty;
        }

        // Privacy: respect clipboard-manager exclusion formats.
        if skip_excluded {
            let flagged = format_ids.iter().any(|&f| {
                if f == excl.exclude_monitor || f == excl.can_upload_cloud {
                    // Presence alone signals exclusion.
                    return true;
                }
                if f == excl.can_include_history {
                    // DWORD payload: 0 = do not include.
                    if let Ok(h) = GetClipboardData(f) {
                        if let Some(b) = hglobal_bytes(
                            windows::Win32::Foundation::HGLOBAL(h.0 as *mut _),
                            16,
                        ) {
                            return b.len() >= 4 && u32::from_le_bytes(b[..4].try_into().unwrap()) == 0;
                        }
                    }
                }
                false
            });
            if flagged {
                let _ = CloseClipboard();
                return Snapshot::Excluded;
            }
        }

        let mut raws = Vec::new();
        for &f in &format_ids {
            if is_gdi_handle(f) || is_owner_display(f) {
                continue; // DIB variants carry the pixels; owner-display can't render here
            }
            let Ok(handle) = GetClipboardData(f) else {
                continue; // delayed render refused (Photoshop private formats do this)
            };
            if handle.is_invalid() {
                continue;
            }
            let h = windows::Win32::Foundation::HGLOBAL(handle.0 as *mut _);
            match hglobal_bytes(h, max_bytes) {
                Some(bytes) => raws.push(RawFormat { id: f, name: format_name(f), bytes }),
                None => {
                    // Over cap: per spec the whole clip is rejected.
                    let size = windows::Win32::System::Memory::GlobalSize(h) as u64;
                    let name = format_name(f);
                    let _ = CloseClipboard();
                    return Snapshot::TooBig(name, size);
                }
            }
        }
        let _ = CloseClipboard();

        // OLE writers put plumbing-only passes on the clipboard before the
        // real content (survey: first pass is just DataObject + Ole Private
        // Data). A clip with no substantive format has no paste value.
        const PLUMBING: &[&str] = &[
            "DataObject",
            "Ole Private Data",
            "Object Descriptor",
            "Link Source Descriptor",
            "OwnerLink",
            "ObjectLink",
        ];
        if raws.is_empty() || raws.iter().all(|r| PLUMBING.contains(&r.name.as_str())) {
            Snapshot::Empty
        } else {
            Snapshot::Clip(raws, seq)
        }
    }
}

fn classify(raws: &[RawFormat]) -> ClipKind {
    let has = |id: u32| raws.iter().any(|r| r.id == id);
    let has_name = |n: &str| raws.iter().any(|r| r.name == n);
    if has(CF_HDROP) {
        ClipKind::Files
    } else if has(CF_DIB) || has(CF_DIBV5) || has_name("PNG") {
        ClipKind::Image
    } else if has(CF_UNICODETEXT) || has(CF_TEXT) {
        ClipKind::Text
    } else {
        ClipKind::Other
    }
}

fn text_preview(raws: &[RawFormat]) -> Option<String> {
    let uni = raws.iter().find(|r| r.id == CF_UNICODETEXT)?;
    let units: Vec<u16> = uni
        .bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    let s = String::from_utf16_lossy(&units);
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(200).collect())
}

fn files_preview(raws: &[RawFormat]) -> Option<String> {
    let drop = raws.iter().find(|r| r.id == CF_HDROP)?;
    let paths = hdrop_paths(&drop.bytes);
    if paths.is_empty() {
        return None;
    }
    let names: Vec<String> = paths.iter().take(4).map(|p| file_name_of(p)).collect();
    let extra = paths.len().saturating_sub(4);
    Some(if extra > 0 {
        format!("{} +{extra} more", names.join(", "))
    } else {
        names.join(", ")
    })
}

/// (bytes, file name) for the image preview blob, preferring real PNG.
fn image_preview(raws: &[RawFormat]) -> Option<(Vec<u8>, String)> {
    if let Some(png) = raws.iter().find(|r| r.name == "PNG" || r.name == "image/png") {
        if !png.bytes.is_empty() && png.bytes.len() < 8 * 1024 * 1024 {
            return Some((png.bytes.clone(), "preview.png".into()));
        }
    }
    let dib = raws.iter().find(|r| r.id == CF_DIB)?;
    if dib.bytes.len() > 8 * 1024 * 1024 {
        return None;
    }
    dib_to_bmp(&dib.bytes).map(|b| (b, "preview.bmp".into()))
}

fn content_hash(raws: &[RawFormat]) -> u64 {
    let mut h = DefaultHasher::new();
    for r in raws {
        r.id.hash(&mut h);
        r.bytes.hash(&mut h);
    }
    h.finish()
}

#[derive(Serialize, Clone)]
struct SurveyFormat<'a> {
    id: u32,
    name: &'a str,
    size_bytes: usize,
}

fn survey_log(app: &AppHandle, seq: u32, source: &Option<String>, raws: &[RawFormat]) {
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let entry = serde_json::json!({
            "timestamp": format!("unix_ms:{}", now_ms()),
            "sequence": seq,
            "source_exe": source,
            "formats": raws.iter().map(|r| SurveyFormat { id: r.id, name: &r.name, size_bytes: r.bytes.len() }).collect::<Vec<_>>(),
        });
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("clipboard-survey.jsonl"))
        {
            let _ = writeln!(f, "{entry}");
        }
    }
}

pub fn emit_history(app: &AppHandle) {
    let state = app.state::<AppState>();
    let view = state.history.lock().unwrap().view();
    let _ = app.emit("history-updated", view);
}

fn notify(app: &AppHandle, body: &str) {
    let _ = app
        .notification()
        .builder()
        .title("clipandcue")
        .body(body)
        .show();
}

fn handle_update(app: &AppHandle, hwnd: HWND) {
    let state = app.state::<AppState>();

    let seq_now = unsafe { GetClipboardSequenceNumber() };

    // Self-write suppression: sequence ranges recorded by paste-back.
    {
        let mut ranges = state.suppress.lock().unwrap();
        if let Some(pos) = ranges.iter().position(|&(a, b)| seq_now > a && seq_now <= b) {
            // Keep the range until well past it — several WM_CLIPBOARDUPDATE
            // messages can arrive for one write.
            let _ = pos;
            return;
        }
        ranges.retain(|&(_, b)| seq_now <= b.wrapping_add(64));
    }

    // Duplicate events for the same write (observed 2-3 per copy).
    if state.last_seq.swap(seq_now as isize, Ordering::SeqCst) == seq_now as isize {
        return;
    }

    let (skip_excluded, max_bytes, cap, do_survey) = {
        let s = state.settings.lock().unwrap();
        (s.skip_excluded, s.max_format_bytes(), s.history_cap, s.survey_log)
    };

    let result = snapshot(hwnd, skip_excluded, max_bytes);

    match result {
        Snapshot::Clip(raws, seq) => {
            let (source_exe, _via) = clipboard_source_exe();
            if do_survey {
                survey_log(app, seq, &source_exe, &raws);
            }

            let hash = content_hash(&raws);
            let kind = classify(&raws);
            let ts = now_ms();

            let mut history = state.history.lock().unwrap();

            // Consecutive duplicate (apps re-setting identical content).
            if let Some(newest) = history.newest_unpinned() {
                if newest.hash == hash {
                    return;
                }
            }

            let preview = image_preview(&raws);
            let meta = ClipMeta {
                id: format!("{ts}-{seq}"),
                ts_ms: ts,
                source_exe,
                pinned: false,
                kind,
                preview_text: match kind {
                    ClipKind::Files => files_preview(&raws),
                    _ => text_preview(&raws),
                },
                preview_image: preview.as_ref().map(|(_, n)| n.clone()),
                formats: raws
                    .iter()
                    .map(|r| FormatMeta { id: r.id, name: r.name.clone(), size: r.bytes.len() as u64 })
                    .collect(),
                hash,
            };

            // Screenshot tools write in multiple passes: collapse image clips
            // arriving within ~1.2s into one entry (replace, not stack).
            let collapse = kind == ClipKind::Image
                && history
                    .newest_unpinned()
                    .map(|n| n.kind == ClipKind::Image && ts.saturating_sub(n.ts_ms) < 1200)
                    .unwrap_or(false);

            if collapse {
                history.replace_newest_unpinned(meta, &raws, preview.map(|(b, _)| b), cap);
            } else {
                history.insert(meta, &raws, preview.map(|(b, _)| b), cap);
            }
            drop(history);
            emit_history(app);
        }
        Snapshot::TooBig(name, size) => {
            let mb = size / (1024 * 1024);
            notify(app, &format!("Copy too big, not saved ({name}, {mb} MB)"));
        }
        Snapshot::Excluded | Snapshot::Empty | Snapshot::Failed => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CLIPBOARDUPDATE => {
            if let Some(app) = APP.get() {
                handle_update(app, hwnd);
            }
            LRESULT(0)
        }
        MSG_REGISTER_DIGITS => {
            let count = wparam.0.clamp(1, 9);
            for i in 1..=count {
                // VK '1'..'9' = 0x31..0x39, no modifiers.
                let _ = unsafe {
                    RegisterHotKey(Some(hwnd), i as i32, MOD_NOREPEAT, 0x30 + i as u32)
                };
            }
            let _ = unsafe {
                RegisterHotKey(Some(hwnd), HOTKEY_ESCAPE_ID, HOT_KEY_MODIFIERS(MOD_NOREPEAT.0), 0x1B)
            };
            LRESULT(0)
        }
        MSG_UNREGISTER_DIGITS => {
            for id in 1..=HOTKEY_ESCAPE_ID {
                let _ = unsafe { UnregisterHotKey(Some(hwnd), id) };
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            if let Some(app) = APP.get() {
                let id = wparam.0 as i32;
                if id == HOTKEY_ESCAPE_ID {
                    crate::hide_dropdown(app);
                } else if (1..=9).contains(&id) {
                    crate::paste_nth(app, (id - 1) as usize);
                }
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub fn start(app: AppHandle) {
    let _ = APP.set(app);
    std::thread::spawn(|| unsafe {
        let class_name: Vec<u16> = "clipandcue_capture\0".encode_utf16().collect();
        let hinstance = GetModuleHandleW(PCWSTR::null()).expect("GetModuleHandleW failed");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance.into()),
            None,
        )
        .expect("capture window creation failed");
        CAPTURE_HWND.store(hwnd.0 as isize, std::sync::atomic::Ordering::SeqCst);
        AddClipboardFormatListener(hwnd).expect("AddClipboardFormatListener failed");

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
}
