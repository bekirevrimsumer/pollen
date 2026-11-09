//! Hides the Windows taskbar while Pollen replaces it, and always gives it back.
//!
//! The taskbar is switched to auto-hide (so it reserves no screen space), hidden,
//! parked below its monitor and taken off the shell's list of auto-hide bars (see
//! `drop_autohide_bars`). Its original state is written to a marker file first,
//! so a crash or a killed process can be undone on the next start or with
//! `pollen --restore-taskbar`.
//!
//! Approach ported from Nectar (GPL-3.0-only, https://github.com/kua8/Nectar),
//! `src-tauri/src/utils.rs` `set_taskbar_visibility`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Mutex;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_GETAUTOHIDEBAREX, ABM_GETSTATE,
    ABM_SETAUTOHIDEBAREX, ABM_SETSTATE, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, GetClassNameW, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible, IsZoomed,
    SendMessageTimeoutW, SetWindowPos, ShowWindow, EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_SHOW,
    WINEVENT_OUTOFCONTEXT,
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SW_HIDE, SW_SHOW, WM_SETTINGCHANGE,
};

const ABS_AUTOHIDE: isize = 1;

struct Saved {
    state: isize,
    rects: Vec<(isize, RECT)>,
}

static SAVED: Mutex<Option<Saved>> = Mutex::new(None);
/// Set while Pollen keeps the taskbar hidden; read by the WinEvent callback.
static HIDING: AtomicBool = AtomicBool::new(false);
/// The hook on Explorer's taskbar thread.
static WATCH_HOOK: AtomicIsize = AtomicIsize::new(0);

fn marker() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("APPDATA")?).join("Pollen");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("taskbar-hidden.txt"))
}

fn read_marker() -> Option<isize> {
    std::fs::read_to_string(marker()?).ok()?.trim().strip_prefix("abstate=")?.parse().ok()
}

fn taskbars() -> Vec<HWND> {
    let mut list = Vec::new();
    unsafe {
        if let Ok(primary) = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) {
            if !primary.0.is_null() {
                list.push(primary);
            }
        }
        let mut previous = None;
        while let Ok(next) = FindWindowExW(None, previous, w!("Shell_SecondaryTrayWnd"), PCWSTR::null()) {
            if next.0.is_null() {
                break;
            }
            list.push(next);
            previous = Some(next);
        }
    }
    list
}

fn monitor_of(hwnd: HWND) -> RECT {
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        info.rcMonitor
    }
}

fn current_state() -> isize {
    let mut abd = APPBARDATA { cbSize: std::mem::size_of::<APPBARDATA>() as u32, ..Default::default() };
    unsafe { SHAppBarMessage(ABM_GETSTATE, &mut abd) as isize }
}

fn is_taskbar(hwnd: HWND) -> bool {
    let mut class = [0u16; 32];
    let len = unsafe { GetClassNameW(hwnd, &mut class) }.max(0) as usize;
    matches!(String::from_utf16_lossy(&class[..len]).as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

fn set_state(state: isize) {
    let mut abd = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        lParam: LPARAM(state),
        ..Default::default()
    };
    unsafe { SHAppBarMessage(ABM_SETSTATE, &mut abd) };
}

const EDGES: [u32; 4] = [ABE_LEFT, ABE_TOP, ABE_RIGHT, ABE_BOTTOM];

fn edge_data(hwnd: Option<HWND>, edge: u32, monitor: RECT, register: bool) -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd.unwrap_or_default(),
        uEdge: edge,
        rc: monitor,
        lParam: LPARAM(register as isize),
        ..Default::default()
    }
}

/// Windows Terminal, VS Code, Chrome and other apps stop a maximized window
/// 2 px short of a screen edge that has an auto-hide taskbar, so the mouse can
/// still bring the taskbar up; above the dock that strip showed the wallpaper.
/// Pollen's taskbar never comes up, so it is taken off the shell's list of
/// auto-hide bars (`restore` puts it back). Returns whether anything changed.
fn drop_autohide_bars() -> bool {
    let mut dropped = false;
    for tb in taskbars() {
        let monitor = monitor_of(tb);
        for edge in EDGES {
            let mut query = edge_data(None, edge, monitor, false);
            let bar = unsafe { SHAppBarMessage(ABM_GETAUTOHIDEBAREX, &mut query) };
            if bar != 0 && bar == tb.0 as usize {
                let mut drop = edge_data(Some(tb), edge, monitor, false);
                unsafe { SHAppBarMessage(ABM_SETAUTOHIDEBAREX, &mut drop) };
                dropped = true;
            }
        }
    }
    if dropped {
        refresh_maximized();
    }
    dropped
}

