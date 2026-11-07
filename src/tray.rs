//! Tray (notification area) icons, through Windows' own "hidden icons" flyout.
//!
//! The taskbar is hidden, so its flyout cannot be reached directly. When the
//! dock's tray button is clicked, the taskbar is shown fully transparent and
//! click-through, moved so that its own "hidden icons" button sits right under
//! the dock's button, and Win+B, Enter open its flyout. Windows then places the
//! flyout above the dock by itself (moving the flyout afterwards makes Windows 11
//! close it). Once the flyout closes the taskbar is hidden again. The icons, their
//! menus and their tooltips are all Windows' own.
//!
//! Approach ported from Nectar (GPL-3.0-only, https://github.com/kua8/Nectar),
//! `src-tauri/src/tray_icons.rs`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_B, VK_ESCAPE, VK_LWIN, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetAncestor, GetClassNameW, GetForegroundWindow, GetShellWindow, GetWindowLongW, GetWindowRect,
    GetWindowThreadProcessId, IsWindow, IsWindowVisible, SetForegroundWindow, SetLayeredWindowAttributes,
    SetWindowLongW, SetWindowPos, ShowWindow, GA_ROOT, GWL_EXSTYLE, HWND_TOPMOST, LWA_ALPHA, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNA, WS_EX_LAYERED, WS_EX_TRANSPARENT,
};

use crate::shell::press_keys;
use crate::taskbar;

/// A tray open is in progress (it runs on its own thread for a while).
static BUSY: AtomicBool = AtomicBool::new(false);

macro_rules! log {
    ($($arg:tt)*) => {
        #[cfg(debug_assertions)]
        eprintln!("[tray {:>5} ms] {}", crate::tray::since_start(), format!($($arg)*));
    };
}

#[cfg(debug_assertions)]
pub fn since_start() -> u128 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis()
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..len])
}

fn rect_of(hwnd: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    r
}

/// The overflow flyout: a XAML island on Windows 11, the classic window before.
fn flyout_window() -> Option<HWND> {
    unsafe {
        FindWindowW(w!("TopLevelWindowForOverflowXamlIsland"), PCWSTR::null())
            .or_else(|_| FindWindowW(w!("NotifyIconOverflowWindow"), PCWSTR::null()))
            .ok()
            .filter(|h| !h.0.is_null())
    }
}

fn visible_flyout() -> Option<HWND> {
    flyout_window().filter(|&h| unsafe { IsWindowVisible(h).as_bool() })
}

