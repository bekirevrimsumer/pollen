//! "Apply to Windows": the Pollen theme carried over to Windows itself, each
//! part turned on separately in Settings › Windows:
//!
//! - light or dark mode, and the accent color (Windows' own settings),
//! - every window's border and title bar colors and corner shape (DWM window
//!   attributes, set on each app window as it opens),
//! - a wallpaper made from the theme's colors.
//!
//! Before Pollen first changes one of Windows' settings, it keeps the user's
//! own value in `%APPDATA%\Pollen\windows-original.txt`; turning the part off
//! (or uninstalling) puts it back. Window colors last only while Pollen runs.

use std::collections::HashMap;
use std::path::PathBuf;

use windows::core::HSTRING;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows::Win32::System::Registry::{
    RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_BINARY, REG_DWORD, REG_SZ, REG_VALUE_TYPE, RRF_RT_ANY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, SystemParametersInfoW, HWND_BROADCAST, SMTO_ABORTIFHUNG, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE,
    SPI_GETDESKWALLPAPER, SPI_SETDESKWALLPAPER, WM_SETTINGCHANGE,
};

use crate::settings::{self, Palette, Settings, Theme};
use crate::util::{pcwstr, wide};

const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const ACCENT: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent";
const DWM: &str = r"Software\Microsoft\Windows\DWM";
const DESKTOP: &str = r"Control Panel\Desktop";

// DWM window attributes (Windows 11).
const CORNER_PREFERENCE: u32 = 33;
const BORDER_COLOR: u32 = 34;
const CAPTION_COLOR: u32 = 35;
const TEXT_COLOR: u32 = 36;
const COLOR_DEFAULT: u32 = 0xFFFF_FFFF;

// ---- the user's own values ---------------------------------------------------------------

fn originals_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Pollen").join("windows-original.txt"))
}

/// "key\tvalue" lines; values as stored in the registry, hex-encoded.
fn read_originals() -> HashMap<String, String> {
    let Some(text) = originals_path().and_then(|p| std::fs::read_to_string(p).ok()) else { return HashMap::new() };
    text.lines().filter_map(|l| l.split_once('\t')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn write_originals(map: &HashMap<String, String>) {
    if let Some(p) = originals_path() {
        let mut lines: Vec<String> = map.iter().map(|(k, v)| format!("{k}\t{v}")).collect();
        lines.sort();
        let _ = std::fs::write(p, lines.join("\n") + "\n");
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).filter_map(|i| u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()).collect()
}

/// A registry value under HKCU as (type, bytes), or None if it is not there.
fn read_value(key: &str, name: &str) -> Option<(u32, Vec<u8>)> {
    let key_w = wide(key);
    let name_w = wide(name);
    let mut kind = REG_VALUE_TYPE::default();
    let mut size = 0u32;
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, pcwstr(&key_w), pcwstr(&name_w), RRF_RT_ANY, Some(&mut kind), None, Some(&mut size))
            .ok()
            .ok()?;
        let mut buf = vec![0u8; size as usize];
        RegGetValueW(
            HKEY_CURRENT_USER,
            pcwstr(&key_w),
            pcwstr(&name_w),
            RRF_RT_ANY,
            Some(&mut kind),
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
        .ok()
        .ok()?;
        buf.truncate(size as usize);
        Some((kind.0, buf))
    }
}

fn write_value(key: &str, name: &str, kind: u32, data: &[u8]) {
    let key_w = wide(key);
    let name_w = wide(name);
    unsafe {
        let _ = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            pcwstr(&key_w),
            pcwstr(&name_w),
            kind,
            Some(data.as_ptr() as *const _),
            data.len() as u32,
        );
    }
}

fn write_dword(key: &str, name: &str, value: u32) {
    write_value(key, name, REG_DWORD.0, &value.to_le_bytes());
}

