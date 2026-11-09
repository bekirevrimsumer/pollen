//! Pinned taskbar apps and running app windows.
//!
//! Reading the taskbar pins (the `User Pinned\TaskBar` shortcuts, ordered by the
//! `Taskband\Favorites` blob) is ported from Nectar (GPL-3.0-only,
//! https://github.com/kua8/Nectar), `src-tauri/src/taskbar_pins.rs`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use windows::core::{Interface, BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HWND, LPARAM, PROPERTYKEY, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
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
    /// The app's AppUserModelID from its package, for a packaged (Store) app's
    /// process: its icon and name live in the package, not always in the exe.
    pub package: Option<String>,
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
/// A pin made in Pollen, from what it launches: a program's path or
/// `shell:AppsFolder\<AUMID>`.
pub fn pin_for(launch: &str) -> Pin {
    match launch.strip_prefix("shell:AppsFolder\\").or_else(|| launch.strip_prefix("shell:appsfolder\\")) {
        Some(aumid) => Pin {
            name: shell_display_name(launch).unwrap_or_else(|| aumid.to_string()),
            launch: launch.to_string(),
            exe: None,
            aumid: Some(aumid.to_string()),
        },
        None => Pin {
            name: exe_description(launch).unwrap_or_else(|| {
                Path::new(launch).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
            }),
            launch: launch.to_string(),
            exe: Some(launch.to_lowercase()),
            aumid: None,
        },
    }
}

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
        // Untitled windows are mostly helpers, but one that asks for a taskbar
        // button gets it, as on the taskbar: a console started with `start ""`
        // (GoodbyeDPI's scripts, for one) has no title.
        if GetWindowTextLengthW(hwnd) == 0 && ex & WS_EX_APPWINDOW.0 == 0 {
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

/// A packaged app's AppUserModelID (`Package_family!App`) from its process.
fn package_aumid(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let result = GetApplicationUserModelId(handle, &mut len, Some(PWSTR(buf.as_mut_ptr())));
        let _ = CloseHandle(handle);
        if result != ERROR_SUCCESS {
            return None;
        }
        let id = from_wide_buf(&buf);
        (!id.is_empty()).then_some(id)
    }
}

/// The window's AppUserModelID, the identity the taskbar groups by.
/// Ported from Nectar's `get_window_app_user_model_id`.
pub fn window_aumid(hwnd: HWND) -> Option<String> {
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

/// The highest of these windows in the z-order (the one used last).
pub fn topmost_of(windows: &[HWND]) -> Option<HWND> {
    struct Search<'a> {
        windows: &'a [HWND],
        found: Option<HWND>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut Search);
        if search.windows.contains(&hwnd) {
            search.found = Some(hwnd);
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut search = Search { windows, found: None };
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut _ as isize));
    }
    search.found.or_else(|| windows.first().copied())
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
            let exe = process_path(pid);
            let package = if exe.contains("\\windowsapps\\") { package_aumid(pid) } else { None };
            Some(AppWindow { hwnd, exe, aumid: window_aumid(hwnd), package })
        })
        .collect()
}

/// The "File description" from a program's version resource, e.g. "Google Chrome".
/// Ported from Nectar's `exe_description`.
pub fn exe_description(path: &str) -> Option<String> {
    version_string(path, "FileDescription")
}

/// A string from a program's version information ("FileDescription",
/// "ProductVersion", ...).
pub fn version_string(path: &str, name: &str) -> Option<String> {
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
        let desc = query(&format!("\\StringFileInfo\\{lang:04x}{codepage:04x}\\{name}"))?;
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
        if title.is_empty() && !is_app_window(hwnd) {
            continue;
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        let owner = unsafe { GetWindow(hwnd, GW_OWNER) }.map(|o| !o.0.is_null()).unwrap_or(false);
        let mut cloaked = 0u32;
        let _ = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4) };
        println!(
            "  [{}] {:?} class={} ex={:#x} owner={} cloaked={} exe={} aumid={:?} package={:?}",
            if is_app_window(hwnd) { "APP" } else { "   " },
            title.chars().take(40).collect::<String>(),
            class_name(hwnd),
            ex,
            owner,
            cloaked,
            process_path(pid),
            window_aumid(hwnd),
            package_aumid(pid),
        );
    }
}

