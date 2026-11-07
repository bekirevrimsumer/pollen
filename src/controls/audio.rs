//! The default output device: its name, volume and mute, and the apps' own
//! volumes in the mixer (Core Audio).

use windows::core::{Interface, GUID, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, PROPERTYKEY, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl, IAudioMeterInformation,
};
use windows::Win32::Media::Audio::{
    eMultimedia, eRender, EDataFlow, ERole, IAudioSessionControl2, IMMNotificationClient, IMMNotificationClient_Impl,
    AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator, ISimpleAudioVolume,
    MMDeviceEnumerator,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub name: String,
    /// 0..=1
    pub volume: f32,
    pub muted: bool,
}

fn endpoint() -> Option<(IMMDevice, IAudioEndpointVolume)> {
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eMultimedia).ok()?;
        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).ok()?;
        Some((device, volume))
    }
}

fn device_name(device: &IMMDevice) -> Option<String> {
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let mut value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
        let name = PropVariantToStringAlloc(&value).ok().map(|p| {
            let s = String::from_utf16_lossy(p.as_wide());
            CoTaskMemFree(Some(p.0 as *const _));
            s
        });
        let _ = PropVariantClear(&mut value);
        name
    }
}

pub fn read() -> Option<Output> {
    let (device, volume) = endpoint()?;
    unsafe {
        Some(Output {
            name: device_name(&device).unwrap_or_default(),
            volume: volume.GetMasterVolumeLevelScalar().ok()?,
            muted: volume.GetMute().ok()?.as_bool(),
        })
    }
}

/// Sets the volume (0..=1); turning it up unmutes.
pub fn set_volume(level: f32) {
    let Some((_, volume)) = endpoint() else { return };
    unsafe {
        let _ = volume.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), &POLLEN_CHANGE);
        if level > 0.0 && volume.GetMute().map(|m| m.as_bool()).unwrap_or(false) {
            let _ = volume.SetMute(false, &POLLEN_CHANGE);
        }
    }
}

pub fn toggle_mute() {
    let Some((_, volume)) = endpoint() else { return };
    unsafe {
        let muted = volume.GetMute().map(|m| m.as_bool()).unwrap_or(false);
        let _ = volume.SetMute(!muted, &POLLEN_CHANGE);
    }
}

/// The output's current loudness per channel, for the notch's level bars.
/// Keeps the meter between calls and picks up a new default device every few
/// seconds.
pub struct Meter {
    meter: Option<(IAudioMeterInformation, std::time::Instant)>,
}

impl Meter {
    pub fn new() -> Self {
        Meter { meter: None }
    }

    /// Left and right peak levels, 0..=1.
    pub fn peaks(&mut self) -> (f32, f32) {
        let stale = self.meter.as_ref().is_none_or(|(_, at)| at.elapsed().as_secs() >= 5);
        if stale {
            self.meter = unsafe {
                let devices: Option<IMMDeviceEnumerator> = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok();
                devices
                    .and_then(|d| d.GetDefaultAudioEndpoint(eRender, eMultimedia).ok())
                    .and_then(|d| d.Activate::<IAudioMeterInformation>(CLSCTX_ALL, None).ok())
                    .map(|m| (m, std::time::Instant::now()))
            };
        }
        let Some((meter, _)) = &self.meter else { return (0.0, 0.0) };
        unsafe {
            let count = meter.GetMeteringChannelCount().unwrap_or(0).clamp(0, 8) as usize;
            if count == 0 {
                let p = meter.GetPeakValue().unwrap_or(0.0);
                return (p, p);
            }
            let mut values = vec![0f32; count];
            if meter.GetChannelsPeakValues(&mut values).is_err() {
                return (0.0, 0.0);
            }
            (values[0], if count > 1 { values[1] } else { values[0] })
        }
    }
}

/// One app's own volume in Windows' mixer: what the volume mixer shows next to
/// the app, applied on top of the device volume.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AppVolume {
    /// 0..=1
    pub volume: f32,
    pub muted: bool,
}

/// A media session's app id ("Chrome", "Spotify.exe",
/// "SpotifyAB.SpotifyMusic_zpdnekdrxrrzw!Spotify") boiled down to a name to
/// compare with process names.
fn app_key(app: &str) -> String {
    let app = app.to_lowercase();
    // Firefox and its forks (Zen, LibreWolf) report a hash of their install
    // folder; their display name has to do.
    if app.len() == 16 && app.chars().all(|c| c.is_ascii_hexdigit()) {
        return String::new();
    }
    let app = app.rsplit(['\\', '/']).next().unwrap_or(&app);
    let app = app.split('!').next().unwrap_or(app);
    let app = app.split('_').next().unwrap_or(app);
    app.trim_end_matches(".exe").to_string()
}

/// The lower-case file name, without ".exe", of a process.
fn process_name(pid: u32) -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 520];
        let mut len = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(process);
        if !ok {
            return None;
        }
        let path = String::from_utf16_lossy(&buffer[..len as usize]).to_lowercase();
        let name = path.rsplit('\\').next().unwrap_or(&path);
        Some(name.trim_end_matches(".exe").to_string())
    }
}

