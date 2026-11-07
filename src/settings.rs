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

pub const MIN_OPACITY: f32 = 0.6;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    pub dock_mode: DockMode,
    pub dock_size: DockSize,
    pub magnify: Magnify,
    /// The dock's and the notch's background, MIN_OPACITY..=1.
    pub opacity: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { dock_mode: DockMode::Fixed, dock_size: DockSize::Medium, magnify: Magnify::Low, opacity: 0.9 }
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
                "dock-mode={}\ndock-size={}\nmagnify={}\nopacity={:.0}\n",
                settings.dock_mode.key(),
                settings.dock_size.key(),
                settings.magnify.key(),
                settings.opacity * 100.0
            ),
        );
    }
}
