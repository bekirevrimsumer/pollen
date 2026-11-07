//! The default output device: its name, volume and mute (Core Audio).

use windows::core::GUID;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::Endpoints::{IAudioEndpointVolume, IAudioMeterInformation};
use windows::Win32::Media::Audio::{eMultimedia, eRender, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator};
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
        let _ = volume.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), &GUID::zeroed());
        if level > 0.0 && volume.GetMute().map(|m| m.as_bool()).unwrap_or(false) {
            let _ = volume.SetMute(false, &GUID::zeroed());
        }
    }
}

pub fn toggle_mute() {
    let Some((_, volume)) = endpoint() else { return };
    unsafe {
        let muted = volume.GetMute().map(|m| m.as_bool()).unwrap_or(false);
        let _ = volume.SetMute(!muted, &GUID::zeroed());
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
