//! Current weather, like Nectar: the rough location from the IP address
//! (ipwho.is, falling back to geojs.io), then the weather for it from Open-Meteo.
//! Refreshed every 30 minutes on a worker thread.
//!
//! That sends the computer's public IP address to the location service. A
//! place picked by hand in the notch's Settings tab (found with Open-Meteo's
//! geocoding) replaces the IP lookup; it is kept in
//! `%APPDATA%\Pollen\weather-place.txt`.

use std::sync::mpsc::{channel, Sender};
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
    /// The next hours.
    pub hours: Vec<Hour>,
    /// The next days, after today.
    pub days: Vec<Day>,
}

#[derive(Clone, Debug)]
pub struct Hour {
    pub hour: u32,
    pub temperature: f64,
    pub code: i64,
    pub day: bool,
}

#[derive(Clone, Debug)]
pub struct Day {
    /// Year, month, day.
    pub date: (i32, u32, u32),
    pub high: f64,
    pub low: f64,
    pub code: i64,
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
/// Wakes the worker to read the weather again now.
static WAKE: Mutex<Option<Sender<()>>> = Mutex::new(None);

/// A place to read the weather for.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub name: String,
    /// "Kayseri, Türkiye": where it is, for telling places apart.
    pub region: String,
    pub latitude: f64,
    pub longitude: f64,
}

fn place_path() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var_os("APPDATA")?).join("Pollen");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("weather-place.txt"))
}

/// The place picked by hand, if any.
pub fn saved_place() -> Option<Place> {
    let text = std::fs::read_to_string(place_path()?).ok()?;
    let mut parts = text.trim().split('|');
    let name = parts.next()?.to_string();
    let region = parts.next()?.to_string();
    let latitude = parts.next()?.parse().ok()?;
    let longitude = parts.next()?.parse().ok()?;
    Some(Place { name, region, latitude, longitude })
}

/// Picks a place by hand, or goes back to the IP location with `None`, and
/// reads the weather again.
pub fn set_place(place: Option<&Place>) {
    if let Some(path) = place_path() {
        match place {
            Some(p) => {
                let clean = |s: &str| s.replace(['|', '\n', '\r'], " ");
                let _ = std::fs::write(path, format!("{}|{}|{}|{}", clean(&p.name), clean(&p.region), p.latitude, p.longitude));
            }
            None => {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    if let Ok(mut w) = WEATHER.lock() {
        *w = None;
    }
    if let Some(wake) = WAKE.lock().ok().and_then(|w| w.clone()) {
        let _ = wake.send(());
    }
}

/// Places matching a name, best first (Open-Meteo geocoding, in Turkish).
pub fn search(name: &str) -> Vec<Place> {
    let query: String = name
        .trim()
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    if query.is_empty() {
        return Vec::new();
    }
    let url = format!("https://geocoding-api.open-meteo.com/v1/search?name={query}&count=4&language=tr&format=json");
    let Some(j) = get_json(&url) else { return Vec::new() };
    let Some(results) = j["results"].as_array() else { return Vec::new() };
    results
        .iter()
        .filter_map(|r| {
            let name = r["name"].as_str()?.to_string();
            let mut region: Vec<&str> = Vec::new();
            if let Some(a) = r["admin1"].as_str().filter(|a| *a != name) {
                region.push(a);
            }
            if let Some(c) = r["country"].as_str() {
                region.push(if c == "Türkiye Cumhuriyeti" { "Türkiye" } else { c });
            }
            Some(Place {
                region: region.join(", "),
                name,
                latitude: number(&r["latitude"])?,
                longitude: number(&r["longitude"])?,
            })
        })
        .collect()
}

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
            let (wake, woken) = channel();
            if let Ok(mut w) = WAKE.lock() {
                *w = Some(wake);
            }
            let mut place: Option<(f64, f64, String)> = None;
            loop {
                // A place picked by hand wins over the IP location.
                if let Some(p) = saved_place() {
                    place = Some((p.latitude, p.longitude, p.name));
                } else if place.is_none() {
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
                // Try again sooner when offline or a service failed; a new
                // place wakes the worker at once.
                let woke = woken.recv_timeout(Duration::from_secs(if ok { 30 * 60 } else { 5 * 60 })).is_ok();
                if !ok || woke {
                    place = None;
                }
            }
        })
        .ok();
}

/// Locates and reads the weather once, on this thread (for `pollen --controls`).
pub fn check() -> Option<Weather> {
    let (lat, lon, city) = match saved_place() {
        Some(p) => (p.latitude, p.longitude, p.name),
        None => locate()?,
    };
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
         &current=temperature_2m,weather_code,is_day&hourly=temperature_2m,weather_code,is_day\
         &daily=weather_code,temperature_2m_max,temperature_2m_min&forecast_days=6&timezone=auto"
    );
    let j = get_json(&url)?;
    let current = &j["current"];
    let now = current["time"].as_str().unwrap_or_default().to_string();

    // The hours after the current one ("2026-11-07T18:00" sorts like time).
    let hourly = &j["hourly"];
    let mut hours = Vec::new();
    if let Some(times) = hourly["time"].as_array() {
        for (i, time) in times.iter().enumerate() {
            let Some(time) = time.as_str() else { continue };
            if time <= now.as_str() {
                continue;
            }
            let Some(temperature) = number(&hourly["temperature_2m"][i]) else { continue };
            hours.push(Hour {
                hour: time.get(11..13).and_then(|h| h.parse().ok()).unwrap_or(0),
                temperature,
                code: hourly["weather_code"][i].as_i64().unwrap_or(-1),
                day: hourly["is_day"][i].as_i64() != Some(0),
            });
            if hours.len() == 6 {
                break;
            }
        }
    }

    let daily = &j["daily"];
    let mut days = Vec::new();
    if let Some(dates) = daily["time"].as_array() {
        for (i, date) in dates.iter().enumerate().skip(1) {
            let Some(date) = date.as_str() else { continue };
            let mut parts = date.split('-').filter_map(|p| p.parse::<i64>().ok());
            let (Some(y), Some(m), Some(d)) = (parts.next(), parts.next(), parts.next()) else { continue };
            days.push(Day {
                date: (y as i32, m as u32, d as u32),
                high: number(&daily["temperature_2m_max"][i]).unwrap_or(f64::NAN),
                low: number(&daily["temperature_2m_min"][i]).unwrap_or(f64::NAN),
                code: daily["weather_code"][i].as_i64().unwrap_or(-1),
            });
        }
    }

    Some(Weather {
        city: city.to_string(),
        temperature: number(&current["temperature_2m"])?,
        code: current["weather_code"].as_i64().unwrap_or(-1),
        day: current["is_day"].as_i64() != Some(0),
        high: number(&daily["temperature_2m_max"][0]).unwrap_or(f64::NAN),
        low: number(&daily["temperature_2m_min"][0]).unwrap_or(f64::NAN),
        hours,
        days,
    })
}
