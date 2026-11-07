//! Pollen's settings, kept as `key=value` lines in `%APPDATA%\Pollen\settings.txt`.
//! A proper settings window comes later; for now the dock's menu writes here.

use std::path::PathBuf;

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
    fn key(self) -> &'static str {
        match self {
            DockMode::Fixed => "fixed",
            DockMode::Smart => "smart",
            DockMode::Peek => "peek",
        }
    }

    fn parse(s: &str) -> Option<DockMode> {
        match s.trim() {
            "fixed" => Some(DockMode::Fixed),
            "smart" => Some(DockMode::Smart),
            "peek" => Some(DockMode::Peek),
            _ => None,
        }
    }
}

pub struct Settings {
    pub dock_mode: DockMode,
}

fn path() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("APPDATA")?).join("Pollen");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("settings.txt"))
}

pub fn load() -> Settings {
    let mut settings = Settings { dock_mode: DockMode::Fixed };
    let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else { return settings };
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "dock-mode" {
                if let Some(mode) = DockMode::parse(value) {
                    settings.dock_mode = mode;
                }
            }
        }
    }
    settings
}

pub fn save(settings: &Settings) {
    if let Some(p) = path() {
        let _ = std::fs::write(p, format!("dock-mode={}\n", settings.dock_mode.key()));
    }
}
