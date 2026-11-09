//! Starting, focusing and closing apps.

use windows::core::{w, Interface, Result, BSTR, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
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
    AllowSetForegroundWindow, GetShellWindow, GetWindowThreadProcessId, IsIconic, PostMessageW, SetForegroundWindow,
    ShowWindow, SW_RESTORE, SW_SHOWNORMAL, WM_CLOSE,
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
