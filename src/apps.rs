//! Pinned taskbar apps and running app windows.
//!
//! Reading the taskbar pins (the `User Pinned\TaskBar` shortcuts, ordered by the
//! `Taskband\Favorites` blob) is ported from Nectar (GPL-3.0-only,
//! https://github.com/kua8/Nectar), `src-tauri/src/taskbar_pins.rs`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use windows::core::{Interface, BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, PROPERTYKEY, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, WIN32_FIND_DATAW,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IBindCtx, IPersistFile, CLSCTX_ALL, STGM};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_BINARY};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::Shell::{
    IShellItem, IShellLinkW, SHCreateItemFromParsingName, SHGetNameFromIDList, ShellLink,
    SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_NORMALDISPLAY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindow, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, GWL_EXSTYLE, GW_OWNER, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW,
};

use crate::util::{from_wide_buf, pcwstr, wide};

/// An app pinned to the Windows taskbar.
pub struct Pin {
    pub name: String,
    /// What to open: the pinned shortcut, or `shell:AppsFolder\<AUMID>` for
    /// Store apps that are pinned without a shortcut file.
    pub launch: String,
    /// Lowercased full path of the program the shortcut starts, if it is a desktop app.
    pub exe: Option<String>,
    /// AppUserModelID, for Store app pins.
    pub aumid: Option<String>,
}

/// A top-level window that would get a taskbar button.
pub struct AppWindow {
    pub hwnd: HWND,
    /// Lowercased full path of the owning process, empty if it could not be read.
    pub exe: String,
    pub aumid: Option<String>,
}

fn taskbar_dir() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    Some(Path::new(&appdata).join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar"))
}

fn lnk_files() -> Vec<PathBuf> {
    let Some(dir) = taskbar_dir() else { return Vec::new() };
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    read.filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")))
        .collect()
}

/// The binary blob Explorer keeps the pinned-button order in.
fn taskband_favorites() -> Vec<u8> {
    let sub = wide(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Taskband");
    let value = wide("Favorites");
    unsafe {
        let mut size = 0u32;
        if RegGetValueW(HKEY_CURRENT_USER, pcwstr(&sub), pcwstr(&value), RRF_RT_REG_BINARY, None, None, Some(&mut size))
            .is_err()
        {
            return Vec::new();
        }
        let mut buf = vec![0u8; size as usize];
        if RegGetValueW(
            HKEY_CURRENT_USER,
            pcwstr(&sub),
            pcwstr(&value),
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
        .is_err()
        {
            return Vec::new();
        }
        buf.truncate(size as usize);
        buf
    }
}

/// Changes whenever a pin is added, removed or reordered.
pub fn pins_signature() -> (Option<SystemTime>, Vec<u8>) {
    let modified = taskbar_dir().and_then(|d| std::fs::metadata(d).ok()).and_then(|m| m.modified().ok());
    (modified, taskband_favorites())
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

unsafe fn load_link(lnk: &Path) -> Option<IShellLinkW> {
    let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_ALL).ok()?;
    let persist: IPersistFile = link.cast().ok()?;
    let path = wide(&lnk.to_string_lossy());
    persist.Load(pcwstr(&path), STGM(0)).ok()?;
    Some(link)
}

/// Target and arguments of a shortcut.
fn resolve_shortcut(lnk: &Path) -> Option<(String, String)> {
    unsafe {
        let link = load_link(lnk)?;
        let mut target = [0u16; 260];
        let mut data = WIN32_FIND_DATAW::default();
        link.GetPath(&mut target, &mut data, 0).ok()?;
        let mut args = [0u16; 1024];
        let _ = link.GetArguments(&mut args);
        let target = from_wide_buf(&target);
        if target.trim().is_empty() {
            None
        } else {
            Some((target, from_wide_buf(&args)))
        }
    }
}

/// `shell:AppsFolder\<AUMID>` for a Store app shortcut.
unsafe fn store_item_path(lnk: &Path) -> Option<String> {
    let link = load_link(lnk)?;
    let pidl = link.GetIDList().ok()?;
    let name = SHGetNameFromIDList(pidl, SIGDN_DESKTOPABSOLUTEPARSING).ok();
    CoTaskMemFree(Some(pidl as *const _));
    let name = name?;
    let s = String::from_utf16_lossy(name.as_wide());
    CoTaskMemFree(Some(name.0 as *const _));
    let prefix = "shell:appsfolder\\";
    s.to_lowercase().starts_with(prefix).then(|| s[prefix.len()..].to_string())
}

/// Squirrel installers (Discord, Slack, ...) pin `Update.exe --processStart App.exe`;
/// the running process is the newest `app-*\App.exe`.
fn effective_target(target: &str, args: &str) -> String {
    if target.to_lowercase().ends_with("\\update.exe") && args.to_lowercase().contains("--processstart") {
        let exe_name = args
            .split_whitespace()
            .skip_while(|a| !a.eq_ignore_ascii_case("--processStart"))
            .nth(1)
            .map(|s| s.trim_matches('"').to_string());
        if let (Some(exe_name), Some(root)) = (exe_name, Path::new(target).parent()) {
            let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("app-")))
                .collect();
            let version = |p: &PathBuf| -> Vec<u32> {
                p.file_name()
                    .map(|n| {
                        n.to_string_lossy()
                            .trim_start_matches("app-")
                            .split('.')
                            .filter_map(|s| s.parse().ok())
                            .collect()
                    })
                    .unwrap_or_default()
            };
            dirs.sort_by_key(version);
            if let Some(found) = dirs.iter().rev().map(|d| d.join(&exe_name)).find(|p| p.exists()) {
                return found.to_string_lossy().to_string();
            }
        }
    }
    target.to_string()
}