/// A file or folder from an app's recent list.
pub struct Recent {
    pub name: String,
    pub path: String,
}

static START_IDS: std::sync::Mutex<Option<std::collections::HashMap<String, String>>> = std::sync::Mutex::new(None);

/// Reads, on a worker thread, which ID the Start menu lists each program
/// under. Apps such as VS Code ("Microsoft.VisualStudioCode") set their ID for
/// the whole process, where it cannot be read from their windows.
pub fn prefetch_start_ids() {
    std::thread::spawn(|| {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        let map = read_start_ids();
        if let Ok(mut ids) = START_IDS.lock() {
            *ids = Some(map);
        }
    });
}

/// Program path (lower case) → the Start menu's ID for it.
fn read_start_ids() -> std::collections::HashMap<String, String> {
    use windows::Win32::UI::Shell::{
        BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem2, SHGetKnownFolderItem, KF_FLAG_DEFAULT,
        SIGDN_PARENTRELATIVEPARSING,
    };
    // System.Link.TargetParsingPath: what a Start menu shortcut starts.
    const TARGET: PROPERTYKEY = PROPERTYKEY {
        fmtid: windows::core::GUID::from_u128(0xb9b4b3fc_2b51_4a42_b5d8_324146afcf25),
        pid: 2,
    };
    let mut map = std::collections::HashMap::new();
    unsafe {
        let Ok(folder) = SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) else { return map };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else { return map };
        loop {
            let mut batch: [Option<IShellItem>; 32] = Default::default();
            let mut fetched = 0u32;
            let _ = items.Next(&mut batch, Some(&mut fetched));
            if fetched == 0 {
                break;
            }
            for item in batch.iter().take(fetched as usize).flatten() {
                let Ok(item2) = item.cast::<IShellItem2>() else { continue };
                let Ok(target) = item2.GetString(&TARGET) else { continue };
                let target_s = String::from_utf16_lossy(target.as_wide()).to_lowercase();
                CoTaskMemFree(Some(target.0 as *const _));
                let Ok(id) = item.GetDisplayName(SIGDN_PARENTRELATIVEPARSING) else { continue };
                let id_s = String::from_utf16_lossy(id.as_wide());
                CoTaskMemFree(Some(id.0 as *const _));
                if target_s.ends_with(".exe") && !id_s.is_empty() {
                    map.entry(target_s).or_insert(id_s);
                }
            }
        }
    }
    map
}

/// The app IDs Windows may file an app's recent list under: its explicit
/// AppUserModelID if it has one, else one made from the program's path with
/// the known folder in front swapped for its GUID (as the Start menu does).
pub fn app_ids(aumid: Option<&str>, exe: Option<&str>) -> Vec<String> {
    use windows::Win32::UI::Shell::{
        FOLDERID_LocalAppData, FOLDERID_ProgramFilesX64, FOLDERID_ProgramFilesX86, FOLDERID_RoamingAppData,
        FOLDERID_System, FOLDERID_SystemX86, FOLDERID_Windows, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
    };
    let mut ids = Vec::new();
    if let Some(a) = aumid {
        ids.push(a.to_string());
    }
    let Some(exe) = exe else { return ids };
    if let Some(id) = START_IDS.lock().ok().and_then(|m| m.as_ref().and_then(|m| m.get(&exe.to_lowercase()).cloned())) {
        ids.push(id);
    }
    let folders = [
        (FOLDERID_ProgramFilesX64, "{6D809377-6AF0-444B-8957-A3773F02200E}"),
        (FOLDERID_ProgramFilesX86, "{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}"),
        (FOLDERID_System, "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}"),
        (FOLDERID_SystemX86, "{D65231B0-B2F1-4857-A4CE-A8E7C6EA7D27}"),
        (FOLDERID_Windows, "{F38BF404-1D43-42F2-9305-67DE0B28FC23}"),
        (FOLDERID_LocalAppData, "{F1B32785-6FBA-4FCF-9D55-7B8E7F157091}"),
        (FOLDERID_RoamingAppData, "{3EB685DB-65F9-4CF6-A03A-E3EF65729F3D}"),
    ];
    let lower = exe.to_lowercase();
    // The longest folder that holds the program wins (System32 over Windows).
    let mut best: Option<(usize, String)> = None;
    for (id, guid) in folders {
        let Ok(path) = (unsafe { SHGetKnownFolderPath(&id, KF_FLAG_DEFAULT, None) }) else { continue };
        let folder = unsafe { String::from_utf16_lossy(path.as_wide()) };
        unsafe { CoTaskMemFree(Some(path.0 as *const _)) };
        let folder_l = folder.to_lowercase();
        if lower.starts_with(&folder_l) && lower[folder_l.len()..].starts_with('\\') && best.as_ref().is_none_or(|b| folder_l.len() > b.0) {
            best = Some((folder_l.len(), format!("{guid}{}", &exe[folder.len()..])));
        }
    }
    if let Some((_, id)) = best {
        ids.push(id);
    }
    ids.push(exe.to_string());
    ids
}

