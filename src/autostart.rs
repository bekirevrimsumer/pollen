//! Starting Pollen with Windows: a value under the user's `Run` key, set from
//! the Settings tab, the tray icon's menu or the installer. Off unless the
//! user turns it on.

use std::path::Path;

use windows::core::w;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

use crate::util::{pcwstr, wide};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn command_for(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

/// The command Windows runs at sign-in, if any.
fn current_value() -> Option<String> {
    let run = wide(RUN);
    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            pcwstr(&run),
            w!("Pollen"),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
        .ok()
        .ok()?;
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

/// Whether Windows starts this copy of Pollen at sign-in.
pub fn is_enabled() -> bool {
    match (current_value(), std::env::current_exe()) {
        (Some(value), Ok(exe)) => value.eq_ignore_ascii_case(&command_for(&exe)),
        _ => false,
    }
}

/// Whether Windows starts some copy of Pollen (an update keeps the choice).
pub fn is_enabled_anywhere() -> bool {
    current_value().is_some()
}

/// Turns it on for this copy, or off.
pub fn set(enabled: bool) {
    match (enabled, std::env::current_exe()) {
        (true, Ok(exe)) => enable_for(&exe),
        (true, Err(_)) => {}
        (false, _) => disable(),
    }
}

pub fn enable_for(exe: &Path) {
    let run = wide(RUN);
    let value = wide(&command_for(exe));
    unsafe {
        let _ = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            pcwstr(&run),
            w!("Pollen"),
            REG_SZ.0,
            Some(value.as_ptr() as *const _),
            (value.len() * 2) as u32,
        );
    }
}

pub fn disable() {
    let run = wide(RUN);
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, pcwstr(&run), w!("Pollen"));
    }
}