fn is_explorer_pin(lnk: &Path) -> bool {
    let needle: Vec<u8> = "Microsoft.Windows.Explorer".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    std::fs::read(lnk).ok().is_some_and(|b| find_bytes(&b, &needle).is_some())
}

/// The taskbar pins, in the order Explorer shows them.
pub fn read_pins() -> Vec<Pin> {
    let blob = taskband_favorites();
    let mut keyed: Vec<(usize, SystemTime, Pin)> = Vec::new();

    for lnk in lnk_files() {
        let file_name = lnk.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let utf16: Vec<u8> = file_name.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let pos = find_bytes(&blob, &utf16).unwrap_or(usize::MAX);
        let created = std::fs::metadata(&lnk).and_then(|m| m.created()).unwrap_or(SystemTime::UNIX_EPOCH);

        let name = lnk.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let (exe, aumid) = if let Some((target, args)) = resolve_shortcut(&lnk) {
            (Some(effective_target(&target, &args).to_lowercase()), None)
        } else if is_explorer_pin(&lnk) {
            let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".to_string());
            (Some(format!(r"{windir}\explorer.exe").to_lowercase()), None)
        } else if let Some(aumid) = unsafe { store_item_path(&lnk) } {
            (None, Some(aumid))
        } else {
            continue;
        };
        let launch = lnk.to_string_lossy().to_string();
        keyed.push((pos, created, Pin { name, launch, exe, aumid }));
    }

    // Store apps pinned from Start have no shortcut file; they only appear in
    // the Favorites blob, by AppUserModelID.
    for (pos, aumid) in blob_aumids(&blob) {
        let known = keyed.iter().any(|(_, _, p)| p.aumid.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(&aumid)));
        if known {
            continue;
        }
        let launch = format!("shell:AppsFolder\\{aumid}");
        let Some(name) = shell_display_name(&launch) else { continue };
        keyed.push((pos, SystemTime::UNIX_EPOCH, Pin { name, launch, exe: None, aumid: Some(aumid) }));
    }

    keyed.sort_by_key(|(pos, created, _)| (*pos, *created));
    keyed.into_iter().map(|(_, _, pin)| pin).collect()
}

