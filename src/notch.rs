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

use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows::core::{w, Result};
use windows::Devices::Radios::RadioKind;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D_RECT_F, D2D_SIZE_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1PathGeometry, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_ROUNDED_RECT, D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_MEDIUM, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VIRTUAL_KEY, VK_CONTROL, VK_LWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetWindowLongPtrW, KillTimer, LoadCursorW, RegisterClassExW, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, GWLP_USERDATA, HWND_TOPMOST, IDC_ARROW, MA_NOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, WM_APP, WM_CAPTURECHANGED,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_SETTINGCHANGE, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};
use windows_numerics::Vector2;

use crate::anim::Spring;
use crate::controls::{audio, displays, network};
use crate::pacer;
use crate::render::Surface;
use crate::shell;

// Sizes in DIPs (Nectar's CSS pixels), multiplied by the monitor scale.
const BAR: f32 = 36.0;
const CLOSED_MIN_WIDTH: f32 = 140.0;
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
const WINDOW_HEIGHT: f32 = 420.0;

/// Nectar's Midnight surface; it gets more opaque open (Nectar: +0.12).
const SURFACE: (f32, f32, f32) = (15.0 / 255.0, 21.0 / 255.0, 36.0 / 255.0);
const ACCENT: D2D1_COLOR_F = rgba(0.478, 0.635, 1.0, 1.0);
const ON_ACCENT: D2D1_COLOR_F = rgba(0.04, 0.06, 0.12, 1.0);

const TIMER_COLLAPSE: usize = 1;
const TIMER_CLOCK: usize = 2;
const TIMER_REFRESH: usize = 3;

/// Posted by the display worker when monitors or values change.
const WM_DISPLAYS: u32 = WM_APP + 4;
/// Posted when the radios were read again.
const WM_RADIOS: u32 = WM_APP + 5;
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
const ICON_GEAR: char = '\u{E713}';
const ICON_DISPLAY: char = '\u{E7F4}';

static NOTCH_HWND: AtomicIsize = AtomicIsize::new(0);

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
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
            Button::Notifications => "Bildirimler ve Rahatsız Etme",
            Button::Sound => "Ses ayarları",
            Button::Display => "Ekran ayarları",
            Button::Network => "Ağ ayarları",
            Button::Settings => "Windows Ayarları",
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
enum Slider {
    Volume,
    /// A monitor, by GDI device name.
    Brightness(String),
}

#[derive(Clone, PartialEq, Debug)]
enum Control {
    Tab(usize),
    Tile(Tile),
    Button(Button),
    /// The slider's track (drag / click / wheel).
    Slider(Slider),
    /// The slider's icon (mute for the volume).
    SliderIcon(Slider),
}

/// Tabs: (icon, available yet).
const TABS: [(char, bool); 4] = [(ICON_MUSIC, false), (ICON_CONTROLS, true), (ICON_STATUS, false), (ICON_CALENDAR, false)];
const CONTROL_CENTER_TAB: usize = 1;

