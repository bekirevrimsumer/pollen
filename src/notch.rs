//! The notch: an island attached to the top edge of the primary monitor.
//!
//! Closed, it shows the time and date. Hovered, it opens into a panel with tabs
//! across the top (Music, Control Center, Status, Calendar; only the Control
//! Center exists so far) instead of Nectar's scroll-to-switch.
//!
//! Look and motion follow Nectar's notch: Midnight color, flat top with inward
//! flares where it meets the edge, 18 px bottom corners; at start a circle rises
//! to the edge, flattens against it at 240 ms and spreads into the island.
//!
//! The Control Center: network / sound output (and Wi-Fi / Bluetooth when the
//! computer has them) tiles, a row of shortcut buttons, and sliders for the volume
//! and for every monitor's brightness on its own.

use std::collections::HashMap;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows::core::{w, Result};
use windows::Devices::Radios::RadioKind;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D1_PIXEL_FORMAT,
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1PathGeometry, ID2D1SolidColorBrush,
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_BRUSH_PROPERTIES, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_BITMAP_PROPERTIES, D2D1_EXTEND_MODE_CLAMP,
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_ROUNDED_RECT, D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_MEDIUM, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_METRICS,
    DWRITE_WORD_WRAPPING_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VIRTUAL_KEY, VK_CONTROL, VK_LWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetCursorPos, GetForegroundWindow, SetCursor, IDC_SIZENS, IDC_SIZEWE, WM_SETCURSOR, GetWindowLongPtrW, SetForegroundWindow, GWL_EXSTYLE, KillTimer, LoadCursorW, RegisterClassExW, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, GWLP_USERDATA, HWND_TOPMOST, IDC_ARROW, MA_NOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, WM_APP, WM_CAPTURECHANGED,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_SETTINGCHANGE, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};
use windows_numerics::{Matrix3x2, Vector2};

use crate::anim::Spring;
use crate::controls::{audio, battery, displays, media, network, notifications, privacy, system, weather};
use crate::pacer;
use crate::render::Surface;
use crate::settings::{self, DockMode, DockSize, Magnify, NotchEdge, Settings};
use crate::i18n::{self, t};
use crate::shell;

// Sizes in DIPs (Nectar's CSS pixels), multiplied by the monitor scale.
const BAR: f32 = 36.0;
const CLOSED_MIN_WIDTH: f32 = 140.0;
/// Room for the weather and CPU figures beside the closed island's clock.
/// Room for the weather on the closed island without music.
const STATUS_SIDE: f32 = 52.0;
const OPEN_WIDTH: f32 = 420.0;
const RADIUS: f32 = 18.0;
const FLARE: f32 = 13.0;
const CIRCLE: f32 = 34.0;
/// How far below the edge the opening circle starts.
const RISE: f32 = 140.0;
const PAD: f32 = 14.0;
const TILE_HEIGHT: f32 = 42.0;
const BUTTON: f32 = 32.0;
const SLIDER_HEIGHT: f32 = 22.0;
const LABEL_WIDTH: f32 = 112.0;
/// Room in the window for the widest the island can get.
const WINDOW_WIDTH: f32 = OPEN_WIDTH + FLARE * 2.0 + 24.0;
const WINDOW_HEIGHT: f32 = 660.0;
/// The closed island on a side edge: a tall pill (DIPs).
const SIDE_WIDTH: f32 = 46.0;
const SIDE_LENGTH: f32 = 176.0;
/// Moving the notch, it snaps to an edge or the middle this close (DIPs).
const SNAP: f32 = 28.0;

/// Nectar's Midnight surface; it gets more opaque open (Nectar: +0.12).

const TIMER_COLLAPSE: usize = 1;
const TIMER_CLOCK: usize = 2;
const TIMER_REFRESH: usize = 3;
/// Level bars and the progress bar while something plays (~30 fps).
const TIMER_LEVEL: usize = 4;
/// The album art in the open Music tab.
const ART: f32 = 76.0;
/// CPU / memory / disk / network readings: every second in the Status tab,
/// every few seconds for the closed island's CPU figure.
const TIMER_STATS: usize = 5;
/// Searches for the typed place a moment after the typing stops.
const TIMER_SEARCH: usize = 6;
/// How long the volume indicator stays up after the last change.
const TIMER_OSD: usize = 7;
/// The banner (time's up, a notification) goes after this.
const TIMER_BANNER: usize = 8;
/// While the timer, stopwatch or pomodoro runs.
const TIMER_CLOCKWORK: usize = 9;
/// The closed island's width while it shows a banner (DIPs).
const BANNER_WIDTH: f32 = 360.0;
const OSD_MS: u32 = 1500;
/// The closed island's width while it shows the volume (DIPs).
const OSD_WIDTH: f32 = 240.0;

/// Posted by the display worker when monitors or values change.
const WM_DISPLAYS: u32 = WM_APP + 4;
/// Posted when the radios were read again.
const WM_RADIOS: u32 = WM_APP + 5;
/// Posted by the media worker when what is playing changes.
const WM_MEDIA: u32 = WM_APP + 6;
/// Posted by the weather worker after a refresh.
const WM_WEATHER: u32 = WM_APP + 7;
/// The settings changed (`settings::set`).
const WM_SETTINGS_CHANGED: u32 = WM_APP + 8;
/// A place search finished (`SEARCH`).
const WM_PLACES: u32 = WM_APP + 9;
/// The apps using the microphone or camera changed.
const WM_PRIVACY: u32 = WM_APP + 10;
/// The output's volume changed (from the keyboard, the tray, an app).
const WM_VOLUME: u32 = WM_APP + 11;
/// A new default output.
const WM_AUDIO_DEVICE: u32 = WM_APP + 12;
/// Open on the Settings tab (the tray icon).
const WM_SHOW_SETTINGS: u32 = WM_APP + 13;
/// New Windows notifications are waiting in `notifications::take()`.
const WM_NOTIFICATIONS: u32 = WM_APP + 14;
/// The devices reporting a battery level changed (`battery::current()`).
const WM_BATTERIES: u32 = WM_APP + 15;
/// iOS's colors: orange for the microphone, green for the camera.
const MIC_COLOR: D2D1_COLOR_F = rgba(1.0, 0.62, 0.04, 1.0);
const CAMERA_COLOR: D2D1_COLOR_F = rgba(0.19, 0.82, 0.35, 1.0);
const WM_CHAR: u32 = 0x0102;
const WM_ACTIVATE: u32 = 0x0006;

/// The latest place search: its number and what it found.
static SEARCH: std::sync::Mutex<Option<(u64, Vec<weather::Place>)>> = std::sync::Mutex::new(None);
const WM_MOUSELEAVE: u32 = 0x02A3;

// Segoe Fluent Icons.
const ICON_MUSIC: char = '\u{E8D6}';
const ICON_CONTROLS: char = '\u{E9E9}';
const ICON_STATUS: char = '\u{EC4A}';
const ICON_CALENDAR: char = '\u{E787}';
const ICON_WIFI: char = '\u{E701}';
const ICON_BLUETOOTH: char = '\u{E702}';
const ICON_ETHERNET: char = '\u{E839}';
const ICON_VOLUME: char = '\u{E767}';
const ICON_MUTE: char = '\u{E74F}';
const ICON_BRIGHTNESS: char = '\u{E706}';
const ICON_BELL: char = '\u{EA8F}';
const ICON_HEADPHONES: char = '\u{E7F6}';
const ICON_GEAR: char = '\u{E713}';
const ICON_DISPLAY: char = '\u{E7F4}';
const ICON_PREVIOUS: char = '\u{E892}';
const ICON_PLAY: char = '\u{E768}';
const ICON_PAUSE: char = '\u{E769}';
const ICON_NEXT: char = '\u{E893}';
const ICON_SUN: char = '\u{E706}';
const ICON_MOON: char = '\u{E708}';
const ICON_CLOUD: char = '\u{E753}';
const ICON_STORM: char = '\u{E945}';
const ICON_CPU: char = '\u{E950}';
const ICON_MEMORY: char = '\u{E964}';
const ICON_DISK: char = '\u{EDA2}';
const ICON_DOWN: char = '\u{E896}';
const ICON_LEFT: char = '\u{E76B}';
const ICON_RIGHT: char = '\u{E76C}';
const ICON_PIN: char = '\u{E718}';
const ICON_SIZE: char = '\u{E740}';
const ICON_ZOOM: char = '\u{E71E}';
const ICON_TILES: char = '\u{ECA5}';
const ICON_NOTCH: char = '\u{E737}';
const ICON_MOVE: char = '\u{E7C2}';
const ICON_EDGE: char = '\u{E746}';
const IDENTITY: Matrix3x2 = Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: 0.0, M32: 0.0 };
const ICON_COLOR: char = '\u{E790}';
const ICON_POWER: char = '\u{E7E8}';
const ICON_PLACE: char = '\u{E81D}';
const ICON_THEME: char = '\u{E771}';
const ICON_LANGUAGE: char = '\u{E774}';
const ICON_STOPWATCH: char = '\u{E916}';
/// The closed island marks a running timer with this (the date's font has it).
const ICON_STOPWATCH_TEXT: char = '⏱';
const ICON_ALARM: char = '\u{EA8F}';
const ICON_UPDATE: char = '\u{E895}';
const ICON_CLOCK: char = '\u{E823}';
const ICON_INFO: char = '\u{E946}';
// Segoe Fluent Icons has no rain or snow; DirectWrite falls back to Segoe UI Symbol.
const ICON_PARTLY: char = '\u{26C5}';
const ICON_RAIN: char = '\u{2602}';
const ICON_SNOW: char = '\u{2744}';
/// A place search result's row.
const RESULT_HEIGHT: f32 = 28.0;


static NOTCH_HWND: AtomicIsize = AtomicIsize::new(0);

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

/// The theme's text and icon color (white on dark themes, near-black on
/// light ones) at opacity `a`.
fn ink_color(a: f32) -> D2D1_COLOR_F {
    let (r, g, b) = crate::settings::palette().ink;
    rgba(r, g, b, a)
}

/// White, whatever the theme: text on the dark tooltip bubbles and on red.
fn pure_white(a: f32) -> D2D1_COLOR_F {
    rgba(1.0, 1.0, 1.0, a)
}

/// The theme's background color.
fn surface() -> (f32, f32, f32) {
    crate::settings::palette().surface
}

/// The theme's accent: what is selected or on.
fn accent() -> D2D1_COLOR_F {
    let (r, g, b) = crate::settings::palette().accent;
    rgba(r, g, b, 1.0)
}

fn on_accent() -> D2D1_COLOR_F {
    let (r, g, b) = crate::settings::palette().on_accent;
    rgba(r, g, b, 1.0)
}

fn with_alpha(c: D2D1_COLOR_F, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a: c.a * a, ..c }
}