/// The app's recent files and folders, newest first, from the first of
/// `ids` that has a list (what the taskbar showed on right-click).
pub fn recent_documents(ids: &[String], max: u32) -> Vec<Recent> {
    // The jump list file itself first: the shell's interface for it only
    // answers for a few of Windows' own apps.
    for id in ids {
        let list = jump_list_recent(id, max as usize);
        if !list.is_empty() {
            return list;
        }
    }
    use windows::Win32::UI::Shell::Common::IObjectArray;
    use windows::Win32::UI::Shell::{
        ApplicationDocumentLists, IApplicationDocumentLists, ADLT_RECENT, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_NORMALDISPLAY,
    };
    for id in ids {
        let list = unsafe {
            let Ok(lists) = CoCreateInstance::<_, IApplicationDocumentLists>(&ApplicationDocumentLists, None, CLSCTX_ALL) else {
                return Vec::new();
            };
            let id_w = wide(id);
            if lists.SetAppID(pcwstr(&id_w)).is_err() {
                continue;
            }
            let array = match lists.GetList::<IObjectArray>(ADLT_RECENT, max) {
                Ok(a) => a,
                Err(e) => {
                    if std::env::var_os("POLLEN_DEBUG_RECENT").is_some() {
                        eprintln!("{id}: GetList failed: {e}");
                    }
                    continue;
                }
            };
            let count = array.GetCount().unwrap_or(0);
            if std::env::var_os("POLLEN_DEBUG_RECENT").is_some() {
                eprintln!("{id}: {count} items");
            }
            let mut out = Vec::new();
            for i in 0..count {
                let Ok(item) = array.GetAt::<IShellItem>(i) else { continue };
                let read = |kind| -> Option<String> {
                    let p = item.GetDisplayName(kind).ok()?;
                    let s = String::from_utf16_lossy(p.as_wide());
                    CoTaskMemFree(Some(p.0 as *const _));
                    Some(s)
                };
                if let (Some(name), Some(path)) = (read(SIGDN_NORMALDISPLAY), read(SIGDN_DESKTOPABSOLUTEPARSING)) {
                    out.push(Recent { name, path });
                }
            }
            out
        };
        if !list.is_empty() {
            return list;
        }
    }
    Vec::new()
}

/// The name Windows gives an app's jump list file: a CRC-64 of its ID,
/// upper-cased, as UTF-16LE.
fn jump_list_hash(app_id: &str) -> String {
    const POLY: u64 = 0x92C6_4265_D321_39A4;
    let mut crc = u64::MAX;
    for unit in app_id.to_uppercase().encode_utf16() {
        for byte in unit.to_le_bytes() {
            crc ^= byte as u64;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ POLY } else { crc >> 1 };
            }
        }
    }
    format!("{crc:016x}")
}

