//! Pollen's settings, kept as `key=value` lines in `%APPDATA%\Pollen\settings.txt`.
//!
//! One copy lives in memory for the whole process. The notch's Settings tab and
//! the dock's menu change it with `set`, which tells the dock and the notch to
//! pick the change up.

use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DockMode {
    /// Always visible; reserves its strip like the taskbar.
    Fixed,
    /// Hides while a window covers it; shows when the mouse reaches the bottom edge.
    Smart,
    /// Hidden until the mouse reaches the bottom edge.
    Peek,
}

impl DockMode {
    pub const ALL: [DockMode; 3] = [DockMode::Fixed, DockMode::Smart, DockMode::Peek];

    fn key(self) -> &'static str {
        match self {
            DockMode::Fixed => "fixed",
            DockMode::Smart => "smart",
            DockMode::Peek => "peek",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DockMode::Fixed => "Sabit",
            DockMode::Smart => "Akıllı",
            DockMode::Peek => "Göz at",
        }
    }

    fn parse(s: &str) -> Option<DockMode> {
        DockMode::ALL.into_iter().find(|m| m.key() == s.trim())
    }
}

/// The dock's overall size: icons, bar, clock and all.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DockSize {
    Small,
    Medium,
    Large,
}

impl DockSize {
    pub const ALL: [DockSize; 3] = [DockSize::Small, DockSize::Medium, DockSize::Large];

    fn key(self) -> &'static str {
        match self {
            DockSize::Small => "small",
            DockSize::Medium => "medium",
            DockSize::Large => "large",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DockSize::Small => "Küçük",
            DockSize::Medium => "Orta",
            DockSize::Large => "Büyük",
        }
    }

    /// Multiplies the dock's DPI scale.
    pub fn factor(self) -> f32 {
        match self {
            DockSize::Small => 0.88,
            DockSize::Medium => 1.0,
            DockSize::Large => 1.14,
        }
    }

    fn parse(s: &str) -> Option<DockSize> {
        DockSize::ALL.into_iter().find(|m| m.key() == s.trim())
    }
}

/// How much the icon under the mouse grows.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Magnify {
    Off,
    Low,
    High,
}

impl Magnify {
    pub const ALL: [Magnify; 3] = [Magnify::Off, Magnify::Low, Magnify::High];

    fn key(self) -> &'static str {
        match self {
            Magnify::Off => "off",
            Magnify::Low => "low",
            Magnify::High => "high",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Magnify::Off => "Yok",
            Magnify::Low => "Az",
            Magnify::High => "Çok",
        }
    }

    /// The hovered icon's scale.
    pub fn scale(self) -> f32 {
        match self {
            Magnify::Off => 1.0,
            Magnify::Low => 1.3,
            Magnify::High => 1.5,
        }
    }

    fn parse(s: &str) -> Option<Magnify> {
        Magnify::ALL.into_iter().find(|m| m.key() == s.trim())
    }
}

/// A color theme, after Nectar's presets: a tint for the background and an
/// accent for what is selected or on.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Theme {
    Midnight,
    Graphite,
    Ocean,
    Forest,
    Plum,
    Rose,
    Ember,
    /// Follows Windows' accent color.
    Windows,
}

impl Theme {
    pub const ALL: [Theme; 8] = [
        Theme::Midnight,
        Theme::Graphite,
        Theme::Ocean,
        Theme::Forest,
        Theme::Plum,
        Theme::Rose,
        Theme::Ember,
        Theme::Windows,
    ];