fn taskbar_has_focus() -> bool {
    let fg = unsafe { GetForegroundWindow() };
    !fg.0.is_null() && matches!(class_name(fg).as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

fn flyout_has_focus() -> bool {
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() {
        return true;
    }
    let root = unsafe { GetAncestor(fg, GA_ROOT) };
    let root = if root.0.is_null() { fg } else { root };
    matches!(
        class_name(root).as_str(),
        "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "TopLevelWindowForOverflowXamlIsland" | "NotifyIconOverflowWindow"
    )
}

/// Opens the flyout above `anchor` (screen point: the button's center, the
/// dock's top edge), or closes it if it is open.
pub fn toggle(anchor: (i32, i32)) {
    if let Some(flyout) = visible_flyout() {
        log!("toggle: flyout open, closing it");
        close(flyout);
        return;
    }
    if BUSY.swap(true, Ordering::SeqCst) {
        log!("toggle: an open is still running");
        return;
    }
    let previous = unsafe { GetForegroundWindow() }.0 as isize;
    std::thread::spawn(move || {
        open(anchor, HWND(previous as *mut _));
        BUSY.store(false, Ordering::SeqCst);
    });
}

fn close(flyout: HWND) {
    unsafe {
        if GetForegroundWindow() == flyout {
            press_keys(&[VK_ESCAPE]);
        } else {
            let _ = ShowWindow(flyout, SW_HIDE);
        }
    }
}

/// Shows the hidden taskbar fully transparent and click-through; undoes it all,
/// including hiding the taskbar again, when dropped.
struct InvisibleTaskbar {
    tray: HWND,
    original_ex: i32,
}

impl InvisibleTaskbar {
    fn show(tray: HWND) -> Self {
        taskbar::suspend_hiding();
        unsafe {
            let original_ex = GetWindowLongW(tray, GWL_EXSTYLE);
            SetWindowLongW(tray, GWL_EXSTYLE, original_ex | WS_EX_LAYERED.0 as i32 | WS_EX_TRANSPARENT.0 as i32);
            let _ = SetLayeredWindowAttributes(tray, COLORREF(0), 0, LWA_ALPHA);
            let _ = ShowWindow(tray, SW_SHOWNA);
            InvisibleTaskbar { tray, original_ex }
        }
    }
}

impl Drop for InvisibleTaskbar {
    fn drop(&mut self) {
        unsafe {
            let _ = ShowWindow(self.tray, SW_HIDE);
            SetWindowLongW(self.tray, GWL_EXSTYLE, self.original_ex);
        }
        taskbar::resume_hiding();
    }
}

fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn open(anchor: (i32, i32), previous: HWND) {
    let Some(tray) = (unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }).ok().filter(|h| !h.0.is_null())
    else {
        return;
    };
    log!("open: anchor {anchor:?}");
    let invisible = InvisibleTaskbar::show(tray);

    std::thread::sleep(Duration::from_millis(60));

    // Win+B focuses the notification area on its "hidden icons" button; Enter opens it.
    press_keys(&[VK_LWIN, VK_B]);
    let mut opened = false;
    if wait_until(Duration::from_millis(600), taskbar_has_focus) {
        log!("taskbar focused at {:?}", rect_of(tray));
        press_keys(&[VK_RETURN]);
        opened = wait_until(Duration::from_millis(600), || visible_flyout().is_some());
    } else {
        log!("taskbar never took focus");
    }

    if let (true, Some(flyout)) = (opened, flyout_window()) {
        log!("flyout open at {:?}, taskbar {:?}", rect_of(flyout), rect_of(tray));
        // Windows opens it above the taskbar's own button; move it over the dock's.
        // Kept inside the dock's monitor: centered on a button near the screen's
        // edge it would otherwise spill onto the next monitor.
        let first = rect_of(flyout);
        let (w, h) = (first.right - first.left, first.bottom - first.top);
        let screen = monitor_at(anchor);
        let x = (anchor.0 - w / 2).clamp(screen.left + 8, (screen.right - w - 8).max(screen.left + 8));
        let y = (anchor.1 - h - 6).max(screen.top + 8);
        let settle = Instant::now();
        while settle.elapsed() < Duration::from_millis(500) && visible_flyout().is_some() {
            let r = rect_of(flyout);
            if (r.left, r.top) != (x, y) {
                unsafe {
                    let _ = SetWindowPos(flyout, Some(HWND_TOPMOST), x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
                }
                log!("flyout was at ({},{}), moved to ({x},{y}), now {:?}", r.left, r.top, rect_of(flyout));
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        // Keep it above the dock (both are topmost) until it closes; Windows closes
        // it when it loses focus or an icon is picked.
        let start = Instant::now();
        let mut last = rect_of(flyout);
        while start.elapsed() < Duration::from_secs(600) {
            if visible_flyout().is_none() {
                log!("flyout closed after {} ms", start.elapsed().as_millis());
                break;
            }
            let now = rect_of(flyout);
            if now != last {
                log!("flyout moved to {now:?}");
                last = now;
            }
            unsafe {
                if flyout_has_focus() {
                    let _ = SetWindowPos(
                        flyout,
                        Some(HWND_TOPMOST),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    } else {
        log!("flyout did not open");
    }

    // The invisible taskbar may still hold the focus; give it back.
    if wait_until(Duration::from_millis(200), taskbar_has_focus) {
        restore_foreground(previous);
    }
    drop(invisible);
    log!("done");
}

fn monitor_at(point: (i32, i32)) -> RECT {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: point.0, y: point.1 }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        info.rcMonitor
    }
}

fn restore_foreground(target: HWND) {
    unsafe {
        let target = if !target.0.is_null() && IsWindow(Some(target)).as_bool() { target } else { GetShellWindow() };
        if SetForegroundWindow(target).as_bool() {
            return;
        }
        let ours = GetCurrentThreadId();
        let theirs = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let attached = theirs != 0 && theirs != ours && AttachThreadInput(ours, theirs, true).as_bool();
        let _ = SetForegroundWindow(target);
        if attached {
            let _ = AttachThreadInput(ours, theirs, false);
        }
    }
}