/// The recent list straight from the app's jump list file: its "DestList"
/// stream holds each entry's path and when it was last used.
fn jump_list_recent(app_id: &str, max: usize) -> Vec<Recent> {
    use windows::Win32::System::Com::StructuredStorage::{IStorage, StgOpenStorage};
    use windows::Win32::System::Com::{STATFLAG_NONAME, STGM_READ, STGM_SHARE_EXCLUSIVE};
    let Some(appdata) = std::env::var_os("APPDATA") else { return Vec::new() };
    let file = PathBuf::from(appdata)
        .join(r"Microsoft\Windows\Recent\AutomaticDestinations")
        .join(format!("{}.automaticDestinations-ms", jump_list_hash(app_id)));
    let debug = std::env::var_os("POLLEN_DEBUG_RECENT").is_some();
    if debug {
        eprintln!("{app_id}: file {} exists={}", jump_list_hash(app_id), file.exists());
    }
    if !file.exists() {
        return Vec::new();
    }
    // Explorer may hold the file open; read a copy.
    let copy = std::env::temp_dir().join(format!("pollen-jumplist-{}.tmp", std::process::id()));
    if std::fs::copy(&file, &copy).is_err() {
        return Vec::new();
    }
    let bytes = unsafe {
        let copy_w = wide(&copy.display().to_string());
        let read = || -> Option<Vec<u8>> {
            let storage: IStorage = StgOpenStorage(pcwstr(&copy_w), None, STGM_READ | STGM_SHARE_EXCLUSIVE, None, 0).ok()?;
            let stream = storage.OpenStream(windows::core::w!("DestList"), None, STGM_READ | STGM_SHARE_EXCLUSIVE, 0).ok()?;
            let mut stat = Default::default();
            stream.Stat(&mut stat, STATFLAG_NONAME).ok()?;
            let size = (stat.cbSize as usize).min(4 * 1024 * 1024);
            let mut buf = vec![0u8; size];
            let mut read = 0u32;
            stream.Read(buf.as_mut_ptr() as *mut _, size as u32, Some(&mut read)).ok().ok()?;
            buf.truncate(read as usize);
            Some(buf)
        };
        read()
    };
    let _ = std::fs::remove_file(&copy);
    if debug {
        eprintln!("{app_id}: DestList bytes {:?}, version {:?}", bytes.as_ref().map(|b| b.len()), bytes.as_ref().and_then(|b| b.get(0..4).map(|x| x[0])));
    }
    let Some(bytes) = bytes else { return Vec::new() };
    parse_dest_list(&bytes, max)
}

/// The DestList stream: a 32-byte header, then one entry per item. Windows 10
/// and 11 write version 3 or 4 entries (path length at 0x80, then the path,
/// then 4 bytes); Windows 7 wrote version 1 (path length at 0x70).
fn parse_dest_list(b: &[u8], max: usize) -> Vec<Recent> {
    let u16_at = |o: usize| b.get(o..o + 2).map(|x| u16::from_le_bytes([x[0], x[1]]));
    let u32_at = |o: usize| b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]));
    let u64_at = |o: usize| b.get(o..o + 8).map(|x| u64::from_le_bytes(x.try_into().unwrap_or([0; 8])));
    let Some(version) = u32_at(0) else { return Vec::new() };
    let (time_at, len_at, tail) = if version >= 3 { (0x64, 0x80, 4) } else { (0x64, 0x70, 0) };
    let mut entries: Vec<(u64, String)> = Vec::new();
    let mut o = 32;
    while o + len_at + 2 <= b.len() && entries.len() < 500 {
        let Some(len) = u16_at(o + len_at).map(|l| l as usize) else { break };
        let start = o + len_at + 2;
        let Some(raw) = b.get(start..start + len * 2) else { break };
        let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        entries.push((u64_at(o + time_at).unwrap_or(0), String::from_utf16_lossy(&units)));
        o = start + len * 2 + tail;
    }
    if std::env::var_os("POLLEN_DEBUG_RECENT").is_some() {
        eprintln!("  {} entries parsed", entries.len());
        for (t, p) in entries.iter().take(4) {
            eprintln!("  t={t} len={} starts={:?} exists={}", p.len(), p.chars().take(3).collect::<String>(), Path::new(p).exists());
        }
    }
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    let mut seen = std::collections::HashSet::new();
    entries
        .into_iter()
        // Files and folders that are still there; not web addresses and the like.
        .filter(|(_, path)| path.len() > 3 && path.as_bytes().get(1) == Some(&b':') && Path::new(path).exists())
        .filter(|(_, path)| seen.insert(path.to_lowercase()))
        .take(max)
        .map(|(_, path)| Recent {
            name: Path::new(&path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.clone()),
            path,
        })
        .collect()
}

