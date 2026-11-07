//! Starting, focusing and closing apps.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_LWIN, VK_MENU,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    IsIconic, PostMessageW, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOWNORMAL, WM_CLOSE,
};

use crate::util::{pcwstr, wide};

/// Opens a shortcut, program or `shell:AppsFolder\...` item.
pub fn launch(target: &str) {
    let target = wide(target);
    unsafe {
        ShellExecuteW(None, w!("open"), pcwstr(&target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
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