fn pt(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F { left, top, right, bottom }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn contains(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tile {
    Wifi,
    Bluetooth,
    Network,
    Output,
    /// A device reporting its battery (Bluetooth headphones), by index.
    Battery(usize),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Button {
    Notifications,
    Sound,
    Display,
    Network,
    Settings,
}

impl Button {
    const ALL: [Button; 5] = [Button::Notifications, Button::Sound, Button::Display, Button::Network, Button::Settings];

    fn icon(self) -> char {
        match self {
            Button::Notifications => ICON_BELL,
            Button::Sound => ICON_VOLUME,
            Button::Display => ICON_DISPLAY,
            Button::Network => ICON_ETHERNET,
            Button::Settings => ICON_GEAR,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Button::Notifications => t("Bildirimler ve Rahatsız Etme", "Notifications and Do not disturb"),
            Button::Sound => t("Ses ayarları", "Sound settings"),
            Button::Display => t("Ekran ayarları", "Display settings"),
            Button::Network => t("Ağ ayarları", "Network settings"),
            Button::Settings => t("Windows Ayarları", "Windows Settings"),
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
enum Slider {
    Volume,
    /// A monitor, by GDI device name.
    Brightness(String),
    /// The shown player's own volume in the Windows mixer.
    App,
    /// The dock's and the notch's background opacity (Settings tab).
    Opacity,
}

/// A row of options in the Settings tab.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Choice {
    Mode,
    Size,
    Magnify,
    Tiles,
    NotchHide,
    NotchEdge,
    NotchPlace,
    Autostart,
    Language,
    Updates,
    Notifications,
}

impl Choice {
    const ALL: [Choice; 11] = [
        Choice::Mode,
        Choice::Size,
        Choice::Magnify,
        Choice::Tiles,
        Choice::NotchHide,
        Choice::NotchEdge,
        Choice::NotchPlace,
        Choice::Autostart,
        Choice::Notifications,
        Choice::Updates,
        Choice::Language,
    ];

    fn icon(self) -> char {
        match self {
            Choice::Mode => ICON_PIN,
            Choice::Size => ICON_SIZE,
            Choice::Magnify => ICON_ZOOM,
            Choice::Tiles => ICON_TILES,
            Choice::NotchHide => ICON_NOTCH,
            Choice::NotchEdge => ICON_EDGE,
            Choice::NotchPlace => ICON_MOVE,
            Choice::Autostart => ICON_POWER,
            Choice::Language => ICON_LANGUAGE,
            Choice::Updates => ICON_UPDATE,
            Choice::Notifications => ICON_BELL,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Choice::Mode => t("Dock modu", "Dock mode"),
            Choice::Size => t("Dock boyutu", "Dock size"),
            Choice::Magnify => t("Büyütme", "Magnify"),
            Choice::Tiles => t("Simge kutuları", "Icon boxes"),
            Choice::NotchHide => t("Notch", "Notch"),
            Choice::NotchEdge => t("Notch kenarı", "Notch edge"),
            Choice::NotchPlace => t("Notch konumu", "Notch place"),
            Choice::Autostart => t("Windows ile aç", "Autostart"),
            Choice::Language => t("Dil", "Language"),
            Choice::Updates => t("Güncellemeler", "Updates"),
            Choice::Notifications => t("Bildirimler", "Notifications"),
        }
    }

    /// What an option does, for its tooltip; None where the label says it all.
    fn hint(self, i: usize) -> Option<&'static str> {
        Some(match (self, i) {
            (Choice::Mode, 0) => t(
                "Dock hep görünür; ekranın altını kendine ayırır, pencereler onun üstünde biter",
                "The dock always shows and keeps the bottom strip, like the taskbar",
            ),
            (Choice::Mode, 1) => t(
                "Bir pencere dock'a değince gizlenir; fareyi ekranın altına götürünce açılır",
                "Hides when a window touches it; move the mouse to the bottom edge to show it",
            ),
            (Choice::Mode, 2) => t(
                "Hep gizli; yalnızca fareyi ekranın altına götürünce açılır",
                "Always hidden; shows only when the mouse reaches the bottom edge",
            ),
            (Choice::Size, _) => t("Simgeler, saat ve çubuk birlikte büyür", "Icons, clock and bar together"),
            (Choice::Magnify, 0) => t("Simgeler fareyle büyümez", "Icons don't grow under the mouse"),
            (Choice::Magnify, _) => t(
                "Farenin altındaki simge ve komşuları büyür",
                "The icon under the mouse and its neighbors grow",
            ),
            (Choice::Tiles, 0) => t("Simgeler çubukta tek başına durur", "Icons sit on the bar by themselves"),
            (Choice::Tiles, _) => t(
                "Her simge yuvarlak bir kutuda; büyütünce simge kutusunun içinde büyür",
                "Each icon sits in a rounded box; magnified icons grow inside it",
            ),
            (Choice::NotchHide, 0) => t(
                "Saat ve tarihle hep ekranın üstünde durur",
                "Always at the top of the screen, with the time and date",
            ),
            (Choice::NotchHide, _) => t(
                "Ekranın üstünde gizlenir; fareyi üst kenarda onun yerine götürünce açılır. Bildirimler ve ses göstergesi yine görünür",
                "Hides above the screen and opens when the mouse touches the top edge where it sits. Notifications and the volume still show",
            ),
            (Choice::NotchEdge, 0) => t("Ekranın üst kenarında, Nectar'daki gibi", "On the screen's top edge, as in Nectar"),
            (Choice::NotchEdge, _) => t(
                "Ekranın yan kenarında dik durur; açılınca panel yandan çıkar",
                "Stands on a side edge of the screen; the panel slides out sideways",
            ),
            (Choice::NotchPlace, _) => t(
                "Notch'u açıkken üst satırındaki saatten ya da tarihten tutup sürükleyerek de taşıyabilirsin; uçlara ve ortaya yapışır",
                "You can also drag the open notch by the time or date in its top row; it snaps to the ends and the middle",
            ),
            (Choice::Autostart, 1) => t("Windows açılınca Pollen da başlar", "Pollen starts with Windows"),
            (Choice::Autostart, _) => t("Pollen'ı kendin açarsın", "You start Pollen yourself"),
            (Choice::Notifications, 1) => t(
                "Yeni Windows bildirimleri kapalı notch'ta birkaç saniye görünür (tam ekran oyunlarda değil)",
                "New Windows notifications show on the closed notch for a few seconds (not over fullscreen games)",
            ),
            (Choice::Notifications, _) => t("Bildirimler yalnızca Windows'ta görünür", "Notifications show only in Windows"),
            (Choice::Updates, 1) => t(
                "Günde bir kez GitHub'da yeni sürüme bakar; kurmak için senin tıklaman gerekir",
                "Looks for a new version on GitHub once a day; it installs only when you click",
            ),
            (Choice::Updates, _) => t("Yeni sürümlere bakılmaz", "Pollen never looks for new versions"),
            (Choice::Language, 0) => t("Windows'un diliyle aynı", "The same as Windows"),
            _ => return None,
        })
    }

    fn options(self, s: &Settings) -> Vec<&'static str> {
        match self {
            Choice::Mode => DockMode::ALL.map(|m| m.label()).to_vec(),
            Choice::Size => DockSize::ALL.map(|m| m.label()).to_vec(),
            Choice::Magnify => Magnify::ALL.map(|m| m.label()).to_vec(),
            Choice::Tiles => vec![t("Kapalı", "Off"), t("Açık", "On")],
            Choice::NotchHide => vec![t("Hep görünür", "Always"), t("Üstüne gelince", "On hover")],
            Choice::NotchEdge => NotchEdge::ALL.map(|e| e.label()).to_vec(),
            Choice::NotchPlace if s.notch_edge.side() => vec![t("Üst", "Top"), t("Orta", "Middle"), t("Alt", "Bottom")],
            Choice::NotchPlace => vec![t("Sol", "Left"), t("Orta", "Middle"), t("Sağ", "Right")],
            Choice::Autostart => vec![t("Kapalı", "Off"), t("Açık", "On")],
            Choice::Language => settings::Language::ALL.map(|l| l.label()).to_vec(),
            Choice::Updates => vec![t("Kapalı", "Off"), t("Denetle", "Check")],
            Choice::Notifications => vec![t("Kapalı", "Off"), t("Notch'ta göster", "Show on the notch")],
        }
    }

    /// `autostart` is read from the registry, not kept in the settings.
    fn selected(self, s: &Settings, autostart: bool) -> usize {
        match self {
            Choice::Mode => DockMode::ALL.iter().position(|m| *m == s.dock_mode),
            Choice::Size => DockSize::ALL.iter().position(|m| *m == s.dock_size),
            Choice::Magnify => Magnify::ALL.iter().position(|m| *m == s.magnify),
            Choice::Tiles => Some(s.tiles as usize),
            Choice::NotchHide => Some(s.notch_hide as usize),
            Choice::NotchEdge => NotchEdge::ALL.iter().position(|e| *e == s.notch_edge),
            Choice::NotchPlace => {
                let pos = if s.notch_edge.side() { s.notch_y } else { s.notch_x };
                Some(if pos < 0.25 { 0 } else if pos > 0.75 { 2 } else { 1 })
            }
            Choice::Autostart => Some(autostart as usize),
            Choice::Language => settings::Language::ALL.iter().position(|l| *l == s.language),
            Choice::Updates => Some(s.check_updates as usize),
            Choice::Notifications => Some(s.notifications as usize),
        }
        .unwrap_or(0)
    }

    fn select(self, s: &mut Settings, i: usize) {
        match self {
            Choice::Mode => s.dock_mode = DockMode::ALL[i],
            Choice::Size => s.dock_size = DockSize::ALL[i],
            Choice::Magnify => s.magnify = Magnify::ALL[i],
            Choice::Tiles => s.tiles = i == 1,
            Choice::NotchHide => s.notch_hide = i == 1,
            Choice::NotchEdge => s.notch_edge = NotchEdge::ALL[i],
            Choice::NotchPlace if s.notch_edge.side() => s.notch_y = [0.0, 0.5, 1.0][i.min(2)],
            Choice::NotchPlace => s.notch_x = [0.0, 0.5, 1.0][i.min(2)],
            Choice::Autostart => {}
            Choice::Language => s.language = settings::Language::ALL[i],
            Choice::Updates => s.check_updates = i == 1,
            Choice::Notifications => s.notifications = i == 1,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum MediaButton {
    Previous,
    PlayPause,
    Next,
}

#[derive(Clone, PartialEq, Debug)]
enum Control {
    Tab(usize),
    /// Picks which player the Music tab shows (by AppUserModelID).
    Player(String),
    Media(MediaButton),
    /// The progress bar: click to seek.
    Progress,
    CalendarPrevious,
    CalendarNext,
    /// The month title: back to this month.
    CalendarTitle,
    Tile(Tile),
    Button(Button),
    /// The slider's track (drag / click / wheel).
    Slider(Slider),
    /// The slider's icon (mute for the volume).
    SliderIcon(Slider),
    /// One option of a Settings row.
    Choice(Choice, usize),
    /// A theme's swatch.
    Theme(usize),
    Quit,
    /// The weather's place: click to type a city.
    CityField,
    /// Back to the place from the IP address.
    CityAuto,
    /// Install the newer version (Settings footer).
    Update,
    /// Timer tab: the mode (timer, stopwatch, pomodoro), a length in
    /// minutes, start or pause, reset.
    ClockMode(usize),
    ClockPreset(u32),
    ClockStart,
    ClockReset,
    CityResult(usize),
    /// The open panel's top row beside the tabs: drag to move the notch.
    Grip,
}

/// A message shown on the closed island for a moment.
struct Banner {
    glyph: char,
    title: String,
    body: String,
}

#[derive(Clone, Copy, PartialEq, Default)]
enum ClockMode {
    #[default]
    Timer,
    Stopwatch,
    Pomodoro,
}

impl ClockMode {
    const ALL: [ClockMode; 3] = [ClockMode::Timer, ClockMode::Stopwatch, ClockMode::Pomodoro];

    fn label(self) -> &'static str {
        match self {
            ClockMode::Timer => t("Zamanlayıcı", "Timer"),
            ClockMode::Stopwatch => t("Kronometre", "Stopwatch"),
            ClockMode::Pomodoro => "Pomodoro",
        }
    }
}

/// The Timer tab's timer, stopwatch and pomodoro.
struct Clockwork {
    mode: ClockMode,
    /// Running since, and what ran before the last pause.
    since: Option<Instant>,
    banked: Duration,
    /// The timer's length.
    length: Duration,
    /// Pomodoro: focusing (else a break), and which of four rounds.
    focus: bool,
    round: u32,
}

impl Default for Clockwork {
    fn default() -> Self {
        Clockwork {
            mode: ClockMode::Timer,
            since: None,
            banked: Duration::ZERO,
            length: Duration::from_secs(5 * 60),
            focus: true,
            round: 1,
        }
    }
}

impl Clockwork {
    fn running(&self) -> bool {
        self.since.is_some()
    }

    fn elapsed(&self) -> Duration {
        self.banked + self.since.map(|at| at.elapsed()).unwrap_or_default()
    }

    /// Started at all (running or paused part way).
    fn active(&self) -> bool {
        self.running() || self.banked > Duration::ZERO
    }

    /// How long the current stretch lasts; None for the stopwatch.
    fn target(&self) -> Option<Duration> {
        match self.mode {
            ClockMode::Timer => Some(self.length),
            ClockMode::Stopwatch => None,
            ClockMode::Pomodoro if self.focus => Some(Duration::from_secs(25 * 60)),
            // Every fourth break is a long one.
            ClockMode::Pomodoro if self.round % 4 == 0 => Some(Duration::from_secs(15 * 60)),
            ClockMode::Pomodoro => Some(Duration::from_secs(5 * 60)),
        }
    }

    /// "04:32" left (or, for the stopwatch, gone by); "1:02:03" past an hour.
    fn text(&self) -> String {
        let shown = match self.target() {
            // Rounded up, so a timer shows 5:00 when it starts and 0:01 last.
            Some(target) => {
                let left = target.saturating_sub(self.elapsed());
                Duration::from_secs((left.as_millis() as u64).div_ceil(1000))
            }
            None => Duration::from_secs(self.elapsed().as_secs()),
        };
        let secs = shown.as_secs();
        if secs >= 3600 {
            format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
        } else {
            format!("{:02}:{:02}", secs / 60, secs % 60)
        }
    }

    fn reset(&mut self) {
        self.since = None;
        self.banked = Duration::ZERO;
        self.focus = true;
        self.round = 1;
    }
}

/// Typing a city in the Settings tab.
struct CityEdit {
    text: String,
    results: Vec<weather::Place>,
    searching: bool,
    /// The window that had the focus before typing started.
    previous: HWND,
}

/// Tabs: (icon, available yet).
const TABS: [(char, bool); 7] = [
    (ICON_MUSIC, true),
    (ICON_CONTROLS, true),
    (ICON_STATUS, true),
    (ICON_CLOUD, true),
    (ICON_CALENDAR, true),
    (ICON_STOPWATCH, true),
    (ICON_GEAR, true),
];
/// A tab button's width (DIPs); seven fit between the time and the date.
const TAB_WIDTH: f32 = 38.0;
const MUSIC_TAB: usize = 0;
const CONTROL_CENTER_TAB: usize = 1;
const STATUS_TAB: usize = 2;
const WEATHER_TAB: usize = 3;
const CALENDAR_TAB: usize = 4;
const TIMER_TAB: usize = 5;
const SETTINGS_TAB: usize = 6;
/// A row of the Weather tab's coming days.
const DAY_ROW: f32 = 26.0;
/// A Settings row's height.
const CHOICE_HEIGHT: f32 = 28.0;

pub struct Notch {
    hwnd: HWND,
    factory: ID2D1Factory,
    surface: Surface,
    brush: ID2D1SolidColorBrush,
    dwrite: IDWriteFactory,
    /// Text layouts kept between frames.
    texts: crate::text::TextCache,
    time_format: IDWriteTextFormat,
    date_format: IDWriteTextFormat,
    title_format: IDWriteTextFormat,
    small_format: IDWriteTextFormat,
    icon_format: IDWriteTextFormat,
    tab_icon_format: IDWriteTextFormat,
    scale: f32,
    /// The window's top-left corner on the screen.
    origin: (i32, i32),
    /// The monitor the notch is on, and its part not kept by the dock
    /// (screen pixels).
    monitor: RECT,
    work: RECT,
    /// The edge the window is laid out for (Settings::notch_edge).
    edge: NotchEdge,
    /// Where along the top edge (Settings::notch_x), springing to a new place.
    slide: Spring,
    /// 1 = tucked away above the top edge (Settings › Notch › On hover).
    tuck: Spring,
    /// Moving the notch: the cursor's distance from the island's left edge.
    grab: Option<f32>,
    // Motion.
    /// 0 = closed island, 1 = open panel.
    open: Spring,
    /// Opening sequence: vertical offset (rises from below), circle -> island.
    rise: Spring,
    spread: Spring,
    round_top: Spring,
    flares: bool,
    content: f32,
    content_fade_from: Option<Instant>,
    opening_since: Option<Instant>,
    animating: bool,
    last_tick: Option<Instant>,
    last_drawn: Option<(RECT, (i32, i32))>,
    // State.
    hovered: bool,
    mouse: Option<(f32, f32)>,
    pressed: Option<Control>,
    dragging: Option<Slider>,
    tab: usize,
    hidden_for_fullscreen: bool,
    time: String,
    date: String,
    output: Option<audio::Output>,
    settings: Settings,
    /// Whether Windows starts Pollen (the registry's word, re-read on change).
    autostart: bool,
    /// The shown player's mixer volume, while it has a sound session.
    app_volume: Option<audio::AppVolume>,
    internet: bool,
    radios: network::Radios,
    displays: Vec<displays::Display>,
    /// What is playing.
    media: media::Snapshot,
    /// The player picked in the Music tab, if not the default one.
    player: Option<String>,
    /// Album art bitmaps, by track key.
    art: HashMap<String, ID2D1Bitmap>,
    meter: audio::Meter,
    /// Level bars in the closed island, 0..1 each.
    bars: [f32; 4],
    level_running: bool,
    /// 0..1: how much room the closed island gives to the playing track.
    music: Spring,
    /// Devices with a battery level, for the Control Center.
    batteries: Vec<battery::Device>,
    /// A message on the closed island for a few seconds, and its room (0..1).
    banner: Option<Banner>,
    banner_room: Spring,
    clockwork: Clockwork,
    /// What the closed island showed for it last, to redraw only on change.
    clockwork_text: String,
    /// The volume indicator's share of the closed island (0..1).
    osd_room: Spring,
    volume_watch: Option<audio::Watch>,
    /// Who uses the microphone and camera, and the dots' room (0..1).
    privacy: privacy::InUse,
    privacy_room: Spring,
    track_format: IDWriteTextFormat,
    play_format: IDWriteTextFormat,
    big_format: IDWriteTextFormat,
    big_icon_format: IDWriteTextFormat,
    clock_format: IDWriteTextFormat,
    symbol_format: IDWriteTextFormat,
    big_symbol_format: IDWriteTextFormat,
    weather: Option<weather::Weather>,
    /// The weather's place picked by hand.
    place: Option<weather::Place>,
    city_edit: Option<CityEdit>,
    search_seq: u64,
    sampler: system::Sampler,
    stats: system::Stats,
    stats_interval: u32,
    /// Today (year, month 1-12, day) and the month the calendar shows.
    today: (i32, u32, u32),
    month: (i32, u32),
    /// The open panel's height; springs when the tab changes.
    panel_h: Spring,
    /// `--snapshot`: never shown, draws on an opaque backdrop.
    headless: bool,
}

impl Notch {
    pub fn create() -> Result<Box<Notch>> {
        Self::build(false)
    }

    fn build(headless: bool) -> Result<Box<Notch>> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: w!("PollenNotch"),
                ..Default::default()
            };
            RegisterClassExW(&class);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                w!("PollenNotch"),
                w!("Pollen notch"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance.into()),
                None,
            )?;
            if !headless {
                NOTCH_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
            }

            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let surface = Surface::new(&factory, 1, 1)?;
            surface.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            surface.rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = surface.rt.CreateSolidColorBrush(&rgba(1.0, 1.0, 1.0, 1.0), None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let formats = Formats::new(&dwrite, 1.0)?;

            let mut notch = Box::new(Notch {
                hwnd,
                factory,
                surface,
                brush,
                time_format: formats.time,
                date_format: formats.date,
                title_format: formats.title,
                small_format: formats.small,
                icon_format: formats.icon,
                tab_icon_format: formats.tab_icon,
                dwrite,
                texts: Default::default(),
                scale: 1.0,
                origin: (0, 0),
                monitor: RECT::default(),
                work: RECT::default(),
                edge: settings::get().notch_edge,
                slide: Spring::new(edge_position(&settings::get())),
                tuck: Spring::new(if settings::get().notch_hide { 1.0 } else { 0.0 }),
                grab: None,
                open: Spring::new(0.0),
                rise: Spring::new(0.0),
                spread: Spring::new(0.0),
                round_top: Spring::new(1.0),
                flares: false,
                content: 0.0,
                content_fade_from: None,
                opening_since: None,
                animating: false,
                last_tick: None,
                last_drawn: None,
                hovered: false,
                mouse: None,
                pressed: None,
                dragging: None,
                tab: CONTROL_CENTER_TAB,
                hidden_for_fullscreen: false,
                time: String::new(),
                date: String::new(),
                output: None,
                settings: settings::get(),
                autostart: crate::autostart::is_enabled(),
                app_volume: None,
                internet: false,
                radios: network::Radios::default(),
                displays: Vec::new(),
                media: media::Snapshot::default(),
                player: None,
                art: HashMap::new(),
                meter: audio::Meter::new(),
                bars: [0.0; 4],
                level_running: false,
                music: Spring::new(0.0),
                batteries: Vec::new(),
                banner: None,
                banner_room: Spring::new(0.0),
                clockwork: Clockwork::default(),
                clockwork_text: String::new(),
                osd_room: Spring::new(0.0),
                volume_watch: None,
                privacy: privacy::InUse::default(),
                privacy_room: Spring::new(0.0),
                track_format: formats.track,
                play_format: formats.play,
                big_format: formats.big,
                big_icon_format: formats.big_icon,
                clock_format: formats.clock,
                symbol_format: formats.symbol,
                big_symbol_format: formats.big_symbol,
                weather: None,
                place: weather::saved_place(),
                city_edit: None,
                search_seq: 0,
                sampler: system::Sampler::new(),
                stats: system::Stats::default(),
                stats_interval: 0,
                today: (2000, 1, 1),
                month: (2000, 1),
                panel_h: Spring::new(0.0),
                headless,
            });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, notch.as_mut() as *mut Notch as isize);

            notch.place();
            notch.update_clock();
            if headless {
                return Ok(notch);
            }
            displays::start(hwnd, WM_DISPLAYS);
            media::start(hwnd, WM_MEDIA);
            weather::start(hwnd, WM_WEATHER);
            privacy::start(hwnd, WM_PRIVACY);
            notifications::start(hwnd, WM_NOTIFICATIONS);
            notch.volume_watch = audio::Watch::start(hwnd, WM_VOLUME, WM_AUDIO_DEVICE);
            notch.update_stats_timer();
            network::refresh_radios(|| post(WM_RADIOS));

            // Nectar's opening: a circle rises to the edge, flattens, spreads out.
            notch.rise = Spring { value: RISE * notch.scale, velocity: 0.0, target: 0.0 };
            notch.opening_since = Some(Instant::now());
            notch.render();
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            notch.opening_since = Some(Instant::now());
            notch.start_animation();
            Ok(notch)
        }
    }

    // ---- placement ---------------------------------------------------------

    fn place(&mut self) {
        unsafe {
            let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(monitor, &mut info);
            let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
            let scale = dpi_x as f32 / 96.0;
            if (scale - self.scale).abs() > f32::EPSILON {
                self.scale = scale;
                if let Ok(f) = Formats::new(&self.dwrite, scale) {
                    self.time_format = f.time;
                    self.date_format = f.date;
                    self.title_format = f.title;
                    self.small_format = f.small;
                    self.icon_format = f.icon;
                    self.tab_icon_format = f.tab_icon;
                    self.track_format = f.track;
                    self.play_format = f.play;
                    self.big_format = f.big;
                    self.big_icon_format = f.big_icon;
                    self.clock_format = f.clock;
                    self.symbol_format = f.symbol;
                    self.big_symbol_format = f.big_symbol;
                }
            }
            let m = info.rcMonitor;
            self.monitor = m;
            self.work = info.rcWork;
            self.edge = self.settings.notch_edge;
            // On a side edge the window stands up, with room for the flares
            // at both ends of the open panel.
            let (along, depth) = if self.side() {
                (WINDOW_HEIGHT + FLARE * 2.0 + 24.0, OPEN_WIDTH + 24.0)
            } else {
                (WINDOW_WIDTH, WINDOW_HEIGHT)
            };
            let along = (along * self.scale).ceil() as i32;
            let depth = (depth * self.scale).ceil() as i32;
            let (width, height) = if self.side() { (depth, along) } else { (along, depth) };
            self.origin = (m.left, m.top);
            let _ = self.surface.resize(width, height);
            self.last_drawn = None;
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }

    /// Called by the dock, which already tracks fullscreen windows.
    fn set_hidden_for_fullscreen(&mut self, hide: bool) {
        if hide != self.hidden_for_fullscreen {
            self.hidden_for_fullscreen = hide;
            if hide {
                self.hovered = false;
                self.open.target = 0.0;
            }
            unsafe {
                let _ = ShowWindow(self.hwnd, if hide { SW_HIDE } else { SW_SHOWNOACTIVATE });
            }
            self.update_level();
            self.update_stats_timer();
        }
    }

    // ---- data --------------------------------------------------------------

    fn update_clock(&mut self) {
        let t = unsafe { GetLocalTime() };
        self.today = (t.wYear as i32, t.wMonth as u32, t.wDay as u32);
        self.time = format!("{:02}:{:02}", t.wHour, t.wMinute);
        self.date = i18n::short_date(t.wDayOfWeek as usize, t.wDay as u32, t.wMonth as u32);
        let wait = (60 - t.wSecond as u32) * 1000 - t.wMilliseconds as u32 + 50;
        unsafe { SetTimer(Some(self.hwnd), TIMER_CLOCK, wait, None) };
    }

    /// Reads what the Control Center shows; cheap, done when it opens and while open.
    fn refresh_controls(&mut self) {
        if self.dragging != Some(Slider::Volume) {
            self.output = audio::read();
        }
        self.internet = network::internet();
        if self.dragging.is_none() {
            self.displays = displays::snapshot();
        }
        self.refresh_app_volume();
    }

    /// Reads the shown player's mixer volume; the Music tab grows or shrinks
    /// when its slider comes or goes.
    fn refresh_app_volume(&mut self) {
        if self.dragging == Some(Slider::App) {
            return;
        }
        let volume = self.track().and_then(|t| audio::app_volume(&t.app, &t.app_name));
        let changed = volume.is_some() != self.app_volume.is_some();
        self.app_volume = volume;
        if changed {
            self.sync_panel_height(self.open.value > 0.05);
        }
    }

    // ---- media ---------------------------------------------------------------

    /// The track the notch shows: the picked player, else what plays, else
    /// Windows' current session.
    fn track(&self) -> Option<&media::Track> {
        let tracks = &self.media.tracks;
        self.player
            .as_ref()
            .and_then(|p| tracks.iter().find(|t| &t.app == p))
            .or_else(|| tracks.iter().find(|t| t.playing))
            .or_else(|| self.media.current.as_ref().and_then(|c| tracks.iter().find(|t| &t.app == c)))
            .or_else(|| tracks.first())
    }

    fn playing(&self) -> bool {
        self.media.tracks.iter().any(|t| t.playing)
    }

    fn on_media(&mut self) {
        self.apply_media(media::snapshot());
    }

    fn apply_media(&mut self, snapshot: media::Snapshot) {
        self.media = snapshot;
        if let Some(p) = &self.player {
            if !self.media.tracks.iter().any(|t| &t.app == p) {
                self.player = None;
            }
        }
        let keys: Vec<String> = self.media.tracks.iter().map(|t| t.key.clone()).collect();
        self.art.retain(|k, _| keys.contains(k));
        for track in &self.media.tracks {
            let Some(art) = &track.art else { continue };
            if self.art.contains_key(&track.key) {
                continue;
            }
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            let bitmap = unsafe {
                self.surface.rt.CreateBitmap(
                    D2D_SIZE_U { width: art.width, height: art.height },
                    Some(art.pixels.as_ptr() as *const _),
                    art.width * 4,
                    &props,
                )
            };
            if let Ok(bitmap) = bitmap {
                self.art.insert(track.key.clone(), bitmap);
            }
        }
        self.music.target = if self.playing() { 1.0 } else { 0.0 };
        self.update_level();
        self.update_stats_timer();
        if self.open.target > 0.5 {
            self.refresh_app_volume();
        }
        self.sync_panel_height(true);
        self.start_animation();
        self.render();
    }

    /// Reads CPU, memory, disk and network every second while the Status tab
    /// is open (and on screen); never otherwise.
    fn update_stats_timer(&mut self) {
        let open = self.open.target > 0.5;
        let interval = if self.headless || self.hidden_for_fullscreen {
            0
        } else if open && self.tab == STATUS_TAB {
            1000
        } else {
            0
        };
        if interval != self.stats_interval {
            self.stats_interval = interval;
            unsafe {
                if interval == 0 {
                    let _ = KillTimer(Some(self.hwnd), TIMER_STATS);
                } else {
                    SetTimer(Some(self.hwnd), TIMER_STATS, interval, None);
                }
            }
        }
    }

    fn on_stats(&mut self) {
        self.stats = self.sampler.sample();
        self.render();
    }

    /// Springs the open panel to the current tab's height (or jumps there).
    fn sync_panel_height(&mut self, animate: bool) {
        let target = self.panel_height();
        if !animate {
            self.panel_h = Spring::new(target);
        } else if (self.panel_h.target - target).abs() > 0.1 {
            self.panel_h.target = target;
            self.start_animation();
        }
    }

    /// Runs the level-bar timer only while something plays and the closed
    /// island is on screen (and a little longer, to let the bars fall). The
    /// open panel has no bars; its progress bar follows the refresh timer.
    fn update_level(&mut self) {
        let want = !self.headless
            && !self.hidden_for_fullscreen
            && self.open.target == 0.0
            && self.tuck.target < 0.5
            && (self.playing() || self.bars.iter().any(|b| *b > 0.01));
        if want != self.level_running {
            self.level_running = want;
            unsafe {
                if want {
                    SetTimer(Some(self.hwnd), TIMER_LEVEL, 33, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd), TIMER_LEVEL);
                }
            }
        }
    }

    fn on_level(&mut self) {
        let (l, r) = if self.playing() { self.meter.peaks() } else { (0.0, 0.0) };
        // Square roots lift quiet passages so the bars stay readable.
        let (l, r) = (l.sqrt(), r.sqrt());
        let targets = [l * 0.8, (l + r) * 0.55, r * 0.95, l.max(r) * 0.7];
        for (bar, target) in self.bars.iter_mut().zip(targets) {
            let rate = if target > *bar { 0.6 } else { 0.18 };
            *bar += (target.min(1.0) - *bar) * rate;
        }
        self.update_level();
        if !self.animating && !self.render_bars(false) {
            self.render();
        }
    }

    /// Where the level bars are on the closed island (with room for their
    /// rounded ends).
    fn bars_box(&self, island: D2D_RECT_F) -> D2D_RECT_F {
        let s = self.scale;
        let total = 3.0 * s * 4.0 + 2.5 * s * 3.0;
        let x0 = island.right - 14.0 * s - self.privacy_inset() - total;
        let cy = island.top + BAR * s / 2.0;
        rect((x0 - 2.0 * s).floor(), (cy - 10.0 * s).floor(), (x0 + total + 2.0 * s).ceil(), (cy + 10.0 * s).ceil())
    }

    /// A level-bar frame: redraws only the bars' box (the island behind them,
    /// then the bars) and sends only that to the screen, instead of the whole
    /// window 30 times a second. False when anything else is changing too.
    fn render_bars(&mut self, force: bool) -> bool {
        // On a side edge the bars sit elsewhere; a full frame is small there.
        if self.side() {
            return false;
        }
        let steady = !self.animating
            && self.open.value < 0.001
            && self.music.value > 0.999
            && self.osd_room.value < 0.001
            && self.content >= 1.0
            && !self.headless;
        if !(steady || force) || self.last_drawn.is_none() {
            return false;
        }
        let s = self.scale;
        let (island, top_radius, bottom_radius, flare) = self.island();
        let docked = self.docked(island);
        let clip = self.bars_box(island);
        let cy = island.top + BAR * s / 2.0;
        let rt = self.surface.rt.clone();
        unsafe {
            rt.BeginDraw();
            rt.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
            rt.Clear(Some(&rgba(0.0, 0.0, 0.0, 0.0)));
            if let Some(path) = self.island_path(island, top_radius, bottom_radius, flare, docked) {
                let (r, g, b) = surface();
                self.brush.SetColor(&rgba(r, g, b, self.settings.opacity));
                rt.FillGeometry(&path, &self.brush, None);
            }
            // Exactly where draw_closed_music puts them.
            let (w, gap) = (3.0 * s, 2.5 * s);
            let x0 = island.right - 14.0 * s - self.privacy_inset() - (w * 4.0 + gap * 3.0);
            for (i, level) in self.bars.iter().enumerate() {
                let h = 3.0 * s + level.clamp(0.0, 1.0) * 13.0 * s;
                let x = x0 + i as f32 * (w + gap);
                self.fill_rounded(rect(x, cy - h / 2.0, x + w, cy + h / 2.0), w / 2.0, accent());
            }
            rt.PopAxisAlignedClip();
            let _ = rt.EndDraw(None, None);
        }
        if !self.headless {
            let dirty = RECT { left: clip.left as i32, top: clip.top as i32, right: clip.right as i32, bottom: clip.bottom as i32 };
            let _ = self.surface.present(self.hwnd, self.origin.0, self.origin.1, Some(dirty));
        }
        true
    }

    // ---- layout ------------------------------------------------------------

    fn tiles(&self) -> Vec<Tile> {
        let mut tiles = Vec::new();
        if self.radios.wifi.is_some() {
            tiles.push(Tile::Wifi);
        }
        if self.radios.bluetooth.is_some() {
            tiles.push(Tile::Bluetooth);
        }
        tiles.push(Tile::Network);
        tiles.push(Tile::Output);
        tiles.extend((0..self.batteries.len().min(4)).map(Tile::Battery));
        tiles
    }

    fn sliders(&self) -> Vec<Slider> {
        let mut sliders = vec![Slider::Volume];
        sliders.extend(self.displays.iter().map(|d| Slider::Brightness(d.device.clone())));
        sliders
    }

    fn music_height(&self) -> f32 {
        if self.track().is_none() {
            return 64.0;
        }
        let chips = if self.media.tracks.len() > 1 { 30.0 } else { 0.0 };
        let volume = if self.app_volume.is_some() { 14.0 + SLIDER_HEIGHT } else { 0.0 };
        PAD * 0.7 + chips + ART + 14.0 + 16.0 + 8.0 + 40.0 + volume + PAD * 0.8
    }

    fn panel_height(&self) -> f32 {
        match self.tab {
            MUSIC_TAB => return self.music_height(),
            STATUS_TAB => return PAD * 0.7 + 52.0 * 3.0 + 10.0 * 2.0 + PAD * 0.8,
            WEATHER_TAB => {
                let days = self.weather.as_ref().map_or(0, |w| w.days.len()) as f32;
                return match &self.weather {
                    Some(_) => PAD * 0.7 + 64.0 + 10.0 + 72.0 + 10.0 + days * DAY_ROW + 12.0 + PAD * 0.8,
                    None => PAD * 0.7 + 64.0 + PAD * 0.8,
                };
            }
            CALENDAR_TAB => return PAD * 0.7 + 28.0 + 6.0 + 18.0 + 6.0 * 28.0 + PAD * 0.6,
            TIMER_TAB => return PAD * 0.7 + CHOICE_HEIGHT + 14.0 + 56.0 + 22.0 + 12.0 + 28.0 + 16.0 + 34.0 + PAD * 0.8,
            SETTINGS_TAB => {
                // The choices, the weather's place and the theme.
                let rows = Choice::ALL.len() as f32 + 2.0;
                return PAD * 0.7
                    + rows * (CHOICE_HEIGHT + 10.0)
                    + self.city_list_height()
                    + SLIDER_HEIGHT
                    + 16.0
                    + 30.0
                    + PAD * 0.8;
            }
            _ => {}
        }
        let rows = self.tiles().len().div_ceil(2) as f32;
        let sliders = self.sliders().len() as f32;
        PAD * 0.7
            + rows * TILE_HEIGHT
            + (rows - 1.0).max(0.0) * 10.0
            + 14.0
            + BUTTON
            + 16.0
            + sliders * SLIDER_HEIGHT
            + (sliders - 1.0).max(0.0) * 12.0
            + PAD
    }

    fn closed_width(&self) -> f32 {
        let (tw, _) = self.text_size(&self.time, &self.time_format);
        let (date, _) = self.closed_date();
        let (dw, _) = self.text_size(&date, &self.date_format);
        let s = self.scale;
        let m = self.music.value.clamp(0.0, 1.0);
        // Time centered; weather on the left and the date on the right, each
        // in a side as wide as the date.
        let status = tw + 2.0 * (dw.max(STATUS_SIDE * s) + 30.0 * s);
        let music = (tw + dw + 8.0 * s + 36.0 * s).max(CLOSED_MIN_WIDTH * s) + 64.0 * s;
        let normal = lerp(status, music, m) + 2.0 * self.privacy_inset();
        let with_osd = lerp(normal, OSD_WIDTH * s, self.osd_room.value.clamp(0.0, 1.0));
        lerp(with_osd, BANNER_WIDTH * s, self.banner_room.value.clamp(0.0, 1.0))
    }

    /// A volume change from outside Pollen: the closed island widens for a
    /// moment to show it (the open panel's slider just follows).
    // ---- the timer and the banner ------------------------------------------------------

    /// Ticks while something runs; the closed island shows what is left.
    fn update_clockwork_timer(&mut self) {
        unsafe {
            if self.clockwork.running() && !self.headless {
                SetTimer(Some(self.hwnd), TIMER_CLOCKWORK, 200, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_CLOCKWORK);
            }
        }
        self.start_animation();
        self.render();
    }

    fn on_clockwork(&mut self) {
        let done = self.clockwork.target().is_some_and(|target| self.clockwork.elapsed() >= target);
        if done {
            let (title, body) = match self.clockwork.mode {
                ClockMode::Pomodoro if self.clockwork.focus => {
                    self.clockwork.focus = false;
                    (t("Mola zamanı", "Time for a break"), t("Biraz dinlen", "Rest a little"))
                }
                ClockMode::Pomodoro => {
                    self.clockwork.focus = true;
                    self.clockwork.round = self.clockwork.round % 4 + 1;
                    (t("Odaklanma zamanı", "Back to focus"), t("Yeni tur başladı", "A new round has started"))
                }
                _ => (t("Süre doldu", "Time's up"), ""),
            };
            if self.clockwork.mode == ClockMode::Pomodoro {
                // The next stretch starts by itself.
                self.clockwork.banked = Duration::ZERO;
                self.clockwork.since = Some(Instant::now());
            } else {
                self.clockwork.reset();
            }
            play_alarm();
            self.show_banner(ICON_ALARM, title.to_string(), body.to_string(), 8);
            self.update_clockwork_timer();
            return;
        }
        let text = self.clockwork.text();
        if text != self.clockwork_text {
            self.clockwork_text = text;
            if !self.animating {
                self.render();
            }
        }
    }

    /// The closed island's right side: the date, or what the timer has left.
    fn closed_date(&self) -> (String, bool) {
        if self.clockwork.active() {
            (format!("{} {}", ICON_STOPWATCH_TEXT, self.clockwork.text()), true)
        } else {
            (self.date.clone(), false)
        }
    }

    /// Widens the closed island for a few seconds with a message.
    fn show_banner(&mut self, glyph: char, title: String, body: String, seconds: u32) {
        if self.headless {
            return;
        }
        self.banner = Some(Banner { glyph, title, body });
        self.banner_room.target = 1.0;
        unsafe { SetTimer(Some(self.hwnd), TIMER_BANNER, seconds * 1000, None) };
        self.start_animation();
    }

    /// The banner: a glyph in a disc, the title and a line under it.
    fn draw_banner(&self, island: D2D_RECT_F, alpha: f32) {
        let Some(banner) = &self.banner else { return };
        let s = self.scale;
        let ink = |a: f32| ink_color(a * alpha);
        let d = 30.0 * s;
        let cy = (island.top + island.bottom) / 2.0;
        let disc = rect(island.left + 14.0 * s, cy - d / 2.0, island.left + 14.0 * s + d, cy + d / 2.0);
        self.fill_rounded(disc, d / 2.0, with_alpha(accent(), alpha));
        self.icon(banner.glyph, &self.icon_format, disc, with_alpha(on_accent(), alpha));
        let x = disc.right + 12.0 * s;
        let limit = island.right - 18.0 * s - x;
        let (_, th) = self.text_size(&banner.title, &self.title_format);
        if banner.body.is_empty() {
            self.text(&banner.title, &self.title_format, pt(x, cy - th / 2.0), ink(0.95), limit);
        } else {
            let (_, bh) = self.text_size(&banner.body, &self.small_format);
            let top = cy - (th + bh + 2.0 * s) / 2.0;
            self.text(&banner.title, &self.title_format, pt(x, top), ink(0.95), limit);
            self.text(&banner.body, &self.small_format, pt(x, top + th + 2.0 * s), ink(0.6), limit);
        }
    }

    fn on_volume(&mut self, packed: usize) {
        let volume = (packed & 0xFFFF) as f32 / 1000.0;
        let muted = (packed >> 16) & 1 != 0;
        if self.dragging == Some(Slider::Volume) {
            return;
        }
        match &mut self.output {
            Some(o) => {
                o.volume = volume;
                o.muted = muted;
            }
            None => self.output = audio::read(),
        }
        if self.open.target < 0.5 && !self.hidden_for_fullscreen && !self.headless {
            self.osd_room.target = 1.0;
            unsafe { SetTimer(Some(self.hwnd), TIMER_OSD, OSD_MS, None) };
            self.start_animation();
        }
        self.render();
    }

    /// The volume indicator: speaker, level bar, percentage.
    fn draw_osd(&self, island: D2D_RECT_F, cy: f32, alpha: f32) {
        let s = self.scale;
        let ink = |a: f32| ink_color(a * alpha);
        let (volume, muted) = self.output.as_ref().map_or((0.0, false), |o| (o.volume, o.muted));
        let level = if muted { 0.0 } else { volume };
        let glyph = if level <= 0.0 { ICON_MUTE } else { ICON_VOLUME };
        let x = island.left + 16.0 * s;
        self.icon(glyph, &self.icon_format, rect(x, cy - 9.0 * s, x + 18.0 * s, cy + 9.0 * s), ink(0.95));
        let text = i18n::percent(level);
        let (tw, th) = self.text_size(&text, &self.date_format);
        let right = island.right - 16.0 * s;
        self.text(&text, &self.date_format, pt(right - tw, cy - th / 2.0), ink(0.8), tw + 4.0);
        let track = rect(x + 28.0 * s, cy - 2.5 * s, right - 40.0 * s, cy + 2.5 * s);
        self.fill_rounded(track, 2.5 * s, ink(0.18));
        let fill = rect(track.left, track.top, track.left + (track.right - track.left) * level, track.bottom);
        if fill.right - fill.left > 0.5 {
            self.fill_rounded(fill, 2.5 * s, ink(0.95));
        }
    }

    /// Room for the microphone / camera dots on the right of the closed
    /// island (the island grows by as much on the left, to stay centered).
    fn privacy_inset(&self) -> f32 {
        let n = (!self.privacy.microphone.is_empty()) as usize + (!self.privacy.camera.is_empty()) as usize;
        let width = if n == 0 { 14.0 } else { n as f32 * 7.0 + (n as f32 - 1.0) * 4.0 + 8.0 };
        width * self.privacy_room.value.clamp(0.0, 1.0) * self.scale
    }

    /// The dots, colored, right-aligned at `right`; returns their left edge.
    fn draw_privacy_dots(&self, right: f32, cy: f32, alpha: f32) -> f32 {
        let s = self.scale;
        let mut x = right;
        for (on, color) in [(!self.privacy.camera.is_empty(), CAMERA_COLOR), (!self.privacy.microphone.is_empty(), MIC_COLOR)] {
            if on {
                let d = 7.0 * s;
                self.fill_rounded(rect(x - d, cy - d / 2.0, x, cy + d / 2.0), d / 2.0, with_alpha(color, alpha));
                x -= d + 4.0 * s;
            }
        }
        x
    }

    fn on_privacy(&mut self) {
        self.privacy = privacy::current();
        self.privacy_room.target = if self.privacy.any() { 1.0 } else { 0.0 };
        self.start_animation();
        self.render();
    }

    /// Whether the notch stands on a side edge of the screen.
    fn side(&self) -> bool {
        self.edge.side()
    }

    /// The island right now, in the window.
    fn island(&self) -> (D2D_RECT_F, f32, f32, f32) {
        let (r, top_radius, bottom_radius, flare) = self.edge_island();
        (self.edge_to_window(r), top_radius, bottom_radius, flare)
    }

    /// The island in edge space: x runs along the screen edge the notch sits
    /// on (as in the window), y away from it. On the top edge that is the
    /// window itself; on a side edge, the window turned (`edge_to_window`).
    fn edge_island(&self) -> (D2D_RECT_F, f32, f32, f32) {
        let s = self.scale;
        let o = self.open.value.clamp(0.0, 1.05);
        // A banner with a second line makes the closed island taller.
        let extra = if self.banner.as_ref().is_some_and(|b| !b.body.is_empty()) { 16.0 } else { 6.0 };
        let banner = self.banner_room.value.clamp(0.0, 1.0);
        let (w, h) = if self.side() {
            // A tall pill; the volume and a banner make it a short, wide card.
            let osd = self.osd_room.value.clamp(0.0, 1.0);
            let along = lerp(lerp(SIDE_LENGTH * s, BAR * s, osd), (BAR + extra) * s, banner);
            let depth = lerp(lerp(SIDE_WIDTH * s, OSD_WIDTH * s, osd), BANNER_WIDTH * s, banner);
            (lerp(along, (BAR + self.panel_h.value) * s, o), lerp(depth, OPEN_WIDTH * s, o))
        } else {
            (
                lerp(self.closed_width(), OPEN_WIDTH * s, o),
                lerp((BAR + banner * extra) * s, (BAR + self.panel_h.value) * s, o),
            )
        };
        let e = self.spread.value.clamp(0.0, 1.05);
        let c = CIRCLE * s;
        let w = lerp(c, w, e);
        let h = lerp(c, h, e);
        // Tucked away, it waits beyond the edge (with the flares).
        let top = self.rise.value - self.tuck.value.clamp(0.0, 1.0) * (h + (FLARE + 4.0) * s);
        let (_, left) = self.placement(w);
        let r = rect(left, top, left + w, top + h);
        let round = self.round_top.value.clamp(0.0, 1.0);
        let top_radius = round * c / 2.0;
        let bottom_radius = lerp(c / 2.0, RADIUS * s, e.min(1.0));
        let flare = if self.flares { FLARE * s } else { 0.0 };
        (r, top_radius, bottom_radius, flare)
    }

    /// Edge space to the window: on the left edge x and y swap; on the right
    /// edge, mirrored too.
    fn edge_to_window(&self, r: D2D_RECT_F) -> D2D_RECT_F {
        let width = self.surface.width as f32;
        match self.edge {
            NotchEdge::Top => r,
            NotchEdge::Left => rect(r.top, r.left, r.bottom, r.right),
            NotchEdge::Right => rect(width - r.bottom, r.left, width - r.top, r.right),
        }
    }

    /// The same as a transform, for the island's shape.
    fn edge_matrix(&self) -> Matrix3x2 {
        let width = self.surface.width as f32;
        match self.edge {
            NotchEdge::Top => IDENTITY,
            NotchEdge::Left => Matrix3x2 { M11: 0.0, M12: 1.0, M21: 1.0, M22: 0.0, M31: 0.0, M32: 0.0 },
            NotchEdge::Right => Matrix3x2 { M11: 0.0, M12: 1.0, M21: -1.0, M22: 0.0, M31: width, M32: 0.0 },
        }
    }

    /// The stretch of screen the island slides along (screen pixels): the
    /// whole top edge, or a side edge clear of the dock, with room for the
    /// flares at both ends.
    fn span(&self) -> (f32, f32) {
        if self.side() {
            let flare = FLARE * self.scale;
            (self.work.top as f32 + flare, self.work.bottom as f32 - flare)
        } else {
            (self.monitor.left as f32, self.monitor.right as f32)
        }
    }

    /// For an island `len` long along its edge: where the window starts
    /// along the edge on the screen, and where the island starts in the
    /// window. `slide` runs the island from one end (0) to the other (1), so
    /// it grows away from the nearer end; the window stays on the monitor.
    fn placement(&self, len: f32) -> (i32, f32) {
        let (lo, hi) = self.span();
        let start = lo + self.slide.value.clamp(0.0, 1.0) * (hi - lo - len).max(0.0);
        let m = self.monitor;
        let (size, min, max) = if self.side() { (self.surface.height, m.top, m.bottom) } else { (self.surface.width, m.left, m.right) };
        let origin = ((start + len / 2.0 - size as f32 / 2.0).round() as i32).clamp(min, (max - size).max(min));
        (origin, start - origin as f32)
    }

    /// The window's top-left corner when it starts at `along` along the edge.
    fn window_origin(&self, along: i32) -> (i32, i32) {
        let m = self.monitor;
        match self.edge {
            NotchEdge::Top => (along, m.top),
            NotchEdge::Left => (m.left, along),
            NotchEdge::Right => (m.right - self.surface.width, along),
        }
    }

    /// How much the island (edge space) hugs the monitor's left and right
    /// edges (0..1). On a side edge it keeps clear of the ends.
    fn docked(&self, island: D2D_RECT_F) -> (f32, f32) {
        if self.side() {
            return (0.0, 0.0);
        }
        let reach = FLARE * self.scale;
        let (origin, _) = self.placement(island.right - island.left);
        let left = origin as f32 + island.left - self.monitor.left as f32;
        let right = self.monitor.right as f32 - (origin as f32 + island.right);
        (1.0 - (left / reach).clamp(0.0, 1.0), 1.0 - (right / reach).clamp(0.0, 1.0))
    }

    /// The cursor along the notch's edge (screen pixels).
    fn cursor_along(&self) -> f32 {
        let mut cursor = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cursor);
        }
        if self.side() {
            cursor.y as f32
        } else {
            cursor.x as f32
        }
    }

    /// Follows the cursor while the notch is moved by its top row, snapping
    /// to both ends and the middle.
    fn drag_notch(&mut self) {
        let Some(offset) = self.grab else { return };
        let (island, ..) = self.edge_island();
        let (lo, hi) = self.span();
        let room = (hi - lo - (island.right - island.left)).max(1.0);
        let x = self.cursor_along() - offset - lo;
        let snap = SNAP * self.scale;
        let x = if x < snap {
            0.0
        } else if x > room - snap {
            room
        } else if (x - room / 2.0).abs() < snap {
            room / 2.0
        } else {
            x
        };
        let pos = (x / room).clamp(0.0, 1.0);
        if pos != self.slide.target {
            if self.side() {
                self.settings.notch_y = pos;
            } else {
                self.settings.notch_x = pos;
            }
            self.slide.target = pos;
            self.start_animation();
        }
    }

    /// Moves the notch to another edge: it closes and comes in again there.
    fn change_edge(&mut self) {
        self.set_open(false);
        self.open = Spring::new(0.0);
        self.hovered = false;
        self.mouse = None;
        self.pressed = None;
        self.place();
        self.slide = Spring::new(edge_position(&self.settings));
        self.rise = Spring { value: RISE * self.scale, velocity: 0.0, target: 0.0 };
        self.round_top = Spring::new(1.0);
        self.spread = Spring::new(0.0);
        self.flares = false;
        self.content = 0.0;
        self.content_fade_from = None;
        self.opening_since = Some(Instant::now());
        self.start_animation();
    }

    /// Every control with its rectangle, for drawing and hit testing.
    fn controls(&self, island: D2D_RECT_F) -> Vec<(D2D_RECT_F, Control)> {
        let s = self.scale;
        let mut list = Vec::new();
        // Tabs in the top row.
        let tab_w = TAB_WIDTH * s;
        let tabs_left = (island.left + island.right) / 2.0 - tab_w * TABS.len() as f32 / 2.0;
        for i in 0..TABS.len() {
            let x = tabs_left + i as f32 * tab_w;
            list.push((rect(x, island.top, x + tab_w, island.top + BAR * s), Control::Tab(i)));
        }
        // Beside the tabs (the time and the date) the notch can be dragged.
        let tabs_right = tabs_left + tab_w * TABS.len() as f32;
        list.push((rect(island.left, island.top, tabs_left, island.top + BAR * s), Control::Grip));
        list.push((rect(tabs_right, island.top, island.right, island.top + BAR * s), Control::Grip));
        if self.tab == MUSIC_TAB {
            self.music_controls(island, &mut list);
            return list;
        }
        if self.tab == TIMER_TAB {
            let left = island.left + PAD * s;
            let right = island.right - PAD * s;
            let mut y = island.top + BAR * s + PAD * 0.7 * s;
            let w = (right - left) / ClockMode::ALL.len() as f32;
            for i in 0..ClockMode::ALL.len() {
                let x = left + i as f32 * w;
                list.push((rect(x, y, x + w, y + CHOICE_HEIGHT * s), Control::ClockMode(i)));
            }
            y += (CHOICE_HEIGHT + 14.0 + 56.0 + 22.0 + 12.0) * s;
            if self.clockwork.mode == ClockMode::Timer {
                let presets = [1u32, 3, 5, 10, 15, 25, 45, 60];
                let w = (right - left) / presets.len() as f32;
                for (i, m) in presets.iter().enumerate() {
                    let x = left + i as f32 * w;
                    list.push((rect(x + 2.0 * s, y, x + w - 2.0 * s, y + 28.0 * s), Control::ClockPreset(*m)));
                }
            }
            y += (28.0 + 16.0) * s;
            let mid = (left + right) / 2.0;
            list.push((rect(mid - 128.0 * s, y, mid - 6.0 * s, y + 34.0 * s), Control::ClockStart));
            list.push((rect(mid + 6.0 * s, y, mid + 128.0 * s, y + 34.0 * s), Control::ClockReset));
            return list;
        }
        if self.tab == CALENDAR_TAB {
            let left = island.left + PAD * s;
            let right = island.right - PAD * s;
            let y = island.top + BAR * s + PAD * 0.7 * s;
            list.push((rect(left, y, left + 28.0 * s, y + 28.0 * s), Control::CalendarPrevious));
            list.push((rect(right - 28.0 * s, y, right, y + 28.0 * s), Control::CalendarNext));
            let mid = (left + right) / 2.0;
            list.push((rect(mid - 70.0 * s, y, mid + 70.0 * s, y + 28.0 * s), Control::CalendarTitle));
            return list;
        }
        if self.tab == SETTINGS_TAB {
            let left = island.left + PAD * s;
            let right = island.right - PAD * s;
            let mut y = island.top + BAR * s + PAD * 0.7 * s;
            let x0 = left + LABEL_WIDTH * s;
            for choice in Choice::ALL {
                let n = choice.options(&self.settings).len();
                let w = (right - x0) / n as f32;
                for i in 0..n {
                    let x = x0 + i as f32 * w;
                    list.push((rect(x, y, x + w, y + CHOICE_HEIGHT * s), Control::Choice(choice, i)));
                }
                y += (CHOICE_HEIGHT + 10.0) * s;
            }
            // The weather's place; "Otomatik" sits inside the field, so it goes first.
            if self.city_edit.is_none() && self.place.is_some() {
                let h = CHOICE_HEIGHT * s;
                list.push((rect(right - 78.0 * s, y + 4.0 * s, right - 4.0 * s, y + h - 4.0 * s), Control::CityAuto));
            }
            list.push((rect(x0, y, right, y + CHOICE_HEIGHT * s), Control::CityField));
            y += (CHOICE_HEIGHT + 6.0) * s;
            if let Some(edit) = &self.city_edit {
                for i in 0..edit.results.len() {
                    let top = y + i as f32 * RESULT_HEIGHT * s;
                    list.push((rect(x0, top, right, top + (RESULT_HEIGHT - 2.0) * s), Control::CityResult(i)));
                }
            }
            y += (self.city_list_height() + 4.0) * s;
            // Theme swatches, spread across.
            let n = settings::Theme::ALL.len() as f32;
            let d = 20.0 * s;
            let gap = ((right - x0) - n * d) / (n - 1.0);
            for i in 0..settings::Theme::ALL.len() {
                let x = x0 + i as f32 * (d + gap);
                let top = y + (CHOICE_HEIGHT * s - d) / 2.0;
                list.push((rect(x, top, x + d, top + d), Control::Theme(i)));
            }
            y += (CHOICE_HEIGHT + 10.0) * s;
            let icon = rect(left, y, left + 18.0 * s, y + SLIDER_HEIGHT * s);
            list.push((icon, Control::SliderIcon(Slider::Opacity)));
            list.push((rect(x0, y, right - 40.0 * s, y + SLIDER_HEIGHT * s), Control::Slider(Slider::Opacity)));
            y += (SLIDER_HEIGHT + 16.0) * s;
            list.push((rect(right - 132.0 * s, y, right, y + 30.0 * s), Control::Quit));
            if crate::update::available().is_some() {
                list.push((rect(left, y, right - 140.0 * s, y + 30.0 * s), Control::Update));
            }
            return list;
        }
        if self.tab != CONTROL_CENTER_TAB {
            return list;
        }

        let left = island.left + PAD * s;
        let right = island.right - PAD * s;
        let mut y = island.top + BAR * s + PAD * 0.7 * s;
        // Tiles, two per row.
        let col = (right - left - 10.0 * s) / 2.0;
        for (i, tile) in self.tiles().into_iter().enumerate() {
            let x = left + (i % 2) as f32 * (col + 10.0 * s);
            let row_y = y + (i / 2) as f32 * (TILE_HEIGHT + 10.0) * s;
            list.push((rect(x, row_y, x + col, row_y + TILE_HEIGHT * s), Control::Tile(tile)));
        }
        let rows = self.tiles().len().div_ceil(2) as f32;
        y += rows * TILE_HEIGHT * s + (rows - 1.0).max(0.0) * 10.0 * s + 14.0 * s;
        // Buttons, spread across.
        let n = Button::ALL.len() as f32;
        let gap = (right - left - 8.0 * s - n * BUTTON * s) / (n - 1.0);
        for (i, button) in Button::ALL.iter().enumerate() {
            let x = left + 4.0 * s + i as f32 * (BUTTON * s + gap);
            list.push((rect(x, y, x + BUTTON * s, y + BUTTON * s), Control::Button(*button)));
        }
        y += (BUTTON + 16.0) * s;
        // Sliders.
        for slider in self.sliders() {
            let icon = rect(left, y, left + 18.0 * s, y + SLIDER_HEIGHT * s);
            let track = rect(left + LABEL_WIDTH * s, y, right - 40.0 * s, y + SLIDER_HEIGHT * s);
            list.push((icon, Control::SliderIcon(slider.clone())));
            list.push((track, Control::Slider(slider)));
            y += (SLIDER_HEIGHT + 12.0) * s;
        }
        list
    }

    /// Player chips, the progress bar and the playback buttons.
    fn music_controls(&self, island: D2D_RECT_F, list: &mut Vec<(D2D_RECT_F, Control)>) {
        let s = self.scale;
        let left = island.left + PAD * s;
        let right = island.right - PAD * s;
        let mut y = island.top + BAR * s + PAD * 0.7 * s;
        if self.media.tracks.len() > 1 {
            let mut x = left;
            for track in &self.media.tracks {
                let (w, _) = self.text_size(&track.app_name, &self.small_format);
                let chip = rect(x, y, (x + w + 20.0 * s).min(right), y + 22.0 * s);
                list.push((chip, Control::Player(track.app.clone())));
                x = chip.right + 6.0 * s;
                if x > right - 30.0 * s {
                    break;
                }
            }
            y += 30.0 * s;
        }
        if self.track().is_none() {
            return;
        }
        y += (ART + 14.0) * s;
        list.push((rect(left + 40.0 * s, y, right - 40.0 * s, y + 16.0 * s), Control::Progress));
        y += (16.0 + 8.0) * s;
        let cx = (left + right) / 2.0;
        let gap = 26.0 * s;
        let (small, big) = (34.0 * s, 40.0 * s);
        let mid = y + big / 2.0;
        list.push((rect(cx - big / 2.0, y, cx + big / 2.0, y + big), Control::Media(MediaButton::PlayPause)));
        let px = cx - big / 2.0 - gap - small;
        list.push((rect(px, mid - small / 2.0, px + small, mid + small / 2.0), Control::Media(MediaButton::Previous)));
        let nx = cx + big / 2.0 + gap;
        list.push((rect(nx, mid - small / 2.0, nx + small, mid + small / 2.0), Control::Media(MediaButton::Next)));
        if self.app_volume.is_some() {
            let y = y + big + 14.0 * s;
            let icon = rect(left, y, left + 18.0 * s, y + SLIDER_HEIGHT * s);
            let track = rect(left + LABEL_WIDTH * s, y, right - 40.0 * s, y + SLIDER_HEIGHT * s);
            list.push((icon, Control::SliderIcon(Slider::App)));
            list.push((track, Control::Slider(Slider::App)));
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<Control> {
        if self.open.value < 0.9 || self.content < 0.5 {
            return None;
        }
        let (island, ..) = self.island();
        self.controls(island).into_iter().find(|(r, _)| contains(r, x, y)).map(|(_, c)| c)
    }

    // ---- values --------------------------------------------------------------

    fn slider_value(&self, slider: &Slider) -> Option<f32> {
        match slider {
            Slider::Volume => self.output.as_ref().map(|o| if o.muted { 0.0 } else { o.volume }),
            Slider::App => self.app_volume.map(|a| if a.muted { 0.0 } else { a.volume }),
            Slider::Opacity => Some(self.settings.opacity),
            Slider::Brightness(device) => self
                .displays
                .iter()
                .find(|d| &d.device == device)
                .and_then(|d| d.brightness)
                .map(|b| b as f32 / 100.0),
        }
    }

    fn set_slider(&mut self, slider: &Slider, value: f32) {
        let value = value.clamp(0.0, 1.0);
        match slider {
            Slider::Volume => {
                audio::set_volume(value);
                if let Some(o) = &mut self.output {
                    o.volume = value;
                    if value > 0.0 {
                        o.muted = false;
                    }
                }
            }
            Slider::App => {
                if let Some(t) = self.track() {
                    let (app, name) = (t.app.clone(), t.app_name.clone());
                    audio::set_app_volume(&app, &name, value);
                }
                if let Some(a) = &mut self.app_volume {
                    a.volume = value;
                    if value > 0.0 {
                        a.muted = false;
                    }
                }
            }
            Slider::Opacity => {
                // Saved when the slider is let go.
                self.settings.opacity = value.max(settings::MIN_OPACITY);
                settings::set(self.settings, false);
            }
            Slider::Brightness(device) => {
                if let Some(d) = self.displays.iter_mut().find(|d| &d.device == device) {
                    if d.brightness.is_none() {
                        return;
                    }
                    let percent = (value * 100.0).round() as u32;
                    d.brightness = Some(percent);
                    displays::set(device, percent);
                }
            }
        }
        self.render();
    }

    fn slider_at(&self, slider: &Slider, x: f32) -> f32 {
        let (island, ..) = self.island();
        self.controls(island)
            .into_iter()
            .find(|(_, c)| c == &Control::Slider(slider.clone()))
            .map(|(r, _)| (x - r.left) / (r.right - r.left).max(1.0))
            .unwrap_or(0.0)
    }

    // ---- motion --------------------------------------------------------------

    fn start_animation(&mut self) {
        if !self.animating && !self.headless {
            self.animating = true;
            self.last_tick = Some(Instant::now());
            pacer::start(self.hwnd);
        }
    }

    fn set_open(&mut self, open: bool) {
        let target = if open { 1.0 } else { 0.0 };
        if self.open.target != target {
            self.open.target = target;
            if open {
                // Opening replaces the volume indicator and any banner.
                self.osd_room.target = 0.0;
                self.banner_room.target = 0.0;
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_OSD);
                }
            }
            if open {
                // Music while something plays, otherwise the Control Center.
                self.tab = if self.playing() { MUSIC_TAB } else { CONTROL_CENTER_TAB };
                self.player = None;
                self.month = (self.today.0, self.today.1);
                self.stats = self.sampler.sample();
                self.refresh_controls();
                displays::refresh();
                battery::refresh(self.hwnd, WM_BATTERIES);
                unsafe { SetTimer(Some(self.hwnd), TIMER_REFRESH, 700, None) };
                self.sync_panel_height(false);
            } else {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_REFRESH);
                }
            }
            self.update_stats_timer();
            self.update_level();
            self.start_animation();
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = self.last_tick.map_or(1.0 / 60.0, |t| (now - t).as_secs_f32()).min(0.05);
        self.last_tick = Some(now);
        let mut moving = false;

        // Nectar's notch opening: impact and spread together at 240 ms.
        if let Some(since) = self.opening_since {
            if now - since >= Duration::from_millis(240) {
                self.round_top.target = 0.0;
                self.spread.target = 1.0;
                self.flares = true;
                self.content_fade_from = Some(now);
                self.opening_since = None;
            }
            moving = true;
        }
        if let Some(from) = self.content_fade_from {
            self.content = ((now - from).as_secs_f32() / 0.15).min(1.0);
            if self.content >= 1.0 {
                self.content_fade_from = None;
            }
            moving = true;
        }

        // Settings › Notch › On hover: out of sight unless something shows.
        let peek = !self.settings.notch_hide
            || self.hovered
            || self.grab.is_some()
            || self.open.target > 0.0
            || self.banner_room.target > 0.0
            || self.osd_room.target > 0.0;
        let tuck = if peek { 0.0 } else { 1.0 };
        if tuck != self.tuck.target {
            self.tuck.target = tuck;
            self.update_level();
        }
        moving |= self.tuck.step(dt, 312.5, 35.0);
        moving |= self.slide.step(dt, 400.0, 40.0);
        moving |= self.rise.step(dt, 500.0, 43.75);
        moving |= self.round_top.step(dt, 312.5, 35.0);
        moving |= self.spread.step(dt, 312.5, 35.0);
        moving |= self.open.step(dt, 312.5, 35.0);
        moving |= self.music.step(dt, 312.5, 35.0);
        moving |= self.panel_h.step(dt, 312.5, 35.0);
        moving |= self.privacy_room.step(dt, 312.5, 35.0);
        moving |= self.osd_room.step(dt, 400.0, 34.0);
        moving |= self.banner_room.step(dt, 400.0, 34.0);
        if self.banner_room.target == 0.0 && self.banner_room.value < 0.01 {
            self.banner = None;
        }

        self.render();
        if !moving {
            self.animating = false;
            self.last_tick = None;
            pacer::stop(self.hwnd);
            // Closed, the panel's lines are not needed until it opens again.
            if self.open.target == 0.0 {
                self.texts.clear();
            }
        }
    }

    // ---- drawing ---------------------------------------------------------------

    fn text_size(&self, text: &str, format: &IDWriteTextFormat) -> (f32, f32) {
        self.texts.size(&self.dwrite, text, format)
    }

    /// Draws `text` with its top-left at `origin`, cut off with an ellipsis past
    /// `max_width`.
    fn text(&self, text: &str, format: &IDWriteTextFormat, origin: Vector2, color: D2D1_COLOR_F, max_width: f32) {
        let Some(layout) = self.texts.line(&self.dwrite, text, format, max_width) else { return };
        unsafe {
            self.brush.SetColor(&color);
            self.surface.rt.DrawTextLayout(origin, &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
    }

    /// A glyph centered in `r`.
    fn icon(&self, glyph: char, format: &IDWriteTextFormat, r: D2D_RECT_F, color: D2D1_COLOR_F) {
        let s = glyph.to_string();
        let (w, h) = self.text_size(&s, format);
        self.text(&s, format, pt((r.left + r.right - w) / 2.0, (r.top + r.bottom - h) / 2.0), color, w + 4.0);
    }

    /// A weather glyph: Segoe Fluent Icons, or Segoe UI Symbol for the ones it lacks.
    fn weather_symbol(&self, glyph: char, big: bool, r: D2D_RECT_F, color: D2D1_COLOR_F) {
        let symbol = matches!(glyph, ICON_PARTLY | ICON_RAIN | ICON_SNOW);
        let format = match (symbol, big) {
            (true, true) => &self.big_symbol_format,
            (true, false) => &self.symbol_format,
            (false, true) => &self.big_icon_format,
            (false, false) => &self.icon_format,
        };
        self.icon(glyph, format, r, color);
    }

    fn fill_rounded(&self, r: D2D_RECT_F, radius: f32, color: D2D1_COLOR_F) {
        unsafe {
            self.brush.SetColor(&color);
            self.surface
                .rt
                .FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &self.brush);
        }
    }

    /// Nectar's SurfaceShape for the top edge: inward flares at the top, round
    /// bottom corners; a free circle (round top corners) during the opening.
    /// `docked`: how much each side hugs a side edge of the screen. That side
    /// loses its top flare, and its bottom corner turns into a flare down the
    /// edge (square halfway).
    fn island_path(&self, r: D2D_RECT_F, top_radius: f32, bottom_radius: f32, flare: f32, docked: (f32, f32)) -> Option<ID2D1PathGeometry> {
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let tr = top_radius.min(w / 2.0).min(h / 2.0).max(0.0);
        let br = bottom_radius.min(w / 2.0).min(h / 2.0).max(0.0);
        let side = |d: f32| (flare * (1.0 - d), br * (1.0 - 2.0 * d).max(0.0), flare * (2.0 * d - 1.0).max(0.0));
        let (left_flare, left_round, left_drop) = side(docked.0);
        let (right_flare, right_round, right_drop) = side(docked.1);
        unsafe {
            let path = self.factory.CreatePathGeometry().ok()?;
            let sink = path.Open().ok()?;
            let arc = |x: f32, y: f32, radius: f32, clockwise: bool| D2D1_ARC_SEGMENT {
                point: pt(x, y),
                size: D2D_SIZE_F { width: radius, height: radius },
                rotationAngle: 0.0,
                sweepDirection: if clockwise {
                    D2D1_SWEEP_DIRECTION_CLOCKWISE
                } else {
                    D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE
                },
                arcSize: D2D1_ARC_SIZE_SMALL,
            };
            if left_flare > tr {
                sink.BeginFigure(pt(r.left - left_flare, r.top), D2D1_FIGURE_BEGIN_FILLED);
                sink.AddArc(&arc(r.left, r.top + left_flare, left_flare, true));
            } else {
                sink.BeginFigure(pt(r.left + tr, r.top), D2D1_FIGURE_BEGIN_FILLED);
                if tr >= 0.5 {
                    sink.AddArc(&arc(r.left, r.top + tr, tr, false));
                }
            }
            if left_drop >= 0.5 {
                sink.AddLine(pt(r.left, r.bottom + left_drop));
                sink.AddArc(&arc(r.left + left_drop, r.bottom, left_drop, true));
            } else {
                sink.AddLine(pt(r.left, r.bottom - left_round));
                if left_round >= 0.5 {
                    sink.AddArc(&arc(r.left + left_round, r.bottom, left_round, false));
                }
            }
            if right_drop >= 0.5 {
                sink.AddLine(pt(r.right - right_drop, r.bottom));
                sink.AddArc(&arc(r.right, r.bottom + right_drop, right_drop, true));
            } else {
                sink.AddLine(pt(r.right - right_round, r.bottom));
                if right_round >= 0.5 {
                    sink.AddArc(&arc(r.right, r.bottom - right_round, right_round, false));
                }
            }
            if right_flare > tr {
                sink.AddLine(pt(r.right, r.top + right_flare));
                sink.AddArc(&arc(r.right + right_flare, r.top, right_flare, true));
            } else {
                sink.AddLine(pt(r.right, r.top + tr));
                if tr >= 0.5 {
                    sink.AddArc(&arc(r.right - tr, r.top, tr, false));
                }
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close().ok()?;
            Some(path)
        }
    }

    fn render(&mut self) {
        let s = self.scale;
        let (edge, top_radius, bottom_radius, flare) = self.edge_island();
        // The window follows the island along its edge.
        let origin = self.window_origin(self.placement(edge.right - edge.left).0);
        if origin != self.origin {
            self.origin = origin;
            self.last_drawn = None;
        }
        let island = self.edge_to_window(edge);
        let docked = self.docked(edge);
        // Tucked away, a strip the mouse can find (too faint to see) stays
        // at the top edge.
        let strip = self.settings.notch_hide && self.tuck.value > 0.5;
        let rt = self.surface.rt.clone();
        let o = self.open.value.clamp(0.0, 1.0);
        unsafe {
            rt.BeginDraw();
            let backdrop = self.headless && !crate::util::demo();
            rt.Clear(Some(&if backdrop { rgba(0.32, 0.40, 0.52, 1.0) } else { rgba(0.0, 0.0, 0.0, 0.0) }));
            if strip {
                self.brush.SetColor(&rgba(0.0, 0.0, 0.0, 2.0 / 255.0));
                rt.FillRectangle(&self.edge_to_window(rect(edge.left, 0.0, edge.right, 2.0)), &self.brush);
            }
            if let Some(path) = self.island_path(edge, top_radius, bottom_radius, flare, docked) {
                let (r, g, b) = surface();
                let base = self.settings.opacity;
                self.brush.SetColor(&rgba(r, g, b, lerp(base, (base + 0.07).min(1.0), o)));
                rt.SetTransform(&self.edge_matrix());
                rt.FillGeometry(&path, &self.brush, None);
                rt.SetTransform(&IDENTITY);
            }

            rt.PushAxisAlignedClip(&island, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            // Closed: time and date, fading out as it opens.
            let closed_all = self.content * (1.0 - o * 2.5).clamp(0.0, 1.0);
            let osd = self.osd_room.value.clamp(0.0, 1.0).max(self.banner_room.value.clamp(0.0, 1.0));
            // The volume indicator or a banner takes the closed island's place for a moment.
            let closed = closed_all * (1.0 - osd * 1.6).clamp(0.0, 1.0);
            let banner = self.banner_room.value.clamp(0.0, 1.0);
            if closed_all > 0.0 && banner > 0.01 {
                self.draw_banner(island, closed_all * ((banner - 0.35) / 0.65).clamp(0.0, 1.0));
            }
            let volume = self.osd_room.value.clamp(0.0, 1.0) * (1.0 - banner);
            if closed_all > 0.0 && volume > 0.01 {
                let cy = island.top + BAR * s / 2.0;
                self.draw_osd(island, cy, closed_all * ((volume - 0.35) / 0.65).clamp(0.0, 1.0));
            }
            if closed > 0.0 && self.side() {
                self.draw_closed_side(island, closed);
            } else if closed > 0.0 {
                let (tw, th) = self.text_size(&self.time, &self.time_format);
                let (date, counting) = self.closed_date();
                let (dw, dh) = self.text_size(&date, &self.date_format);
                let total = tw + 8.0 * s + dw;
                let cy = island.top + BAR * s / 2.0;
                let m = self.music.value.clamp(0.0, 1.0);
                if m > 0.01 {
                    self.draw_closed_music(island, cy, closed * m);
                }
                if m < 0.99 {
                    self.draw_closed_status(island, cy, closed * (1.0 - m));
                }
                // With music the time and date sit together between the art
                // and the bars; without, the time is centered and the date
                // goes to the right edge, across from the weather.
                let mid = (island.left + island.right) / 2.0;
                let together = mid - total / 2.0;
                let x = lerp(mid - tw / 2.0, together, m);
                let inset = self.privacy_inset();
                let date_x = lerp(island.right - 14.0 * s - inset - dw, together + tw + 8.0 * s, m);
                if self.privacy_room.value > 0.01 {
                    let a = closed * self.privacy_room.value.clamp(0.0, 1.0);
                    self.draw_privacy_dots(island.right - 14.0 * s, cy, a);
                }
                self.text(&self.time, &self.time_format, pt(x, cy - th / 2.0), ink_color(0.95 * closed), tw + 4.0);
                let date_color = if counting { with_alpha(accent(), closed) } else { ink_color(0.65 * closed) };
                self.text(
                    &date,
                    &self.date_format,
                    pt(date_x, cy - dh / 2.0),
                    date_color,
                    dw + 4.0,
                );
            }
            // Open: the panel.
            let opened = self.content * ((o - 0.55) / 0.45).clamp(0.0, 1.0);
            if opened > 0.0 {
                self.draw_panel(island, opened);
            }
            rt.PopAxisAlignedClip();
            let _ = rt.EndDraw(None, None);
        }

        // The flares reach out at the top and, against a side edge, down it.
        let reach = rect(edge.left - flare - 2.0, (edge.top - 2.0).max(0.0), edge.right + flare + 2.0, (edge.bottom + flare + 2.0).max(0.0));
        let reach = if strip { rect(reach.left, 0.0, reach.right, reach.bottom.max(3.0)) } else { reach };
        let reach = self.edge_to_window(reach);
        let (sw, sh) = (self.surface.width, self.surface.height);
        let mut drawn = RECT {
            left: (reach.left.floor() as i32).clamp(0, sw),
            top: (reach.top.floor() as i32).clamp(0, sh),
            right: (reach.right.ceil() as i32).clamp(0, sw),
            bottom: (reach.bottom.ceil() as i32).clamp(0, sh),
        };
        drawn.right = drawn.right.max(drawn.left);
        drawn.bottom = drawn.bottom.max(drawn.top);
        let size = (self.surface.width, self.surface.height);
        let dirty = match self.last_drawn {
            Some((last, last_size)) if last_size == size => Some(RECT {
                left: last.left.min(drawn.left),
                top: last.top.min(drawn.top),
                right: last.right.max(drawn.right),
                bottom: last.bottom.max(drawn.bottom),
            }),
            _ => None,
        };
        self.last_drawn = Some((drawn, size));
        if !self.headless {
            let _ = self.surface.present(self.hwnd, self.origin.0, self.origin.1, dirty);
        }
    }

    /// The closed island on a side edge, top to bottom: the album art (or the
    /// weather), the time in two lines, the day and the month (or the
    /// timer), the camera and microphone dots, the level bars (or the
    /// temperature).
    fn draw_closed_side(&self, island: D2D_RECT_F, alpha: f32) {
        let s = self.scale;
        let ink = |a: f32| ink_color(a * alpha);
        let cx = (island.left + island.right) / 2.0;
        let m = self.music.value.clamp(0.0, 1.0);
        let mut y = island.top + 14.0 * s;
        let a = 24.0 * s;
        if m > 0.01 {
            if let Some(track) = self.track() {
                self.draw_art(track, rect(cx - a / 2.0, y, cx + a / 2.0, y + a), 6.0 * s, alpha * m);
            }
        }
        let (glyph, temp) = match &self.weather {
            Some(w) => (weather_icon(w), format!("{:.0}°", w.temperature)),
            None => (ICON_CLOUD, "--°".to_string()),
        };
        if m < 0.99 {
            self.weather_symbol(glyph, false, rect(cx - 9.0 * s, y + 3.0 * s, cx + 9.0 * s, y + 21.0 * s), ink(0.85 * (1.0 - m)));
        }
        y += a + 12.0 * s;
        let (hours, minutes) = self.time.split_once(':').unwrap_or((self.time.as_str(), ""));
        for part in [hours, minutes] {
            let (tw, th) = self.text_size(part, &self.time_format);
            self.text(part, &self.time_format, pt(cx - tw / 2.0, y), ink(0.95), tw + 4.0);
            y += th - 3.0 * s;
        }
        y += 10.0 * s;
        let (top_line, bottom_line, counting) = if self.clockwork.active() {
            (ICON_STOPWATCH_TEXT.to_string(), self.clockwork.text(), true)
        } else {
            let mut parts = self.date.split_whitespace().skip(1);
            (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string(), false)
        };
        let (dw, dh) = self.text_size(&top_line, &self.date_format);
        let color = if counting { with_alpha(accent(), alpha) } else { ink(0.8) };
        self.text(&top_line, &self.date_format, pt(cx - dw / 2.0, y), color, dw + 4.0);
        y += dh - 2.0 * s;
        let (mw, _) = self.text_size(&bottom_line, &self.small_format);
        let color = if counting { with_alpha(accent(), alpha) } else { ink(0.55) };
        self.text(&bottom_line, &self.small_format, pt(cx - mw / 2.0, y), color, mw + 4.0);
        let cy = island.bottom - 22.0 * s;
        if self.privacy_room.value > 0.01 {
            let n = (!self.privacy.microphone.is_empty()) as usize + (!self.privacy.camera.is_empty()) as usize;
            let width = (n as f32 * 7.0 + (n as f32 - 1.0).max(0.0) * 4.0) * s;
            self.draw_privacy_dots(cx + width / 2.0, cy - 20.0 * s, alpha * self.privacy_room.value.clamp(0.0, 1.0));
        }
        if m > 0.01 {
            let (bw, gap) = (3.0 * s, 2.5 * s);
            let x0 = cx - (bw * 4.0 + gap * 3.0) / 2.0;
            for (i, level) in self.bars.iter().enumerate() {
                let bh = 3.0 * s + level.clamp(0.0, 1.0) * 13.0 * s;
                let x = x0 + i as f32 * (bw + gap);
                self.fill_rounded(rect(x, cy - bh / 2.0, x + bw, cy + bh / 2.0), bw / 2.0, with_alpha(accent(), alpha * m));
            }
        }
        if m < 0.99 {
            let (tw, th) = self.text_size(&temp, &self.date_format);
            self.text(&temp, &self.date_format, pt(cx - tw / 2.0, cy - th / 2.0), ink(0.85 * (1.0 - m)), tw + 4.0);
        }
    }

    /// Album art on the left, level bars on the right of the closed island.
    fn draw_closed_music(&self, island: D2D_RECT_F, cy: f32, alpha: f32) {
        let s = self.scale;
        if let Some(track) = self.track() {
            let a = 22.0 * s;
            let r = rect(island.left + 12.0 * s, cy - a / 2.0, island.left + 12.0 * s + a, cy + a / 2.0);
            self.draw_art(track, r, 6.0 * s, alpha);
        }
        let (w, gap) = (3.0 * s, 2.5 * s);
        let total = w * 4.0 + gap * 3.0;
        let x0 = island.right - 14.0 * s - self.privacy_inset() - total;
        for (i, level) in self.bars.iter().enumerate() {
            let h = 3.0 * s + level.clamp(0.0, 1.0) * 13.0 * s;
            let x = x0 + i as f32 * (w + gap);
            self.fill_rounded(rect(x, cy - h / 2.0, x + w, cy + h / 2.0), w / 2.0, with_alpha(accent(), alpha));
        }
    }

    /// The weather on the left of the closed island.
    fn draw_closed_status(&self, island: D2D_RECT_F, cy: f32, alpha: f32) {
        let s = self.scale;
        let ink = |a: f32| ink_color(a * alpha);
        let (glyph, temp) = match &self.weather {
            Some(w) => (weather_icon(w), format!("{:.0}°", w.temperature)),
            None => (ICON_CLOUD, "--°".to_string()),
        };
        let x = island.left + 14.0 * s;
        self.weather_symbol(glyph, false, rect(x, cy - 8.0 * s, x + 16.0 * s, cy + 8.0 * s), ink(0.85));
        let (_, th) = self.text_size(&temp, &self.date_format);
        self.text(&temp, &self.date_format, pt(x + 20.0 * s, cy - th / 2.0), ink(0.85), 40.0 * s);
    }

    /// The Status tab: CPU, memory, disk, network, uptime and Pollen's own memory.
    fn draw_status(&self, island: D2D_RECT_F, alpha: f32) {
        let s = self.scale;
        let ink = |a: f32| ink_color(a * alpha);
        let left = island.left + PAD * s;
        let right = island.right - PAD * s;
        let y = island.top + BAR * s + PAD * 0.7 * s;

        let st = &self.stats;
        let ratio = |used: u64, total: u64| if total > 0 { used as f32 / total as f32 } else { 0.0 };
        let cards = [
            (ICON_CPU, t("İşlemci", "CPU").to_string(), i18n::percent(st.cpu), Some(st.cpu)),
            (
                ICON_MEMORY,
                t("Bellek", "Memory").to_string(),
                format!("{} / {}", system::format_bytes(st.memory_used), system::format_bytes(st.memory_total)),
                Some(ratio(st.memory_used, st.memory_total)),
            ),
            (
                ICON_DISK,
                "Disk (C:)".to_string(),
                format!("{} {}", system::format_bytes(st.disk_total.saturating_sub(st.disk_used)), t("boş", "free")),
                Some(ratio(st.disk_used, st.disk_total)),
            ),
            (
                ICON_DOWN,
                t("Ağ", "Network").to_string(),
                format!("↓ {}   ↑ {}", system::format_rate(st.down), system::format_rate(st.up)),
                None,
            ),
            (
                ICON_CLOCK,
                t("Açık kalma", "Uptime").to_string(),
                if crate::util::demo() { t("2 sa 14 dk", "2 h 14 min").into() } else { system::uptime() },
                None,
            ),
            (
                ICON_INFO,
                "Pollen".to_string(),
                if crate::util::demo() {
                    i18n::decimal("15.1 MB".into())
                } else {
                    system::own_memory().map(system::format_bytes).unwrap_or_default()
                },
                None,
            ),
        ];
        let col = (right - left - 10.0 * s) / 2.0;
        for (i, (glyph, title, value, fraction)) in cards.iter().enumerate() {
            let x = left + (i % 2) as f32 * (col + 10.0 * s);
            let top = y + (i / 2) as f32 * (52.0 + 10.0) * s;
            let r = rect(x, top, x + col, top + 52.0 * s);
            self.fill_rounded(r, 11.0 * s, ink(0.05));
            let icon = rect(r.left + 10.0 * s, r.top + 8.0 * s, r.left + 26.0 * s, r.top + 24.0 * s);
            self.icon(*glyph, &self.icon_format, icon, ink(0.8));
            let (_, th) = self.text_size(title, &self.title_format);
            self.text(title, &self.title_format, pt(icon.right + 6.0 * s, (icon.top + icon.bottom - th) / 2.0), ink(0.9), col);
            let (vw, vh) = self.text_size(value, &self.small_format);
            match fraction {
                Some(f) => {
                    self.text(value, &self.small_format, pt(r.right - 10.0 * s - vw, (icon.top + icon.bottom - vh) / 2.0), ink(0.6), vw + 4.0);
                    let bar = rect(r.left + 10.0 * s, r.bottom - 14.0 * s, r.right - 10.0 * s, r.bottom - 10.0 * s);
                    self.fill_rounded(bar, 2.0 * s, ink(0.12));
                    let fill = rect(bar.left, bar.top, bar.left + (bar.right - bar.left) * f.clamp(0.0, 1.0), bar.bottom);
                    if fill.right - fill.left > 0.5 {
                        let color = if *f > 0.9 { rgba(1.0, 0.45, 0.4, alpha) } else { with_alpha(accent(), alpha) };
                        self.fill_rounded(fill, 2.0 * s, color);
                    }
                }
                None => {
                    self.text(value, &self.small_format, pt(r.left + 10.0 * s, r.bottom - 10.0 * s - vh), ink(0.7), col - 20.0 * s);
                }
            }
        }
    }

    /// The Weather tab: now, the coming hours and the coming days.
    fn draw_weather(&self, island: D2D_RECT_F, alpha: f32) {
        let s = self.scale;
        let ink = |a: f32| ink_color(a * alpha);
        let left = island.left + PAD * s;
        let right = island.right - PAD * s;
        let y = island.top + BAR * s + PAD * 0.7 * s;
        let card = rect(left, y, right, y + 64.0 * s);
        let Some(w) = &self.weather else {
            let text = t("Hava durumu alınıyor…", "Getting the weather…");
            let (tw, th) = self.text_size(text, &self.title_format);
            self.text(text, &self.title_format, pt((left + right - tw) / 2.0, (card.top + card.bottom - th) / 2.0), ink(0.5), tw + 4.0);
            return;
        };

        // Now.
        self.fill_rounded(card, 12.0 * s, ink(0.05));
        let icon = rect(card.left + 10.0 * s, card.top + 10.0 * s, card.left + 54.0 * s, card.bottom - 10.0 * s);
        self.weather_symbol(weather_icon(w), true, icon, ink(0.9));
        let temp = format!("{:.0}°", w.temperature);
        let (tw, th) = self.text_size(&temp, &self.big_format);
        let tx = icon.right + 6.0 * s;
        let cy = (card.top + card.bottom) / 2.0;
        self.text(&temp, &self.big_format, pt(tx, cy - th / 2.0), ink(0.95), tw + 4.0);
        let x = tx + tw + 14.0 * s;
        let condition = w.condition();
        let (_, ch) = self.text_size(condition, &self.title_format);
        let (_, nh) = self.text_size(&w.city, &self.small_format);
        let top = cy - (ch + nh) / 2.0;
        self.text(condition, &self.title_format, pt(x, top), ink(0.9), card.right - x - 90.0 * s);
        self.text(&w.city, &self.small_format, pt(x, top + ch), ink(0.5), card.right - x - 90.0 * s);
        if w.high.is_finite() && w.low.is_finite() {
            let range = format!("↑ {:.0}°   ↓ {:.0}°", w.high, w.low);
            let (rw, rh) = self.text_size(&range, &self.title_format);
            self.text(&range, &self.title_format, pt(card.right - 14.0 * s - rw, cy - rh / 2.0), ink(0.7), rw + 4.0);
        }

        // The coming hours.
        let card = rect(left, card.bottom + 10.0 * s, right, card.bottom + 82.0 * s);
        self.fill_rounded(card, 12.0 * s, ink(0.05));
        let n = w.hours.len().max(1) as f32;
        let col = (card.right - card.left) / n;
        for (i, h) in w.hours.iter().enumerate() {
            let cx = card.left + col * (i as f32 + 0.5);
            let label = format!("{:02}:00", h.hour);
            let (lw, _) = self.text_size(&label, &self.small_format);
            self.text(&label, &self.small_format, pt(cx - lw / 2.0, card.top + 8.0 * s), ink(0.5), lw + 4.0);
            let g = rect(cx - 9.0 * s, card.top + 27.0 * s, cx + 9.0 * s, card.top + 45.0 * s);
            self.weather_symbol(weather_glyph(h.code, h.day), false, g, ink(0.85));
            let t = format!("{:.0}°", h.temperature);
            let (tw, th) = self.text_size(&t, &self.title_format);
            self.text(&t, &self.title_format, pt(cx - tw / 2.0, card.bottom - 8.0 * s - th), ink(0.92), tw + 4.0);
        }

        // The coming days: name, sky, low, a bar across the week's range, high.
        if w.days.is_empty() {
            return;
        }
        let card = rect(left, card.bottom + 10.0 * s, right, card.bottom + 10.0 * s + (w.days.len() as f32 * DAY_ROW + 12.0) * s);
        self.fill_rounded(card, 12.0 * s, ink(0.05));
        let lowest = w.days.iter().map(|d| d.low).filter(|v| v.is_finite()).fold(f64::INFINITY, f64::min);
        let highest = w.days.iter().map(|d| d.high).filter(|v| v.is_finite()).fold(f64::NEG_INFINITY, f64::max);
        let span = (highest - lowest).max(1.0);
        let bar_left = card.left + 170.0 * s;
        let bar_right = card.right - 52.0 * s;
        for (i, d) in w.days.iter().enumerate() {
            let cy = card.top + 6.0 * s + (i as f32 + 0.5) * DAY_ROW * s;
            let name = if i == 0 {
                t("Yarın", "Tomorrow").to_string()
            } else {
                i18n::weekdays()[weekday_monday0(d.date.0, d.date.1, d.date.2) as usize].to_string()
            };
            let (_, nh) = self.text_size(&name, &self.title_format);
            self.text(&name, &self.title_format, pt(card.left + 14.0 * s, cy - nh / 2.0), ink(0.9), 70.0 * s);
            let g = rect(card.left + 84.0 * s, cy - 9.0 * s, card.left + 102.0 * s, cy + 9.0 * s);
            self.weather_symbol(weather_glyph(d.code, true), false, g, ink(0.85));
            if !(d.low.is_finite() && d.high.is_finite()) {
                continue;
            }
            let low = format!("{:.0}°", d.low);
            let (lw, lh) = self.text_size(&low, &self.title_format);
            self.text(&low, &self.title_format, pt(bar_left - 12.0 * s - lw, cy - lh / 2.0), ink(0.55), lw + 4.0);
            let high = format!("{:.0}°", d.high);
            let (_, hh) = self.text_size(&high, &self.title_format);
            self.text(&high, &self.title_format, pt(bar_right + 12.0 * s, cy - hh / 2.0), ink(0.92), 40.0 * s);
            let track = rect(bar_left, cy - 2.0 * s, bar_right, cy + 2.0 * s);
            self.fill_rounded(track, 2.0 * s, ink(0.10));
            let at = |v: f64| bar_left + (bar_right - bar_left) * ((v - lowest) / span) as f32;
            let fill = rect(at(d.low), track.top, at(d.high).max(at(d.low) + 4.0 * s), track.bottom);
            self.fill_rounded(fill, 2.0 * s, with_alpha(accent(), alpha));
        }
    }

    /// The Timer tab: the big readout and what it is counting.
    fn draw_timer(&self, island: D2D_RECT_F, alpha: f32) {
        let s = self.scale;
        let ink = |a: f32| ink_color(a * alpha);
        let mid = (island.left + island.right) / 2.0;
        let y = island.top + BAR * s + PAD * 0.7 * s + (CHOICE_HEIGHT + 14.0) * s;
        let text = self.clockwork.text();
        let (tw, th) = self.text_size(&text, &self.clock_format);
        let color = if self.clockwork.running() { with_alpha(accent(), alpha) } else { ink(0.95) };
        self.text(&text, &self.clock_format, pt(mid - tw / 2.0, y + (56.0 * s - th) / 2.0), color, tw + 8.0);
        let note = match self.clockwork.mode {
            ClockMode::Pomodoro if self.clockwork.focus => format!("{} · {}/4", t("Odaklan", "Focus"), self.clockwork.round),
            ClockMode::Pomodoro => t("Mola", "Break").to_string(),
            ClockMode::Timer => t("Bitince notch haber verir", "The notch tells you when it is done").to_string(),
            ClockMode::Stopwatch => String::new(),
        };
        let (nw, _) = self.text_size(&note, &self.small_format);
        self.text(&note, &self.small_format, pt(mid - nw / 2.0, y + 58.0 * s), ink(0.5), nw + 4.0);
    }

    /// The Calendar tab: a Monday-first month grid with today marked.
    /// The Settings tab's row titles and a note; the options are controls.
    fn draw_settings(&self, island: D2D_RECT_F, alpha: f32) {
        let s = self.scale;
        let ink = |a: f32| ink_color(a * alpha);
        let left = island.left + PAD * s;
        let mut y = island.top + BAR * s + PAD * 0.7 * s;
        for choice in Choice::ALL {
            let cy = y + CHOICE_HEIGHT * s / 2.0;
            self.icon(choice.icon(), &self.icon_format, rect(left, cy - 9.0 * s, left + 18.0 * s, cy + 9.0 * s), ink(0.8));
            let (_, th) = self.text_size(choice.title(), &self.title_format);
            let x = left + 24.0 * s;
            self.text(choice.title(), &self.title_format, pt(x, cy - th / 2.0), ink(0.9), LABEL_WIDTH * s - 30.0 * s);
            y += (CHOICE_HEIGHT + 10.0) * s;
        }
        let cy = y + CHOICE_HEIGHT * s / 2.0;
        self.icon(ICON_PLACE, &self.icon_format, rect(left, cy - 9.0 * s, left + 18.0 * s, cy + 9.0 * s), ink(0.8));
        let place_title = t("Hava konumu", "Weather place");
        let (_, th) = self.text_size(place_title, &self.title_format);
        self.text(place_title, &self.title_format, pt(left + 24.0 * s, cy - th / 2.0), ink(0.9), LABEL_WIDTH * s - 30.0 * s);
        y += (CHOICE_HEIGHT + 6.0) * s;
        if let Some(edit) = &self.city_edit {
            if edit.results.is_empty() && !edit.text.trim().is_empty() {
                let status = if edit.searching { t("Aranıyor…", "Searching…") } else { t("Bulunamadı", "Nothing found") };
                let (_, sh) = self.text_size(status, &self.small_format);
                let x = left + LABEL_WIDTH * s + 12.0 * s;
                self.text(status, &self.small_format, pt(x, y + (RESULT_HEIGHT * s - sh) / 2.0), ink(0.5), 200.0 * s);
            }
        }
        y += (self.city_list_height() + 4.0) * s;
        let cy = y + CHOICE_HEIGHT * s / 2.0;
        self.icon(ICON_THEME, &self.icon_format, rect(left, cy - 9.0 * s, left + 18.0 * s, cy + 9.0 * s), ink(0.8));
        // With the theme's name when it fits.
        let room = LABEL_WIDTH * s - 30.0 * s;
        let named = format!("{} · {}", t("Tema", "Theme"), self.settings.theme.label());
        let title = if self.text_size(&named, &self.title_format).0 <= room { named } else { t("Tema", "Theme").to_string() };
        let (_, th) = self.text_size(&title, &self.title_format);
        self.text(&title, &self.title_format, pt(left + 24.0 * s, cy - th / 2.0), ink(0.9), LABEL_WIDTH * s - 30.0 * s);
        y += (CHOICE_HEIGHT + 10.0) * s;
        y += (SLIDER_HEIGHT + 16.0) * s;
        match crate::update::available() {
            Some(release) => {
                let hot = self.hovered_control() == Some(Control::Update);
                let note = format!("↓ Pollen {} {}", release.version, t("hazır · kurmak için tıkla", "is ready · click to install"));
                let (_, nh) = self.text_size(&note, &self.small_format);
                let color = if hot { ink(1.0) } else { with_alpha(accent(), alpha) };
                self.text(&note, &self.small_format, pt(left, y + 15.0 * s - nh / 2.0), color, 245.0 * s);
            }
            None => {
                let note = format!("Pollen {} · {}", crate::install::VERSION, t("ayarlar kendiliğinden kaydedilir", "settings save themselves"));
                let (_, nh) = self.text_size(&note, &self.small_format);
                self.text(&note, &self.small_format, pt(left, y + 15.0 * s - nh / 2.0), ink(0.4), 245.0 * s);
            }
        }
    }

    fn draw_calendar(&self, island: D2D_RECT_F, alpha: f32) {
        let s = self.scale;
        let ink = |a: f32| ink_color(a * alpha);
        let hovered = self.hovered_control();
        let left = island.left + PAD * s;
        let right = island.right - PAD * s;
        let mut y = island.top + BAR * s + PAD * 0.7 * s;
        let (year, month) = self.month;

        for (control, glyph) in [(Control::CalendarPrevious, ICON_LEFT), (Control::CalendarNext, ICON_RIGHT)] {
            let r = if control == Control::CalendarPrevious {
                rect(left, y, left + 28.0 * s, y + 28.0 * s)
            } else {
                rect(right - 28.0 * s, y, right, y + 28.0 * s)
            };
            let hot = hovered == Some(control);
            if hot {
                self.fill_rounded(r, 14.0 * s, ink(0.10));
            }
            self.icon(glyph, &self.icon_format, r, ink(if hot { 1.0 } else { 0.7 }));
        }
        let title = format!("{} {}", i18n::months()[(month - 1) as usize], year);
        let (tw, th) = self.text_size(&title, &self.track_format);
        let current = (year, month) == (self.today.0, self.today.1);
        let title_alpha = if hovered == Some(Control::CalendarTitle) && !current { 1.0 } else { 0.95 };
        self.text(&title, &self.track_format, pt((left + right - tw) / 2.0, y + 14.0 * s - th / 2.0), ink(title_alpha), tw + 4.0);
        y += 34.0 * s;

        let cell = (right - left) / 7.0;
        for (i, day) in i18n::weekdays().iter().enumerate() {
            let (w, h) = self.text_size(day, &self.small_format);
            let x = left + i as f32 * cell + (cell - w) / 2.0;
            self.text(day, &self.small_format, pt(x, y + (18.0 * s - h) / 2.0), ink(if i >= 5 { 0.4 } else { 0.55 }), w + 4.0);
        }
        y += 18.0 * s;

        let first = weekday_monday0(year, month, 1);
        let days = days_in_month(year, month);
        let (py, pm) = if month == 1 { (year - 1, 12) } else { (year, month - 1) };
        let previous_days = days_in_month(py, pm);
        for row in 0..6 {
            for col in 0..7 {
                let index = row * 7 + col;
                let n = index as i32 - first as i32 + 1;
                let (label, inside) = if n < 1 {
                    ((previous_days as i32 + n) as u32, false)
                } else if n as u32 > days {
                    (n as u32 - days, false)
                } else {
                    (n as u32, true)
                };
                let cx = left + col as f32 * cell + cell / 2.0;
                let cy = y + row as f32 * 28.0 * s + 14.0 * s;
                let today = inside && (year, month, label) == self.today;
                let text = label.to_string();
                let (w, h) = self.text_size(&text, &self.title_format);
                if today {
                    let r = 12.0 * s;
                    self.fill_rounded(rect(cx - r, cy - r, cx + r, cy + r), r, with_alpha(accent(), alpha));
                }
                let color = if today {
                    with_alpha(on_accent(), alpha)
                } else if !inside {
                    ink(0.22)
                } else if col >= 5 {
                    ink(0.6)
                } else {
                    ink(0.9)
                };
                self.text(&text, &self.title_format, pt(cx - w / 2.0, cy - h / 2.0), color, w + 4.0);
            }
        }
    }

    /// The track's art with rounded corners, or a note on a tile without art.
    fn draw_art(&self, track: &media::Track, r: D2D_RECT_F, radius: f32, alpha: f32) {
        let Some(bitmap) = self.art.get(&track.key) else {
            self.fill_rounded(r, radius, ink_color(0.08 * alpha));
            self.icon(ICON_MUSIC, &self.icon_format, r, ink_color(0.6 * alpha));
            return;
        };
        unsafe {
            let size = bitmap.GetSize();
            let props = D2D1_BITMAP_BRUSH_PROPERTIES {
                extendModeX: D2D1_EXTEND_MODE_CLAMP,
                extendModeY: D2D1_EXTEND_MODE_CLAMP,
                interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            };
            let Ok(brush) = self.surface.rt.CreateBitmapBrush(bitmap, Some(&props), None) else { return };
            // Cover the box: scale to fill, centered.
            let k = ((r.right - r.left) / size.width).max((r.bottom - r.top) / size.height);
            let dx = r.left + ((r.right - r.left) - size.width * k) / 2.0;
            let dy = r.top + ((r.bottom - r.top) - size.height * k) / 2.0;
            brush.SetTransform(&Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: dx, M32: dy });
            brush.SetOpacity(alpha);
            self.surface
                .rt
                .FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &brush);
        }
    }

    /// The open Music tab's art and text (its buttons are drawn as controls).
    fn draw_music(&self, island: D2D_RECT_F, alpha: f32) {
        let s = self.scale;
        let ink = |a: f32| ink_color(a * alpha);
        let left = island.left + PAD * s;
        let right = island.right - PAD * s;
        let mut y = island.top + BAR * s + PAD * 0.7 * s;
        if self.media.tracks.len() > 1 {
            y += 30.0 * s;
        }
        let Some(track) = self.track() else {
            let text = t("Şu an çalan bir şey yok", "Nothing is playing");
            let (w, h) = self.text_size(text, &self.title_format);
            let cy = island.top + BAR * s + 64.0 * s / 2.0;
            self.text(text, &self.title_format, pt((left + right - w) / 2.0, cy - h / 2.0), ink(0.55), w + 4.0);
            return;
        };
        let art = rect(left, y, left + ART * s, y + ART * s);
        self.draw_art(track, art, 10.0 * s, alpha);

        let x = art.right + 14.0 * s;
        let limit = right - x;
        let (_, th) = self.text_size(&track.title, &self.track_format);
        let (_, ah) = self.text_size(&track.artist, &self.title_format);
        let (_, nh) = self.text_size(&track.app_name, &self.small_format);
        let top = y + (ART * s - th - ah - nh - 6.0 * s) / 2.0;
        self.text(&track.title, &self.track_format, pt(x, top), ink(0.95), limit);
        self.text(&track.artist, &self.title_format, pt(x, top + th + 2.0 * s), ink(0.65), limit);
        self.text(&track.app_name, &self.small_format, pt(x, top + th + ah + 6.0 * s), ink(0.4), limit);

        // Times beside the progress bar.
        let bar_y = y + (ART + 14.0) * s;
        let elapsed = format_time(track.position_now());
        let total = format_time(track.duration_ms);
        let (ew, eh) = self.text_size(&elapsed, &self.small_format);
        let (tw, _) = self.text_size(&total, &self.small_format);
        let cy = bar_y + 8.0 * s;
        self.text(&elapsed, &self.small_format, pt(left + 34.0 * s - ew, cy - eh / 2.0), ink(0.55), ew + 4.0);
        self.text(&total, &self.small_format, pt(right - 34.0 * s, cy - eh / 2.0), ink(0.55), tw + 4.0);
    }

    fn hovered_control(&self) -> Option<Control> {
        self.mouse.and_then(|(x, y)| self.hit(x, y))
    }

    fn draw_panel(&self, island: D2D_RECT_F, alpha: f32) {
        let s = self.scale;
        let ink = |a: f32| ink_color(a * alpha);
        let hovered = self.hovered_control();
        let cy = island.top + BAR * s / 2.0;

        // Top row: time, tabs, date.
        let (_, th) = self.text_size(&self.time, &self.time_format);
        self.text(&self.time, &self.time_format, pt(island.left + 16.0 * s, cy - th / 2.0), ink(0.95), 80.0 * s);
        if self.privacy.any() {
            // Who is listening or watching, in place of the date.
            let mut names: Vec<&str> = self.privacy.camera.iter().map(String::as_str).collect();
            for n in &self.privacy.microphone {
                if !names.contains(&n.as_str()) {
                    names.push(n);
                }
            }
            let label = match names.len() {
                0 | 1 => names.join(""),
                n => format!("{} +{}", names[0], n - 1),
            };
            let dots_left = self.draw_privacy_dots(island.right - 16.0 * s, cy, alpha);
            let (lw, lh) = self.text_size(&label, &self.date_format);
            // Between the tabs and the dots.
            let tabs_right = (island.left + island.right) / 2.0 + TAB_WIDTH * s * TABS.len() as f32 / 2.0;
            let limit = (dots_left - 4.0 * s - tabs_right - 8.0 * s).max(0.0);
            let x = dots_left - 4.0 * s - lw.min(limit);
            self.text(&label, &self.date_format, pt(x, cy - lh / 2.0), ink(0.8), limit);
        } else {
            let (dw, dh) = self.text_size(&self.date, &self.date_format);
            self.text(&self.date, &self.date_format, pt(island.right - 16.0 * s - dw, cy - dh / 2.0), ink(0.65), dw + 4.0);
        }

        match self.tab {
            SETTINGS_TAB => self.draw_settings(island, alpha),
            MUSIC_TAB => self.draw_music(island, alpha),
            STATUS_TAB => self.draw_status(island, alpha),
            WEATHER_TAB => self.draw_weather(island, alpha),
            CALENDAR_TAB => self.draw_calendar(island, alpha),
            TIMER_TAB => self.draw_timer(island, alpha),
            _ => {}
        }
        for (r, control) in self.controls(island) {
            match control {
                Control::Player(app) => {
                    let selected = self.track().is_some_and(|t| t.app == app);
                    let hot = hovered == Some(Control::Player(app.clone()));
                    let bg = if selected { 0.16 } else if hot { 0.10 } else { 0.05 };
                    self.fill_rounded(r, 11.0 * s, ink(bg));
                    let name = self.media.tracks.iter().find(|t| t.app == app).map(|t| t.app_name.clone()).unwrap_or_default();
                    let (w, h) = self.text_size(&name, &self.small_format);
                    self.text(
                        &name,
                        &self.small_format,
                        pt((r.left + r.right - w) / 2.0, (r.top + r.bottom - h) / 2.0),
                        ink(if selected { 0.95 } else { 0.6 }),
                        r.right - r.left,
                    );
                }
                Control::CalendarPrevious | Control::CalendarNext | Control::CalendarTitle => {}
                Control::Progress => {
                    if let Some(track) = self.track() {
                        let hot = hovered == Some(Control::Progress);
                        let thickness = if hot { 6.0 } else { 4.0 } * s;
                        let cy = (r.top + r.bottom) / 2.0;
                        let bar = rect(r.left, cy - thickness / 2.0, r.right, cy + thickness / 2.0);
                        self.fill_rounded(bar, thickness / 2.0, ink(0.15));
                        if track.duration_ms > 0 {
                            let f = track.position_now() as f32 / track.duration_ms as f32;
                            let fill = rect(bar.left, bar.top, bar.left + (bar.right - bar.left) * f.clamp(0.0, 1.0), bar.bottom);
                            if fill.right - fill.left > 0.5 {
                                self.fill_rounded(fill, thickness / 2.0, ink(0.9));
                            }
                        }
                    }
                }
                Control::Media(button) => {
                    let hot = hovered == Some(Control::Media(button));
                    let pressed = self.pressed == Some(Control::Media(button));
                    let playing = self.track().is_some_and(|t| t.playing);
                    let (glyph, format) = match button {
                        MediaButton::Previous => (ICON_PREVIOUS, &self.icon_format),
                        MediaButton::Next => (ICON_NEXT, &self.icon_format),
                        MediaButton::PlayPause => (if playing { ICON_PAUSE } else { ICON_PLAY }, &self.play_format),
                    };
                    let k = if pressed { 0.92 } else { 1.0 };
                    let b = rect(
                        (r.left + r.right) / 2.0 - (r.right - r.left) / 2.0 * k,
                        (r.top + r.bottom) / 2.0 - (r.bottom - r.top) / 2.0 * k,
                        (r.left + r.right) / 2.0 + (r.right - r.left) / 2.0 * k,
                        (r.top + r.bottom) / 2.0 + (r.bottom - r.top) / 2.0 * k,
                    );
                    if button == MediaButton::PlayPause {
                        self.fill_rounded(b, (b.right - b.left) / 2.0, ink(if hot { 1.0 } else { 0.92 }));
                        let (sr, sg, sb) = surface();
                        self.icon(glyph, format, b, rgba(sr, sg, sb, alpha));
                    } else {
                        if hot {
                            self.fill_rounded(b, (b.right - b.left) / 2.0, ink(0.10));
                        }
                        self.icon(glyph, format, b, ink(if hot { 1.0 } else { 0.8 }));
                    }
                }
                Control::Tab(i) => {
                    let (glyph, available) = TABS[i];
                    let active = i == self.tab;
                    let a = if active {
                        1.0
                    } else if !available {
                        0.25
                    } else if hovered == Some(Control::Tab(i)) {
                        0.85
                    } else {
                        0.55
                    };
                    self.icon(glyph, &self.tab_icon_format, rect(r.left, r.top, r.right, r.bottom - 2.0 * s), ink(a));
                    if active {
                        let mid = (r.left + r.right) / 2.0;
                        self.fill_rounded(
                            rect(mid - 7.0 * s, r.bottom - 5.0 * s, mid + 7.0 * s, r.bottom - 2.0 * s),
                            1.5 * s,
                            with_alpha(accent(), alpha),
                        );
                    }
                }
                Control::Tile(tile) => self.draw_tile(r, tile, hovered == Some(Control::Tile(tile)), alpha),
                Control::Button(button) => {
                    let hot = hovered == Some(Control::Button(button));
                    let pressed = self.pressed == Some(Control::Button(button));
                    let bg = if pressed { 0.16 } else if hot { 0.10 } else { 0.05 };
                    let lift = if hot && !pressed { 1.0 * s } else { 0.0 };
                    let b = rect(r.left, r.top - lift, r.right, r.bottom - lift);
                    self.fill_rounded(b, BUTTON * s / 2.0, ink(bg));
                    self.icon(button.icon(), &self.icon_format, b, ink(if hot { 1.0 } else { 0.75 }));
                }
                Control::SliderIcon(slider) => {
                    let glyph = match &slider {
                        Slider::Volume if self.output.as_ref().is_some_and(|o| o.muted) => ICON_MUTE,
                        Slider::Volume => ICON_VOLUME,
                        Slider::App if self.app_volume.is_some_and(|a| a.muted) => ICON_MUTE,
                        Slider::App => ICON_VOLUME,
                        Slider::Brightness(_) => ICON_BRIGHTNESS,
                        Slider::Opacity => ICON_COLOR,
                    };
                    let hot = hovered == Some(Control::SliderIcon(slider.clone())) && matches!(slider, Slider::Volume | Slider::App);
                    self.icon(glyph, &self.icon_format, r, ink(if hot { 1.0 } else { 0.8 }));
                    let name = match &slider {
                        Slider::Volume => t("Ses", "Volume").to_string(),
                        Slider::App => self.track().map(|t| t.app_name.clone()).unwrap_or_default(),
                        Slider::Opacity => t("Opaklık", "Opacity").to_string(),
                        Slider::Brightness(device) => {
                            self.displays.iter().find(|d| &d.device == device).map(|d| d.name.clone()).unwrap_or_default()
                        }
                    };
                    let (_, nh) = self.text_size(&name, &self.title_format);
                    let x = r.right + 6.0 * s;
                    let limit = island.left + PAD * s + LABEL_WIDTH * s - 8.0 * s - x;
                    self.text(&name, &self.title_format, pt(x, (r.top + r.bottom - nh) / 2.0), ink(0.9), limit);
                }
                Control::Slider(slider) => self.draw_slider(r, &slider, hovered == Some(Control::Slider(slider.clone())), alpha),
                Control::Theme(i) => {
                    let theme = settings::Theme::ALL[i];
                    let palette = theme.palette();
                    let selected = self.settings.theme == theme;
                    let hot = hovered == Some(Control::Theme(i));
                    let d = r.right - r.left;
                    if selected || hot {
                        let ring = rect(r.left - 3.0 * s, r.top - 3.0 * s, r.right + 3.0 * s, r.bottom + 3.0 * s);
                        unsafe {
                            self.brush.SetColor(&ink(if selected { 0.9 } else { 0.35 }));
                            self.surface.rt.DrawRoundedRectangle(
                                &D2D1_ROUNDED_RECT { rect: ring, radiusX: d / 2.0 + 3.0 * s, radiusY: d / 2.0 + 3.0 * s },
                                &self.brush,
                                1.5 * s,
                                None,
                            );
                        }
                    }
                    // The accent around a dot of the background tint.
                    let (ar, ag, ab) = palette.accent;
                    self.fill_rounded(r, d / 2.0, rgba(ar, ag, ab, alpha));
                    let (sr, sg, sb) = palette.surface;
                    let inner = rect(r.left + 5.0 * s, r.top + 5.0 * s, r.right - 5.0 * s, r.bottom - 5.0 * s);
                    self.fill_rounded(inner, d / 2.0 - 5.0 * s, rgba(sr, sg, sb, alpha));
                }
                Control::Choice(choice, i) => {
                    let n = choice.options(&self.settings).len();
                    let selected = choice.selected(&self.settings, self.autostart) == i;
                    let hot = hovered == Some(Control::Choice(choice, i));
                    let radius = (r.bottom - r.top) / 2.0;
                    // The row's track once, behind its first option.
                    if i == 0 {
                        let track = rect(r.left, r.top, r.left + (r.right - r.left) * n as f32, r.bottom);
                        self.fill_rounded(track, radius, ink(0.06));
                    }
                    let inner = rect(r.left + 2.0 * s, r.top + 2.0 * s, r.right - 2.0 * s, r.bottom - 2.0 * s);
                    if selected {
                        self.fill_rounded(inner, radius - 2.0 * s, with_alpha(accent(), alpha));
                    } else if hot {
                        self.fill_rounded(inner, radius - 2.0 * s, ink(0.08));
                    }
                    let label = choice.options(&self.settings)[i];
                    let (lw, lh) = self.text_size(label, &self.title_format);
                    let color = if selected { with_alpha(on_accent(), alpha) } else { ink(if hot { 0.95 } else { 0.7 }) };
                    self.text(label, &self.title_format, pt((r.left + r.right - lw) / 2.0, (r.top + r.bottom - lh) / 2.0), color, lw + 4.0);
                }
                Control::CityField => {
                    let editing = self.city_edit.is_some();
                    let hot = hovered == Some(Control::CityField);
                    let radius = (r.bottom - r.top) / 2.0;
                    self.fill_rounded(r, radius, ink(if editing { 0.10 } else if hot { 0.09 } else { 0.06 }));
                    if editing {
                        let edge = rect(r.left + 0.75 * s, r.top + 0.75 * s, r.right - 0.75 * s, r.bottom - 0.75 * s);
                        unsafe {
                            self.brush.SetColor(&with_alpha(accent(), alpha));
                            self.surface.rt.DrawRoundedRectangle(
                                &D2D1_ROUNDED_RECT { rect: edge, radiusX: radius, radiusY: radius },
                                &self.brush,
                                1.5 * s,
                                None,
                            );
                        }
                    }
                    let x = r.left + 12.0 * s;
                    let cy = (r.top + r.bottom) / 2.0;
                    match &self.city_edit {
                        Some(edit) => {
                            let (tw, th) = self.text_size(&edit.text, &self.title_format);
                            if edit.text.is_empty() {
                                let hint = t("Şehir ara…", "Search for a city…");
                                let (_, ph) = self.text_size(hint, &self.title_format);
                                self.text(hint, &self.title_format, pt(x + 3.0 * s, cy - ph / 2.0), ink(0.4), r.right - x);
                            } else {
                                self.text(&edit.text, &self.title_format, pt(x, cy - th / 2.0), ink(0.95), r.right - x - 8.0 * s);
                            }
                            let cx = (x + tw + 1.0 * s).min(r.right - 10.0 * s);
                            self.fill_rounded(rect(cx, cy - 7.0 * s, cx + 1.5 * s, cy + 7.0 * s), 0.75 * s, with_alpha(accent(), alpha));
                        }
                        None => {
                            let label = match (&self.place, &self.weather) {
                                (Some(p), _) => p.name.clone(),
                                (None, Some(w)) if !w.city.is_empty() => format!("{} ({})", w.city, t("otomatik", "automatic")),
                                _ => t("Otomatik", "Automatic").to_string(),
                            };
                            let limit = if self.place.is_some() { r.right - 84.0 * s - x } else { r.right - 12.0 * s - x };
                            let (_, th) = self.text_size(&label, &self.title_format);
                            self.text(&label, &self.title_format, pt(x, cy - th / 2.0), ink(0.9), limit);
                        }
                    }
                }
                Control::CityAuto => {
                    let hot = hovered == Some(Control::CityAuto);
                    self.fill_rounded(r, (r.bottom - r.top) / 2.0, ink(if hot { 0.18 } else { 0.09 }));
                    let auto = t("Otomatik", "Automatic");
                    let (lw, lh) = self.text_size(auto, &self.small_format);
                    self.text(
                        auto,
                        &self.small_format,
                        pt((r.left + r.right - lw) / 2.0, (r.top + r.bottom - lh) / 2.0),
                        ink(if hot { 1.0 } else { 0.75 }),
                        lw + 4.0,
                    );
                }
                Control::CityResult(i) => {
                    let Some(place) = self.city_edit.as_ref().and_then(|e| e.results.get(i)) else { continue };
                    let hot = hovered == Some(Control::CityResult(i));
                    if hot || i == 0 {
                        self.fill_rounded(r, 8.0 * s, ink(if hot { 0.10 } else { 0.05 }));
                    }
                    let x = r.left + 12.0 * s;
                    let cy = (r.top + r.bottom) / 2.0;
                    let (nw, nh) = self.text_size(&place.name, &self.title_format);
                    let name_limit = (r.right - x) * 0.55;
                    self.text(&place.name, &self.title_format, pt(x, cy - nh / 2.0), ink(0.95), name_limit);
                    let rx = x + nw.min(name_limit) + 8.0 * s;
                    let (_, rh) = self.text_size(&place.region, &self.small_format);
                    self.text(&place.region, &self.small_format, pt(rx, cy - rh / 2.0), ink(0.5), r.right - 8.0 * s - rx);
                }
                Control::Update | Control::Grip => {}
                Control::ClockMode(i) => {
                    let selected = self.clockwork.mode == ClockMode::ALL[i];
                    let hot = hovered == Some(Control::ClockMode(i));
                    let radius = (r.bottom - r.top) / 2.0;
                    if i == 0 {
                        let track = rect(r.left, r.top, r.left + (r.right - r.left) * ClockMode::ALL.len() as f32, r.bottom);
                        self.fill_rounded(track, radius, ink(0.06));
                    }
                    let inner = rect(r.left + 2.0 * s, r.top + 2.0 * s, r.right - 2.0 * s, r.bottom - 2.0 * s);
                    if selected {
                        self.fill_rounded(inner, radius - 2.0 * s, with_alpha(accent(), alpha));
                    } else if hot {
                        self.fill_rounded(inner, radius - 2.0 * s, ink(0.08));
                    }
                    let label = ClockMode::ALL[i].label();
                    let (lw, lh) = self.text_size(label, &self.title_format);
                    let color = if selected { with_alpha(on_accent(), alpha) } else { ink(if hot { 0.95 } else { 0.7 }) };
                    self.text(label, &self.title_format, pt((r.left + r.right - lw) / 2.0, (r.top + r.bottom - lh) / 2.0), color, lw + 4.0);
                }
                Control::ClockPreset(minutes) => {
                    let chosen = self.clockwork.length == Duration::from_secs(minutes as u64 * 60);
                    let hot = hovered == Some(Control::ClockPreset(minutes));
                    let bg = if chosen { 0.16 } else if hot { 0.10 } else { 0.05 };
                    self.fill_rounded(r, (r.bottom - r.top) / 2.0, ink(bg));
                    let label = format!("{minutes} {}", t("dk", "min"));
                    let (lw, lh) = self.text_size(&label, &self.small_format);
                    self.text(&label, &self.small_format, pt((r.left + r.right - lw) / 2.0, (r.top + r.bottom - lh) / 2.0), ink(if chosen { 0.95 } else { 0.7 }), lw + 4.0);
                }
                Control::ClockStart | Control::ClockReset => {
                    let start = control == Control::ClockStart;
                    let hot = hovered == Some(control.clone());
                    let pressed = self.pressed == Some(control.clone());
                    let radius = (r.bottom - r.top) / 2.0;
                    let label = if !start {
                        t("Sıfırla", "Reset")
                    } else if self.clockwork.running() {
                        t("Duraklat", "Pause")
                    } else if self.clockwork.active() {
                        t("Devam", "Resume")
                    } else {
                        t("Başlat", "Start")
                    };
                    let (lw, lh) = self.text_size(label, &self.title_format);
                    let text_at = pt((r.left + r.right - lw) / 2.0, (r.top + r.bottom - lh) / 2.0);
                    if start {
                        let k = if pressed { 0.85 } else if hot { 0.92 } else { 1.0 };
                        self.fill_rounded(r, radius, with_alpha(accent(), alpha * k));
                        self.text(label, &self.title_format, text_at, with_alpha(on_accent(), alpha), lw + 4.0);
                    } else {
                        self.fill_rounded(r, radius, ink(if pressed { 0.16 } else if hot { 0.11 } else { 0.06 }));
                        self.text(label, &self.title_format, text_at, ink(0.85), lw + 4.0);
                    }
                }
                Control::Quit => {
                    let hot = hovered == Some(Control::Quit);
                    let pressed = self.pressed == Some(Control::Quit);
                    let bg = if pressed { 0.16 } else if hot { 0.11 } else { 0.06 };
                    self.fill_rounded(r, 15.0 * s, ink(bg));
                    let label = t("Pollen'dan çık", "Quit Pollen");
                    let (lw, lh) = self.text_size(label, &self.title_format);
                    let iw = 16.0 * s;
                    let x = (r.left + r.right - iw - 6.0 * s - lw) / 2.0;
                    let cy = (r.top + r.bottom) / 2.0;
                    self.icon(ICON_POWER, &self.icon_format, rect(x, cy - 8.0 * s, x + iw, cy + 8.0 * s), ink(if hot { 1.0 } else { 0.8 }));
                    self.text(label, &self.title_format, pt(x + iw + 6.0 * s, cy - lh / 2.0), ink(if hot { 1.0 } else { 0.85 }), lw + 4.0);
                }
            }
        }

        // The hovered settings option's explanation, under its row.
        if let Some(Control::Choice(choice, i)) = hovered.clone() {
            let r = self
                .controls(island)
                .into_iter()
                .find(|(_, c)| c == &Control::Choice(choice, i))
                .map(|(r, _)| r);
            if let (Some(r), Some(hint)) = (r, choice.hint(i)) {
                self.draw_hint(hint, r, island, alpha);
            }
        }

        // The hovered button's name, under its row.
        if let Some(Control::Button(button)) = &hovered {
            let r = self
                .controls(island)
                .into_iter()
                .find(|(_, c)| c == &Control::Button(*button))
                .map(|(r, _)| r);
            if let Some(r) = r {
                let label = button.label();
                let (lw, lh) = self.text_size(label, &self.small_format);
                let (pad_x, pad_y) = (8.0 * s, 3.0 * s);
                let cx = (r.left + r.right) / 2.0;
                let left = (cx - lw / 2.0 - pad_x).min(island.right - 6.0 * s - lw - pad_x * 2.0).max(island.left + 6.0 * s);
                let top = r.bottom + 5.0 * s;
                let bubble = rect(left, top, left + lw + pad_x * 2.0, top + lh + pad_y * 2.0);
                self.fill_rounded(bubble, 6.0 * s, rgba(0.0, 0.0, 0.0, 0.8 * alpha));
                self.text(label, &self.small_format, pt(left + pad_x, top + pad_y), pure_white(0.95 * alpha), lw + 4.0);
            }
        }
    }

    /// A wrapped explanation in the dark tooltip bubble, under `r` (or above
    /// it near the island's bottom), kept inside the island.
    fn draw_hint(&self, text: &str, r: D2D_RECT_F, island: D2D_RECT_F, alpha: f32) {
        let s = self.scale;
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let (pad_x, pad_y) = (9.0 * s, 5.0 * s);
        let room = 300.0 * s;
        unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(&utf16, &self.small_format, room, 400.0 * s) else { return };
            let _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = layout.GetMetrics(&mut m);
            // Even lines: narrow the box while the text keeps its line count,
            // so no word is left alone on the last line.
            let lines = m.lineCount;
            let mut width = m.width;
            while lines > 1 && width > 120.0 * s {
                let _ = layout.SetMaxWidth(width - 8.0 * s);
                let mut n = DWRITE_TEXT_METRICS::default();
                let _ = layout.GetMetrics(&mut n);
                if n.lineCount != lines {
                    break;
                }
                width -= 8.0 * s;
            }
            let _ = layout.SetMaxWidth(width + 1.0);
            let _ = layout.GetMetrics(&mut m);
            let (w, h) = (m.width + pad_x * 2.0, m.height + pad_y * 2.0);
            let cx = (r.left + r.right) / 2.0;
            let left = (cx - w / 2.0).min(island.right - 6.0 * s - w).max(island.left + 6.0 * s);
            let below = r.bottom + 5.0 * s;
            let top = if below + h > island.bottom - 6.0 * s { r.top - 5.0 * s - h } else { below };
            self.fill_rounded(rect(left, top, left + w, top + h), 6.0 * s, rgba(0.0, 0.0, 0.0, 0.85 * alpha));
            self.brush.SetColor(&pure_white(0.95 * alpha));
            self.surface
                .rt
                .DrawTextLayout(pt(left + pad_x, top + pad_y), &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
    }

    /// Nectar's pill tile: icon in a circle, title and status; filled with the
    /// accent while on.
    fn draw_tile(&self, r: D2D_RECT_F, tile: Tile, hot: bool, alpha: f32) {
        let s = self.scale;
        let output_name = self.output.as_ref().map(|o| o.name.clone()).unwrap_or_else(|| t("Yok", "None").into());
        let (icon, title, status, on) = match tile {
            Tile::Wifi => {
                let on = self.radios.wifi == Some(true);
                (ICON_WIFI, "Wi-Fi".to_string(), if on { t("Açık", "On") } else { t("Kapalı", "Off") }.to_string(), on)
            }
            Tile::Bluetooth => {
                let on = self.radios.bluetooth == Some(true);
                (ICON_BLUETOOTH, "Bluetooth".to_string(), if on { t("Açık", "On") } else { t("Kapalı", "Off") }.to_string(), on)
            }
            Tile::Network => (
                ICON_ETHERNET,
                t("Ağ", "Network").to_string(),
                if self.internet { t("İnternete bağlı", "Connected") } else { t("Bağlantı yok", "No internet") }.to_string(),
                self.internet,
            ),
            Tile::Output => (ICON_VOLUME, t("Ses çıkışı", "Sound output").to_string(), output_name, false),
            Tile::Battery(i) => {
                let device = self.batteries.get(i);
                let name = device.map(|d| d.name.clone()).unwrap_or_default();
                let level = device.map_or(0.0, |d| d.percent as f32 / 100.0);
                (ICON_HEADPHONES, name, format!("{} {}", i18n::percent(level), t("pil", "battery")), false)
            }
        };
        let pressed = self.pressed == Some(Control::Tile(tile));
        let lift = if hot && !pressed { 0.5 * s } else { 0.0 };
        let r = rect(r.left, r.top - lift, r.right, r.bottom - lift);
        let bg = if on {
            with_alpha(accent(), alpha * if hot { 0.9 } else { 1.0 })
        } else {
            ink_color((if hot { 0.08 } else { 0.04 }) * alpha)
        };
        self.fill_rounded(r, 11.0 * s, bg);
        let fg = if on { with_alpha(on_accent(), alpha) } else { ink_color(0.9 * alpha) };
        let muted = if on { rgba(on_accent().r, on_accent().g, on_accent().b, 0.6 * alpha) } else { ink_color(0.45 * alpha) };
        let circle = rect(r.left + 10.0 * s, r.top + 9.0 * s, r.left + 34.0 * s, r.top + 33.0 * s);
        self.fill_rounded(
            circle,
            12.0 * s,
            if on { rgba(0.0, 0.0, 0.0, 0.08 * alpha) } else { ink_color(0.08 * alpha) },
        );
        self.icon(icon, &self.icon_format, circle, fg);
        let x = circle.right + 8.0 * s;
        let limit = r.right - 8.0 * s - x;
        let (_, th) = self.text_size(&title, &self.title_format);
        let (_, sh) = self.text_size(&status, &self.small_format);
        let top = (r.top + r.bottom - th - sh) / 2.0;
        self.text(&title, &self.title_format, pt(x, top), fg, limit);
        self.text(&status, &self.small_format, pt(x, top + th), muted, limit);
    }

    /// Nectar's classic slider: a thin track that thickens on hover, the value on the right.
    fn draw_slider(&self, r: D2D_RECT_F, slider: &Slider, hot: bool, alpha: f32) {
        let s = self.scale;
        let value = self.slider_value(slider);
        let active = hot || self.dragging.as_ref() == Some(slider);
        let thickness = if active { 6.0 } else { 4.0 } * s;
        let cy = (r.top + r.bottom) / 2.0;
        let track = rect(r.left, cy - thickness / 2.0, r.right, cy + thickness / 2.0);
        self.fill_rounded(track, thickness / 2.0, ink_color(0.15 * alpha));
        match value {
            Some(v) => {
                let fill = rect(track.left, track.top, track.left + (track.right - track.left) * v, track.bottom);
                if fill.right - fill.left > 0.5 {
                    self.fill_rounded(fill, thickness / 2.0, ink_color(0.9 * alpha));
                }
                let text = i18n::percent(v);
                let (tw, th) = self.text_size(&text, &self.small_format);
                self.text(
                    &text,
                    &self.small_format,
                    pt(r.right + 40.0 * s - tw, cy - th / 2.0),
                    ink_color(if active { 0.9 } else { 0.55 } * alpha),
                    tw + 4.0,
                );
            }
            None => {
                let text = "—";
                let (tw, th) = self.text_size(text, &self.small_format);
                self.text(text, &self.small_format, pt(r.right + 40.0 * s - tw, cy - th / 2.0), ink_color(0.4 * alpha), tw + 4.0);
            }
        }
    }

    // ---- input -------------------------------------------------------------------

    fn on_mouse_move(&mut self, x: f32, y: f32) {
        if !self.hovered {
            let mut track = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut track);
            }
            self.hovered = true;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_COLLAPSE);
        }
        self.mouse = Some((x, y));
        if let Some(slider) = self.dragging.clone() {
            let v = self.slider_at(&slider, x);
            self.set_slider(&slider, v);
            return;
        }
        if self.grab.is_some() {
            self.drag_notch();
            return;
        }
        if !self.hidden_for_fullscreen && self.opening_since.is_none() {
            self.set_open(true);
        }
        if !self.animating {
            self.render();
        }
    }

    fn on_mouse_leave(&mut self) {
        self.hovered = false;
        self.mouse = None;
        self.pressed = None;
        if self.dragging.is_none() {
            // A short grace period so brushing past the edge does not close it.
            unsafe { SetTimer(Some(self.hwnd), TIMER_COLLAPSE, 180, None) };
        }
        self.render();
    }

    // ---- the weather's place ---------------------------------------------------

    /// Rows under the place field while typing (DIPs).
    fn city_list_height(&self) -> f32 {
        match &self.city_edit {
            Some(edit) if !edit.results.is_empty() => edit.results.len() as f32 * RESULT_HEIGHT,
            Some(edit) if !edit.text.trim().is_empty() => RESULT_HEIGHT,
            _ => 0.0,
        }
    }

    /// The notch takes the keyboard while a city is typed; it normally never
    /// takes the focus.
    fn start_city_edit(&mut self) {
        if self.city_edit.is_some() || self.headless {
            return;
        }
        let previous = unsafe { GetForegroundWindow() };
        self.city_edit = Some(CityEdit { text: String::new(), results: Vec::new(), searching: false, previous });
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex & !(WS_EX_NOACTIVATE.0 as isize));
            let _ = SetForegroundWindow(self.hwnd);
            let _ = SetFocus(Some(self.hwnd));
        }
        self.sync_panel_height(true);
    }

    fn end_city_edit(&mut self, give_focus_back: bool) {
        let Some(edit) = self.city_edit.take() else { return };
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_SEARCH);
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as isize);
            if give_focus_back && !edit.previous.0.is_null() {
                let _ = SetForegroundWindow(edit.previous);
            }
            if !self.hovered {
                SetTimer(Some(self.hwnd), TIMER_COLLAPSE, 180, None);
            }
        }
        self.sync_panel_height(true);
        self.render();
    }

    fn on_char(&mut self, c: u32) {
        let Some(edit) = &mut self.city_edit else { return };
        match c {
            0x08 => {
                edit.text.pop();
            }
            0x0D => {
                if let Some(place) = edit.results.first().cloned() {
                    self.pick_place(place);
                }
                return;
            }
            0x1B => {
                self.end_city_edit(true);
                return;
            }
            c if c >= 0x20 => match char::from_u32(c) {
                Some(ch) if edit.text.chars().count() < 40 => edit.text.push(ch),
                _ => return,
            },
            _ => return,
        }
        edit.searching = !edit.text.trim().is_empty();
        if !edit.searching {
            edit.results.clear();
        }
        // Search once the typing pauses.
        self.search_seq += 1;
        unsafe { SetTimer(Some(self.hwnd), TIMER_SEARCH, 350, None) };
        self.sync_panel_height(true);
        self.render();
    }

    fn search_city(&mut self) {
        let Some(edit) = &self.city_edit else { return };
        let text = edit.text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.search_seq += 1;
        let seq = self.search_seq;
        std::thread::spawn(move || {
            let found = weather::search(&text);
            if let Ok(mut s) = SEARCH.lock() {
                *s = Some((seq, found));
            }
            post(WM_PLACES);
        });
    }

    fn on_places(&mut self) {
        let Some((seq, found)) = SEARCH.lock().ok().and_then(|mut s| s.take()) else { return };
        if seq != self.search_seq {
            return;
        }
        if let Some(edit) = &mut self.city_edit {
            edit.results = found;
            edit.searching = false;
        }
        self.sync_panel_height(true);
        self.render();
    }

    fn pick_place(&mut self, place: weather::Place) {
        weather::set_place(Some(&place));
        self.place = Some(place);
        self.weather = None;
        self.end_city_edit(true);
    }

    fn on_down(&mut self, x: f32, y: f32) {
        let hit = self.hit(x, y);
        if hit == Some(Control::Grip) {
            let (island, ..) = self.edge_island();
            let along = if self.side() { self.origin.1 } else { self.origin.0 };
            self.grab = Some(self.cursor_along() - (along as f32 + island.left));
            unsafe {
                SetCapture(self.hwnd);
            }
            return;
        }
        if let Some(Control::Slider(slider)) = &hit {
            self.dragging = Some(slider.clone());
            unsafe {
                SetCapture(self.hwnd);
            }
            let v = self.slider_at(slider, x);
            self.set_slider(slider, v);
            return;
        }
        self.pressed = hit;
        self.render();
    }

    fn on_up(&mut self, x: f32, y: f32) {
        if self.grab.take().is_some() {
            unsafe {
                let _ = ReleaseCapture();
            }
            settings::set(self.settings, true);
            self.render();
            return;
        }
        if let Some(slider) = self.dragging.take() {
            if slider == Slider::Opacity {
                settings::write_file(&self.settings);
            }
            unsafe {
                let _ = ReleaseCapture();
            }
            self.render();
            return;
        }
        let hit = self.hit(x, y);
        let pressed = self.pressed.take();
        if hit.is_some() && hit == pressed {
            self.activate(hit.unwrap());
        }
        self.render();
    }

    fn activate(&mut self, control: Control) {
        match control {
            Control::Tab(i) => {
                if TABS[i].1 && self.tab != i {
                    self.end_city_edit(true);
                    self.tab = i;
                    if i == STATUS_TAB {
                        self.stats = self.sampler.sample();
                    }
                    self.update_stats_timer();
                    self.sync_panel_height(true);
                }
            }
            Control::CalendarPrevious => {
                let (y, m) = self.month;
                self.month = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
            }
            Control::CalendarNext => {
                let (y, m) = self.month;
                self.month = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
            }
            Control::CalendarTitle => self.month = (self.today.0, self.today.1),
            Control::Tile(Tile::Network) => shell::launch("ms-settings:network-status"),
            // Windows 11's sound output picker.
            Control::Tile(Tile::Battery(_)) => shell::launch("ms-settings:bluetooth"),
            Control::Tile(Tile::Output) => shell::press_keys(&[VK_LWIN, VK_CONTROL, VIRTUAL_KEY(b'V' as u16)]),
            Control::Tile(tile @ (Tile::Wifi | Tile::Bluetooth)) => {
                let (kind, on) = match tile {
                    Tile::Wifi => (RadioKind::WiFi, self.radios.wifi == Some(true)),
                    _ => (RadioKind::Bluetooth, self.radios.bluetooth == Some(true)),
                };
                network::set_radio(kind, !on, || post(WM_RADIOS));
            }
            Control::Button(Button::Notifications) => shell::open_notifications(),
            Control::Button(Button::Sound) => shell::launch("ms-settings:sound"),
            Control::Button(Button::Display) => shell::launch("ms-settings:display"),
            Control::Button(Button::Network) => shell::launch("ms-settings:network"),
            Control::Button(Button::Settings) => shell::launch("ms-settings:"),
            Control::SliderIcon(Slider::Volume) => {
                audio::toggle_mute();
                self.output = audio::read();
            }
            Control::SliderIcon(Slider::App) => {
                if let Some(t) = self.track() {
                    let (app, name) = (t.app.clone(), t.app_name.clone());
                    audio::toggle_app_mute(&app, &name);
                }
                self.refresh_app_volume();
            }
            Control::Theme(i) => {
                let mut settings = self.settings;
                settings.theme = settings::Theme::ALL[i];
                if settings != self.settings {
                    self.settings = settings;
                    settings::set(settings, true);
                }
            }
            Control::Choice(Choice::Autostart, i) => {
                crate::autostart::set(i == 1);
                self.autostart = crate::autostart::is_enabled();
            }
            Control::Choice(choice, i) => {
                let mut settings = self.settings;
                choice.select(&mut settings, i);
                if settings != self.settings {
                    self.settings = settings;
                    settings::set(settings, true);
                }
            }
            Control::Quit => crate::dock::quit(),
            Control::Grip => {}
            Control::ClockMode(i) => {
                if self.clockwork.mode != ClockMode::ALL[i] {
                    self.clockwork.reset();
                    self.clockwork.mode = ClockMode::ALL[i];
                    self.update_clockwork_timer();
                }
            }
            Control::ClockPreset(minutes) => {
                self.clockwork.reset();
                self.clockwork.length = Duration::from_secs(minutes as u64 * 60);
                self.update_clockwork_timer();
            }
            Control::ClockStart => {
                match self.clockwork.since.take() {
                    Some(at) => self.clockwork.banked += at.elapsed(),
                    None => self.clockwork.since = Some(Instant::now()),
                }
                self.update_clockwork_timer();
            }
            Control::ClockReset => {
                self.clockwork.reset();
                self.update_clockwork_timer();
            }
            Control::Update => crate::update::install_available(),
            Control::CityField => self.start_city_edit(),
            Control::CityAuto => {
                weather::set_place(None);
                self.place = None;
                self.weather = None;
            }
            Control::CityResult(i) => {
                if let Some(place) = self.city_edit.as_ref().and_then(|e| e.results.get(i).cloned()) {
                    self.pick_place(place);
                }
            }
            Control::Player(app) => {
                self.player = Some(app);
                self.refresh_app_volume();
                self.sync_panel_height(true);
            }
            Control::Media(button) => {
                if let Some(track) = self.track().cloned() {
                    let command = match button {
                        MediaButton::Previous => media::Command::Previous,
                        MediaButton::Next => media::Command::Next,
                        MediaButton::PlayPause => media::Command::PlayPause,
                    };
                    media::command(&track.app, command);
                    // Show the new state right away; the player confirms shortly.
                    if button == MediaButton::PlayPause {
                        if let Some(t) = self.media.tracks.iter_mut().find(|t| t.app == track.app) {
                            t.position_ms = t.position_now();
                            t.read_at = Instant::now();
                            t.playing = !t.playing;
                        }
                        self.music.target = if self.playing() { 1.0 } else { 0.0 };
                        self.update_level();
                    }
                }
            }
            Control::Progress => {
                let (island, ..) = self.island();
                let bar = self.controls(island).into_iter().find(|(_, c)| c == &Control::Progress).map(|(r, _)| r);
                if let (Some(bar), Some((mx, _)), Some(track)) = (bar, self.mouse, self.track().cloned()) {
                    if track.duration_ms > 0 {
                        let f = ((mx - bar.left) / (bar.right - bar.left).max(1.0)).clamp(0.0, 1.0);
                        let ms = (f * track.duration_ms as f32) as i64;
                        media::command(&track.app, media::Command::Seek(ms));
                        if let Some(t) = self.media.tracks.iter_mut().find(|t| t.app == track.app) {
                            t.position_ms = ms;
                            t.read_at = Instant::now();
                        }
                    }
                }
            }
            Control::SliderIcon(_) | Control::Slider(_) => {}
        }
    }

    fn on_wheel(&mut self, delta: i16, screen_x: i32, screen_y: i32) {
        let (x, y) = ((screen_x - self.origin.0) as f32, (screen_y - self.origin.1) as f32);
        if let Some(Control::Slider(slider) | Control::SliderIcon(slider)) = self.hit(x, y) {
            if let Some(v) = self.slider_value(&slider) {
                self.set_slider(&slider, v + delta as f32 / 120.0 * 0.02);
            }
        }
    }
}