/// The mixer sessions on the default output that belong to a media session's
/// app. A browser has one for all its tabs, so this is the whole browser.
/// `app` is the media session's app id, `name` its display name ("Zen").
fn app_sessions(app: &str, name: &str) -> Vec<ISimpleAudioVolume> {
    let keys: Vec<String> = [app_key(app), name.to_lowercase().replace(' ', "")]
        .into_iter()
        .filter(|k| k.len() >= 3)
        .collect();
    if keys.is_empty() {
        return Vec::new();
    }
    let Some((device, _)) = endpoint() else { return Vec::new() };
    let mut found = Vec::new();
    unsafe {
        let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { return found };
        let Ok(sessions) = manager.GetSessionEnumerator() else { return found };
        for i in 0..sessions.GetCount().unwrap_or(0) {
            let Ok(session) = sessions.GetSession(i) else { continue };
            let Ok(session2) = session.cast::<IAudioSessionControl2>() else { continue };
            let pid = session2.GetProcessId().unwrap_or(0);
            if pid == 0 {
                continue;
            }
            let Some(process) = process_name(pid) else { continue };
            if process.len() >= 3 && keys.iter().any(|k| k.contains(&process) || process.contains(k.as_str())) {
                if let Ok(volume) = session.cast::<ISimpleAudioVolume>() {
                    found.push(volume);
                }
            }
        }
    }
    found
}

/// The app's mixer volume, if it has a session on the default output.
pub fn app_volume(app: &str, name: &str) -> Option<AppVolume> {
    let session = app_sessions(app, name).into_iter().next()?;
    unsafe {
        Some(AppVolume {
            volume: session.GetMasterVolume().ok()?,
            muted: session.GetMute().ok()?.as_bool(),
        })
    }
}

/// Sets the app's mixer volume (0..=1) on all its sessions; turning it up unmutes.
pub fn set_app_volume(app: &str, name: &str, level: f32) {
    for session in app_sessions(app, name) {
        unsafe {
            let _ = session.SetMasterVolume(level.clamp(0.0, 1.0), std::ptr::null());
            if level > 0.0 && session.GetMute().map(|m| m.as_bool()).unwrap_or(false) {
                let _ = session.SetMute(false, std::ptr::null());
            }
        }
    }
}

pub fn toggle_app_mute(app: &str, name: &str) {
    let sessions = app_sessions(app, name);
    let Some(first) = sessions.first() else { return };
    let muted = unsafe { first.GetMute().map(|m| m.as_bool()).unwrap_or(false) };
    for session in &sessions {
        unsafe {
            let _ = session.SetMute(!muted, std::ptr::null());
        }
    }
}

/// Marks volume changes Pollen makes itself, so the change notifications for
/// them can be told apart from the keyboard's.
const POLLEN_CHANGE: GUID = GUID::from_u128(0x5f0c_2a71_7d1e_4b6a_9c3e_0b8a_6d2f_41a9);

/// Tells a window about every volume change on the default output (from the
/// keyboard, the tray, an app) and about a new default output.
pub struct Watch {
    devices: IMMDeviceEnumerator,
    client: IMMNotificationClient,
    endpoint: Option<(IAudioEndpointVolume, IAudioEndpointVolumeCallback)>,
    notify: isize,
    volume_message: u32,
}

#[windows::core::implement(IAudioEndpointVolumeCallback)]
struct VolumeCallback {
    notify: isize,
    message: u32,
}

impl IAudioEndpointVolumeCallback_Impl for VolumeCallback_Impl {
    fn OnNotify(&self, data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows::core::Result<()> {
        let Some(data) = (unsafe { data.as_ref() }) else { return Ok(()) };
        if data.guidEventContext == POLLEN_CHANGE {
            return Ok(());
        }
        // Volume in thousandths, and the mute flag above it.
        let packed = (data.fMasterVolume.clamp(0.0, 1.0) * 1000.0).round() as usize | ((data.bMuted.as_bool() as usize) << 16);
        unsafe {
            let _ = PostMessageW(Some(HWND(self.notify as *mut _)), self.message, WPARAM(packed), LPARAM(0));
        }
        Ok(())
    }
}

#[windows::core::implement(IMMNotificationClient)]
struct DeviceCallback {
    notify: isize,
    message: u32,
}

impl IMMNotificationClient_Impl for DeviceCallback_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _: &PCWSTR) -> windows::core::Result<()> {
        if flow == eRender && role == eMultimedia {
            unsafe {
                let _ = PostMessageW(Some(HWND(self.notify as *mut _)), self.message, WPARAM(0), LPARAM(0));
            }
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

impl Watch {
    /// `volume_message` carries the volume (wparam & 0xFFFF, in thousandths)
    /// and mute (bit 16); `device_message` asks for `rearm`.
    pub fn start(notify: HWND, volume_message: u32, device_message: u32) -> Option<Watch> {
        unsafe {
            let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
            let client: IMMNotificationClient = DeviceCallback { notify: notify.0 as isize, message: device_message }.into();
            devices.RegisterEndpointNotificationCallback(&client).ok()?;
            let mut watch = Watch { devices, client, endpoint: None, notify: notify.0 as isize, volume_message };
            watch.rearm();
            Some(watch)
        }
    }

    /// Follows the default output after it changed.
    pub fn rearm(&mut self) {
        unsafe {
            if let Some((volume, callback)) = self.endpoint.take() {
                let _ = volume.UnregisterControlChangeNotify(&callback);
            }
            let Ok(device) = self.devices.GetDefaultAudioEndpoint(eRender, eMultimedia) else { return };
            let Ok(volume) = device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) else { return };
            let callback: IAudioEndpointVolumeCallback =
                VolumeCallback { notify: self.notify, message: self.volume_message }.into();
            if volume.RegisterControlChangeNotify(&callback).is_ok() {
                self.endpoint = Some((volume, callback));
            }
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        unsafe {
            if let Some((volume, callback)) = self.endpoint.take() {
                let _ = volume.UnregisterControlChangeNotify(&callback);
            }
            let _ = self.devices.UnregisterEndpointNotificationCallback(&self.client);
        }
    }
}