/// Keeps the user's value of these settings, once, before Pollen first changes them.
fn remember(group: &str, values: &[(&str, &str)]) {
    let mut map = read_originals();
    if map.keys().any(|k| k.starts_with(&format!("{group}:"))) {
        return;
    }
    for (key, name) in values {
        let entry = match read_value(key, name) {
            Some((kind, data)) => format!("{kind}:{}", hex(&data)),
            None => "absent".to_string(),
        };
        map.insert(format!("{group}:{key}\\{name}"), entry);
    }
    write_originals(&map);
}

/// Puts back what `remember` kept for this group, and forgets it.
fn restore(group: &str) -> bool {
    let mut map = read_originals();
    let prefix = format!("{group}:");
    let entries: Vec<(String, String)> = map.iter().filter(|(k, _)| k.starts_with(&prefix)).map(|(k, v)| (k.clone(), v.clone())).collect();
    if entries.is_empty() {
        return false;
    }
    for (k, v) in &entries {
        let path = &k[prefix.len()..];
        let Some((key, name)) = path.rsplit_once('\\') else { continue };
        match v.split_once(':') {
            Some((kind, data)) => {
                if let Ok(kind) = kind.parse::<u32>() {
                    write_value(key, name, kind, &unhex(data));
                }
            }
            // It was not there before Pollen set it.
            None => unsafe {
                let key_w = wide(key);
                let name_w = wide(name);
                let _ = windows::Win32::System::Registry::RegDeleteKeyValueW(HKEY_CURRENT_USER, pcwstr(&key_w), pcwstr(&name_w));
            },
        }
        map.remove(k);
    }
    write_originals(&map);
    true
}

/// Tells Windows and the apps that the colors changed.
fn announce(what: &str) {
    let what = HSTRING::from(what);
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(what.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            1000,
            None,
        );
    }
}

// ---- light or dark mode -------------------------------------------------------------------

const MODE_VALUES: [(&str, &str); 2] = [(PERSONALIZE, "AppsUseLightTheme"), (PERSONALIZE, "SystemUsesLightTheme")];

fn apply_mode(light: bool) {
    remember("mode", &MODE_VALUES);
    write_dword(PERSONALIZE, "AppsUseLightTheme", light as u32);
    write_dword(PERSONALIZE, "SystemUsesLightTheme", light as u32);
    announce("ImmersiveColorSet");
}

// ---- the accent color ---------------------------------------------------------------------

const ACCENT_VALUES: [(&str, &str); 7] = [
    (ACCENT, "AccentPalette"),
    (ACCENT, "AccentColorMenu"),
    (ACCENT, "StartColorMenu"),
    (DWM, "AccentColor"),
    (DWM, "ColorizationColor"),
    (DWM, "ColorizationAfterglow"),
    (DESKTOP, "AutoColorization"),
];

fn byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// 0xAABBGGRR, as most of these values store a color.
fn abgr((r, g, b): (f32, f32, f32)) -> u32 {
    0xFF00_0000 | (byte(b) as u32) << 16 | (byte(g) as u32) << 8 | byte(r) as u32
}

/// 0xAARRGGBB, as DWM's colorization values store it.
fn argb((r, g, b): (f32, f32, f32), alpha: u8) -> u32 {
    (alpha as u32) << 24 | (byte(r) as u32) << 16 | (byte(g) as u32) << 8 | byte(b) as u32
}

fn shade((r, g, b): (f32, f32, f32), amount: f32) -> (f32, f32, f32) {
    // Toward white (> 0) or black (< 0).
    let mix = |c: f32| if amount >= 0.0 { c + (1.0 - c) * amount } else { c * (1.0 + amount) };
    (mix(r), mix(g), mix(b))
}