impl Notch {
    /// `pollen --snapshot <dir>` also renders the notch: its opening, closed and
    /// open, without showing anything on screen.
    /// `pollen --demo-shots <dir>`: every tab with made-up data (a sample
    /// song, city and readings), on a transparent background.
    pub fn demo_shots(dir: &str) -> Result<()> {
        let mut n = Self::build(true)?;
        n.time = "09:41".into();
        n.output = Some(audio::Output { name: t("Hoparlör", "Speakers").into(), volume: 0.45, muted: false });
        n.internet = true;
        n.radios = network::Radios { wifi: Some(true), bluetooth: Some(false) };
        n.displays = vec![
            displays::Display { name: t("Monitör 1", "Monitor 1").into(), device: r"\\.\DISPLAY1".into(), left: 0, brightness: Some(70) },
            displays::Display { name: t("Monitör 2", "Monitor 2").into(), device: r"\\.\DISPLAY2".into(), left: 1920, brightness: Some(45) },
        ];
        let track = media::Track {
            app: "demo".into(),
            app_name: t("Medya Oynatıcı", "Media Player").into(),
            title: t("Gün Işığı", "Sunlight").into(),
            artist: t("Polen Dörtlüsü", "The Pollen Quartet").into(),
            playing: true,
            position_ms: 83_000,
            read_at: Instant::now(),
            duration_ms: 214_000,
            art: Some(std::sync::Arc::new(demo_art())),
            key: "demo".into(),
        };
        n.apply_media(media::Snapshot { tracks: vec![track], current: Some("demo".into()) });
        n.music = Spring::new(1.0);
        n.bars = [0.55, 0.8, 0.65, 0.4];
        n.app_volume = Some(audio::AppVolume { volume: 0.8, muted: false });
        n.batteries = vec![battery::Device { name: t("Kulaklık", "Headphones").into(), percent: 80 }];
        let hour = |hour, temperature, code| weather::Hour { hour, temperature, code, day: true };
        let day = |d, high, low, code| weather::Day { date: (n.today.0, n.today.1, d), high, low, code };
        let next = |k: u32| ((n.today.2 + k - 1) % 28) + 1;
        n.weather = Some(weather::Weather {
            city: t("İstanbul", "Istanbul").into(),
            temperature: 18.0,
            high: 21.0,
            low: 13.0,
            code: 2,
            day: true,
            hours: vec![hour(10, 18.0, 2), hour(11, 19.0, 1), hour(12, 20.0, 0), hour(13, 21.0, 0), hour(14, 21.0, 2), hour(15, 20.0, 3)],
            days: vec![
                day(next(1), 20.0, 12.0, 61),
                day(next(2), 17.0, 11.0, 3),
                day(next(3), 19.0, 12.0, 2),
                day(next(4), 22.0, 14.0, 0),
                day(next(5), 23.0, 15.0, 1),
            ],
        });
        n.stats = system::Stats {
            cpu: 0.14,
            memory_used: 7_900_000_000,
            memory_total: 17_100_000_000,
            disk_used: 251_000_000_000,
            disk_total: 511_000_000_000,
            down: 2_300_000.0,
            up: 184_000.0,
        };
        n.place = None;
        n.autostart = true;
        n.month = (n.today.0, n.today.1);
        let step = |n: &mut Notch, frames: usize| {
            for _ in 0..frames {
                std::thread::sleep(Duration::from_millis(16));
                n.last_tick = Some(Instant::now() - Duration::from_millis(16));
                n.tick();
            }
        };
        let save = |n: &Notch, name: &str| -> Result<()> {
            let path = format!("{dir}\\notch-{name}.png");
            n.surface.save_png(&path)?;
            println!("saved {path}");
            Ok(())
        };
        n.rise = Spring { value: RISE * n.scale, velocity: 0.0, target: 0.0 };
        n.opening_since = Some(Instant::now());
        step(&mut n, 60);
        save(&n, "closed")?;
        n.osd_room = Spring::new(1.0);
        n.render();
        save(&n, "volume")?;
        n.osd_room = Spring::new(0.0);
        n.tab = MUSIC_TAB;
        n.sync_panel_height(false);
        n.open.target = 1.0;
        step(&mut n, 50);
        for (tab, name) in [
            (MUSIC_TAB, "music"),
            (CONTROL_CENTER_TAB, "control-center"),
            (STATUS_TAB, "status"),
            (WEATHER_TAB, "weather"),
            (CALENDAR_TAB, "calendar"),
            (SETTINGS_TAB, "settings"),
        ] {
            n.tab = tab;
            n.sync_panel_height(false);
            n.render();
            save(&n, name)?;
        }
        // A settings option's explanation: the mouse over Smart, then Notifications.
        n.tab = SETTINGS_TAB;
        n.sync_panel_height(false);
        for (choice, i, name) in [(Choice::Mode, 1, "settings-hint"), (Choice::Notifications, 1, "settings-hint-low")] {
            let (island, ..) = n.island();
            if let Some((r, _)) = n.controls(island).into_iter().find(|(_, c)| *c == Control::Choice(choice, i)) {
                n.mouse = Some(((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0));
                n.render();
                save(&n, name)?;
            }
        }
        n.mouse = None;
        if std::env::var_os("POLLEN_BENCH").is_some() {
            for (tab, name) in [(MUSIC_TAB, "music"), (CONTROL_CENTER_TAB, "control-center"), (SETTINGS_TAB, "settings")] {
                n.tab = tab;
                n.sync_panel_height(false);
                let t = Instant::now();
                for _ in 0..300 {
                    n.render();
                }
                println!("BENCH notch open {name} {:.3} ms/frame ({}x{})", t.elapsed().as_secs_f64() * 1000.0 / 300.0, n.surface.width, n.surface.height);
            }
            let t = Instant::now();
            for _ in 0..300 {
                n.render_bars(true);
            }
            println!("BENCH notch level bars {:.3} ms/frame", t.elapsed().as_secs_f64() * 1000.0 / 300.0);
        }
        // The timer: running with 4:32 left, in its tab and on the closed island.
        n.clockwork.mode = ClockMode::Timer;
        n.clockwork.length = Duration::from_secs(5 * 60);
        n.clockwork.banked = Duration::from_secs(28);
        n.clockwork.since = Some(Instant::now());
        n.tab = TIMER_TAB;
        n.sync_panel_height(false);
        n.render();
        save(&n, "timer")?;
        n.open.target = 0.0;
        step(&mut n, 40);
        save(&n, "closed-timer")?;
        n.clockwork.reset();
        // Its alarm.
        n.banner = Some(Banner { glyph: ICON_ALARM, title: t("Süre doldu", "Time's up").into(), body: String::new() });
        n.banner_room = Spring::new(1.0);
        n.render();
        save(&n, "banner")?;
        n.banner = Some(Banner {
            glyph: ICON_ALARM,
            title: t("Mola zamanı", "Time for a break").into(),
            body: t("Biraz dinlen", "Rest a little").into(),
        });
        n.render();
        save(&n, "banner-two-lines")?;
        // Against the left edge, closed and open, and against the right one.
        n.banner = None;
        n.banner_room = Spring::new(0.0);
        n.slide = Spring::new(0.0);
        n.render();
        save(&n, "closed-left")?;
        n.tab = CONTROL_CENTER_TAB;
        n.sync_panel_height(false);
        n.open = Spring::new(1.0);
        n.render();
        save(&n, "open-left")?;
        n.open = Spring::new(0.0);
        n.slide = Spring::new(1.0);
        n.render();
        save(&n, "closed-right")?;
        n.slide = Spring::new(0.5);
        // On the left edge (with music) and the right edge (without):
        // closed, open, and the volume.
        for (edge, name, music) in [(NotchEdge::Left, "side-left", 1.0), (NotchEdge::Right, "side-right", 0.0)] {
            n.settings.notch_edge = edge;
            n.place();
            n.slide = Spring::new(0.5);
            n.music = Spring::new(music);
            n.open = Spring::new(0.0);
            n.render();
            save(&n, name)?;
            n.tab = if music > 0.5 { MUSIC_TAB } else { CONTROL_CENTER_TAB };
            n.sync_panel_height(false);
            n.open = Spring::new(1.0);
            n.render();
            save(&n, &format!("{name}-open"))?;
            n.open = Spring::new(0.0);
            n.osd_room = Spring::new(1.0);
            n.render();
            save(&n, &format!("{name}-volume"))?;
            n.osd_room = Spring::new(0.0);
        }
        n.settings.notch_edge = NotchEdge::Top;
        n.place();
        n.music = Spring::new(1.0);
        Ok(())
    }

    pub fn snapshot(dir: &str) -> Result<()> {
        let mut notch = Self::build(true)?;
        notch.output = audio::read();
        notch.internet = network::internet();
        notch.displays = displays::list_now();
        let playing = std::thread::spawn(media::read_now).join().unwrap_or_default();
        notch.apply_media(playing);
        notch.music.value = notch.music.target;
        notch.bars = [0.55, 0.8, 0.65, 0.4];
        let save = |n: &Notch, name: &str| {
            let path = format!("{dir}\\notch-{name}.png");
            match n.surface.save_png(&path) {
                Ok(()) => println!("saved {path}"),
                Err(e) => println!("failed {path}: {e}"),
            }
        };
        let step = |n: &mut Notch, frames: usize| {
            for _ in 0..frames {
                std::thread::sleep(Duration::from_millis(16));
                n.last_tick = Some(Instant::now() - Duration::from_millis(16));
                n.tick();
            }
        };
        notch.rise = Spring { value: RISE * notch.scale, velocity: 0.0, target: 0.0 };
        notch.opening_since = Some(Instant::now());
        for (i, frames) in [(1, 6), (2, 10), (3, 6), (4, 40)] {
            step(&mut notch, frames);
            save(&notch, &format!("1-opening-{i}"));
        }
        save(&notch, "1-closed-music");
        notch.bars = [0.9, 0.2, 0.7, 0.35];
        notch.render_bars(true);
        save(&notch, "1-closed-bars-fast");
        notch.render();
        save(&notch, "1-closed-bars-full");
        let output = notch.output.clone();
        notch.output = Some(audio::Output { name: String::new(), volume: 0.42, muted: false });
        notch.osd_room = Spring::new(1.0);
        notch.render();
        save(&notch, "1-closed-volume");
        notch.osd_room = Spring::new(0.0);
        notch.output = output;
        notch.tab = MUSIC_TAB;
        notch.refresh_app_volume();
        notch.sync_panel_height(false);
        notch.open.target = 1.0;
        step(&mut notch, 50);
        save(&notch, "2-music");
        notch.tab = MUSIC_TAB;
        notch.sync_panel_height(false);
        notch.weather = std::thread::spawn(|| {
            let _ = unsafe {
                windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED)
            };
            weather::check()
        })
        .join()
        .ok()
        .flatten();
        std::thread::sleep(Duration::from_millis(400));
        notch.stats = notch.sampler.sample();
        notch.month = (notch.today.0, notch.today.1);
        for (tab, name) in [(STATUS_TAB, "2b-status"), (WEATHER_TAB, "2d-weather"), (CALENDAR_TAB, "2c-calendar"), (SETTINGS_TAB, "2e-settings")] {
            notch.tab = tab;
            notch.sync_panel_height(false);
            notch.render();
            save(&notch, name);
        }
        let place = |name: &str, region: &str| weather::Place {
            name: name.into(),
            region: region.into(),
            latitude: 0.0,
            longitude: 0.0,
        };
        // Another theme.
        let mut ocean = settings::get();
        ocean.theme = settings::Theme::Ocean;
        settings::set(ocean, false);
        notch.settings = ocean;
        notch.render();
        save(&notch, "2j-theme-ocean");
        settings::use_defaults();
        notch.settings = settings::get();
        notch.privacy = privacy::InUse { microphone: vec!["Discord".into()], camera: vec!["Zen".into()] };
        notch.privacy_room = Spring::new(1.0);
        notch.render();
        save(&notch, "2h-privacy-open");
        notch.open.target = 0.0;
        step(&mut notch, 40);
        save(&notch, "2i-privacy-closed");
        notch.privacy = privacy::InUse::default();
        notch.privacy_room = Spring::new(0.0);
        notch.open.target = 1.0;
        step(&mut notch, 40);
        notch.city_edit = Some(CityEdit {
            text: "Ürg".into(),
            results: vec![place("Ürgüp", "Nevşehir, Türkiye"), place("Ürgenç", "Harezm, Özbekistan")],
            searching: false,
            previous: HWND::default(),
        });
        notch.sync_panel_height(false);
        notch.render();
        save(&notch, "2f-settings-city");
        notch.city_edit = None;
        notch.place = Some(place("Ürgüp", "Nevşehir, Türkiye"));
        notch.sync_panel_height(false);
        notch.render();
        save(&notch, "2g-settings-place");
        notch.place = None;
        let playing = notch.media.clone();
        notch.apply_media(media::Snapshot::default());
        notch.music = Spring::new(0.0);
        notch.open = Spring::new(0.0);
        notch.render();
        save(&notch, "2d-closed-status");
        notch.apply_media(playing);
        notch.music = Spring::new(notch.music.target);
        notch.tab = CONTROL_CENTER_TAB;
        notch.sync_panel_height(false);
        notch.open = Spring::new(0.0);
        notch.open.target = 1.0;
        step(&mut notch, 8);
        save(&notch, "2-opening-panel");
        step(&mut notch, 50);
        save(&notch, "3-open");
        // Hover a slider and a button.
        let (island, ..) = notch.island();
        let controls = notch.controls(island);
        if let Some((r, _)) = controls.iter().find(|(_, c)| matches!(c, Control::Slider(Slider::Brightness(_)))) {
            notch.mouse = Some(((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0));
            notch.render();
            save(&notch, "4-hover-slider");
        }
        if let Some((r, _)) = controls.iter().find(|(_, c)| matches!(c, Control::Button(Button::Notifications))) {
            notch.mouse = Some(((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0));
            notch.render();
            save(&notch, "5-hover-button");
        }
        Ok(())
    }
}

struct Formats {
    time: IDWriteTextFormat,
    date: IDWriteTextFormat,
    title: IDWriteTextFormat,
    small: IDWriteTextFormat,
    icon: IDWriteTextFormat,
    tab_icon: IDWriteTextFormat,
    track: IDWriteTextFormat,
    play: IDWriteTextFormat,
    big: IDWriteTextFormat,
    big_icon: IDWriteTextFormat,
    /// The Timer tab's readout.
    clock: IDWriteTextFormat,
    /// Segoe UI Symbol, for the weather glyphs Segoe Fluent Icons lacks.
    symbol: IDWriteTextFormat,
    big_symbol: IDWriteTextFormat,
}

impl Formats {
    fn new(dwrite: &IDWriteFactory, s: f32) -> Result<Formats> {
        let text = |size: f32, weight: DWRITE_FONT_WEIGHT| -> Result<IDWriteTextFormat> {
            unsafe {
                dwrite.CreateTextFormat(
                    w!("Segoe UI Variable Text"),
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size * s,
                    w!("tr-tr"),
                )
            }
        };
        let icons = |size: f32| -> Result<IDWriteTextFormat> {
            unsafe {
                dwrite.CreateTextFormat(
                    w!("Segoe Fluent Icons"),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size * s,
                    w!("tr-tr"),
                )
            }
        };
        let symbols = |size: f32| -> Result<IDWriteTextFormat> {
            unsafe {
                dwrite.CreateTextFormat(
                    w!("Segoe UI Symbol"),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size * s,
                    w!("tr-tr"),
                )
            }
        };
        Ok(Formats {
            time: text(12.5, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            date: text(11.0, DWRITE_FONT_WEIGHT_MEDIUM)?,
            title: text(11.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            small: text(9.5, DWRITE_FONT_WEIGHT_MEDIUM)?,
            icon: icons(14.0)?,
            tab_icon: icons(16.0)?,
            track: text(13.5, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            play: icons(18.0)?,
            big: text(26.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            big_icon: icons(28.0)?,
            clock: text(40.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            symbol: symbols(15.0)?,
            big_symbol: symbols(30.0)?,
        })
    }
}

/// A made-up album cover for the demo screenshots: a warm sky with a sun.
fn demo_art() -> media::Art {
    let n = 160u32;
    let mut pixels = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let t = (x + y) as f32 / (2 * n) as f32;
            // #ff8a5c to #7a5cff, top left to bottom right.
            let (mut r, mut g, mut b) = (1.0 + (0.48 - 1.0) * t, 0.54 + (0.36 - 0.54) * t, 0.36 + (1.0 - 0.36) * t);
            let d = ((x as f32 - 104.0).powi(2) + (y as f32 - 64.0).powi(2)).sqrt();
            let sun = ((30.0 - d) / 2.0).clamp(0.0, 1.0);
            r += (1.0 - r) * sun;
            g += (0.9 - g) * sun;
            b += (0.6 - b) * sun;
            pixels.extend([(b * 255.0) as u8, (g * 255.0) as u8, (r * 255.0) as u8, 255]);
        }
    }
    media::Art { width: n, height: n, pixels }
}

/// Windows' own alarm sound, once.
fn play_alarm() {
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let path = crate::util::wide(&format!(r"{windir}\Media\Alarm01.wav"));
    unsafe {
        let _ = PlaySoundW(crate::util::pcwstr(&path), None, SND_FILENAME | SND_ASYNC | SND_NODEFAULT);
    }
}

fn weather_icon(w: &weather::Weather) -> char {
    weather_glyph(w.code, w.day)
}

/// A glyph for a WMO weather code.
fn weather_glyph(code: i64, day: bool) -> char {
    match code {
        0 | 1 if day => ICON_SUN,
        0 | 1 => ICON_MOON,
        2 if day => ICON_PARTLY,
        51..=67 | 80..=82 => ICON_RAIN,
        71..=77 | 85 | 86 => ICON_SNOW,
        95..=99 => ICON_STORM,
        _ => ICON_CLOUD,
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

/// Day of the week, Monday = 0 (Sakamoto's method).
fn weekday_monday0(year: i32, month: u32, day: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if month < 3 { year - 1 } else { year };
    let sunday0 = (y + y / 4 - y / 100 + y / 400 + T[(month - 1) as usize] + day as i32).rem_euclid(7);
    ((sunday0 + 6) % 7) as u32
}

/// 3:07, or 1:02:44 past an hour.
fn format_time(ms: i64) -> String {
    let total = (ms.max(0) / 1000) as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Where the notch sits along its edge (0..1).
fn edge_position(s: &Settings) -> f32 {
    if s.notch_edge.side() {
        s.notch_y
    } else {
        s.notch_x
    }
}

fn post(message: u32) {
    let hwnd = HWND(NOTCH_HWND.load(Ordering::Relaxed) as *mut _);
    if !hwnd.0.is_null() {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(hwnd), message, WPARAM(0), LPARAM(0));
        }
    }
}

/// Opens the notch on its Settings tab (the tray icon's click).
pub fn show_settings() {
    post(WM_SHOW_SETTINGS);
}

/// Tells the notch to pick up changed settings.
pub fn settings_changed() {
    post(WM_SETTINGS_CHANGED);
}

/// The dock tells the notch when a fullscreen window takes over the screen.
pub fn set_hidden_for_fullscreen(hide: bool) {
    let hwnd = HWND(NOTCH_HWND.load(Ordering::Relaxed) as *mut _);
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Notch;
        if !ptr.is_null() {
            (*ptr).set_hidden_for_fullscreen(hide);
        }
    }
}

fn mouse_point(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as u16 as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as f32;
    (x, y)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Notch;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            let (x, y) = mouse_point(lparam);
            (*ptr).on_mouse_move(x, y);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            (*ptr).on_mouse_leave();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = mouse_point(lparam);
            (*ptr).on_down(x, y);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = mouse_point(lparam);
            (*ptr).on_up(x, y);
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            let notch = &mut *ptr;
            notch.dragging = None;
            if notch.grab.take().is_some() {
                settings::set(notch.settings, true);
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            // A sideways arrow where the notch can be dragged.
            let notch = &*ptr;
            if (lparam.0 & 0xFFFF) == 1 && (notch.grab.is_some() || notch.hovered_control() == Some(Control::Grip)) {
                if let Ok(cursor) = LoadCursorW(None, if notch.side() { IDC_SIZENS } else { IDC_SIZEWE }) {
                    SetCursor(Some(cursor));
                }
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16;
            let x = (lparam.0 & 0xFFFF) as u16 as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
            (*ptr).on_wheel(delta, x, y);
            LRESULT(0)
        }
        pacer::WM_FRAME => {
            pacer::frame_done(hwnd);
            (*ptr).tick();
            LRESULT(0)
        }
        WM_TIMER => {
            let notch = &mut *ptr;
            match wparam.0 {
                TIMER_CLOCKWORK => notch.on_clockwork(),
                TIMER_BANNER => {
                    let _ = KillTimer(Some(hwnd), TIMER_BANNER);
                    notch.banner_room.target = 0.0;
                    notch.start_animation();
                }
                TIMER_OSD => {
                    let _ = KillTimer(Some(hwnd), TIMER_OSD);
                    notch.osd_room.target = 0.0;
                    notch.start_animation();
                }
                TIMER_SEARCH => {
                    let _ = KillTimer(Some(hwnd), TIMER_SEARCH);
                    notch.search_city();
                }
                TIMER_COLLAPSE => {
                    let _ = KillTimer(Some(hwnd), TIMER_COLLAPSE);
                    if !notch.hovered && notch.dragging.is_none() && notch.city_edit.is_none() {
                        notch.set_open(false);
                    }
                }
                TIMER_CLOCK => {
                    notch.update_clock();
                    notch.render();
                }
                TIMER_REFRESH => {
                    notch.refresh_controls();
                    notch.render();
                }
                TIMER_LEVEL => notch.on_level(),
                TIMER_STATS => notch.on_stats(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DISPLAYS => {
            let notch = &mut *ptr;
            if notch.dragging.is_none() {
                notch.displays = displays::snapshot();
                notch.render();
            }
            LRESULT(0)
        }
        WM_CHAR => {
            (*ptr).on_char(wparam.0 as u32);
            LRESULT(0)
        }
        WM_ACTIVATE => {
            // Clicking elsewhere while typing a city ends the typing.
            if wparam.0 & 0xFFFF == 0 {
                (*ptr).end_city_edit(false);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_VOLUME => {
            (*ptr).on_volume(wparam.0);
            LRESULT(0)
        }
        WM_AUDIO_DEVICE => {
            let notch = &mut *ptr;
            if let Some(watch) = &mut notch.volume_watch {
                watch.rearm();
            }
            notch.output = audio::read();
            notch.render();
            LRESULT(0)
        }
        WM_PRIVACY => {
            (*ptr).on_privacy();
            LRESULT(0)
        }
        WM_PLACES => {
            (*ptr).on_places();
            LRESULT(0)
        }
        WM_BATTERIES => {
            let notch = &mut *ptr;
            notch.batteries = battery::current();
            notch.sync_panel_height(true);
            notch.render();
            LRESULT(0)
        }
        WM_NOTIFICATIONS => {
            let notch = &mut *ptr;
            // The newest one; none over a fullscreen game or the open notch.
            let toasts = notifications::take();
            if let Some(toast) = toasts.last() {
                if notch.settings.notifications && !notch.hidden_for_fullscreen && notch.open.target < 0.5 {
                    let body = if toast.body.is_empty() { toast.app.clone() } else { format!("{} · {}", toast.app, toast.body) };
                    notch.show_banner(ICON_BELL, toast.title.clone(), body, 6);
                }
            }
            LRESULT(0)
        }
        WM_SHOW_SETTINGS => {
            let notch = &mut *ptr;
            if !notch.hidden_for_fullscreen {
                notch.autostart = crate::autostart::is_enabled();
                notch.set_open(true);
                notch.tab = SETTINGS_TAB;
                notch.update_stats_timer();
                notch.sync_panel_height(false);
                // Closes on its own if the mouse never comes.
                if !notch.hovered {
                    SetTimer(Some(hwnd), TIMER_COLLAPSE, 6000, None);
                }
                notch.render();
            }
            LRESULT(0)
        }
        WM_SETTINGS_CHANGED => {
            let notch = &mut *ptr;
            notch.settings = settings::get();
            notch.autostart = crate::autostart::is_enabled();
            if notch.settings.notch_edge != notch.edge {
                notch.change_edge();
            } else if notch.grab.is_none() {
                notch.slide.target = edge_position(&notch.settings);
            }
            notch.start_animation();
            notch.render();
            LRESULT(0)
        }
        WM_WEATHER => {
            let notch = &mut *ptr;
            notch.weather = weather::current();
            notch.render();
            LRESULT(0)
        }
        WM_MEDIA => {
            (*ptr).on_media();
            LRESULT(0)
        }
        WM_RADIOS => {
            let notch = &mut *ptr;
            notch.radios = network::radios();
            notch.sync_panel_height(true);
            notch.render();
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE | WM_DPICHANGED => {
            let notch = &mut *ptr;
            notch.place();
            if msg == WM_DISPLAYCHANGE {
                displays::refresh();
            }
            notch.render();
            if msg == WM_DPICHANGED {
                LRESULT(0)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
