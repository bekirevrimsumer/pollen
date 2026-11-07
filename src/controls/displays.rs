//! Brightness of each monitor on its own, over DDC/CI (like Monitorian).
//!
//! Nectar set one brightness on every monitor at once. Here every monitor gets its
//! own value. DDC/CI calls are slow (tens of milliseconds, sometimes failing on
//! the first try), so they run on a worker thread: the UI updates its copy right
//! away and the worker applies only the latest value per monitor.
//!
//! Monitor names come from the display configuration (the EDID name, e.g.
//! "ASUS VG279QE5A"), since DDC/CI often only reports "Generic PnP Monitor".

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows::core::BOOL;
use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
    GetNumberOfPhysicalMonitorsFromHMONITOR, GetPhysicalMonitorsFromHMONITOR, GetVCPFeatureAndVCPFeatureReply,
    QueryDisplayConfig, SetVCPFeature, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, PHYSICAL_MONITOR,
    QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// VCP code for brightness.
const BRIGHTNESS: u8 = 0x10;

#[derive(Clone, Debug)]
pub struct Display {
    /// "ASUS VG279QE5A"
    pub name: String,
    /// GDI device, e.g. `\\.\DISPLAY2`.
    pub device: String,
    pub left: i32,
    /// 0..=100, or None when the monitor does not answer DDC/CI.
    pub brightness: Option<u32>,
}

enum Command {
    Refresh,
    Set(String, u32),
}

static DISPLAYS: Mutex<Vec<Display>> = Mutex::new(Vec::new());
static WORKER: OnceLock<Mutex<Sender<Command>>> = OnceLock::new();

/// Starts the worker; `notify` gets `message` whenever the list or a value changes.
pub fn start(notify: HWND, message: u32) {
    let (tx, rx) = channel();
    if WORKER.set(Mutex::new(tx)).is_err() {
        return;
    }
    let notify = notify.0 as isize;
    std::thread::Builder::new()
        .name("pollen-displays".into())
        .spawn(move || run(rx, HWND(notify as *mut _), message))
        .ok();
    refresh();
}

fn send(command: Command) {
    if let Some(tx) = WORKER.get().and_then(|m| m.lock().ok()) {
        let _ = tx.send(command);
    }
}

/// Re-reads the monitors and their brightness.
pub fn refresh() {
    send(Command::Refresh);
}

/// The monitors, left to right.
pub fn snapshot() -> Vec<Display> {
    DISPLAYS.lock().map(|d| d.clone()).unwrap_or_default()
}

/// Reads the monitors right now on this thread (for `pollen --controls`).
pub fn list_now() -> Vec<Display> {
    enumerate().0
}

/// Sets one monitor's brightness (0..=100).
pub fn set(device: &str, value: u32) {
    let value = value.min(100);
    if let Ok(mut displays) = DISPLAYS.lock() {
        if let Some(d) = displays.iter_mut().find(|d| d.device == device) {
            d.brightness = Some(value);
        }
    }
    send(Command::Set(device.to_string(), value));
}

struct Physical {
    device: String,
    handles: Vec<PHYSICAL_MONITOR>,
    /// The monitor's own maximum for VCP 0x10 (usually 100).
    max: u32,
}

impl Drop for Physical {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyPhysicalMonitors(&self.handles);
        }
    }
}

