//! Starting, focusing and closing apps.

use windows::core::{w, Interface, Result, BOOL, BSTR, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, TerminateProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IDispatch, IServiceProvider, CLSCTX_LOCAL_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_LWIN, VK_MENU,
};
use windows::Win32::UI::Shell::{
    IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellView, IShellWindows, SID_STopLevelBrowser,
    ShellExecuteW, ShellWindows, SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, EnumChildWindows, GetClassNameW, GetShellWindow, GetWindowThreadProcessId, IsIconic,
    PostMessageW, SetForegroundWindow, SetWindowPos, ShowWindow, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SW_RESTORE, SW_SHOWNOACTIVATE, SW_SHOWNORMAL, WM_CLOSE,
};

use crate::util::{pcwstr, wide};

/// Opens a shortcut, program or `shell:AppsFolder\...` item.
pub fn launch(target: &str) {
    start(target.to_owned(), String::new());
}

/// Runs a program with arguments (a jump list entry).
pub fn run(target: &str, args: &str) {
    start(target.to_owned(), args.to_owned());
}

/// Has Explorer start it, as the taskbar does: the app becomes Explorer's
/// child, not Pollen's. So tools that group processes by parent (PC Manager)
/// don't count the app's memory as Pollen's, and the app gets Explorer's
/// current environment (a PATH changed after Pollen started included),
/// where Pollen's own is the one it started with.
///
/// On its own thread: asking Explorer takes ~45 ms. Pollen's own
/// ShellExecute is the fallback when Explorer can't be reached.
fn start(target: String, args: String) {
    unsafe {
        // The click made Pollen the foreground process; pass that on, so the
        // new window comes to the front though Explorer opens it.
        let mut explorer = 0u32;
        GetWindowThreadProcessId(GetShellWindow(), Some(&mut explorer));
        if explorer != 0 {
            let _ = AllowSetForegroundWindow(explorer);
        }
    }
    std::thread::spawn(move || unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        if via_explorer(&target, &args).is_err() {
            let target = wide(&target);
            let args = wide(&args);
            let args = if args.len() > 1 { pcwstr(&args) } else { PCWSTR::null() };
            ShellExecuteW(None, w!("open"), pcwstr(&target), args, PCWSTR::null(), SW_SHOWNORMAL);
        }
        if com {
            CoUninitialize();
        }
    });
}

/// Explorer's own ShellExecute, reached through the desktop's folder view
/// (Raymond Chen, "How can I launch an unelevated process from my elevated
/// process"). The view's background object has to be asked for as IDispatch
/// first: asking for IShellFolderViewDual directly fails (E_NOINTERFACE).
unsafe fn via_explorer(target: &str, args: &str) -> Result<()> {
    let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)?;
    let desktop = VARIANT::from(0i32); // CSIDL_DESKTOP
    let mut hwnd = 0i32;
    let found: IDispatch = windows.FindWindowSW(&desktop, &VARIANT::default(), SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)?;
    let browser: IShellBrowser = found.cast::<IServiceProvider>()?.QueryService(&SID_STopLevelBrowser)?;
    let view: IShellView = browser.QueryActiveShellView()?;
    let background: IDispatch = view.GetItemObject(SVGIO_BACKGROUND)?;
    let shell: IShellDispatch2 = background.cast::<IShellFolderViewDual>()?.Application()?.cast()?;
    shell.ShellExecute(
        &BSTR::from(target),
        &VARIANT::from(BSTR::from(args)),
        &VARIANT::from(BSTR::new()),
        &VARIANT::from(BSTR::from("open")),
        &VARIANT::from(SW_SHOWNORMAL.0),
    )
}

pub fn focus(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if !SetForegroundWindow(hwnd).as_bool() {
            // Windows only lets the process that got the last input change the
            // foreground window; a synthetic Alt press counts as that input.
            tap_alt();
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

/// Brings a window up for a drag held over its dock icon, so the file can be
/// dropped into it. No synthetic Alt press as in `focus`: it would change the
/// drag's effect. If Windows refuses the focus, the window still comes to the
/// top of the z-order.
pub fn bring_up(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        if !SetForegroundWindow(hwnd).as_bool() {
            let _ = SetWindowPos(hwnd, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }
}

fn tap_alt() {
    press_keys(&[VK_MENU]);
}

/// Presses the keys in order and releases them in reverse, like a shortcut.
pub fn press_keys(keys: &[VIRTUAL_KEY]) {
    let key = |vk, flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: flags, ..Default::default() } },
    };
    let mut inputs: Vec<INPUT> = keys.iter().map(|&k| key(k, Default::default())).collect();
    inputs.extend(keys.iter().rev().map(|&k| key(k, KEYEVENTF_KEYUP)));
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// Opens the Start menu.
pub fn open_start() {
    press_keys(&[VK_LWIN]);
}

/// Shows the desktop, or brings the windows back (the taskbar's corner button).
pub fn show_desktop() {
    press_keys(&[VK_LWIN, VIRTUAL_KEY(b'D' as u16)]);
}

/// Opens the notification center with the calendar.
pub fn open_notifications() {
    press_keys(&[VK_LWIN, VIRTUAL_KEY(b'N' as u16)]);
}

pub fn close(windows: &[HWND]) {
    for &hwnd in windows {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Ends the processes behind the windows outright, as the taskbar's "End
/// task" does, for an app that no longer answers closing. Explorer and the
/// frame host of Store apps are left alone: ending them would take the
/// desktop, or every Store app's window, with them.
pub fn end_task(windows: &[HWND]) {
    let me = std::process::id();
    let mut pids: Vec<u32> = Vec::new();
    for &hwnd in windows {
        let pid = owning_process(hwnd);
        if pid != 0 && pid != me && !pids.contains(&pid) {
            pids.push(pid);
        }
    }
    for pid in pids {
        unsafe {
            let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE, false, pid) else { continue };
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let name = if QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok() {
                String::from_utf16_lossy(&buf[..len as usize]).to_lowercase()
            } else {
                String::new()
            };
            let shell = name.is_empty() || name.ends_with("\\explorer.exe") || name.ends_with("\\applicationframehost.exe");
            if !shell {
                let _ = TerminateProcess(handle, 1);
            }
            let _ = CloseHandle(handle);
        }
    }
}

/// The process behind a window; for a Store app's frame (ApplicationFrameHost),
/// the app's own process, whose window sits inside the frame.
fn owning_process(hwnd: HWND) -> u32 {
    unsafe extern "system" fn other(child: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut (u32, u32));
        let mut pid = 0u32;
        GetWindowThreadProcessId(child, Some(&mut pid));
        if pid != 0 && pid != search.0 {
            search.1 = pid;
            return BOOL(0);
        }
        BOOL(1)
    }
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let mut class = [0u16; 64];
        let len = GetClassNameW(hwnd, &mut class).max(0) as usize;
        if String::from_utf16_lossy(&class[..len]) == "ApplicationFrameWindow" {
            let mut search = (pid, 0u32);
            let _ = EnumChildWindows(Some(hwnd), Some(other), LPARAM(&mut search as *mut _ as isize));
            return search.1;
        }
        pid
    }
}
