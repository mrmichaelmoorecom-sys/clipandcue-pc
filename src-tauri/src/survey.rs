//! Clipboard format survey tool (Phase 1, step 2 of the brief).
//!
//! Listens for clipboard changes via `AddClipboardFormatListener` (event-driven,
//! no polling) and on each change enumerates EVERY format present, recording
//! id, name, and byte size. Results go to a JSONL log in the app data dir and
//! are emitted to the frontend as `clipboard-survey` events.

use std::sync::OnceLock;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardFormatNameW, GetClipboardOwner, GetClipboardSequenceNumber, OpenClipboard,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::GlobalSize;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowThreadProcessId,
    RegisterClassW, TranslateMessage, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_CLIPBOARDUPDATE, WNDCLASSW,
};

static APP: OnceLock<AppHandle> = OnceLock::new();

#[derive(Serialize, Clone)]
pub struct FormatEntry {
    pub id: u32,
    pub id_hex: String,
    pub name: String,
    pub kind: &'static str, // "standard" | "registered" | "private"
    pub size_bytes: Option<u64>,
    pub note: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct SurveyEvent {
    pub timestamp: String,
    pub sequence: u32,
    pub source_exe: Option<String>,
    pub formats: Vec<FormatEntry>,
}

/// Standard (predefined) clipboard format names, keyed by id.
fn standard_format_name(id: u32) -> Option<&'static str> {
    Some(match id {
        1 => "CF_TEXT",
        2 => "CF_BITMAP",
        3 => "CF_METAFILEPICT",
        4 => "CF_SYLK",
        5 => "CF_DIF",
        6 => "CF_TIFF",
        7 => "CF_OEMTEXT",
        8 => "CF_DIB",
        9 => "CF_PALETTE",
        10 => "CF_PENDATA",
        11 => "CF_RIFF",
        12 => "CF_WAVE",
        13 => "CF_UNICODETEXT",
        14 => "CF_ENHMETAFILE",
        15 => "CF_HDROP",
        16 => "CF_LOCALE",
        17 => "CF_DIBV5",
        0x0080 => "CF_OWNERDISPLAY",
        0x0081 => "CF_DSPTEXT",
        0x0082 => "CF_DSPBITMAP",
        0x0083 => "CF_DSPMETAFILEPICT",
        0x008E => "CF_DSPENHMETAFILE",
        _ => return None,
    })
}

/// Formats whose clipboard handle is NOT an HGLOBAL — GlobalSize would be meaningless.
fn is_non_hglobal(id: u32) -> bool {
    matches!(id, 2 | 3 | 9 | 14) // CF_BITMAP, CF_METAFILEPICT, CF_PALETTE, CF_ENHMETAFILE
}

fn registered_format_name(id: u32) -> Option<String> {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClipboardFormatNameW(id, &mut buf) };
    if len > 0 {
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    } else {
        None
    }
}

/// Resolve the exe name of the window that owns the clipboard (the copier).
fn clipboard_owner_exe() -> Option<String> {
    unsafe {
        let owner = GetClipboardOwner().ok()?;
        let mut pid = 0u32;
        GetWindowThreadProcessId(owner, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = windows::Win32::Foundation::CloseHandle(process);
        ok.ok()?;
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        Some(
            full.rsplit('\\')
                .next()
                .unwrap_or(&full)
                .to_string(),
        )
    }
}

fn survey_clipboard(hwnd: HWND) -> Option<SurveyEvent> {
    unsafe {
        // The writing app may still hold the clipboard; retry briefly.
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(Some(hwnd)).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        if !opened {
            return None;
        }

        let sequence = GetClipboardSequenceNumber();
        let source_exe = clipboard_owner_exe();
        let mut formats = Vec::new();
        let mut fmt = 0u32;
        loop {
            fmt = EnumClipboardFormats(fmt);
            if fmt == 0 {
                break;
            }
            let (name, kind) = match standard_format_name(fmt) {
                Some(n) => (n.to_string(), "standard"),
                None => match registered_format_name(fmt) {
                    Some(n) => (n, "registered"),
                    None if (0x0200..0x0300).contains(&fmt) => {
                        (format!("private-{fmt:#06x}"), "private")
                    }
                    None => (format!("unknown-{fmt:#06x}"), "private"),
                },
            };
            let (size_bytes, note) = if is_non_hglobal(fmt) {
                (None, Some("non-HGLOBAL handle (GDI object)".to_string()))
            } else {
                match GetClipboardData(fmt) {
                    Ok(handle) if !handle.is_invalid() => {
                        let size = GlobalSize(windows::Win32::Foundation::HGLOBAL(
                            handle.0 as *mut core::ffi::c_void,
                        ));
                        (Some(size as u64), None)
                    }
                    _ => (None, Some("GetClipboardData failed (delayed render?)".to_string())),
                }
            };
            formats.push(FormatEntry {
                id: fmt,
                id_hex: format!("{fmt:#06x}"),
                name,
                kind,
                size_bytes,
                note,
            });
        }
        let _ = CloseClipboard();

        Some(SurveyEvent {
            timestamp: chrono_free_timestamp(),
            sequence,
            source_exe,
            formats,
        })
    }
}

/// ISO-8601-ish local timestamp without pulling in chrono.
fn chrono_free_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("unix:{}.{:03}", now.as_secs(), now.subsec_millis())
}

fn log_event(app: &AppHandle, event: &SurveyEvent) {
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("clipboard-survey.jsonl");
        if let Ok(line) = serde_json::to_string(event) {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                let _ = writeln!(f, "{line}");
            }
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_CLIPBOARDUPDATE {
        if let Some(app) = APP.get() {
            if let Some(event) = survey_clipboard(hwnd) {
                log_event(app, &event);
                let _ = app.emit("clipboard-survey", &event);
            }
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Spawn the listener thread: a message-only window with a clipboard format
/// listener attached, pumping messages forever.
pub fn start(app: AppHandle) {
    let _ = APP.set(app);
    std::thread::spawn(|| unsafe {
        let class_name: Vec<u16> = "clipandcue_survey\0".encode_utf16().collect();
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
        .expect("survey window creation failed");
        AddClipboardFormatListener(hwnd).expect("AddClipboardFormatListener failed");

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
}