pub struct Notch {
    hwnd: HWND,
    factory: ID2D1Factory,
    surface: Surface,
    brush: ID2D1SolidColorBrush,
    dwrite: IDWriteFactory,
    time_format: IDWriteTextFormat,
    date_format: IDWriteTextFormat,
    title_format: IDWriteTextFormat,
    small_format: IDWriteTextFormat,
    icon_format: IDWriteTextFormat,
    tab_icon_format: IDWriteTextFormat,
    scale: f32,
    origin: (i32, i32),
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
    internet: bool,
    radios: network::Radios,
    displays: Vec<displays::Display>,
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
                scale: 1.0,
                origin: (0, 0),
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
                internet: false,
                radios: network::Radios::default(),
                displays: Vec::new(),
                headless,
            });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, notch.as_mut() as *mut Notch as isize);

            notch.place();
            notch.update_clock();
            if headless {
                return Ok(notch);
            }
            displays::start(hwnd, WM_DISPLAYS);
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
                }
            }
            let m = info.rcMonitor;
            let width = (WINDOW_WIDTH * self.scale).ceil() as i32;
            let height = (WINDOW_HEIGHT * self.scale).ceil() as i32;
            self.origin = (m.left + (m.right - m.left - width) / 2, m.top);
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
        }
    }

    // ---- data --------------------------------------------------------------

    fn update_clock(&mut self) {
        const DAYS: [&str; 7] = ["Paz", "Pzt", "Sal", "Çar", "Per", "Cum", "Cmt"];
        const MONTHS: [&str; 12] = ["Oca", "Şub", "Mar", "Nis", "May", "Haz", "Tem", "Ağu", "Eyl", "Eki", "Kas", "Ara"];
        let t = unsafe { GetLocalTime() };
        self.time = format!("{:02}:{:02}", t.wHour, t.wMinute);
        self.date = format!(
            "{} {} {}",
            DAYS[t.wDayOfWeek as usize % 7],
            t.wDay,
            MONTHS[(t.wMonth as usize).saturating_sub(1) % 12]
        );
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
        tiles
    }

    fn sliders(&self) -> Vec<Slider> {
        let mut sliders = vec![Slider::Volume];
        sliders.extend(self.displays.iter().map(|d| Slider::Brightness(d.device.clone())));
        sliders
    }

    fn panel_height(&self) -> f32 {
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
        let (dw, _) = self.text_size(&self.date, &self.date_format);
        (tw + dw + 8.0 * self.scale + 36.0 * self.scale).max(CLOSED_MIN_WIDTH * self.scale)
    }

    /// The island right now.
    fn island(&self) -> (D2D_RECT_F, f32, f32, f32) {
        let s = self.scale;
        let width = self.surface.width as f32;
        let o = self.open.value.clamp(0.0, 1.05);
        let w = lerp(self.closed_width(), OPEN_WIDTH * s, o);
        let h = lerp(BAR * s, (BAR + self.panel_height()) * s, o);
        let e = self.spread.value.clamp(0.0, 1.05);
        let c = CIRCLE * s;
        let w = lerp(c, w, e);
        let h = lerp(c, h, e);
        let top = self.rise.value;
        let r = rect(width / 2.0 - w / 2.0, top, width / 2.0 + w / 2.0, top + h);
        let round = self.round_top.value.clamp(0.0, 1.0);
        let top_radius = round * c / 2.0;
        let bottom_radius = lerp(c / 2.0, RADIUS * s, e.min(1.0));
        let flare = if self.flares { FLARE * s } else { 0.0 };
        (r, top_radius, bottom_radius, flare)
    }

    /// Every control with its rectangle, for drawing and hit testing.
    fn controls(&self, island: D2D_RECT_F) -> Vec<(D2D_RECT_F, Control)> {
        let s = self.scale;
        let mut list = Vec::new();
        // Tabs in the top row.
        let tab_w = 40.0 * s;
        let tabs_left = (island.left + island.right) / 2.0 - tab_w * TABS.len() as f32 / 2.0;
        for i in 0..TABS.len() {
            let x = tabs_left + i as f32 * tab_w;
            list.push((rect(x, island.top, x + tab_w, island.top + BAR * s), Control::Tab(i)));
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
                self.refresh_controls();
                displays::refresh();
                unsafe { SetTimer(Some(self.hwnd), TIMER_REFRESH, 700, None) };
            } else {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_REFRESH);
                }
            }
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

        moving |= self.rise.step(dt, 500.0, 43.75);
        moving |= self.round_top.step(dt, 312.5, 35.0);
        moving |= self.spread.step(dt, 312.5, 35.0);
        moving |= self.open.step(dt, 312.5, 35.0);

        self.render();
        if !moving {
            self.animating = false;
            self.last_tick = None;
            pacer::stop(self.hwnd);
        }
    }

    // ---- drawing ---------------------------------------------------------------

    fn text_size(&self, text: &str, format: &IDWriteTextFormat) -> (f32, f32) {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            match self.dwrite.CreateTextLayout(&utf16, format, 2000.0, 200.0) {
                Ok(layout) => {
                    let mut m = DWRITE_TEXT_METRICS::default();
                    let _ = layout.GetMetrics(&mut m);
                    (m.width, m.height)
                }
                Err(_) => (0.0, 0.0),
            }
        }
    }

    /// Draws `text` with its top-left at `origin`, cut off with an ellipsis past
    /// `max_width`.
    fn text(&self, text: &str, format: &IDWriteTextFormat, origin: Vector2, color: D2D1_COLOR_F, max_width: f32) {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(&utf16, format, max_width.max(1.0), 200.0) else { return };
            let _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            if let Ok(sign) = self.dwrite.CreateEllipsisTrimmingSign(format) {
                let trimming = DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                };
                let _ = layout.SetTrimming(&trimming, &sign);
            }
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
    fn island_path(&self, r: D2D_RECT_F, top_radius: f32, bottom_radius: f32, flare: f32) -> Option<ID2D1PathGeometry> {
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let tr = top_radius.min(w / 2.0).min(h / 2.0).max(0.0);
        let br = bottom_radius.min(w / 2.0).min(h / 2.0).max(0.0);
        let use_flare = flare > tr;
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
            if use_flare {
                sink.BeginFigure(pt(r.left - flare, r.top), D2D1_FIGURE_BEGIN_FILLED);
                sink.AddArc(&arc(r.left, r.top + flare, flare, true));
            } else {
                sink.BeginFigure(pt(r.left + tr, r.top), D2D1_FIGURE_BEGIN_FILLED);
                if tr >= 0.5 {
                    sink.AddArc(&arc(r.left, r.top + tr, tr, false));
                }
            }
            sink.AddLine(pt(r.left, r.bottom - br));
            if br >= 0.5 {
                sink.AddArc(&arc(r.left + br, r.bottom, br, false));
            }
            sink.AddLine(pt(r.right - br, r.bottom));
            if br >= 0.5 {
                sink.AddArc(&arc(r.right, r.bottom - br, br, false));
            }
            if use_flare {
                sink.AddLine(pt(r.right, r.top + flare));
                sink.AddArc(&arc(r.right + flare, r.top, flare, true));
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
        let (island, top_radius, bottom_radius, flare) = self.island();
        let rt = self.surface.rt.clone();
        let o = self.open.value.clamp(0.0, 1.0);
        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&if self.headless { rgba(0.32, 0.40, 0.52, 1.0) } else { rgba(0.0, 0.0, 0.0, 0.0) }));
            if let Some(path) = self.island_path(island, top_radius, bottom_radius, flare) {
                let (r, g, b) = SURFACE;
                self.brush.SetColor(&rgba(r, g, b, lerp(0.80, 0.92, o)));
                rt.FillGeometry(&path, &self.brush, None);
            }

            rt.PushAxisAlignedClip(&island, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            // Closed: time and date, fading out as it opens.
            let closed = self.content * (1.0 - o * 2.5).clamp(0.0, 1.0);
            if closed > 0.0 {
                let (tw, th) = self.text_size(&self.time, &self.time_format);
                let (dw, dh) = self.text_size(&self.date, &self.date_format);
                let total = tw + 8.0 * s + dw;
                let x = (island.left + island.right - total) / 2.0;
                let cy = island.top + BAR * s / 2.0;
                self.text(&self.time, &self.time_format, pt(x, cy - th / 2.0), rgba(1.0, 1.0, 1.0, 0.95 * closed), tw + 4.0);
                self.text(
                    &self.date,
                    &self.date_format,
                    pt(x + tw + 8.0 * s, cy - dh / 2.0),
                    rgba(1.0, 1.0, 1.0, 0.65 * closed),
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

        let drawn = RECT {
            left: (island.left - flare - 2.0).floor() as i32,
            top: (island.top - 2.0).floor().max(0.0) as i32,
            right: (island.right + flare + 2.0).ceil() as i32,
            bottom: (island.bottom + 2.0).ceil() as i32,
        };
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

    fn hovered_control(&self) -> Option<Control> {
        self.mouse.and_then(|(x, y)| self.hit(x, y))
    }

    fn draw_panel(&self, island: D2D_RECT_F, alpha: f32) {
        let s = self.scale;
        let white = |a: f32| rgba(1.0, 1.0, 1.0, a * alpha);
        let hovered = self.hovered_control();
        let cy = island.top + BAR * s / 2.0;

        // Top row: time, tabs, date.
        let (_, th) = self.text_size(&self.time, &self.time_format);
        self.text(&self.time, &self.time_format, pt(island.left + 16.0 * s, cy - th / 2.0), white(0.95), 80.0 * s);
        let (dw, dh) = self.text_size(&self.date, &self.date_format);
        self.text(&self.date, &self.date_format, pt(island.right - 16.0 * s - dw, cy - dh / 2.0), white(0.65), dw + 4.0);

        for (r, control) in self.controls(island) {
            match control {
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
                    self.icon(glyph, &self.tab_icon_format, rect(r.left, r.top, r.right, r.bottom - 2.0 * s), white(a));
                    if active {
                        let mid = (r.left + r.right) / 2.0;
                        self.fill_rounded(
                            rect(mid - 7.0 * s, r.bottom - 5.0 * s, mid + 7.0 * s, r.bottom - 2.0 * s),
                            1.5 * s,
                            with_alpha(ACCENT, alpha),
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
                    self.fill_rounded(b, BUTTON * s / 2.0, white(bg));
                    self.icon(button.icon(), &self.icon_format, b, white(if hot { 1.0 } else { 0.75 }));
                }
                Control::SliderIcon(slider) => {
                    let glyph = match &slider {
                        Slider::Volume if self.output.as_ref().is_some_and(|o| o.muted) => ICON_MUTE,
                        Slider::Volume => ICON_VOLUME,
                        Slider::Brightness(_) => ICON_BRIGHTNESS,
                    };
                    let hot = hovered == Some(Control::SliderIcon(slider.clone())) && slider == Slider::Volume;
                    self.icon(glyph, &self.icon_format, r, white(if hot { 1.0 } else { 0.8 }));
                    let name = match &slider {
                        Slider::Volume => "Ses".to_string(),
                        Slider::Brightness(device) => {
                            self.displays.iter().find(|d| &d.device == device).map(|d| d.name.clone()).unwrap_or_default()
                        }
                    };
                    let (_, nh) = self.text_size(&name, &self.title_format);
                    let x = r.right + 6.0 * s;
                    let limit = island.left + PAD * s + LABEL_WIDTH * s - 8.0 * s - x;
                    self.text(&name, &self.title_format, pt(x, (r.top + r.bottom - nh) / 2.0), white(0.9), limit);
                }
                Control::Slider(slider) => self.draw_slider(r, &slider, hovered == Some(Control::Slider(slider.clone())), alpha),
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
                let left = (cx - lw / 2.0 - pad_x).clamp(island.left + 6.0 * s, island.right - 6.0 * s - lw - pad_x * 2.0);
                let top = r.bottom + 5.0 * s;
                let bubble = rect(left, top, left + lw + pad_x * 2.0, top + lh + pad_y * 2.0);
                self.fill_rounded(bubble, 6.0 * s, rgba(0.0, 0.0, 0.0, 0.8 * alpha));
                self.text(label, &self.small_format, pt(left + pad_x, top + pad_y), white(0.95), lw + 4.0);
            }
        }
    }

    /// Nectar's pill tile: icon in a circle, title and status; filled with the
    /// accent while on.
    fn draw_tile(&self, r: D2D_RECT_F, tile: Tile, hot: bool, alpha: f32) {
        let s = self.scale;
        let output_name = self.output.as_ref().map(|o| o.name.clone()).unwrap_or_else(|| "Yok".into());
        let (icon, title, status, on) = match tile {
            Tile::Wifi => {
                let on = self.radios.wifi == Some(true);
                (ICON_WIFI, "Wi-Fi".to_string(), if on { "Açık" } else { "Kapalı" }.to_string(), on)
            }
            Tile::Bluetooth => {
                let on = self.radios.bluetooth == Some(true);
                (ICON_BLUETOOTH, "Bluetooth".to_string(), if on { "Açık" } else { "Kapalı" }.to_string(), on)
            }
            Tile::Network => (
                ICON_ETHERNET,
                "Ağ".to_string(),
                if self.internet { "İnternete bağlı" } else { "Bağlantı yok" }.to_string(),
                self.internet,
            ),
            Tile::Output => (ICON_VOLUME, "Ses çıkışı".to_string(), output_name, false),
        };
        let pressed = self.pressed == Some(Control::Tile(tile));
        let lift = if hot && !pressed { 0.5 * s } else { 0.0 };
        let r = rect(r.left, r.top - lift, r.right, r.bottom - lift);
        let bg = if on {
            with_alpha(ACCENT, alpha * if hot { 0.9 } else { 1.0 })
        } else {
            rgba(1.0, 1.0, 1.0, (if hot { 0.08 } else { 0.04 }) * alpha)
        };
        self.fill_rounded(r, 11.0 * s, bg);
        let fg = if on { with_alpha(ON_ACCENT, alpha) } else { rgba(1.0, 1.0, 1.0, 0.9 * alpha) };
        let muted = if on { rgba(ON_ACCENT.r, ON_ACCENT.g, ON_ACCENT.b, 0.6 * alpha) } else { rgba(1.0, 1.0, 1.0, 0.45 * alpha) };
        let circle = rect(r.left + 10.0 * s, r.top + 9.0 * s, r.left + 34.0 * s, r.top + 33.0 * s);
        self.fill_rounded(
            circle,
            12.0 * s,
            if on { rgba(0.0, 0.0, 0.0, 0.08 * alpha) } else { rgba(1.0, 1.0, 1.0, 0.08 * alpha) },
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
        self.fill_rounded(track, thickness / 2.0, rgba(1.0, 1.0, 1.0, 0.15 * alpha));
        match value {
            Some(v) => {
                let fill = rect(track.left, track.top, track.left + (track.right - track.left) * v, track.bottom);
                if fill.right - fill.left > 0.5 {
                    self.fill_rounded(fill, thickness / 2.0, rgba(1.0, 1.0, 1.0, 0.9 * alpha));
                }
                let text = format!("{}%", (v * 100.0).round() as u32);
                let (tw, th) = self.text_size(&text, &self.small_format);
                self.text(
                    &text,
                    &self.small_format,
                    pt(r.right + 40.0 * s - tw, cy - th / 2.0),
                    rgba(1.0, 1.0, 1.0, if active { 0.9 } else { 0.55 } * alpha),
                    tw + 4.0,
                );
            }
            None => {
                let text = "—";
                let (tw, th) = self.text_size(text, &self.small_format);
                self.text(text, &self.small_format, pt(r.right + 40.0 * s - tw, cy - th / 2.0), rgba(1.0, 1.0, 1.0, 0.4 * alpha), tw + 4.0);
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
        if !self.hidden_for_fullscreen && self.opening_since.is_none() {
            self.set_open(true);
        }
        self.render();
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

    fn on_down(&mut self, x: f32, y: f32) {
        let hit = self.hit(x, y);
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
        if self.dragging.take().is_some() {
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
                if TABS[i].1 {
                    self.tab = i;
                }
            }
            Control::Tile(Tile::Network) => shell::launch("ms-settings:network-status"),
            // Windows 11's sound output picker.
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
    pub fn snapshot(dir: &str) -> Result<()> {
        let mut notch = Self::build(true)?;
        notch.output = audio::read();
        notch.internet = network::internet();
        notch.displays = displays::list_now();
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
        Ok(Formats {
            time: text(12.5, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            date: text(11.0, DWRITE_FONT_WEIGHT_MEDIUM)?,
            title: text(11.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            small: text(9.5, DWRITE_FONT_WEIGHT_MEDIUM)?,
            icon: icons(14.0)?,
            tab_icon: icons(16.0)?,
        })
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
            (*ptr).dragging = None;
            LRESULT(0)
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
                TIMER_COLLAPSE => {
                    let _ = KillTimer(Some(hwnd), TIMER_COLLAPSE);
                    if !notch.hovered && notch.dragging.is_none() {
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
        WM_RADIOS => {
            let notch = &mut *ptr;
            notch.radios = network::radios();
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