/// A group in an app's jump list: "Recent", or a category the app named.
pub struct JumpGroup {
    pub title: String,
    pub items: Vec<JumpItem>,
}

/// What to run for one entry: a program with arguments, or a file to open.
pub struct JumpItem {
    pub name: String,
    pub target: String,
    pub args: String,
}

/// An app's jump list as the taskbar showed it: the categories the app keeps
/// itself (VS Code's recent folders, a browser's frequent sites), then its
/// recently opened files. `exe` opens the recent files with the app itself.
pub fn jump_list(ids: &[String], exe: Option<&str>, max: usize) -> Vec<JumpGroup> {
    let mut groups = Vec::new();
    for id in ids {
        groups = custom_destinations(id, max);
        if !groups.is_empty() {
            break;
        }
    }
    let recent = recent_documents(ids, max as u32);
    if !recent.is_empty() {
        let items = recent
            .into_iter()
            .map(|r| match exe {
                Some(exe) if Path::new(exe).exists() => JumpItem { name: r.name, target: exe.to_string(), args: format!("\"{}\"", r.path) },
                _ => JumpItem { name: r.name, target: r.path, args: String::new() },
            })
            .collect();
        groups.push(JumpGroup { title: crate::i18n::t("Son kullanılanlar", "Recent").into(), items });
    }
    groups
}