/// AppUserModelIDs of packaged apps (`Family_publisherid!App`) found in the blob,
/// with their byte offsets. UTF-16 strings can start at odd offsets, so both
/// alignments are scanned.
fn blob_aumids(blob: &[u8]) -> Vec<(usize, String)> {
    let mut found: Vec<(usize, String)> = Vec::new();
    for align in 0..2 {
        let units: Vec<u16> =
            blob[align..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let mut start = 0;
        let mut run = String::new();
        for (i, &u) in units.iter().chain(std::iter::once(&0u16)).enumerate() {
            let c = char::from_u32(u as u32).filter(|c| c.is_ascii_alphanumeric() || "._-!".contains(*c));
            match c {
                Some(c) => {
                    if run.is_empty() {
                        start = i;
                    }
                    run.push(c);
                }
                None => {
                    if is_aumid(&run) && !found.iter().any(|(_, a)| a.eq_ignore_ascii_case(&run)) {
                        found.push((align + start * 2, run.clone()));
                    }
                    run.clear();
                }
            }
        }
    }
    found
}

fn is_aumid(s: &str) -> bool {
    let Some((family, app)) = s.split_once('!') else { return false };
    let Some((name, publisher)) = family.rsplit_once('_') else { return false };
    !name.is_empty()
        && !app.is_empty()
        && !app.contains('!')
        && publisher.len() == 13
        && publisher.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// The name Explorer shows for a shell item, e.g. "Microsoft To Do".
pub fn shell_display_name(parsing_name: &str) -> Option<String> {
    unsafe {
        let name = wide(parsing_name);
        let item: IShellItem = SHCreateItemFromParsingName(pcwstr(&name), None::<&IBindCtx>).ok()?;
        let display = item.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?;
        let s = String::from_utf16_lossy(display.as_wide());
        CoTaskMemFree(Some(display.0 as *const _));
        (!s.is_empty()).then_some(s)
    }
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

pub fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

/// Roughly the rule Explorer uses to give a window a taskbar button.
fn is_app_window(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return false;
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_APPWINDOW.0 == 0 {
            if ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
                return false;
            }
            if GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.0.is_null()) {
                return false;
            }
        }
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4).is_ok() && cloaked != 0 {
            return false;
        }
        if GetWindowTextLengthW(hwnd) == 0 {
            return false;
        }
    }
    !matches!(class_name(hwnd).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

fn process_path(pid: u32) -> String {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(handle);
        if ok {
            String::from_utf16_lossy(&buf[..len as usize]).to_lowercase()
        } else {
            String::new()
        }
    }
}

/// The window's AppUserModelID, the identity the taskbar groups by.
/// Ported from Nectar's `get_window_app_user_model_id`.
fn window_aumid(hwnd: HWND) -> Option<String> {
    const PKEY_APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY {
        fmtid: windows::core::GUID::from_values(
            0x9F4C2855,
            0x9F79,
            0x4B39,
            [0xA8, 0xD0, 0xE1, 0xD4, 0x2D, 0xE1, 0xD5, 0xF3],
        ),
        pid: 5,
    };
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let mut prop = store.GetValue(&PKEY_APP_USER_MODEL_ID).ok()?;
        let result = PropVariantToStringAlloc(&prop).ok().map(|p| {
            let s = String::from_utf16_lossy(p.as_wide()).trim().to_string();
            CoTaskMemFree(Some(p.0 as *const _));
            s
        });
        let _ = PropVariantClear(&mut prop);
        result.filter(|s| !s.is_empty())
    }
}

/// Visible bounds (without the invisible resize border) of every app window that
/// is not minimized. Cheap: no process or property lookups.
pub fn app_window_frames() -> Vec<RECT> {
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam.0 as *mut Vec<RECT>);
        if !IsIconic(hwnd).as_bool() && is_app_window(hwnd) {
            let mut r = RECT::default();
            if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut _, std::mem::size_of::<RECT>() as u32).is_ok() {
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                if pid != std::process::id() {
                    out.push(r);
                }
            }
        }
        BOOL(1)
    }
    let mut frames: Vec<RECT> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut frames as *mut _ as isize));
    }
    frames
}

/// The highest app window in the z-order that is on the given monitor, whether
/// or not it has focus.
pub fn top_app_window_on(monitor: &RECT) -> Option<HWND> {
    struct Search {
        monitor: RECT,
        found: Option<HWND>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut Search);
        if IsIconic(hwnd).as_bool() || !is_app_window(hwnd) {
            return BOOL(1);
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() {
            return BOOL(1);
        }
        let mut r = RECT::default();
        if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut _, std::mem::size_of::<RECT>() as u32).is_err() {
            return BOOL(1);
        }
        // On this monitor if its center is.
        let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        let m = search.monitor;
        if cx >= m.left && cx < m.right && cy >= m.top && cy < m.bottom {
            search.found = Some(hwnd);
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut search = Search { monitor: *monitor, found: None };
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut _ as isize));
    }
    search.found
}