/// Makes maximized windows work out their frame again (without moving), so
/// those that left a strip for an auto-hide taskbar close it.
fn refresh_maximized() {
    for window in crate::apps::app_windows() {
        unsafe {
            if IsZoomed(window.hwnd).as_bool() {
                let _ = SetWindowPos(
                    window.hwnd,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }
}

/// The edge a taskbar sits on.
fn edge_of(tb: HWND) -> u32 {
    let monitor = monitor_of(tb);
    let mut r = RECT::default();
    if unsafe { GetWindowRect(tb, &mut r) }.is_err() {
        return ABE_BOTTOM;
    }
    let vertical = r.bottom - r.top > r.right - r.left;
    match (vertical, r.left <= monitor.left, r.top <= monitor.top) {
        (true, true, _) => ABE_LEFT,
        (true, false, _) => ABE_RIGHT,
        (false, _, true) if r.bottom < monitor.bottom => ABE_TOP,
        _ => ABE_BOTTOM,
    }
}

/// Hides every taskbar. Safe to call repeatedly.
pub fn hide() {
    let Ok(mut saved) = SAVED.lock() else { return };
    if saved.is_none() {
        let current = current_state();
        // After a crash the taskbar is still in our auto-hide state; the user's own
        // state is the one in the marker.
        let state = read_marker().unwrap_or(current);
        if let Some(path) = marker() {
            let _ = std::fs::write(path, format!("abstate={state}"));
        }
        let rects = taskbars()
            .into_iter()
            .filter(|&tb| unsafe { IsWindowVisible(tb).as_bool() })
            .filter_map(|tb| {
                let mut r = RECT::default();
                unsafe { GetWindowRect(tb, &mut r) }.ok()?;
                Some((tb.0 as isize, r))
            })
            .collect();
        *saved = Some(Saved { state, rects });
    }
    drop(saved);
    HIDING.store(true, Ordering::Relaxed);

    // Only when needed: every state change makes Explorer lay the taskbar out
    // again, which shows it.
    if current_state() & ABS_AUTOHIDE == 0 {
        set_state(ABS_AUTOHIDE);
    }
    for tb in taskbars() {
        unsafe {
            let _ = ShowWindow(tb, SW_HIDE);
            // Below its monitor, so the auto-hide strip can never peek out.
            let m = monitor_of(tb);
            let _ = SetWindowPos(tb, None, m.left, m.bottom, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
    drop_autohide_bars();
}

/// Explorer sometimes shows its taskbar again (display changes, Explorer restarts).
pub fn keep_hidden() {
    if !HIDING.load(Ordering::Relaxed) {
        return;
    }
    if current_state() & ABS_AUTOHIDE == 0 {
        hide();
        return;
    }
    for tb in taskbars() {
        unsafe {
            if IsWindowVisible(tb).as_bool() {
                let _ = ShowWindow(tb, SW_HIDE);
            }
        }
    }
    // Explorer signs its taskbar up again when it lays it out anew.
    drop_autohide_bars();
}

/// An auto-hidden taskbar slides up when the mouse touches the bottom edge. This
/// hook on Explorer's taskbar thread hides it again the moment it is shown or
/// moved, before it gets over the dock. Call again after Explorer restarts.
pub fn watch() {
    unwatch();
    unsafe {
        let Ok(tray) = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) else { return };
        if tray.0.is_null() {
            return;
        }
        let mut pid = 0u32;
        let thread = GetWindowThreadProcessId(tray, Some(&mut pid));
        let hook = SetWinEventHook(
            EVENT_OBJECT_SHOW,
            EVENT_OBJECT_LOCATIONCHANGE,
            None,
            Some(on_taskbar_event),
            pid,
            thread,
            WINEVENT_OUTOFCONTEXT,
        );
        WATCH_HOOK.store(hook.0 as isize, Ordering::Relaxed);
    }
}

fn unwatch() {
    let old = WATCH_HOOK.swap(0, Ordering::Relaxed);
    if old != 0 {
        unsafe {
            let _ = UnhookWinEvent(HWINEVENTHOOK(old as *mut _));
        }
    }
}

unsafe extern "system" fn on_taskbar_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object != 0 || hwnd.0.is_null() || !HIDING.load(Ordering::Relaxed) {
        return;
    }
    if (event == EVENT_OBJECT_SHOW || event == EVENT_OBJECT_LOCATIONCHANGE)
        && IsWindowVisible(hwnd).as_bool()
        && is_taskbar(hwnd)
    {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

/// Stops keeping the taskbar hidden for a moment, so it can be shown (fully
/// transparent) to open its tray flyout. Call `resume_hiding` afterwards.
pub fn suspend_hiding() {
    HIDING.store(false, Ordering::Relaxed);
}

/// Hides the taskbar again after `suspend_hiding`, if Pollen is still hiding it.
pub fn resume_hiding() {
    let hidden_by_us = SAVED.lock().map(|s| s.is_some()).unwrap_or(false);
    if hidden_by_us {
        hide();
    }
}

/// Puts the taskbar back the way it was. Does nothing if Pollen did not hide it.
pub fn restore() {
    HIDING.store(false, Ordering::Relaxed);
    unwatch();
    let saved = SAVED.lock().ok().and_then(|mut s| s.take());
    let state = match &saved {
        Some(s) => s.state,
        None => match read_marker() {
            Some(state) => state,
            None => return,
        },
    };

    set_state(state);
    for tb in taskbars() {
        unsafe {
            let mut current = RECT::default();
            let _ = GetWindowRect(tb, &mut current);
            let monitor = monitor_of(tb);
            let original = saved
                .as_ref()
                .and_then(|s| s.rects.iter().find(|(k, _)| *k == tb.0 as isize).map(|(_, r)| *r));
            let (x, y) = match original {
                Some(r) => (r.left, r.top),
                None => (monitor.left, monitor.bottom - (current.bottom - current.top)),
            };
            let _ = SetWindowPos(tb, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            let _ = ShowWindow(tb, SW_SHOW);
            // The user's own taskbar auto-hides: put it back on the shell's list
            // of auto-hide bars (see `drop_autohide_bars`).
            if state & ABS_AUTOHIDE != 0 {
                let mut register = edge_data(Some(tb), edge_of(tb), monitor, true);
                SHAppBarMessage(ABM_SETAUTOHIDEBAREX, &mut register);
            }
        }
    }

    // Lets Explorer re-read its settings and lay the taskbar out again.
    let setting: Vec<u16> = "TraySettings".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(setting.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            500,
            None,
        );
    }
    if let Some(path) = marker() {
        let _ = std::fs::remove_file(path);
    }
}

/// Undoes a hidden taskbar left behind by a session that did not exit cleanly.
pub fn restore_after_crash() {
    if read_marker().is_some() {
        restore();
    }
}