fn apply_accent(accent: (f32, f32, f32)) {
    remember("accent", &ACCENT_VALUES);
    // Windows' eight shades: three lighter, the color, three darker, and one more.
    let shades = [0.5, 0.33, 0.16, 0.0, -0.2, -0.38, -0.55, -0.7].map(|a| shade(accent, a));
    let mut palette = Vec::with_capacity(32);
    for (r, g, b) in shades {
        palette.extend([byte(r), byte(g), byte(b), 0xFF]);
    }
    write_value(ACCENT, "AccentPalette", REG_BINARY.0, &palette);
    write_dword(ACCENT, "AccentColorMenu", abgr(shades[4]));
    write_dword(ACCENT, "StartColorMenu", abgr(shades[5]));
    write_dword(DWM, "AccentColor", abgr(shades[4]));
    write_dword(DWM, "ColorizationColor", argb(shades[4], 0xC4));
    write_dword(DWM, "ColorizationAfterglow", argb(shades[4], 0xC4));
    let zero = wide("0");
    write_value(DESKTOP, "AutoColorization", REG_SZ.0, unsafe { std::slice::from_raw_parts(zero.as_ptr() as *const u8, zero.len() * 2) });
    announce("ImmersiveColorSet");
}

// ---- the wallpaper ------------------------------------------------------------------------

fn current_wallpaper() -> String {
    let mut buf = [0u16; 520];
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETDESKWALLPAPER, buf.len() as u32, Some(buf.as_mut_ptr() as *mut _), Default::default());
    }
    crate::util::from_wide_buf(&buf)
}

fn set_wallpaper(path: &str) {
    let path_w = wide(path);
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_SETDESKWALLPAPER,
            0,
            Some(path_w.as_ptr() as *mut _),
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        );
    }
}

fn wallpaper_path(theme: Theme) -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Pollen").join(format!("wallpaper-{theme:?}.png").to_lowercase()))
}

fn apply_wallpaper(theme: Theme, palette: Palette) {
    let mut map = read_originals();
    if !map.contains_key("wallpaper:path") {
        map.insert("wallpaper:path".into(), hex(current_wallpaper().as_bytes()));
        write_originals(&map);
    }
    let Some(path) = wallpaper_path(theme) else { return };
    if !path.exists() && crate::wallpaper::render(&path, palette).is_err() {
        return;
    }
    // Fill the screen (WallpaperStyle 10).
    let ten = wide("10");
    let zero = wide("0");
    let bytes = |w: &Vec<u16>| unsafe { std::slice::from_raw_parts(w.as_ptr() as *const u8, w.len() * 2).to_vec() };
    write_value(DESKTOP, "WallpaperStyle", REG_SZ.0, &bytes(&ten));
    write_value(DESKTOP, "TileWallpaper", REG_SZ.0, &bytes(&zero));
    set_wallpaper(&path.display().to_string());
}

fn restore_wallpaper() {
    let mut map = read_originals();
    if let Some(original) = map.remove("wallpaper:path") {
        write_originals(&map);
        set_wallpaper(&String::from_utf8_lossy(&unhex(&original)));
    }
}

// ---- windows ------------------------------------------------------------------------------

fn colorref((r, g, b): (f32, f32, f32)) -> u32 {
    (byte(b) as u32) << 16 | (byte(g) as u32) << 8 | byte(r) as u32
}

fn set_attribute(hwnd: HWND, attribute: u32, value: u32) {
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(attribute as i32),
            &value as *const u32 as *const _,
            4,
        );
    }
}

/// One app window, as the settings say (called as windows open).
pub fn decorate(hwnd: HWND) {
    let s = settings::get();
    if !(s.win_borders || s.win_titles || s.win_corners != 0) {
        return;
    }
    let p = settings::palette();
    if s.win_borders {
        set_attribute(hwnd, BORDER_COLOR, colorref(p.accent));
    }
    if s.win_titles {
        set_attribute(hwnd, CAPTION_COLOR, colorref(p.surface));
        set_attribute(hwnd, TEXT_COLOR, colorref(p.ink));
    }
    if s.win_corners != 0 {
        // Pollen's order (round, slightly, square) to DWM's (2, 3, 1).
        set_attribute(hwnd, CORNER_PREFERENCE, [0, 2, 3, 1][s.win_corners.min(3) as usize]);
    }
}