fn run(rx: Receiver<Command>, notify: HWND, message: u32) {
    let mut monitors: Vec<Physical> = Vec::new();
    let post = || unsafe {
        let _ = PostMessageW(Some(notify), message, WPARAM(0), LPARAM(0));
    };
    while let Ok(first) = rx.recv() {
        // Coalesce: while dragging, only the newest value per monitor matters.
        let mut refresh = false;
        let mut sets: HashMap<String, u32> = HashMap::new();
        let mut handle = |c: Command| match c {
            Command::Refresh => refresh = true,
            Command::Set(device, value) => {
                sets.insert(device, value);
            }
        };
        handle(first);
        std::thread::sleep(Duration::from_millis(15));
        while let Ok(c) = rx.try_recv() {
            handle(c);
        }

        if refresh {
            monitors.clear();
            let (list, physical) = enumerate();
            monitors = physical;
            if let Ok(mut displays) = DISPLAYS.lock() {
                *displays = list;
            }
            post();
        }
        for (device, value) in sets {
            if let Some(m) = monitors.iter().find(|m| m.device == device) {
                let raw = m.max * value / 100;
                for handle in &m.handles {
                    for attempt in 0..3 {
                        if unsafe { SetVCPFeature(handle.hPhysicalMonitor, BRIGHTNESS, raw) } != 0 {
                            break;
                        }
                        if attempt < 2 {
                            std::thread::sleep(Duration::from_millis(40));
                        }
                    }
                }
            }
        }
    }
}

fn enumerate() -> (Vec<Display>, Vec<Physical>) {
    unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, lparam: LPARAM) -> BOOL {
        let list = &mut *(lparam.0 as *mut Vec<HMONITOR>);
        list.push(monitor);
        BOOL(1)
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut handles as *mut _ as isize));
    }
    let names = friendly_names();

    let mut displays = Vec::new();
    let mut physical = Vec::new();
    for monitor in handles {
        unsafe {
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            if !GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
                continue;
            }
            let device = crate::util::from_wide_buf(&info.szDevice);
            let mut count = 0u32;
            let mut brightness = None;
            let mut max = 100;
            let mut list = Vec::new();
            if GetNumberOfPhysicalMonitorsFromHMONITOR(monitor, &mut count).is_ok() && count > 0 {
                list = vec![PHYSICAL_MONITOR::default(); count as usize];
                if GetPhysicalMonitorsFromHMONITOR(monitor, &mut list).is_ok() {
                    if let Some(first) = list.first() {
                        for attempt in 0..3 {
                            let (mut current, mut maximum) = (0u32, 0u32);
                            if GetVCPFeatureAndVCPFeatureReply(
                                first.hPhysicalMonitor,
                                BRIGHTNESS,
                                None,
                                &mut current,
                                Some(&mut maximum),
                            ) != 0
                                && maximum > 0
                            {
                                max = maximum;
                                brightness = Some((current * 100 + maximum / 2) / maximum);
                                break;
                            }
                            if attempt < 2 {
                                std::thread::sleep(Duration::from_millis(40));
                            }
                        }
                    }
                } else {
                    list.clear();
                }
            }
            let fallback = list
                .first()
                .map(|m| {
                    // PHYSICAL_MONITOR is packed: copy the field out before borrowing it.
                    let description = m.szPhysicalMonitorDescription;
                    crate::util::from_wide_buf(&description)
                })
                .unwrap_or_else(|| device.clone());
            displays.push(Display {
                name: names.get(&device).cloned().unwrap_or(fallback),
                device: device.clone(),
                left: info.monitorInfo.rcMonitor.left,
                brightness,
            });
            if !list.is_empty() {
                physical.push(Physical { device, handles: list, max });
            }
        }
    }
    displays.sort_by_key(|d| d.left);
    (displays, physical)
}

/// GDI device name (`\\.\DISPLAY2`) -> monitor name from its EDID.
fn friendly_names() -> HashMap<String, String> {
    let mut names = HashMap::new();
    unsafe {
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count).is_err() {
            return names;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        )
        .is_err()
        {
            return names;
        }
        for path in paths.iter().take(path_count as usize) {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                },
                ..Default::default()
            };
            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                    size: std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                    adapterId: path.targetInfo.adapterId,
                    id: path.targetInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut source.header) == 0 && DisplayConfigGetDeviceInfo(&mut target.header) == 0
            {
                let device = crate::util::from_wide_buf(&source.viewGdiDeviceName);
                let name = crate::util::from_wide_buf(&target.monitorFriendlyDeviceName);
                if !name.trim().is_empty() {
                    names.insert(device, name.trim().to_string());
                }
            }
        }
    }
    names
}