    fn key(self) -> &'static str {
        match self {
            Theme::Midnight => "midnight",
            Theme::Graphite => "graphite",
            Theme::Ocean => "ocean",
            Theme::Forest => "forest",
            Theme::Plum => "plum",
            Theme::Rose => "rose",
            Theme::Ember => "ember",
            Theme::Windows => "windows",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Theme::Midnight => "Gece",
            Theme::Graphite => "Grafit",
            Theme::Ocean => "Okyanus",
            Theme::Forest => "Orman",
            Theme::Plum => "Erik",
            Theme::Rose => "Gül",
            Theme::Ember => "Kor",
            Theme::Windows => "Windows",
        }
    }

    fn parse(s: &str) -> Option<Theme> {
        Theme::ALL.into_iter().find(|t| t.key() == s.trim())
    }

    /// The theme's colors (Windows' accent is read once and kept; see
    /// `refresh_palette`).
    pub fn palette(self) -> Palette {
        // Nectar's hue and saturation, a little darker than Nectar (the user's
        // choice for Midnight).
        let (hue, saturation, accent) = match self {
            Theme::Midnight => return Palette::new((9.0 / 255.0, 12.0 / 255.0, 20.0 / 255.0), hex(0x7aa2ff)),
            Theme::Graphite => (240.0, 0.04, hex(0x0a84ff)),
            Theme::Ocean => (187.0, 0.45, hex(0x35d0e0)),
            Theme::Forest => (149.0, 0.28, hex(0x4ade80)),
            Theme::Plum => (273.0, 0.30, hex(0xc58bff)),
            Theme::Rose => (344.0, 0.30, hex(0xff6b95)),
            Theme::Ember => (17.0, 0.32, hex(0xff8a5c)),
            Theme::Windows => {
                let accent = windows_accent().unwrap_or(hex(0x0a84ff));
                let (h, s, l) = to_hsl(accent);
                // Light enough to read on the dark background.
                let accent = from_hsl(h, s, l.max(0.62));
                (h, s.min(0.4), accent)
            }
        };
        Palette::new(from_hsl(hue, saturation, 0.065), accent)
    }
}

/// Colors as 0..1 RGB.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Palette {
    pub surface: (f32, f32, f32),
    pub accent: (f32, f32, f32),
    /// Text and icons on the accent.
    pub on_accent: (f32, f32, f32),
}

impl Palette {
    fn new(surface: (f32, f32, f32), accent: (f32, f32, f32)) -> Palette {
        let (h, _, _) = to_hsl(accent);
        let luminance = 0.2126 * accent.0 + 0.7152 * accent.1 + 0.0722 * accent.2;
        let on_accent = if luminance > 0.35 { from_hsl(h, 0.5, 0.08) } else { (1.0, 1.0, 1.0) };
        Palette { surface, accent, on_accent }
    }
}

fn hex(rgb: u32) -> (f32, f32, f32) {
    (((rgb >> 16) & 0xff) as f32 / 255.0, ((rgb >> 8) & 0xff) as f32 / 255.0, (rgb & 0xff) as f32 / 255.0)
}

fn to_hsl((r, g, b): (f32, f32, f32)) -> (f32, f32, f32) {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    if max == min {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

fn from_hsl(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (r + m, g + m, b + m)
}

/// Windows' accent color (`HKCU\Software\Microsoft\Windows\DWM\AccentColor`, 0xAABBGGRR).
fn windows_accent() -> Option<(f32, f32, f32)> {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let mut value = 0u32;
    let mut size = 4u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\DWM"),
            w!("AccentColor"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
        .ok()
        .ok()?;
    }
    Some(((value & 0xff) as f32 / 255.0, ((value >> 8) & 0xff) as f32 / 255.0, ((value >> 16) & 0xff) as f32 / 255.0))
}

static PALETTE: Mutex<Option<(Theme, Palette)>> = Mutex::new(None);

/// The current theme's colors.
pub fn palette() -> Palette {
    let theme = get().theme;
    let mut cached = PALETTE.lock().unwrap_or_else(|e| e.into_inner());
    match *cached {
        Some((t, p)) if t == theme => p,
        _ => {
            let p = theme.palette();
            *cached = Some((theme, p));
            p
        }
    }
}

/// Windows' accent may have changed: read it again next time.
pub fn refresh_palette() {
    *PALETTE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

pub const MIN_OPACITY: f32 = 0.6;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    pub dock_mode: DockMode,
    pub dock_size: DockSize,
    pub magnify: Magnify,
    /// The dock's and the notch's background, MIN_OPACITY..=1.
    pub opacity: f32,
    pub theme: Theme,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            dock_mode: DockMode::Fixed,
            dock_size: DockSize::Medium,
            magnify: Magnify::Low,
            opacity: 0.9,
            theme: Theme::Midnight,
        }
    }
}

