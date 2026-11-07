//! Starting Pollen with Windows: a value under the user's `Run` key, set from
//! the Settings tab or the tray icon's menu. Off unless the user turns it on.

use windows::core::w;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

use crate::util::{pcwstr, wide};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\"", exe.display()))
}

/// Whether Windows starts this copy of Pollen at sign-in.
pub fn is_enabled() -> bool {
    let run = wide(RUN);
    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            pcwstr(&run),
            w!("Pollen"),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    }
    .is_ok();
    if !ok {
        return false;
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    let value = String::from_utf16_lossy(&buffer[..len]);
    command().is_some_and(|c| c.eq_ignore_ascii_case(&value))
}

pub fn set(enabled: bool) {
    let run = wide(RUN);
    unsafe {
        if enabled {
            let Some(command) = command() else { return };
            let value = wide(&command);
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                pcwstr(&run),
                w!("Pollen"),
                REG_SZ.0,
                Some(value.as_ptr() as *const _),
                (value.len() * 2) as u32,
            );
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, pcwstr(&run), w!("Pollen"));
        }
    }
}