/// One app window back to Windows' own look.
fn undecorate(hwnd: HWND) {
    set_attribute(hwnd, BORDER_COLOR, COLOR_DEFAULT);
    set_attribute(hwnd, CAPTION_COLOR, COLOR_DEFAULT);
    set_attribute(hwnd, TEXT_COLOR, COLOR_DEFAULT);
    set_attribute(hwnd, CORNER_PREFERENCE, 0);
}

fn windows() -> Vec<HWND> {
    crate::apps::app_windows().into_iter().map(|w| w.hwnd).collect()
}

// ---- applying -----------------------------------------------------------------------------

/// Brings Windows in line with the settings: called at start and whenever the
/// settings (or the theme) change. `old` is what was applied before.
pub fn apply(old: Option<&Settings>, new: &Settings) {
    let palette = settings::palette();
    // The Windows theme takes its colors from Windows; nothing to send back.
    let from_windows = new.theme == Theme::Windows;
    let theme_changed = old.is_none_or(|o| o.theme != new.theme);

    if new.win_mode && !from_windows {
        if theme_changed || old.is_none_or(|o| !o.win_mode) {
            apply_mode(palette.light);
        }
    } else if old.is_some_and(|o| o.win_mode) && restore("mode") {
        announce("ImmersiveColorSet");
    }

    if new.win_accent && !from_windows {
        if theme_changed || old.is_none_or(|o| !o.win_accent) {
            apply_accent(palette.accent);
        }
    } else if old.is_some_and(|o| o.win_accent) && restore("accent") {
        announce("ImmersiveColorSet");
    }

    if new.win_wallpaper {
        if theme_changed || old.is_none_or(|o| !o.win_wallpaper) {
            apply_wallpaper(new.theme, palette);
        }
    } else if old.is_some_and(|o| o.win_wallpaper) {
        restore_wallpaper();
    }

    let decorated = |s: &Settings| s.win_borders || s.win_titles || s.win_corners != 0;
    let changed = old.is_none_or(|o| {
        theme_changed || o.win_borders != new.win_borders || o.win_titles != new.win_titles || o.win_corners != new.win_corners
    });
    if changed {
        for hwnd in windows() {
            undecorate(hwnd);
            if decorated(new) {
                decorate(hwnd);
            }
        }
    }
}

/// Pollen is quitting: window colors go back to Windows' (they would not
/// follow new windows without Pollen).
pub fn undecorate_all() {
    let s = settings::get();
    if s.win_borders || s.win_titles || s.win_corners != 0 {
        for hwnd in windows() {
            undecorate(hwnd);
        }
    }
}

/// Uninstalling: every Windows setting Pollen changed goes back.
pub fn restore_all() {
    let mut changed = restore("mode");
    changed |= restore("accent");
    if changed {
        announce("ImmersiveColorSet");
    }
    restore_wallpaper();
}


/// `pollen --restyle-selftest`: remembers, changes and restores two values
/// under a throwaway key, and reports whether they came back.
pub fn selftest() {
    const KEY: &str = r"Software\PollenTest";
    write_dword(KEY, "Kept", 7);
    // "Missing" is not there at first.
    remember("selftest", &[(KEY, "Kept"), (KEY, "Missing")]);
    write_dword(KEY, "Kept", 99);
    write_dword(KEY, "Missing", 5);
    let changed = (read_value(KEY, "Kept").map(|v| v.1), read_value(KEY, "Missing").is_some());
    restore("selftest");
    let back = (read_value(KEY, "Kept").map(|v| v.1), read_value(KEY, "Missing").is_some());
    println!("changed: Kept={:?} Missing present={}", changed.0, changed.1);
    println!("restored: Kept={:?} Missing present={}", back.0, back.1);
    println!("leftover in originals file: {}", read_originals().keys().any(|k| k.starts_with("selftest:")));
    let key_w = wide(KEY);
    unsafe {
        let _ = windows::Win32::System::Registry::RegDeleteTreeW(HKEY_CURRENT_USER, pcwstr(&key_w));
    }
}
