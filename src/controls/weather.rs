//! Current weather, like Nectar: the rough location from the IP address
//! (ipwho.is, falling back to geojs.io), then the weather for it from Open-Meteo.
//! Refreshed every 30 minutes on a worker thread.
//!
//! This sends the computer's public IP address to the location service. A city
//! chosen by hand can replace that once there is a settings window.

use std::sync::Mutex;
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_FLAG_SECURE,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

#[derive(Clone, Debug)]
pub struct Weather {
    pub city: String,
    pub temperature: f64,
    pub high: f64,
    pub low: f64,
    /// WMO weather code.
    pub code: i64,
    pub day: bool,
}

impl Weather {
    pub fn condition(&self) -> &'static str {
        match self.code {
            0 => "Açık",
            1 => "Az bulutlu",
            2 => "Parçalı bulutlu",
            3 => "Kapalı",
            45 | 48 => "Sisli",
            51..=57 => "Çiseliyor",
            61..=67 => "Yağmurlu",
            71..=77 => "Karlı",
            80..=82 => "Sağanak",
            85 | 86 => "Kar sağanağı",
            95..=99 => "Gök gürültülü",
            _ => "",
        }
    }
}

static WEATHER: Mutex<Option<Weather>> = Mutex::new(None);

pub fn current() -> Option<Weather> {
    WEATHER.lock().ok().and_then(|w| w.clone())
}

/// Starts refreshing; `notify` gets `message` after every successful read.
pub fn start(notify: HWND, message: u32) {
    let notify = notify.0 as isize;
    std::thread::Builder::new()
        .name("pollen-weather".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let mut place: Option<(f64, f64, String)> = None;
            loop {
                if place.is_none() {
                    place = locate();
                }
                let ok = match &place {
                    Some((lat, lon, city)) => match fetch(*lat, *lon, city) {
                        Some(weather) => {
                            if let Ok(mut w) = WEATHER.lock() {
                                *w = Some(weather);
                            }
                            unsafe {
                                let _ = PostMessageW(Some(HWND(notify as *mut _)), message, WPARAM(0), LPARAM(0));
                            }
                            true
                        }
                        None => false,
                    },
                    None => false,
                };
                // Try again sooner when offline or a service failed.
                std::thread::sleep(Duration::from_secs(if ok { 30 * 60 } else { 5 * 60 }));
                if !ok {
                    place = None;
                }
            }
        })
        .ok();
}

/// Locates and reads the weather once, on this thread (for `pollen --controls`).
pub fn check() -> Option<Weather> {
    let (lat, lon, city) = locate()?;
    fetch(lat, lon, &city)
}

/// GET over HTTPS with WinHTTP's defaults (the system proxy, normal
/// certificate validation), returning the body as JSON.
fn get_json(url: &str) -> Option<serde_json::Value> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, "/".to_string()));
    let host_w = crate::util::wide(host);
    let path_w = crate::util::wide(&path);
    unsafe {
        let session = WinHttpOpen(w!("Pollen"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() {
            return None;
        }
        let connect = WinHttpConnect(session, crate::util::pcwstr(&host_w), INTERNET_DEFAULT_HTTPS_PORT, 0);
        let request = if connect.is_null() {
            std::ptr::null_mut()
        } else {
            WinHttpOpenRequest(
                connect,
                w!("GET"),
                crate::util::pcwstr(&path_w),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        };
        let mut body = Vec::new();
        let ok = !request.is_null()
            && WinHttpSendRequest(request, None, None, 0, 0, 0).is_ok()
            && WinHttpReceiveResponse(request, std::ptr::null_mut()).is_ok();
        if ok {
            loop {
                let mut available = 0u32;
                if WinHttpQueryDataAvailable(request, &mut available).is_err() || available == 0 {
                    break;
                }
                let mut chunk = vec![0u8; available as usize];
                let mut read = 0u32;
                if WinHttpReadData(request, chunk.as_mut_ptr() as *mut _, available, &mut read).is_err() || read == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..read as usize]);
                if body.len() > 1_000_000 {
                    break;
                }
            }
        }
        for handle in [request, connect, session] {
            if !handle.is_null() {
                let _ = WinHttpCloseHandle(handle);
            }
        }
        if !ok {
            return None;
        }
        serde_json::from_slice(&body).ok()
    }
}

fn number(v: &serde_json::Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Latitude, longitude and city from the public IP address.
fn locate() -> Option<(f64, f64, String)> {
    if let Some(j) = get_json("https://ipwho.is/") {
        if j["success"].as_bool() != Some(false) {
            if let (Some(lat), Some(lon)) = (number(&j["latitude"]), number(&j["longitude"])) {
                return Some((lat, lon, j["city"].as_str().unwrap_or_default().to_string()));
            }
        }
    }
    let j = get_json("https://get.geojs.io/v1/ip/geo.json")?;
    Some((number(&j["latitude"])?, number(&j["longitude"])?, j["city"].as_str().unwrap_or_default().to_string()))
}

fn fetch(lat: f64, lon: f64, city: &str) -> Option<Weather> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat:.3}&longitude={lon:.3}\
         &current=temperature_2m,weather_code,is_day&daily=temperature_2m_max,temperature_2m_min\
         &forecast_days=1&timezone=auto"
    );
    let j = get_json(&url)?;
    let current = &j["current"];
    Some(Weather {
        city: city.to_string(),
        temperature: number(&current["temperature_2m"])?,
        code: current["weather_code"].as_i64().unwrap_or(-1),
        day: current["is_day"].as_i64() != Some(0),
        high: number(&j["daily"]["temperature_2m_max"][0]).unwrap_or(f64::NAN),
        low: number(&j["daily"]["temperature_2m_min"][0]).unwrap_or(f64::NAN),
    })
}
