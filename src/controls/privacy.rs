//! Which apps are using the microphone or the camera right now.
//!
//! Windows records each app's use under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore`:
//! a `LastUsedTimeStop` of 0 means the app is using it now. A worker thread
//! waits for that key to change (no polling) and tells the notch.

use std::sync::Mutex;

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER,
    KEY_NOTIFY, KEY_READ, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME, RRF_RT_REG_QWORD,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::util::{pcwstr, wide};

const STORE: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InUse {
    /// The apps using the microphone, by name.
    pub microphone: Vec<String>,
    pub camera: Vec<String>,
}

impl InUse {
    pub fn any(&self) -> bool {
        !self.microphone.is_empty() || !self.camera.is_empty()
    }
}

static CURRENT: Mutex<Option<InUse>> = Mutex::new(None);

pub fn current() -> InUse {
    CURRENT.lock().ok().and_then(|c| c.clone()).unwrap_or_default()
}

/// Starts watching; `notify` gets `message` whenever the apps in use change.
pub fn start(notify: HWND, message: u32) {
    let notify = notify.0 as isize;
    std::thread::Builder::new()
        .name("pollen-privacy".into())
        .spawn(move || unsafe {
            let mut key = HKEY::default();
            let store = wide(STORE);
            if RegOpenKeyExW(HKEY_CURRENT_USER, pcwstr(&store), None, KEY_NOTIFY | KEY_READ, &mut key).is_err() {
                return;
            }
            let mut last = InUse::default();
            loop {
                let now = read();
                if now != last {
                    if let Ok(mut c) = CURRENT.lock() {
                        *c = Some(now.clone());
                    }
                    let _ = PostMessageW(Some(HWND(notify as *mut _)), message, WPARAM(0), LPARAM(0));
                    last = now;
                }
                // Blocks until something under the store changes.
                if RegNotifyChangeKeyValue(key, true, REG_NOTIFY_CHANGE_LAST_SET | REG_NOTIFY_CHANGE_NAME, None, false)
                    .is_err()
                {
                    break;
                }
            }
            let _ = RegCloseKey(key);
        })
        .ok();
}

/// Reads who is using the microphone and the camera now.
pub fn read() -> InUse {
    InUse { microphone: in_use("microphone"), camera: in_use("webcam") }
}

fn in_use(capability: &str) -> Vec<String> {
    let mut names = Vec::new();
    let base = format!(r"{STORE}\{capability}");
    for app in subkeys(&base) {
        if app == "NonPackaged" {
            for exe in subkeys(&format!(r"{base}\NonPackaged")) {
                if active(&format!(r"{base}\NonPackaged\{exe}")) {
                    names.push(exe_name(&exe.replace('#', "\\")));
                }
            }
        } else if active(&format!(r"{base}\{app}")) {
            names.push(package_name(&app));
        }
    }
    names.sort();
    names.dedup();
    names
}

fn subkeys(path: &str) -> Vec<String> {
    let mut list = Vec::new();
    unsafe {
        let mut key = HKEY::default();
        let p = wide(path);
        if RegOpenKeyExW(HKEY_CURRENT_USER, pcwstr(&p), None, KEY_READ, &mut key).is_err() {
            return list;
        }
        let mut index = 0;
        loop {
            let mut buffer = [0u16; 512];
            let mut len = buffer.len() as u32;
            if RegEnumKeyExW(key, index, Some(PWSTR(buffer.as_mut_ptr())), &mut len, None, None, None, None).is_err() {
                break;
            }
            list.push(String::from_utf16_lossy(&buffer[..len as usize]));
            index += 1;
        }
        let _ = RegCloseKey(key);
    }
    list
}

/// Started and not yet stopped.
fn active(path: &str) -> bool {
    let read = |name: PCWSTR| -> Option<u64> {
        let p = wide(path);
        let mut value = 0u64;
        let mut size = 8u32;
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                pcwstr(&p),
                name,
                RRF_RT_REG_QWORD,
                None,
                Some(&mut value as *mut u64 as *mut _),
                Some(&mut size),
            )
            .ok()
            .ok()?;
        }
        Some(value)
    };
    matches!((read(w!("LastUsedTimeStart")), read(w!("LastUsedTimeStop"))), (Some(start), Some(0)) if start > 0)
}

/// "C:\Program Files\Zen Browser\zen.exe" -> the file's description ("Zen").
fn exe_name(path: &str) -> String {
    crate::apps::exe_description(path).unwrap_or_else(|| {
        std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
    })
}

/// "5319275A.WhatsAppDesktop_cv1g1gvanyjgm" -> "WhatsAppDesktop".
fn package_name(family: &str) -> String {
    let name = family.split('_').next().unwrap_or(family);
    name.rsplit('.').next().unwrap_or(name).to_string()
}
