//! Battery levels of Bluetooth headphones and other devices that report one,
//! as Windows' Bluetooth settings page shows them: a device property
//! ({104EA319-6EE2-4701-BD47-8DDBF425BBE5}, 2) set by the Bluetooth stack.
//! Read on a worker thread when the Control Center opens.

use std::sync::Mutex;

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDevicePropertyW, DIGCF_ALLCLASSES,
    DIGCF_PRESENT, SP_DEVINFO_DATA,
};
use windows::Win32::Devices::Properties::{DEVPKEY_Device_FriendlyName, DEVPROPTYPE};
use windows::Win32::Foundation::{DEVPROPKEY, HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// The Bluetooth battery level, 0..=100.
const BATTERY: DEVPROPKEY = DEVPROPKEY {
    fmtid: windows::core::GUID::from_u128(0x104ea319_6ee2_4701_bd47_8ddbf425bbe5),
    pid: 2,
};

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub name: String,
    pub percent: u8,
}

static LAST: Mutex<Vec<Device>> = Mutex::new(Vec::new());

/// What the last read found.
pub fn current() -> Vec<Device> {
    LAST.lock().map(|l| l.clone()).unwrap_or_default()
}

/// Reads again in the background; `notify` gets `message` when it changed.
pub fn refresh(notify: HWND, message: u32) {
    let notify = notify.0 as isize;
    std::thread::spawn(move || {
        let found = read();
        let changed = LAST.lock().map(|mut l| {
            let changed = *l != found;
            *l = found;
            changed
        });
        if changed.unwrap_or(false) {
            unsafe {
                let _ = PostMessageW(Some(HWND(notify as *mut _)), message, WPARAM(0), LPARAM(0));
            }
        }
    });
}

/// Every present device that reports a battery level.
pub fn read() -> Vec<Device> {
    let mut found = Vec::new();
    unsafe {
        let Ok(set) = SetupDiGetClassDevsW(None, None, None, DIGCF_PRESENT | DIGCF_ALLCLASSES) else { return found };
        let mut index = 0;
        loop {
            let mut info = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            if SetupDiEnumDeviceInfo(set, index, &mut info).is_err() {
                break;
            }
            index += 1;
            let mut kind = DEVPROPTYPE::default();
            let mut level = [0u8; 1];
            if SetupDiGetDevicePropertyW(set, &info, &BATTERY, &mut kind, Some(&mut level), None, 0).is_err() {
                continue;
            }
            let mut name = [0u8; 512];
            let mut size = 0u32;
            let name = if SetupDiGetDevicePropertyW(set, &info, &DEVPKEY_Device_FriendlyName, &mut kind, Some(&mut name), Some(&mut size), 0)
                .is_ok()
            {
                let units: Vec<u16> =
                    name[..size as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
                String::from_utf16_lossy(&units)
            } else {
                String::new()
            };
            if !name.is_empty() && level[0] <= 100 && !found.iter().any(|d: &Device| d.name == name) {
                found.push(Device { name, percent: level[0] });
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    found
}