/// The categories an app wrote into its custom jump list file: a small header,
/// then per category its type (0 named, 1 known, 2 tasks), and for named ones
/// the name and serialized shell links, each after its class ID.
fn custom_destinations(app_id: &str, max: usize) -> Vec<JumpGroup> {
    use windows::Win32::System::Com::{IPersistStream, IStream, STREAM_SEEK_CUR};
    use windows::Win32::UI::Shell::SHCreateMemStream;
    let Some(appdata) = std::env::var_os("APPDATA") else { return Vec::new() };
    let file = PathBuf::from(appdata)
        .join(r"Microsoft\Windows\Recent\CustomDestinations")
        .join(format!("{}.customDestinations-ms", jump_list_hash(app_id)));
    let Ok(bytes) = std::fs::read(&file) else { return Vec::new() };
    let Some(stream) = (unsafe { SHCreateMemStream(Some(&bytes)) }) else { return Vec::new() };
    let read = |n: usize| -> Option<Vec<u8>> {
        let mut buf = vec![0u8; n];
        let mut got = 0u32;
        unsafe { stream.Read(buf.as_mut_ptr() as *mut _, n as u32, Some(&mut got)).ok().ok()? };
        (got as usize == n).then_some(buf)
    };
    let read_u32 = || read(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let read_u16 = || read(2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    // One serialized shell link; the stream moves past it.
    let read_entry = |stream: &IStream| -> Option<Option<JumpItem>> {
        let id = read(16)?;
        let clsid = windows::core::GUID::from_values(
            u32::from_le_bytes([id[0], id[1], id[2], id[3]]),
            u16::from_le_bytes([id[4], id[5]]),
            u16::from_le_bytes([id[6], id[7]]),
            [id[8], id[9], id[10], id[11], id[12], id[13], id[14], id[15]],
        );
        unsafe {
            let object: IPersistStream = match CoCreateInstance(&clsid, None, CLSCTX_ALL) {
                Ok(o) => o,
                Err(e) => {
                    if std::env::var_os("POLLEN_DEBUG_RECENT").is_some() {
                        eprintln!("  entry class {clsid:?}: {e}");
                    }
                    return None;
                }
            };
            if let Err(e) = object.Load(stream) {
                if std::env::var_os("POLLEN_DEBUG_RECENT").is_some() {
                    eprintln!("  entry load {clsid:?}: {e}");
                }
                return None;
            }
            let Ok(link) = object.cast::<IShellLinkW>() else { return Some(None) };
            let mut path = [0u16; 1024];
            let _ = link.GetPath(&mut path, std::ptr::null_mut(), 0);
            let mut args = [0u16; 2048];
            let _ = link.GetArguments(&mut args);
            let target = from_wide_buf(&path);
            let args = from_wide_buf(&args);
            // The title the app gave it (System.Title), else the link's description.
            let title = object.cast::<IPropertyStore>().ok().and_then(|store| {
                const TITLE: PROPERTYKEY = PROPERTYKEY {
                    fmtid: windows::core::GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9),
                    pid: 2,
                };
                let mut value = store.GetValue(&TITLE).ok()?;
                let text = PropVariantToStringAlloc(&value).ok().map(|p| {
                    let s = String::from_utf16_lossy(p.as_wide());
                    CoTaskMemFree(Some(p.0 as *const _));
                    s
                });
                let _ = PropVariantClear(&mut value);
                text.filter(|t| !t.is_empty())
            });
            let mut description = [0u16; 512];
            let _ = link.GetDescription(&mut description);
            let name = title.or_else(|| Some(from_wide_buf(&description)).filter(|d| !d.is_empty())).unwrap_or_else(|| {
                Path::new(&target).file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
            });
            if target.is_empty() || name.is_empty() {
                return Some(None);
            }
            Some(Some(JumpItem { name, target, args }))
        }
    };
    let mut groups = Vec::new();
    let debug = std::env::var_os("POLLEN_DEBUG_RECENT").is_some();
    let (Some(version), Some(count), Some(third)) = (read_u32(), read_u32(), read_u32()) else { return groups };
    if debug {
        eprintln!("{app_id}: custom list version {version}, {count} categories, then {third}");
    }
    for _ in 0..count.min(32) {
        let Some(kind) = read_u32() else { break };
        if debug {
            eprintln!("  category type {kind}");
        }
        match kind {
            0 => {
                let Some(len) = read_u16() else { break };
                let Some(name) = read(len as usize * 2) else { break };
                let title = String::from_utf16_lossy(&name.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>());
                let Some(n) = read_u32() else { break };
                let mut items = Vec::new();
                for _ in 0..n.min(64) {
                    match read_entry(&stream) {
                        Some(Some(item)) => items.push(item),
                        Some(None) => {}
                        None => return groups,
                    }
                }
                let _footer = read_u32();
                items.truncate(max);
                // Names like "@{...}" are resource references; show those plainly.
                let title = if title.starts_with('@') { crate::i18n::t("Son kullanılanlar", "Recent").into() } else { title };
                if !items.is_empty() {
                    groups.push(JumpGroup { title, items });
                }
            }
            1 => {
                let _known = read_u32();
            }
            2 => {
                // Tasks the app offers ("New InPrivate window", "New Window").
                let Some(n) = read_u32() else { break };
                let mut items = Vec::new();
                for _ in 0..n.min(64) {
                    match read_entry(&stream) {
                        Some(Some(item)) => items.push(item),
                        Some(None) => {}
                        None => return groups,
                    }
                }
                let _footer = read_u32();
                items.truncate(max);
                if !items.is_empty() {
                    groups.push(JumpGroup { title: crate::i18n::t("Görevler", "Tasks").into(), items });
                }
            }
            _ => break,
        }
    }
    let _ = unsafe { stream.Seek(0, STREAM_SEEK_CUR, None) };
    groups
}