static CURRENT: Mutex<Option<Settings>> = Mutex::new(None);

fn path() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("APPDATA")?).join("Pollen");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("settings.txt"))
}

fn read_file() -> Settings {
    let mut settings = Settings::default();
    let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else { return settings };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        match key.trim() {
            "dock-mode" => settings.dock_mode = DockMode::parse(value).unwrap_or(settings.dock_mode),
            "dock-size" => settings.dock_size = DockSize::parse(value).unwrap_or(settings.dock_size),
            "magnify" => settings.magnify = Magnify::parse(value).unwrap_or(settings.magnify),
            "theme" => settings.theme = Theme::parse(value).unwrap_or(settings.theme),
            "opacity" => {
                if let Ok(v) = value.trim().parse::<f32>() {
                    settings.opacity = (v / 100.0).clamp(MIN_OPACITY, 1.0);
                }
            }
            _ => {}
        }
    }
    settings
}

/// The settings now (read from the file the first time).
pub fn get() -> Settings {
    let mut current = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    *current.get_or_insert_with(read_file)
}

/// Snapshots and tests use the defaults instead of the user's file.
pub fn use_defaults() {
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings::default());
}

/// Changes the settings, writes them to the file when `save` (a slider being
/// dragged saves once, when it is let go), and tells the dock and the notch.
pub fn set(settings: Settings, save: bool) {
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(settings);
    if save {
        write_file(&settings);
    }
    crate::dock::settings_changed();
    crate::notch::settings_changed();
}

fn read_lines(name: &str) -> Vec<String> {
    let Some(dir) = path().and_then(|p| p.parent().map(|d| d.to_path_buf())) else { return Vec::new() };
    std::fs::read_to_string(dir.join(name))
        .map(|text| text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

fn write_lines(name: &str, lines: &[String]) {
    if let Some(dir) = path().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        let mut text = lines.join("\n");
        text.push('\n');
        let _ = std::fs::write(dir.join(name), text);
    }
}

/// Apps pinned to the dock in Pollen (what to launch: a program's path or
/// `shell:AppsFolder\<AUMID>`), after Windows' own taskbar pins.
pub fn own_pins() -> Vec<String> {
    read_lines("pins.txt")
}

pub fn save_own_pins(pins: &[String]) {
    write_lines("pins.txt", pins);
}

/// Windows taskbar pins removed from the dock (their launch targets,
/// lowercased). Windows' own pins cannot be changed by other apps.
pub fn hidden_pins() -> Vec<String> {
    read_lines("hidden-pins.txt")
}

pub fn save_hidden_pins(pins: &[String]) {
    write_lines("hidden-pins.txt", pins);
}

/// The dock's own order of the pins (their keys), set by dragging. Windows'
/// taskbar pins are left as they are.
pub fn pin_order() -> Vec<String> {
    let Some(dir) = path().and_then(|p| p.parent().map(|d| d.to_path_buf())) else { return Vec::new() };
    std::fs::read_to_string(dir.join("pin-order.txt"))
        .map(|text| text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

pub fn save_pin_order(keys: &[String]) {
    if let Some(dir) = path().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        let _ = std::fs::write(dir.join("pin-order.txt"), keys.join("\n") + "\n");
    }
}

pub fn write_file(settings: &Settings) {
    if let Some(p) = path() {
        let _ = std::fs::write(
            p,
            format!(
                "dock-mode={}\ndock-size={}\nmagnify={}\nopacity={:.0}\ntheme={}\n",
                settings.dock_mode.key(),
                settings.dock_size.key(),
                settings.magnify.key(),
                settings.opacity * 100.0,
                settings.theme.key()
            ),
        );
    }
}
