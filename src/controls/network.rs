//! Network status and the Wi-Fi / Bluetooth radios.

use std::sync::Mutex;

use windows::Devices::Radios::{Radio, RadioKind, RadioState};
use windows::Win32::Networking::NetworkListManager::{
    INetworkListManager, NetworkListManager, NLM_CONNECTIVITY_IPV4_INTERNET, NLM_CONNECTIVITY_IPV6_INTERNET,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};

/// Whether the computer can reach the internet right now.
pub fn internet() -> bool {
    unsafe {
        let Ok(list) = CoCreateInstance::<_, INetworkListManager>(&NetworkListManager, None, CLSCTX_ALL) else {
            return false;
        };
        list.GetConnectivity()
            .map(|c| c.0 & (NLM_CONNECTIVITY_IPV4_INTERNET.0 | NLM_CONNECTIVITY_IPV6_INTERNET.0) != 0)
            .unwrap_or(false)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Radios {
    /// None when the computer has no such radio.
    pub wifi: Option<bool>,
    pub bluetooth: Option<bool>,
}

static RADIOS: Mutex<Radios> = Mutex::new(Radios { wifi: None, bluetooth: None });

pub fn radios() -> Radios {
    RADIOS.lock().map(|r| *r).unwrap_or_default()
}

/// Reads the radios on a background thread (the WinRT calls block), then runs
/// `done` there.
pub fn refresh_radios(done: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mut found = Radios::default();
        if let Ok(list) = Radio::GetRadiosAsync().and_then(|op| op.get()) {
            for radio in list {
                let on = radio.State().map(|s| s == RadioState::On).unwrap_or(false);
                match radio.Kind() {
                    Ok(RadioKind::WiFi) => found.wifi = Some(on),
                    Ok(RadioKind::Bluetooth) => found.bluetooth = Some(on),
                    _ => {}
                }
            }
        }
        if let Ok(mut r) = RADIOS.lock() {
            *r = found;
        }
        done();
    });
}

/// Turns a radio on or off on a background thread, then runs `done` there.
pub fn set_radio(kind: RadioKind, on: bool, done: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let _ = Radio::RequestAccessAsync().and_then(|op| op.get());
        if let Ok(list) = Radio::GetRadiosAsync().and_then(|op| op.get()) {
            for radio in list {
                if radio.Kind().ok() == Some(kind) {
                    let state = if on { RadioState::On } else { RadioState::Off };
                    let _ = radio.SetStateAsync(state).and_then(|op| op.get());
                }
            }
        }
        refresh_radios(done);
    });
}
