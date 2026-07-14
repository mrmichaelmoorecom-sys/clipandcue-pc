//! Clipboard format knowledge and low-level Win32 helpers shared by the
//! capture and paste-back sides.

use windows::Win32::Foundation::{HWND, HGLOBAL};
use windows::Win32::System::DataExchange::{
    GetClipboardFormatNameW, GetClipboardOwner, OpenClipboard, RegisterClipboardFormatW,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::PCWSTR;

pub fn standard_format_name(id: u32) -> Option<&'static str> {
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

/// Formats whose handle is a GDI object, not HGLOBAL bytes. We don't store
/// these; a DIB variant coexists in practice (see docs/clipboard-format-survey.md)
/// and Windows synthesizes the GDI forms from DIB on paste.
pub fn is_gdi_handle(id: u32) -> bool {
    matches!(id, 2 | 3 | 9 | 14)
}

/// Owner-display formats only render inside the owner's window; skip.
pub fn is_owner_display(id: u32) -> bool {
    (0x0080..=0x008F).contains(&id)
}

pub fn registered_format_name(id: u32) -> Option<String> {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClipboardFormatNameW(id, &mut buf) };
    (len > 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
}

pub fn format_name(id: u32) -> String {
    standard_format_name(id)
        .map(str::to_string)
        .or_else(|| registered_format_name(id))
        .unwrap_or_else(|| format!("format-{id:#06x}"))
}

fn register(name: &str) -> u32 {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) }
}

/// Privacy exclusion format ids (password managers etc. set these).
pub struct ExclusionFormats {
    pub exclude_monitor: u32,
    pub can_include_history: u32,
    pub can_upload_cloud: u32,
}

pub fn exclusion_formats() -> ExclusionFormats {
    ExclusionFormats {
        exclude_monitor: register("ExcludeClipboardContentFromMonitorProcessing"),
        can_include_history: register("CanIncludeInClipboardHistory"),
        can_upload_cloud: register("CanUploadToCloudClipboard"),
    }
}

/// Copy an HGLOBAL's bytes out. Caller must hold the clipboard open.
pub unsafe fn hglobal_bytes(h: HGLOBAL, max: usize) -> Option<Vec<u8>> {
    let size = unsafe { GlobalSize(h) };
    if size == 0 || size > max {
        return if size == 0 { Some(Vec::new()) } else { None };
    }
    let ptr = unsafe { GlobalLock(h) } as *const u8;
    if ptr.is_null() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec();
    let _ = unsafe { GlobalUnlock(h) };
    Some(bytes)
}

/// Open the clipboard politely: back off first so we never make the writer's
/// own follow-up operations fail, then retry with escalating delays.
pub fn open_clipboard_polite(hwnd: Option<HWND>, initial_delay_ms: u64) -> bool {
    if initial_delay_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(initial_delay_ms));
    }
    for attempt in 0..10u64 {
        if unsafe { OpenClipboard(hwnd) }.is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1)));
    }
    false
}

pub fn window_exe(hwnd: HWND) -> Option<String> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
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
        Some(full.rsplit('\\').next().unwrap_or(&full).to_string())
    }
}

/// Source app of the current clipboard contents: owner window's exe, falling
/// back to the foreground window (OLE writers often own no window).
pub fn clipboard_source_exe() -> (Option<String>, &'static str) {
    let own_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));
    let not_self = |exe: Option<String>| exe.filter(|e| Some(e) != own_exe.as_ref());
    unsafe {
        if let Ok(owner) = GetClipboardOwner() {
            if let Some(exe) = not_self(window_exe(owner)) {
                return (Some(exe), "owner");
            }
        }
        (not_self(window_exe(GetForegroundWindow())), "foreground")
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