/// Every window that would have a taskbar button, topmost first.
pub fn app_windows() -> Vec<AppWindow> {
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam.0 as *mut Vec<HWND>);
        if is_app_window(hwnd) {
            out.push(hwnd);
        }
        BOOL(1)
    }

    let mut hwnds: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut hwnds as *mut _ as isize));
    }

    let me = std::process::id();
    hwnds
        .into_iter()
        .filter_map(|hwnd| {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if pid == me {
                return None;
            }
            Some(AppWindow { hwnd, exe: process_path(pid), aumid: window_aumid(hwnd) })
        })
        .collect()
}

/// The "File description" from a program's version resource, e.g. "Google Chrome".
/// Ported from Nectar's `exe_description`.
pub fn exe_description(path: &str) -> Option<String> {
    unsafe {
        let path_w = wide(path);
        let size = GetFileVersionInfoSizeW(pcwstr(&path_w), None);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        GetFileVersionInfoW(pcwstr(&path_w), None, size, buf.as_mut_ptr() as *mut _).ok()?;
        let query = |sub: &str| -> Option<*mut std::ffi::c_void> {
            let sub_w = wide(sub);
            let mut ptr: *mut std::ffi::c_void = std::ptr::null_mut();
            let mut len = 0u32;
            let ok = VerQueryValueW(buf.as_ptr() as *const _, pcwstr(&sub_w), &mut ptr, &mut len);
            (ok.as_bool() && !ptr.is_null() && len > 0).then_some(ptr)
        };
        let translation = query("\\VarFileInfo\\Translation")? as *const u16;
        let (lang, codepage) = (*translation, *translation.add(1));
        let desc = query(&format!("\\StringFileInfo\\{lang:04x}{codepage:04x}\\FileDescription"))?;
        let s = String::from_utf16_lossy(windows::core::PCWSTR(desc as *const u16).as_wide()).trim().to_string();
        (!s.is_empty()).then_some(s)
    }
}

/// `pollen --diag`: prints what the dock sees, for debugging matching problems.
pub fn print_diagnostics() {
    println!("== pins ==");
    for pin in read_pins() {
        println!("  {:<28} exe={:?} aumid={:?} launch={}", pin.name, pin.exe, pin.aumid, pin.launch);
    }

    // Readable UTF-16 runs in the Favorites blob, to spot pins that have no .lnk file.
    let blob = taskband_favorites();
    println!("== favorites blob: {} bytes ==", blob.len());
    let units: Vec<u16> = blob.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let mut run = String::new();
    for &u in &units {
        match char::from_u32(u as u32) {
            Some(c) if !c.is_control() && u >= 0x20 => run.push(c),
            _ => {
                if run.len() >= 12 && (run.contains('!') || run.contains(".lnk") || run.contains('_')) {
                    println!("  {run}");
                }
                run.clear();
            }
        }
    }

    unsafe {
        use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
        use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
        let monitor = MonitorFromPoint(windows::Win32::Foundation::POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        if let Some(top) = top_app_window_on(&info.rcMonitor) {
            let mut r = RECT::default();
            let _ = GetWindowRect(top, &mut r);
            println!(
                "== top window on primary: {:?} class={} rect=({},{})-({},{}) monitor={:?} ==",
                window_title(top), class_name(top), r.left, r.top, r.right, r.bottom, info.rcMonitor
            );
        }
    }
    println!("== visible top-level windows ==");
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam.0 as *mut Vec<HWND>);
        if IsWindowVisible(hwnd).as_bool() {
            out.push(hwnd);
        }
        BOOL(1)
    }
    let mut hwnds: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut hwnds as *mut _ as isize));
    }
    for hwnd in hwnds {
        let title = window_title(hwnd);
        if title.is_empty() {
            continue;
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        let owner = unsafe { GetWindow(hwnd, GW_OWNER) }.map(|o| !o.0.is_null()).unwrap_or(false);
        let mut cloaked = 0u32;
        let _ = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4) };
        println!(
            "  [{}] {:?} class={} ex={:#x} owner={} cloaked={} exe={} aumid={:?}",
            if is_app_window(hwnd) { "APP" } else { "   " },
            title.chars().take(40).collect::<String>(),
            class_name(hwnd),
            ex,
            owner,
            cloaked,
            process_path(pid),
            window_aumid(hwnd),
        );
    }
}
