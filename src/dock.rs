//! The dock window: layout, magnification, the bar's shape and motion, drawing and input.
//!
//! The look and motion follow Nectar's dock (src/Dock.tsx, Dock.css, theme.ts):
//! - A bar attached to the bottom edge of the screen, rounded on top, with small
//!   inward flares where it meets the edge; Nectar's "Midnight" theme color at 80%;
//!   app icons without tiles (Nectar's icon-only look); a glowing line under
//!   running apps.
//! - Opening: a 34 px circle drops in from above, flattens against the edge at
//!   280 ms, then spreads out into the bar at 350 ms while its contents fade in and
//!   the icons pop in with a springy scale.
//! - Hiding: the bar shrinks back into the circle and drops 100 px below the edge;
//!   showing again reverses it.
//! - Apps that open pop in from nothing while their neighbors slide apart; apps that
//!   close shrink away and the gap closes.
//!
//! Three modes, as in Nectar:
//! - Fixed: always visible and reserves its strip. While a window is maximized or
//!   snapped down to the strip, the bar widens to a solid full-width strip, so the
//!   reserved strip never shows the wallpaper or a game behind it.
//! - Smart: reserves nothing; hides while a window covers it.
//! - Peek: hidden until the mouse reaches the bottom edge.
//!
//! Nothing runs while the dock is idle. Redraws happen on input, on shell
//! notifications, on window moves (debounced) and, while anything is still moving,
//! once per display refresh (see `pacer`) until everything settles.

use std::collections::HashMap;
use std::f32::consts::PI;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED,
    D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1Factory, ID2D1PathGeometry,
    ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL,
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, 
    D2D1_ROUNDED_RECT, D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_TEXT_METRICS,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_LBUTTON,
};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_BOTTOM, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, GetAncestor,
    GetClassNameW, GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, IsIconic, IsZoomed, KillTimer, GWL_STYLE, WS_CAPTION, WS_THICKFRAME,
    LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassExW, RegisterShellHookWindow, RegisterWindowMessageW,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, TrackPopupMenu,
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, GA_ROOT, GWLP_USERDATA,
    HWND_TOPMOST, IDC_ARROW, MA_NOACTIVATE, MF_CHECKED, MF_SEPARATOR, MF_STRING, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SW_HIDE, SW_MINIMIZE, SW_SHOWNOACTIVATE, TPM_BOTTOMALIGN, TPM_CENTERALIGN, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP, WM_DESTROY, WM_DISPLAYCHANGE,
    WM_CAPTURECHANGED, WM_DPICHANGED, WM_ENDSESSION, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NULL,
    WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows_numerics::Vector2;

use crate::anim::Spring;
use crate::apps::{self, AppWindow};
use crate::icons;
use crate::pacer;
use crate::render::Surface;
use crate::settings::{self, DockMode, Settings};
use crate::shell;
use crate::taskbar;
use crate::tray;

// Sizes are in DIPs (Nectar's CSS pixels) and multiplied by the monitor scale.
/// The box each icon sits in (Nectar's 36 px icon slot).
const TILE: f32 = 36.0;
/// Share of the box the visible part of an icon fills (Nectar's icon-only look:
/// 30 px icons in 36 px slots, no tile behind them).
const ICON_FILL: f32 = 0.83;
/// How many neighboring icons the magnification reaches.
const REACH: f32 = 2.4;
const GAP: f32 = 12.0;
const PAD_X: f32 = 10.0;
const PAD_Y: f32 = 8.0;
const BAR_HEIGHT: f32 = TILE + PAD_Y * 2.0;
const TOP_RADIUS: f32 = 18.0;
/// The inward curves where the bar meets the screen edge.
const FLARE: f32 = 13.0;
/// The circle the bar shrinks into when it hides.
const COLLAPSED: f32 = 34.0;
/// How far below the edge the hidden circle drops (Nectar: y 100).
const HIDDEN_DROP: f32 = 100.0;
/// How far above its place the circle starts when Pollen opens.
const OPENING_DROP: f32 = 420.0;
const LABEL_SPACE: f32 = 40.0;
const CLOCK_PAD: f32 = 6.0;
/// Smart and Peek: how long the dock stays after the mouse leaves.
const HIDE_DELAY_MS: u32 = 800;

/// Nectar's "Midnight" theme: hsl(#5e8bff's hue, 40%, 10%) = #0f1524.
const SURFACE: (f32, f32, f32) = (9.0 / 255.0, 12.0 / 255.0, 20.0 / 255.0);
/// Nectar's default (and "solid" style) background opacity.
/// The accent / highlight color the theme was used with.
const ACCENT: D2D1_COLOR_F = rgba(0.478, 0.635, 1.0, 1.0);

const TIMER_REFRESH: usize = 2;
const TIMER_POLL: usize = 3;
const TIMER_CLOCK: usize = 4;
const TIMER_WINDOWS_MOVED: usize = 5;
const TIMER_HIDE: usize = 6;
const TIMER_PREVIEW: usize = 7;
/// Nectar's default delay before the window previews open.
const PREVIEW_DELAY_MS: u32 = 300;

const CMD_OPEN: usize = 1;
const CMD_CLOSE: usize = 2;
const CMD_QUIT: usize = 3;
const CMD_MODE_FIXED: usize = 10;
const CMD_MODE_SMART: usize = 11;
const CMD_MODE_PEEK: usize = 12;

const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_APPBAR: u32 = WM_APP + 1;
/// Testing aid: presses the tray button as if clicked (`PostMessage` from a script).
const WM_PRESS_TRAY: u32 = WM_APP + 9;
/// The settings changed (`settings::set`).
const WM_SETTINGS_CHANGED: u32 = WM_APP + 10;
/// The mouse left the window previews.
const WM_PREVIEW_LEFT: u32 = WM_APP + 11;
const ABN_POSCHANGED: usize = 1;
const ABN_FULLSCREENAPP: usize = 2;

// Shell hook notifications.
const HSHELL_WINDOWCREATED: usize = 1;
const HSHELL_WINDOWDESTROYED: usize = 2;
const HSHELL_WINDOWACTIVATED: usize = 4;
/// Windows asks where a window minimizes to, for its minimize animation.
const HSHELL_GETMINRECT: usize = 5;
const HSHELL_REDRAW: usize = 6;
const HSHELL_WINDOWREPLACED: usize = 13;
const HSHELL_RUDEAPPACTIVATED: usize = 0x8004;

/// The dock window, for the WinEvent callback (which gets no context pointer).
static DOCK_HWND: AtomicIsize = AtomicIsize::new(0);

/// Quits Pollen (the notch's Settings tab): closing the dock restores the
/// taskbar and ends the message loop.
pub fn quit() {
    let hwnd = DOCK_HWND.load(Ordering::Relaxed);
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), windows::Win32::UI::WindowsAndMessaging::WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// The window previews tell the dock the mouse left them, so it can hide
/// (Smart and Peek) as if the mouse had left the dock.
pub fn preview_left() {
    let hwnd = DOCK_HWND.load(Ordering::Relaxed);
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_PREVIEW_LEFT, WPARAM(0), LPARAM(0));
        }
    }
}

/// Tells the dock to pick up changed settings.
pub fn settings_changed() {
    let hwnd = DOCK_HWND.load(Ordering::Relaxed);
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_SETTINGS_CHANGED, WPARAM(0), LPARAM(0));
        }
    }
}

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
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

fn lerp_rect(a: D2D_RECT_F, b: D2D_RECT_F, t: f32) -> D2D_RECT_F {
    rect(lerp(a.left, b.left, t), lerp(a.top, b.top, t), lerp(a.right, b.right, t), lerp(a.bottom, b.bottom, t))
}

/// Scales a rectangle around its center.
fn scale_rect(r: D2D_RECT_F, k: f32) -> D2D_RECT_F {
    let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
    let (hw, hh) = ((r.right - r.left) / 2.0 * k, (r.bottom - r.top) / 2.0 * k);
    rect(cx - hw, cy - hh, cx + hw, cy + hh)
}

/// CSS `ease`, for Nectar's 0.2 s tooltip transition.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[derive(Clone)]
struct IconBitmap {
    bitmap: ID2D1Bitmap,
    width: f32,
    height: f32,
    content: [f32; 4],
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Start,
    App,
}

struct Item {
    kind: Kind,
    key: String,
    name: String,
    /// What to open when the app is not running.
    launch: Option<String>,
    exe: Option<String>,
    aumid: Option<String>,
    icon_source: String,
    pinned: bool,
    windows: Vec<HWND>,
    icon: Option<IconBitmap>,
    /// Hover magnification.
    scale: Spring,
    /// Room the item takes in the row: 0 = none, 1 = a full slot (Nectar's layout animation).
    presence: Spring,
    /// Visual pop-in / shrink-away scale (Nectar: scale 0 -> 1, spring 400/25).
    pop: Spring,
    /// When the pop-in starts (Nectar delays it by 150 ms).
    pop_at: Option<Instant>,
    /// Nectar's tap: 0.95 while pressed.
    press: Spring,
    /// Sideways offset (DIPs) that springs back to 0: neighbors making room
    /// while an icon is dragged, and the dragged icon settling in.
    shift: Spring,
    /// Closing; removed once it has shrunk away and its slot has closed.
    leaving: bool,
}

impl Item {
    fn new(kind: Kind, key: String, name: String, icon_source: String, pinned: bool, appear: bool) -> Item {
        let start = if appear { 0.0 } else { 1.0 };
        Item {
            kind,
            key,
            name,
            launch: None,
            exe: None,
            aumid: None,
            icon_source,
            pinned,
            windows: Vec::new(),
            icon: None,
            scale: Spring::new(1.0),
            presence: Spring { value: start, velocity: 0.0, target: 1.0 },
            pop: Spring::new(start),
            pop_at: appear.then(|| Instant::now() + Duration::from_millis(150)),
            press: Spring::new(1.0),
            shift: Spring::new(0.0),
            leaving: false,
        }
    }

    fn owns(&self, window: &AppWindow) -> bool {
        let exe_match = self.exe.as_deref().is_some_and(|e| !window.exe.is_empty() && e == window.exe);
        let aumid_match = match (&self.aumid, &window.aumid) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        };
        exe_match || aumid_match
    }

    fn set_leaving(&mut self, leaving: bool) {
        self.leaving = leaving;
        if leaving {
            // Shrink away first; the slot closes once it is gone (see `tick`).
            self.pop.target = 0.0;
            self.pop_at = None;
        } else {
            self.presence.target = 1.0;
            self.pop.target = 1.0;
        }
    }

    /// Nectar fades items in over 0.15 s while the scale spring takes longer.
    fn opacity(&self) -> f32 {
        (self.pop.value * 1.6).clamp(0.0, 1.0)
    }
}

struct Slot {
    x: f32,
    size: f32,
}

struct Layout {
    /// The bar as drawn right now (circle, dropping, spreading or full width).
    bar: D2D_RECT_F,
    /// The fully spread-out bar, ignoring hide/show motion.
    full: D2D_RECT_F,
    top_radius: f32,
    bottom_radius: f32,
    flare: f32,
    slots: Vec<Slot>,
    separators: Vec<f32>,
    /// The tray ("hidden icons") button, before the clock.
    tray: D2D_RECT_F,
    clock: D2D_RECT_F,
    icon_bottom: f32,
}

/// An icon held down, and dragged once it has moved far enough.
struct Drag {
    key: String,
    /// Where the press started (window x).
    start_x: f32,
    /// The mouse x now.
    x: f32,
    /// Where in the icon it was grabbed, as a fraction of its width.
    grab: f32,
    active: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Item(usize),
    Tray,
    Clock,
}

pub struct Dock {
    hwnd: HWND,
    factory: ID2D1Factory,
    surface: Surface,
    brush: ID2D1SolidColorBrush,
    dwrite: IDWriteFactory,
    label_format: IDWriteTextFormat,
    time_format: IDWriteTextFormat,
    items: Vec<Item>,
    icon_cache: HashMap<String, Option<IconBitmap>>,
    names: HashMap<String, String>,
    pins_signature: (Option<SystemTime>, Vec<u8>),
    settings: Settings,
    mouse: Option<(f32, f32)>,
    pressed: Option<Hit>,
    /// The hovered item and when its tooltip started fading in.
    label: Option<(usize, Instant)>,
    scale: f32,
    origin: (i32, i32),
    monitor: RECT,
    appbar_rect: Option<RECT>,
    appbar_registered: bool,
    // The bar's motion, after Nectar's framer-motion springs.
    /// 0 = the circle, 1 = the spread-out bar (Nectar's width/height spring).
    expand: Spring,
    /// 1 = round bottom corners (a free circle), 0 = flat against the edge.
    round_bottom: Spring,
    /// Vertical offset in pixels: negative above, positive below the edge.
    drop: Spring,
    /// Fixed mode: 0 = bar around the icons, 1 = solid full-width strip.
    widen: Spring,
    /// Nectar switches the flares on and off with the expanded state.
    flares: bool,
    /// How visible the icons and clock are; they fade in over 0.15 s at opening.
    content: f32,
    content_fade_from: Option<Instant>,
    /// The opening sequence: drop, flatten at 280 ms, spread out at 350 ms.
    opening_since: Option<Instant>,
    /// The window is extra tall while the opening circle drops in from above.
    tall: bool,
    /// What the previous frame drew, and the surface size it was drawn at: only
    /// the union of that and the new frame is sent to the compositor.
    last_drawn: Option<(RECT, (i32, i32))>,
    shown: bool,
    /// Smart mode: a window covers the dock.
    covered: bool,
    /// Smart and Peek: the mouse left only a moment ago.
    lingering: bool,
    /// The hovered app whose window previews are due or up.
    preview_for: Option<String>,
    hidden_for_fullscreen: bool,
    clock: String,
    clock_width: f32,
    shell_message: u32,
    taskbar_created_message: u32,
    animating: bool,
    last_tick: Option<Instant>,
    foreground: HWND,
    /// New items only animate in after the first fill.
    ready: bool,
    drag: Option<Drag>,
    /// `--snapshot`: never shown, touches neither the taskbar nor the work area,
    /// and paints an opaque backdrop so the PNGs are readable.
    headless: bool,
    _event_hooks: Vec<HWINEVENTHOOK>,
}

impl Dock {
    pub fn create() -> Result<Box<Dock>> {
        Self::build(false)
    }

    fn build(headless: bool) -> Result<Box<Dock>> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: w!("PollenDock"),
                ..Default::default()
            };
            RegisterClassExW(&class);

            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                w!("PollenDock"),
                w!("Pollen"),
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
                DOCK_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
            }

            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let surface = Surface::new(&factory, 1, 1)?;
            surface.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            surface.rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = surface.rt.CreateSolidColorBrush(&rgba(1.0, 1.0, 1.0, 1.0), None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

            let settings = if headless { Settings::default() } else { settings::get() };
            let mut dock = Box::new(Dock {
                hwnd,
                factory,
                surface,
                brush,
                label_format: text_format(&dwrite, 12.0, DWRITE_FONT_WEIGHT_NORMAL)?,
                time_format: text_format(&dwrite, 12.5, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
                dwrite,
                items: Vec::new(),
                icon_cache: HashMap::new(),
                names: HashMap::new(),
                pins_signature: (None, Vec::new()),
                settings,
                mouse: None,
                pressed: None,
                label: None,
                scale: 0.0,
                origin: (0, 0),
                monitor: RECT::default(),
                appbar_rect: None,
                appbar_registered: false,
                expand: Spring::new(0.0),
                round_bottom: Spring::new(1.0),
                drop: Spring::new(0.0),
                widen: Spring::new(0.0),
                flares: false,
                content: 0.0,
                content_fade_from: None,
                opening_since: None,
                tall: false,
                last_drawn: None,
                shown: false,
                covered: false,
                lingering: false,
                preview_for: None,
                hidden_for_fullscreen: false,
                clock: String::new(),
                clock_width: 0.0,
                shell_message: RegisterWindowMessageW(w!("SHELLHOOK")),
                taskbar_created_message: RegisterWindowMessageW(w!("TaskbarCreated")),
                animating: false,
                last_tick: None,
                foreground: HWND::default(),
                ready: false,
                drag: None,
                headless,
                _event_hooks: Vec::new(),
            });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, dock.as_mut() as *mut Dock as isize);

            if !headless {
                taskbar::hide();
                taskbar::watch();
            }
            dock.place();
            dock.apply_mode();
            dock.reload_pins();
            dock.refresh_windows();
            dock.update_clock();
            dock.widen.value = dock.widen.target;
            dock.ready = true;
            if headless {
                return Ok(dock);
            }

            if dock.wants_to_show() {
                dock.start_opening();
            } else {
                dock.shown = false;
                dock.drop = Spring::new(dock.hidden_offset());
            }
            dock.render();

            let _ = RegisterShellHookWindow(hwnd);
            // Window moves, maximize/restore and minimize decide whether the bar
            // widens (Fixed) or hides (Smart).
            dock._event_hooks = [
                (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
                (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            ]
            .into_iter()
            .map(|(min, max)| {
                SetWinEventHook(min, max, None, Some(win_event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS)
            })
            .collect();
            SetTimer(Some(hwnd), TIMER_POLL, 5_000, None);
            if !dock.hidden_for_fullscreen {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
            // Setting up took a while; start the opening's clock from now so its
            // first frame does not jump ahead.
            if dock.opening_since.is_some() {
                dock.opening_since = Some(Instant::now());
                dock.last_tick = Some(Instant::now());
            }
            Ok(dock)
        }
    }

    // ---- screen space ------------------------------------------------------

    fn bar_height(&self) -> i32 {
        (BAR_HEIGHT * self.scale).ceil() as i32
    }

    fn register_appbar(&mut self) {
        if self.headless || self.appbar_registered {
            return;
        }
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: self.hwnd,
            uCallbackMessage: WM_APPBAR,
            ..Default::default()
        };
        unsafe { SHAppBarMessage(ABM_NEW, &mut abd) };
        self.appbar_registered = true;
        self.appbar_rect = None;
    }

    /// Fixed mode: reserves the bottom strip so maximized windows stop above the dock.
    fn update_appbar(&mut self, force: bool) {
        if !self.appbar_registered {
            return;
        }
        let height = self.bar_height();
        let desired = RECT {
            left: self.monitor.left,
            top: self.monitor.bottom - height,
            right: self.monitor.right,
            bottom: self.monitor.bottom,
        };
        if !force && self.appbar_rect == Some(desired) {
            return;
        }
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: self.hwnd,
            uEdge: ABE_BOTTOM,
            rc: desired,
            ..Default::default()
        };
        unsafe {
            SHAppBarMessage(ABM_QUERYPOS, &mut abd);
            abd.rc.top = abd.rc.bottom - height;
            SHAppBarMessage(ABM_SETPOS, &mut abd);
        }
        self.appbar_rect = Some(desired);
    }

    fn remove_appbar(&mut self) {
        if !self.appbar_registered {
            return;
        }
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: self.hwnd,
            ..Default::default()
        };
        unsafe { SHAppBarMessage(ABM_REMOVE, &mut abd) };
        self.appbar_registered = false;
        self.appbar_rect = None;
    }

    /// Only Fixed reserves screen space.
    fn apply_mode(&mut self) {
        if self.settings.dock_mode == DockMode::Fixed {
            self.register_appbar();
            self.update_appbar(true);
        } else {
            self.remove_appbar();
            self.widen.target = 0.0;
        }
    }

    fn set_mode(&mut self, mode: DockMode) {
        let mut s = settings::get();
        if s.dock_mode != mode {
            s.dock_mode = mode;
            settings::set(s, true);
        }
    }

    /// The hovered icon's scale.
    fn max_scale(&self) -> f32 {
        self.settings.magnify.scale()
    }

    /// Picks up changed settings, from the notch's Settings tab or the menu.
    fn reload_settings(&mut self) {
        if self.headless {
            return;
        }
        let new = settings::get();
        let old = std::mem::replace(&mut self.settings, new);
        if old.dock_size != new.dock_size || old.magnify != new.magnify {
            // Re-measure everything and fetch the icons at the new size.
            self.scale = 0.0;
            self.place();
            self.update_appbar(true);
            self.update_targets();
        }
        if old.dock_mode != new.dock_mode {
            self.apply_mode();
            self.update_visibility();
        }
        self.update_window_state();
        self.start_animation();
        self.render();
    }

    /// Bottom of the primary monitor, full width.
    fn place(&mut self) {
        unsafe {
            let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(monitor, &mut info);
            let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
            let scale = dpi_x as f32 / 96.0 * self.settings.dock_size.factor();
            if (scale - self.scale).abs() > f32::EPSILON {
                self.scale = scale;
                self.icon_cache.clear();
                for item in &mut self.items {
                    item.icon = None;
                }
                if let (Ok(label), Ok(time)) = (
                    text_format(&self.dwrite, 12.0 * scale, DWRITE_FONT_WEIGHT_NORMAL),
                    text_format(&self.dwrite, 12.5 * scale, DWRITE_FONT_WEIGHT_SEMI_BOLD),
                ) {
                    self.label_format = label;
                    self.time_format = time;
                }
            }

            self.monitor = info.rcMonitor;
            self.update_appbar(false);

            let extra = if self.tall { OPENING_DROP } else { 0.0 };
            let height = ((BAR_HEIGHT + TILE * (self.max_scale() - 1.0) + LABEL_SPACE + 12.0 + extra) * self.scale).ceil() as i32;
            let width = self.monitor.right - self.monitor.left;
            self.origin = (self.monitor.left, self.monitor.bottom - height);
            let _ = self.surface.resize(width, height);
            if !self.headless {
                let _ =
                    SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        let missing: Vec<usize> = (0..self.items.len()).filter(|&i| self.items[i].icon.is_none()).collect();
        for i in missing {
            self.load_icon(i);
        }
        self.measure_clock();
    }

    /// Hides the dock outright while a fullscreen window (a game, a video) is the
    /// top window on its monitor, even when the focus is on another monitor.
    fn update_fullscreen(&mut self) {
        if self.headless {
            return;
        }
        let hide = is_fullscreen(self.foreground, &self.monitor)
            || apps::top_app_window_on(&self.monitor).is_some_and(|top| is_fullscreen(top, &self.monitor));
        if hide != self.hidden_for_fullscreen {
            self.hidden_for_fullscreen = hide;
            unsafe {
                let _ = ShowWindow(self.hwnd, if hide { SW_HIDE } else { SW_SHOWNOACTIVATE });
            }
        }
        crate::notch::set_hidden_for_fullscreen(hide);
    }

    /// Fixed: widen while a window reaches the strip. Smart: hide while a window
    /// covers the bar.
    fn update_window_state(&mut self) {
        if self.headless {
            return;
        }
        let m = self.monitor;
        let strip_top = m.bottom - self.bar_height();
        let full = self.layout(false).full;
        let (ox, oy) = (self.origin.0 as f32, self.origin.1 as f32);
        let bar = (ox + full.left, oy + full.top, ox + full.right, oy + full.bottom);
        let frames = apps::app_window_frames();
        let on_monitor = |r: &RECT| r.left < m.right && r.right > m.left && r.top < m.bottom && r.bottom > m.top;

        let widen = self.settings.dock_mode == DockMode::Fixed
            && frames.iter().any(|r| on_monitor(r) && r.top < strip_top && r.bottom >= strip_top - 2);
        let target = if widen { 1.0 } else { 0.0 };
        if self.widen.target != target {
            self.widen.target = target;
            self.start_animation();
        }

        self.covered = self.settings.dock_mode == DockMode::Smart
            && frames.iter().any(|r| {
                (r.left as f32) < bar.2 && (r.right as f32) > bar.0 && (r.top as f32) < bar.3 && (r.bottom as f32) > bar.1
            });
        self.update_visibility();
    }

    fn wants_to_show(&self) -> bool {
        let engaged = self.mouse.is_some() || self.lingering || crate::preview::is_hovered();
        match self.settings.dock_mode {
            DockMode::Fixed => true,
            DockMode::Smart => engaged || !self.covered,
            DockMode::Peek => engaged,
        }
    }

    fn hidden_offset(&self) -> f32 {
        HIDDEN_DROP * self.scale
    }

    /// Nectar's opening: a circle drops in from above, flattens against the edge,
    /// then spreads out while the icons pop in.
    fn start_opening(&mut self) {
        self.tall = true;
        self.place();
        self.shown = true;
        self.drop = Spring { value: -OPENING_DROP * self.scale, velocity: 0.0, target: 0.0 };
        self.expand = Spring::new(0.0);
        self.round_bottom = Spring::new(1.0);
        self.flares = false;
        self.content = 0.0;
        for item in &mut self.items {
            item.pop = Spring::new(0.0);
            item.pop_at = None;
        }
        self.opening_since = Some(Instant::now());
        self.start_animation();
    }

    /// Showing again after hiding: Nectar animates everything back at once.
    fn update_visibility(&mut self) {
        let show = self.wants_to_show();
        if show == self.shown {
            return;
        }
        self.shown = show;
        if !show {
            crate::preview::hide_now();
        }
        if show {
            self.drop.target = 0.0;
            self.expand.target = 1.0;
            self.round_bottom.target = 0.0;
            self.flares = true;
            self.content = 1.0;
        } else {
            self.opening_since = None;
            self.drop.target = self.hidden_offset();
            self.expand.target = 0.0;
            self.round_bottom.target = 1.0;
            self.flares = false;
        }
        self.start_animation();
    }

    // ---- items -------------------------------------------------------------

    fn icon_pixels(&self) -> i32 {
        (TILE * self.max_scale() * ICON_FILL * self.scale * 1.25).ceil() as i32
    }

    fn load_icon(&mut self, index: usize) {
        if self.items[index].kind != Kind::App {
            return;
        }
        let source = self.items[index].icon_source.clone();
        if !self.icon_cache.contains_key(&source) {
            let bitmap = icons::load(&source, self.icon_pixels()).and_then(|icon| unsafe {
                let props = D2D1_BITMAP_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                };
                let bitmap = self
                    .surface
                    .rt
                    .CreateBitmap(
                        D2D_SIZE_U { width: icon.width, height: icon.height },
                        Some(icon.pixels.as_ptr() as *const _),
                        icon.width * 4,
                        &props,
                    )
                    .ok()?;
                Some(IconBitmap {
                    bitmap,
                    width: icon.width as f32,
                    height: icon.height as f32,
                    content: icon.content,
                })
            });
            self.icon_cache.insert(source.clone(), bitmap);
        }
        self.items[index].icon = self.icon_cache.get(&source).cloned().flatten();
    }

    fn reload_pins(&mut self) {
        self.pins_signature = apps::pins_signature();
        let mut pins = apps::read_pins();
        let order = settings::pin_order();
        let rank = |launch: &str| {
            let key = format!("pin:{}", launch.to_lowercase());
            order.iter().position(|k| *k == key).unwrap_or(usize::MAX)
        };
        // Pins the dock has not ordered yet keep Windows' order, after the rest.
        pins.sort_by_key(|pin| rank(&pin.launch));
        let appear = self.ready;
        let mut old: Vec<Item> = self.items.drain(..).collect();
        let take = |old: &mut Vec<Item>, key: &str| old.iter().position(|i| i.key == key).map(|p| old.remove(p));

        let mut items = Vec::new();
        items.push(take(&mut old, "start").unwrap_or_else(|| {
            Item::new(Kind::Start, "start".into(), "Başlat".into(), String::new(), true, false)
        }));
        for pin in pins {
            let key = format!("pin:{}", pin.launch.to_lowercase());
            let item = match take(&mut old, &key) {
                Some(mut item) => {
                    item.set_leaving(false);
                    item
                }
                None => {
                    let mut item = Item::new(Kind::App, key, pin.name, pin.launch.clone(), true, appear);
                    item.launch = Some(pin.launch);
                    item.exe = pin.exe;
                    item.aumid = pin.aumid;
                    item
                }
            };
            items.push(item);
        }
        // Unpinned pins shrink away; running apps stay after the pins.
        for mut item in old {
            if item.pinned {
                item.set_leaving(true);
            }
            items.push(item);
        }
        self.items = items;
        for i in 0..self.items.len() {
            if self.items[i].icon.is_none() {
                self.load_icon(i);
            }
        }
        self.start_animation();
    }

    fn refresh_windows(&mut self) {
        let windows = apps::app_windows();
        for item in &mut self.items {
            item.windows.clear();
        }
        for window in windows {
            if let Some(i) = self.items.iter().position(|it| it.pinned && !it.leaving && it.owns(&window)) {
                self.items[i].windows.push(window.hwnd);
                continue;
            }
            let key = window_key(&window);
            if let Some(i) = self.items.iter().position(|it| !it.pinned && it.key == key) {
                self.items[i].windows.push(window.hwnd);
                continue;
            }
            let item = self.unpinned_item(key, &window);
            self.items.push(item);
            self.load_icon(self.items.len() - 1);
        }
        for item in &mut self.items {
            if !item.pinned {
                let gone = item.windows.is_empty();
                if gone != item.leaving {
                    item.set_leaving(gone);
                }
            }
            // A stable order so clicking cycles through every window.
            item.windows.sort_by_key(|h| h.0 as usize);
        }
        // Start, then pins, then running apps.
        self.items.sort_by_key(|it| match (it.kind, it.pinned) {
            (Kind::Start, _) => 0,
            (_, true) => 1,
            _ => 2,
        });
        self.label = None;
        self.foreground = unsafe { GetForegroundWindow() };
        self.update_fullscreen();
        self.update_window_state();
        self.start_animation();
    }

    fn unpinned_item(&mut self, key: String, window: &AppWindow) -> Item {
        let store_app = key.starts_with("aumid:");
        let (name, source) = if store_app {
            let aumid = window.aumid.clone().unwrap_or_default();
            (apps::window_title(window.hwnd), format!("shell:AppsFolder\\{aumid}"))
        } else {
            let exe = window.exe.clone();
            let name = self
                .names
                .entry(exe.clone())
                .or_insert_with(|| {
                    apps::exe_description(&exe).unwrap_or_else(|| {
                        std::path::Path::new(&exe)
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| apps::window_title(window.hwnd))
                    })
                })
                .clone();
            (name, exe)
        };
        let mut item = Item::new(Kind::App, key, name, source.clone(), false, self.ready);
        item.launch = (!source.is_empty()).then_some(source);
        item.exe = (!store_app && !window.exe.is_empty()).then(|| window.exe.clone());
        item.aumid = if store_app { window.aumid.clone() } else { None };
        item.windows = vec![window.hwnd];
        item
    }

    // ---- clock -------------------------------------------------------------

    fn update_clock(&mut self) {
        let t = unsafe { GetLocalTime() };
        self.clock = format!("{:02}:{:02}", t.wHour, t.wMinute);
        self.measure_clock();
        if !self.headless {
            let wait = (60 - t.wSecond as u32) * 1000 - t.wMilliseconds as u32 + 50;
            unsafe { SetTimer(Some(self.hwnd), TIMER_CLOCK, wait, None) };
        }
    }

    fn text_size(&self, text: &str, format: &IDWriteTextFormat) -> (f32, f32) {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            match self.dwrite.CreateTextLayout(&utf16, format, 1000.0, 200.0) {
                Ok(layout) => {
                    let mut m = DWRITE_TEXT_METRICS::default();
                    let _ = layout.GetMetrics(&mut m);
                    (m.width, m.height)
                }
                Err(_) => (0.0, 0.0),
            }
        }
    }

    fn measure_clock(&mut self) {
        let (tw, _) = self.text_size(&self.clock, &self.time_format);
        self.clock_width = tw + CLOCK_PAD * 2.0 * self.scale;
    }

    // ---- layout ------------------------------------------------------------

    fn layout(&self, magnified: bool) -> Layout {
        let s = self.scale;
        let tile = TILE * s;
        let gap = GAP * s;
        let width = self.surface.width as f32;
        let height = self.surface.height as f32;

        let mut x = 0.0f32;
        let mut slots = Vec::with_capacity(self.items.len());
        let mut separators = Vec::new();
        let mut running_started = false;
        for (i, item) in self.items.iter().enumerate() {
            let share = item.presence.value.clamp(0.0, 1.0);
            if !item.pinned && !running_started {
                running_started = true;
                // The divider before running apps grows with the first of them.
                separators.push(x + gap * share);
                x += gap * 2.0 * share;
            } else if i > 0 {
                x += gap * share;
            }
            let size = tile * share * if magnified { item.scale.value } else { 1.0 };
            slots.push(Slot { x, size });
            x += size;
        }
        separators.push(x + gap);
        x += gap * 2.0;
        let tray_x = x;
        x += tile * 0.75;
        let clock_x = x;
        x += self.clock_width;
        let content_width = x;

        let left = (width - content_width) / 2.0;
        let drag = self.drag.as_ref().filter(|d| d.active);
        for (slot, item) in slots.iter_mut().zip(&self.items) {
            slot.x += left + item.shift.value * s;
            if let Some(d) = drag.filter(|d| d.key == item.key) {
                slot.x = d.x - d.grab * slot.size;
            }
        }
        for sep in &mut separators {
            *sep += left;
        }

        // The spread-out bar, then Fixed mode's full-width version of it.
        let bar_top = height - BAR_HEIGHT * s;
        let around = rect(left - PAD_X * s, bar_top, left + content_width + PAD_X * s, height);
        let w = self.widen.value.clamp(0.0, 1.0);
        let full = lerp_rect(around, rect(0.0, bar_top, width, height), w);

        // Hide/show motion: the circle, flattening, spreading out, dropping.
        let c = COLLAPSED * s;
        let circle = rect(width / 2.0 - c / 2.0, height - c, width / 2.0 + c / 2.0, height);
        let e = self.expand.value.clamp(0.0, 1.05);
        let drop = self.drop.value;
        let mut bar = lerp_rect(circle, full, e);
        bar.top += drop;
        bar.bottom += drop;

        let round = self.round_bottom.value.clamp(0.0, 1.0);
        let top_radius = lerp(c / 2.0, TOP_RADIUS * s * (1.0 - w), e.min(1.0));
        let bottom_radius = round * c / 2.0;
        let flare = if self.flares { FLARE * s * (1.0 - w) } else { 0.0 };

        let clock_left = lerp(left + clock_x, width - self.clock_width - 16.0 * s, w);
        let clock = rect(clock_left, bar_top + drop, clock_left + self.clock_width, height + drop);
        let tray_left = lerp(left + tray_x, clock_left - tile * 0.75, w);
        let tray = rect(tray_left, bar_top + drop, tray_left + tile * 0.75, height + drop);
        if let Some(last) = separators.last_mut() {
            *last = lerp(*last, tray_left - gap, w);
        }

        Layout {
            bar,
            full,
            top_radius,
            bottom_radius,
            flare,
            slots,
            separators,
            tray,
            clock,
            icon_bottom: height - PAD_Y * s + drop,
        }
    }

    fn interactive(&self) -> bool {
        self.shown && self.expand.value > 0.5 && self.content > 0.5
    }

    fn hit_test(&self, x: f32, y: f32) -> Option<Hit> {
        if !self.interactive() {
            return None;
        }
        let layout = self.layout(true);
        if y > layout.bar.bottom {
            return None;
        }
        let gap = GAP * self.scale;
        let c = layout.clock;
        if x >= c.left && x < c.right && y >= c.top {
            return Some(Hit::Clock);
        }
        let t = layout.tray;
        if x >= t.left && x < t.right && y >= t.top {
            return Some(Hit::Tray);
        }
        layout
            .slots
            .iter()
            .enumerate()
            .find(|(i, slot)| {
                !self.items[*i].leaving
                    && x >= slot.x - gap / 2.0
                    && x < slot.x + slot.size + gap / 2.0
                    && y >= layout.icon_bottom - slot.size - 2.0 * self.scale
            })
            .map(|(i, _)| Hit::Item(i))
    }

    // ---- animation ---------------------------------------------------------

    fn update_targets(&mut self) {
        let base = self.layout(false);
        let pitch = (TILE + GAP) * self.scale;
        let max_scale = self.max_scale();
        let interactive = self.interactive();
        for (item, slot) in self.items.iter_mut().zip(base.slots.iter()) {
            item.scale.target = match self.mouse {
                Some((mx, _)) if interactive => {
                    let distance = (mx - (slot.x + slot.size / 2.0)).abs() / pitch;
                    let t = if distance < REACH { ((PI * distance / REACH).cos() + 1.0) / 2.0 } else { 0.0 };
                    1.0 + (max_scale - 1.0) * t
                }
                _ => 1.0,
            };
        }
        // The tooltip fades in again whenever the hovered item changes.
        let hovered = self.mouse.and_then(|(x, y)| self.hit_test(x, y)).and_then(|h| match h {
            Hit::Item(i) => Some(i),
            Hit::Tray | Hit::Clock => None,
        });
        let dragging = self.drag.as_ref().is_some_and(|d| d.active);
        let hovered = if dragging { None } else { hovered };
        if hovered != self.label.map(|(i, _)| i) {
            self.label = hovered.map(|i| (i, Instant::now()));
        }
        self.update_preview(hovered);
        self.start_animation();
    }

    /// Window previews: after a short hover over a running app, or at once
    /// when moving from one app to another while previews are up.
    fn update_preview(&mut self, hovered: Option<usize>) {
        if self.headless {
            return;
        }
        let target = hovered
            .filter(|&i| self.items[i].kind == Kind::App && !self.items[i].windows.is_empty())
            .map(|i| self.items[i].key.clone());
        if target == self.preview_for {
            return;
        }
        self.preview_for = target.clone();
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_PREVIEW);
        }
        match target {
            Some(key) if crate::preview::shown_key().is_some() => self.show_preview(&key),
            Some(_) => unsafe {
                SetTimer(Some(self.hwnd), TIMER_PREVIEW, PREVIEW_DELAY_MS, None);
            },
            None => crate::preview::hide_soon(),
        }
    }

    fn show_preview(&mut self, key: &str) {
        let Some(i) = self.items.iter().position(|item| item.key == key) else { return };
        if self.items[i].windows.is_empty() || !self.interactive() {
            return;
        }
        let layout = self.layout(true);
        let Some(slot) = layout.slots.get(i) else { return };
        let s = self.scale;
        let top = layout.icon_bottom - TILE * self.max_scale() * s - 6.0 * s;
        let item = &self.items[i];
        crate::preview::show(crate::preview::Request {
            key: item.key.clone(),
            name: item.name.clone(),
            icon_source: item.icon_source.clone(),
            windows: item.windows.clone(),
            anchor: (self.origin.0 + (slot.x + slot.size / 2.0) as i32, self.origin.1 + top as i32),
            monitor: self.monitor,
        });
        // The previews replace the tooltip.
        self.label = None;
    }

    fn start_animation(&mut self) {
        if !self.animating && !self.headless {
            self.animating = true;
            self.last_tick = Some(Instant::now());
            pacer::start(self.hwnd);
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let interval = self.last_tick.map(|t| (now - t).as_secs_f32() * 1000.0);
        let dt = self.last_tick.map_or(1.0 / 60.0, |t| (now - t).as_secs_f32()).min(0.05);
        self.last_tick = Some(now);
        let mut moving = false;

        // Nectar's opening timeline: impact at 280 ms, spread out at 350 ms.
        if let Some(since) = self.opening_since {
            let elapsed = now - since;
            if elapsed >= Duration::from_millis(280) {
                self.round_bottom.target = 0.0;
            }
            if elapsed >= Duration::from_millis(350) {
                self.expand.target = 1.0;
                self.flares = true;
                self.content_fade_from = Some(now);
                for item in &mut self.items {
                    item.pop = Spring::new(0.0);
                    item.pop_at = Some(now + Duration::from_millis(150));
                }
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

        // Nectar's springs (stiffness and damping divided by its mass of 0.8).
        moving |= self.drop.step(dt, 500.0, 43.75);
        moving |= self.round_bottom.step(dt, 312.5, 35.0);
        moving |= self.expand.step(dt, 312.5, 35.0);
        moving |= self.widen.step(dt, 312.5, 35.0);

        let pressed = self.pressed;
        let mut removed = false;
        let mut index = 0;
        self.items.retain_mut(|item| {
            let i = index;
            index += 1;
            if let Some(at) = item.pop_at {
                if now >= at {
                    item.pop.target = 1.0;
                    item.pop_at = None;
                }
                moving = true;
            }
            item.press.target = if pressed == Some(Hit::Item(i)) { 0.95 } else { 1.0 };
            moving |= item.scale.step(dt, 420.0, 36.0);
            moving |= item.pop.step(dt, 400.0, 25.0);
            moving |= item.press.step(dt, 500.0, 38.0);
            moving |= item.shift.step(dt, 400.0, 35.0);
            // A closing app's slot closes once its icon has shrunk away.
            if item.leaving && item.pop.value < 0.05 {
                item.presence.target = 0.0;
            }
            let presence_moving = item.presence.step(dt, 300.0, 35.0);
            moving |= presence_moving;
            let keep = !(item.leaving && item.presence.target == 0.0 && !presence_moving);
            removed |= !keep;
            keep
        });
        if removed {
            self.pressed = None;
            self.label = None;
        }
        if let Some((_, since)) = self.label {
            moving |= now - since < Duration::from_millis(200);
        }

        if !moving && self.tall {
            // The opening drop is over; back to the normal window height.
            self.tall = false;
            self.place();
        }
        let render_start = Instant::now();
        self.render();
        profile::frame(interval, render_start.elapsed().as_secs_f32() * 1000.0, !moving);
        if !moving {
            self.animating = false;
            self.last_tick = None;
            pacer::stop(self.hwnd);
        }
    }

    // ---- drawing -----------------------------------------------------------

    fn fill_rounded(&self, r: D2D_RECT_F, radius: f32, color: D2D1_COLOR_F) {
        unsafe {
            self.brush.SetColor(&color);
            self.surface
                .rt
                .FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &self.brush);
        }
    }

    /// The bar outline (Nectar's SurfaceShape): rounded top corners and, at the
    /// bottom, either inward flares (attached to the edge) or round corners (the
    /// free circle).
    fn bar_path(&self, layout: &Layout) -> Option<ID2D1PathGeometry> {
        let r = layout.bar;
        let (bw, bh) = (r.right - r.left, r.bottom - r.top);
        let tr = layout.top_radius.min(bh / 2.0).min(bw / 2.0).max(0.0);
        let br = layout.bottom_radius.min(bh / 2.0).min(bw / 2.0).max(0.0);
        let f = layout.flare;
        let use_flare = f > br;
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
                sink.BeginFigure(pt(r.left - f, r.bottom), D2D1_FIGURE_BEGIN_FILLED);
                sink.AddArc(&arc(r.left, r.bottom - f, f, false));
            } else {
                sink.BeginFigure(pt(r.left, r.bottom - br), D2D1_FIGURE_BEGIN_FILLED);
            }
            sink.AddLine(pt(r.left, r.top + tr));
            if tr >= 0.5 {
                sink.AddArc(&arc(r.left + tr, r.top, tr, true));
            }
            sink.AddLine(pt(r.right - tr, r.top));
            if tr >= 0.5 {
                sink.AddArc(&arc(r.right, r.top + tr, tr, true));
            }
            if use_flare {
                sink.AddLine(pt(r.right, r.bottom - f));
                sink.AddArc(&arc(r.right + f, r.bottom, f, false));
            } else {
                sink.AddLine(pt(r.right, r.bottom - br));
                if br >= 0.5 {
                    sink.AddArc(&arc(r.right - br, r.bottom, br, true));
                }
                sink.AddLine(pt(r.left + br, r.bottom));
                if br >= 0.5 {
                    sink.AddArc(&arc(r.left, r.bottom - br, br, true));
                }
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close().ok()?;
            Some(path)
        }
    }

    fn render(&mut self) {
        let s = self.scale;
        let layout = self.layout(true);
        let rt = self.surface.rt.clone();
        let height = self.surface.height as f32;
        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&if self.headless { rgba(0.32, 0.40, 0.52, 1.0) } else { rgba(0.0, 0.0, 0.0, 0.0) }));

            // Invisible (alpha 1/255) but mouse-catching areas: above the magnified
            // icons while hovering, and a thin strip at the screen edge while hidden,
            // so reaching the edge brings the dock back.
            if self.mouse.is_some() && self.interactive() {
                let top = layout.icon_bottom - TILE * self.max_scale() * s - 6.0 * s;
                self.fill_rounded(
                    rect(layout.bar.left, top, layout.bar.right, layout.bar.bottom),
                    0.0,
                    rgba(0.0, 0.0, 0.0, 1.0 / 255.0),
                );
            }
            if !self.shown {
                self.fill_rounded(
                    rect(layout.full.left, height - 2.0 * s, layout.full.right, height),
                    0.0,
                    rgba(0.0, 0.0, 0.0, 1.0 / 255.0),
                );
            }

            if layout.bar.top < height {
                if let Some(path) = self.bar_path(&layout) {
                    // Midnight at 80%; solid once it is a full-width strip, so a
                    // game or the wallpaper behind it never shows through.
                    let (r, g, b) = SURFACE;
                    let opacity = lerp(self.settings.opacity, 1.0, self.widen.value.clamp(0.0, 1.0));
                    self.brush.SetColor(&rgba(r, g, b, opacity));
                    rt.FillGeometry(&path, &self.brush, None);
                }
            }

            let content = self.content;
            if content > 0.0 {
                // Keep the icons inside the bar while it spreads out or shrinks.
                let clip = self.expand.value < 0.999;
                if clip {
                    rt.PushAxisAlignedClip(
                        &rect(layout.bar.left, 0.0, layout.bar.right, height),
                        D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    );
                }
                // Nectar's divider: --nectar-border, white at 12%.
                self.brush.SetColor(&rgba(1.0, 1.0, 1.0, 0.12 * content));
                for &x in &layout.separators {
                    let _ = rt.DrawLine(
                        pt(x, layout.icon_bottom - TILE * s + 6.0 * s),
                        pt(x, layout.icon_bottom - 6.0 * s),
                        &self.brush,
                        1.0 * s,
                        None,
                    );
                }

                for (item, slot) in self.items.iter().zip(layout.slots.iter()) {
                    let visible = item.pop.value * item.press.value;
                    if visible <= 0.01 || slot.size <= 0.5 {
                        continue;
                    }
                    let cx = slot.x + slot.size / 2.0;
                    let base = rect(slot.x, layout.icon_bottom - slot.size, slot.x + slot.size, layout.icon_bottom);
                    let tile = scale_rect(base, visible);
                    let opacity = item.opacity() * content;
                    match (item.kind, &item.icon) {
                        (Kind::Start, _) => self.draw_start(tile, opacity),
                        (Kind::App, Some(icon)) => draw_icon(&rt, icon, tile, opacity),
                        (Kind::App, None) => self.draw_placeholder(item, tile, opacity),
                    }

                    // Nectar's running indicator: a glowing 14 x 3 line (20 wide
                    // with several windows), 4 px under the tile.
                    if !item.windows.is_empty() && !item.leaving {
                        let half = if item.windows.len() > 1 { 10.0 } else { 7.0 } * s;
                        let top = layout.icon_bottom + 4.0 * s;
                        let line = rect(cx - half, top, cx + half, top + 3.0 * s);
                        for (spread, alpha) in [(3.0, 0.10), (1.5, 0.22)] {
                            let g = spread * s;
                            self.fill_rounded(
                                rect(line.left - g, line.top - g, line.right + g, line.bottom + g),
                                2.0 * s + g,
                                rgba(ACCENT.r, ACCENT.g, ACCENT.b, alpha * opacity),
                            );
                        }
                        self.fill_rounded(line, 1.5 * s, rgba(ACCENT.r, ACCENT.g, ACCENT.b, opacity));
                    }
                }

                self.draw_tray_button(&layout, content);
                self.draw_clock(&layout, content);
                if clip {
                    rt.PopAxisAlignedClip();
                }

                // Nectar's tooltip: fades in and rises 4 px over 0.2 s.
                if let Some((i, since)) = self.label {
                    if let (Some(item), Some(slot)) = (self.items.get(i), layout.slots.get(i)) {
                        let t = ease(since.elapsed().as_secs_f32() / 0.2);
                        let rise = (1.0 - t) * 4.0 * s;
                        let bottom = layout.icon_bottom - slot.size - 10.0 * s + rise;
                        self.draw_label(&item.name, slot.x + slot.size / 2.0, bottom, t * content);
                    }
                }
            }

            let _ = rt.EndDraw(None, None);
        }
        if !self.headless {
            let drawn = self.drawn_bounds(&layout);
            let size = (self.surface.width, self.surface.height);
            let dirty = match self.last_drawn {
                Some((last, last_size)) if last_size == size => Some(RECT {
                    left: last.left.min(drawn.left),
                    top: last.top.min(drawn.top),
                    right: last.right.max(drawn.right),
                    bottom: last.bottom.max(drawn.bottom),
                }),
                // First frame at this size: send everything.
                _ => None,
            };
            self.last_drawn = Some((drawn, size));
            let _ = self.surface.present(self.hwnd, self.origin.0, self.origin.1, dirty);
        }
    }

    /// Everything this frame may have painted: the bar with its flares, the
    /// magnified icons, the tooltip above them and the edge strip.
    fn drawn_bounds(&self, layout: &Layout) -> RECT {
        let s = self.scale;
        let height = self.surface.height as f32;
        let margin = layout.flare + 4.0 * s;
        let (mut left, mut right) = (layout.bar.left - margin, layout.bar.right + margin);
        let mut top = layout.bar.top - margin;
        let mut bottom = layout.bar.bottom + margin;
        if self.content > 0.0 {
            // Magnified icons and their tooltip rise above the bar; tooltips can be
            // wider than the end icons.
            let label = if self.label.is_some() { 160.0 * s } else { 0.0 };
            left = left.min(layout.full.left - margin) - label;
            right = right.max(layout.full.right + margin) + label;
            top = top.min(layout.icon_bottom - TILE * self.max_scale() * s - LABEL_SPACE * s - 4.0 * s);
        }
        if !self.shown {
            // The invisible edge strip.
            left = left.min(layout.full.left);
            right = right.max(layout.full.right);
            top = top.min(height - 4.0 * s);
            bottom = height;
        }
        RECT {
            left: left.floor() as i32,
            top: top.floor().max(0.0) as i32,
            right: right.ceil() as i32,
            bottom: bottom.ceil().min(height) as i32,
        }
    }

    /// The four-pane Windows logo.
    fn draw_start(&self, r: D2D_RECT_F, opacity: f32) {
        // Nectar's icon-only start icon: 24 px in the 36 px slot.
        let side = (r.right - r.left) * 0.62;
        let gap = side * 0.08;
        let pane = (side - gap) / 2.0;
        let cx = (r.left + r.right) / 2.0;
        let cy = (r.top + r.bottom) / 2.0;
        let (x0, y0) = (cx - side / 2.0, cy - side / 2.0);
        let color = rgba(0.36, 0.66, 1.0, opacity);
        for (dx, dy) in [(0.0, 0.0), (pane + gap, 0.0), (0.0, pane + gap), (pane + gap, pane + gap)] {
            self.fill_rounded(rect(x0 + dx, y0 + dy, x0 + dx + pane, y0 + dy + pane), pane * 0.12, color);
        }
    }

    /// An upward chevron, like the taskbar's "show hidden icons" button.
    fn draw_tray_button(&self, layout: &Layout, content: f32) {
        let s = self.scale;
        let t = layout.tray;
        let hovered = self.mouse.and_then(|(x, y)| self.hit_test(x, y)) == Some(Hit::Tray);
        let pressed = self.pressed == Some(Hit::Tray);
        let (cx, cy) = ((t.left + t.right) / 2.0, layout.icon_bottom - TILE * s / 2.0);
        if hovered || pressed {
            let r = 13.0 * s;
            self.fill_rounded(
                rect(cx - r, cy - r, cx + r, cy + r),
                7.0 * s,
                rgba(1.0, 1.0, 1.0, if pressed { 0.16 } else { 0.10 } * content),
            );
        }
        let (half, rise) = (5.0 * s, 2.75 * s);
        unsafe {
            self.brush.SetColor(&rgba(1.0, 1.0, 1.0, 0.85 * content));
            let rt = &self.surface.rt;
            let _ = rt.DrawLine(pt(cx - half, cy + rise), pt(cx, cy - rise), &self.brush, 1.6 * s, None);
            let _ = rt.DrawLine(pt(cx, cy - rise), pt(cx + half, cy + rise), &self.brush, 1.6 * s, None);
        }
    }

    fn draw_clock(&self, layout: &Layout, content: f32) {
        let c = layout.clock;
        let (tw, th) = self.text_size(&self.clock, &self.time_format);
        let pressed = if self.pressed == Some(Hit::Clock) { 0.6 } else { 1.0 };
        let origin = pt((c.left + c.right - tw) / 2.0, (c.top + c.bottom - th) / 2.0);
        self.draw_text(&self.clock, &self.time_format, origin, rgba(1.0, 1.0, 1.0, 0.95 * content * pressed));
    }

    fn draw_text(&self, text: &str, format: &IDWriteTextFormat, origin: Vector2, color: D2D1_COLOR_F) {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            if let Ok(layout) = self.dwrite.CreateTextLayout(&utf16, format, 1000.0, 200.0) {
                self.brush.SetColor(&color);
                self.surface.rt.DrawTextLayout(origin, &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
        }
    }

    fn draw_placeholder(&self, item: &Item, r: D2D_RECT_F, opacity: f32) {
        let letter = item.name.chars().next().unwrap_or('?').to_uppercase().collect::<String>();
        let (w, h) = self.text_size(&letter, &self.label_format);
        self.draw_text(
            &letter,
            &self.label_format,
            pt((r.left + r.right - w) / 2.0, (r.top + r.bottom - h) / 2.0),
            rgba(1.0, 1.0, 1.0, 0.9 * opacity),
        );
    }

    /// Nectar's tooltip: black at 70%, 6 px corners, 4 x 10 padding, 12 px text.
    fn draw_label(&self, text: &str, center_x: f32, bottom: f32, alpha: f32) {
        let s = self.scale;
        let (tw, th) = self.text_size(text, &self.label_format);
        let (pad_x, pad_y) = (10.0 * s, 4.0 * s);
        let k = 0.95 + 0.05 * alpha;
        let width = (tw + pad_x * 2.0) * k;
        let height = (th + pad_y * 2.0) * k;
        let max_left = self.surface.width as f32 - width - 4.0 * s;
        let left = (center_x - width / 2.0).clamp(4.0 * s, max_left.max(4.0 * s));
        let top = bottom - height;
        self.fill_rounded(rect(left, top, left + width, bottom), 6.0 * s, rgba(0.0, 0.0, 0.0, 0.7 * alpha));
        self.draw_text(
            text,
            &self.label_format,
            pt(left + (width - tw) / 2.0, top + (height - th) / 2.0),
            rgba(1.0, 1.0, 1.0, alpha),
        );
    }

    // ---- input -------------------------------------------------------------

    fn on_mouse_move(&mut self, x: f32, y: f32) {
        if self.mouse.is_none() {
            let mut track = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut track);
                let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
            }
            self.lingering = false;
        }
        self.mouse = Some((x, y));
        self.drag_to(x);
        self.update_visibility();
        self.update_targets();
    }

    // ---- dragging ------------------------------------------------------------

    fn on_down(&mut self, x: f32, y: f32) {
        self.pressed = self.hit_test(x, y);
        if let Some(Hit::Item(i)) = self.pressed {
            if self.items[i].kind == Kind::App {
                let layout = self.layout(true);
                let slot = &layout.slots[i];
                self.drag = Some(Drag {
                    key: self.items[i].key.clone(),
                    start_x: x,
                    x,
                    grab: ((x - slot.x) / slot.size.max(1.0)).clamp(0.0, 1.0),
                    active: false,
                });
            }
        }
        self.start_animation();
    }

    /// Follows the mouse; returns whether a drag is under way.
    fn drag_to(&mut self, x: f32) -> bool {
        let threshold = 6.0 * self.scale;
        let Some(drag) = &mut self.drag else { return false };
        drag.x = x;
        if !drag.active {
            // A press released outside the dock never reached it.
            if unsafe { GetKeyState(VK_LBUTTON.0 as i32) } >= 0 {
                self.drag = None;
                return false;
            }
            if (x - drag.start_x).abs() < threshold {
                return false;
            }
            drag.active = true;
            self.pressed = None;
            self.label = None;
            crate::preview::hide_now();
            unsafe {
                SetCapture(self.hwnd);
            }
        }
        self.reorder();
        true
    }

    /// Moves the dragged icon past the neighbors its center has crossed, in its
    /// own group (pins or running apps); they slide over to make room.
    fn reorder(&mut self) {
        let Some(key) = self.drag.as_ref().map(|d| d.key.clone()) else { return };
        let Some(mut i) = self.items.iter().position(|it| it.key == key) else { return };
        let s = self.scale;
        let pitch = TILE + GAP;
        let same_group = |a: &Item, b: &Item| b.kind == Kind::App && !b.leaving && a.pinned == b.pinned;
        for _ in 0..self.items.len() {
            let layout = self.layout(false);
            let center = layout.slots[i].x + layout.slots[i].size / 2.0;
            let rest_center = |j: usize| layout.slots[j].x - self.items[j].shift.value * s + layout.slots[j].size / 2.0;
            if i + 1 < self.items.len() && same_group(&self.items[i], &self.items[i + 1]) && center > rest_center(i + 1) {
                self.items.swap(i, i + 1);
                self.items[i].shift.value += pitch;
                i += 1;
            } else if i > 0 && same_group(&self.items[i], &self.items[i - 1]) && center < rest_center(i - 1) {
                self.items.swap(i, i - 1);
                self.items[i].shift.value -= pitch;
                i -= 1;
            } else {
                break;
            }
        }
        self.start_animation();
    }

    /// Drops the dragged icon into its slot and keeps the new order.
    fn end_drag(&mut self) -> bool {
        let Some(drag) = self.drag.take() else { return false };
        if !drag.active {
            return false;
        }
        unsafe {
            let _ = ReleaseCapture();
        }
        if let Some(i) = self.items.iter().position(|it| it.key == drag.key) {
            // Settle from where it was drawn.
            let s = self.scale;
            let layout = self.layout(false);
            let size = layout.slots[i].size;
            let drawn = drag.x - drag.grab * size;
            self.drag = None;
            let rest = self.layout(false).slots[i].x - self.items[i].shift.value * s;
            self.items[i].shift = Spring { value: (drawn - rest) / s, velocity: 0.0, target: 0.0 };
            if self.items[i].pinned {
                let order: Vec<String> = self.items.iter().filter(|it| it.pinned && it.kind == Kind::App).map(|it| it.key.clone()).collect();
                settings::save_pin_order(&order);
            }
        }
        self.pressed = None;
        self.update_targets();
        self.start_animation();
        true
    }

    fn on_mouse_leave(&mut self) {
        if self.drag.as_ref().is_some_and(|d| d.active) {
            return;
        }
        self.mouse = None;
        self.pressed = None;
        if self.settings.dock_mode != DockMode::Fixed {
            // Stay a moment, like Nectar, so a slip of the mouse does not hide it.
            self.lingering = true;
            unsafe { SetTimer(Some(self.hwnd), TIMER_HIDE, HIDE_DELAY_MS, None) };
        }
        self.update_targets();
    }

    fn launch(&mut self, index: usize) {
        if let Some(target) = self.items[index].launch.clone() {
            shell::launch(&target);
        }
    }

    fn activate(&mut self, hit: Hit) {
        crate::preview::hide_now();
        match hit {
            Hit::Clock => shell::open_notifications(),
            Hit::Tray => {
                let layout = self.layout(true);
                let t = layout.tray;
                // Where the bar rests, even if it is still rising.
                let anchor = (
                    self.origin.0 + ((t.left + t.right) / 2.0) as i32,
                    self.origin.1 + layout.full.top as i32,
                );
                tray::toggle(anchor);
            }
            Hit::Item(i) => match self.items[i].kind {
                Kind::Start => shell::open_start(),
                Kind::App if self.items[i].windows.is_empty() => self.launch(i),
                Kind::App => {
                    let windows = self.items[i].windows.clone();
                    let foreground = unsafe { GetForegroundWindow() };
                    let iconic = |h: HWND| unsafe { IsIconic(h).as_bool() };
                    if windows.len() == 1 && windows[0] == foreground && !iconic(foreground) {
                        // Windows animates it into the icon (see HSHELL_GETMINRECT).
                        unsafe {
                            let _ = ShowWindow(foreground, SW_MINIMIZE);
                        }
                    } else {
                        let next = windows
                            .iter()
                            .position(|&w| w == foreground)
                            .map(|k| (k + 1) % windows.len())
                            .unwrap_or(0);
                        shell::focus(windows[next]);
                    }
                }
            },
        }
        self.schedule_refresh();
    }

    /// The icon's rectangle on screen.
    fn icon_screen_rect(&self, index: usize) -> RECT {
        let layout = self.layout(true);
        let slot = &layout.slots[index];
        let (ox, oy) = (self.origin.0 as f32, self.origin.1 as f32);
        RECT {
            left: (ox + slot.x).round() as i32,
            top: (oy + layout.icon_bottom - slot.size).round() as i32,
            right: (ox + slot.x + slot.size).round() as i32,
            bottom: (oy + layout.icon_bottom).round() as i32,
        }
    }

    /// Where a window's minimize and restore animations should go: its icon.
    fn minimize_target(&self, hwnd: HWND) -> Option<RECT> {
        if self.hidden_for_fullscreen || !self.shown {
            return None;
        }
        let index = self.items.iter().position(|it| !it.leaving && it.windows.contains(&hwnd))?;
        Some(self.icon_screen_rect(index))
    }

    fn schedule_refresh(&self) {
        unsafe { SetTimer(Some(self.hwnd), TIMER_REFRESH, 120, None) };
    }

    fn on_poll(&mut self) {
        taskbar::keep_hidden();
        if apps::pins_signature() != self.pins_signature {
            self.reload_pins();
        }
        self.refresh_windows();
        self.update_targets();
    }

    fn shutdown(&mut self) {
        self.remove_appbar();
        taskbar::restore();
    }
}

impl Dock {
    /// `pollen --snapshot <dir>`: renders key moments of the dock's motion to PNG
    /// files without showing anything on screen.
    pub fn snapshot(dir: &str) -> Result<()> {
        let mut dock = Self::build(true)?;
        let save = |dock: &Dock, name: &str| {
            let path = format!("{dir}\\{name}.png");
            match dock.surface.save_png(&path) {
                Ok(()) => println!("saved {path}"),
                Err(e) => println!("failed {path}: {e}"),
            }
        };
        // Steps the animation in real time, so the timed parts play as they would.
        let step = |dock: &mut Dock, frames: usize| {
            for _ in 0..frames {
                std::thread::sleep(Duration::from_millis(16));
                dock.last_tick = Some(Instant::now() - Duration::from_millis(16));
                dock.tick();
            }
        };

        // The opening: drop in, flatten, spread out, icons pop in.
        dock.widen = Spring::new(0.0);
        dock.start_opening();
        for (n, frames) in [(1, 7), (2, 9), (3, 4), (4, 4), (5, 7), (6, 40)] {
            step(&mut dock, frames);
            save(&dock, &format!("1-open-{n}"));
        }

        // Hover over the sixth item.
        let layout = dock.layout(false);
        let i = 6.min(layout.slots.len() - 1);
        let slot = &layout.slots[i];
        dock.mouse = Some((slot.x + slot.size / 2.0, layout.icon_bottom - 10.0));
        dock.update_targets();
        step(&mut dock, 30);
        save(&dock, "2-hover");
        dock.mouse = None;
        dock.update_targets();
        step(&mut dock, 40);

        // Hiding: shrink into the circle and drop.
        dock.settings.dock_mode = DockMode::Peek;
        dock.lingering = false;
        dock.update_visibility();
        for (n, frames) in [(1, 4), (2, 6)] {
            step(&mut dock, frames);
            save(&dock, &format!("3-hide-{n}"));
        }
        // Showing again.
        dock.settings.dock_mode = DockMode::Fixed;
        dock.update_visibility();
        for (n, frames) in [(1, 5), (2, 6)] {
            step(&mut dock, frames);
            save(&dock, &format!("4-show-{n}"));
        }
        step(&mut dock, 40);

        // An app closing and a new one opening.
        let last = dock.items.len() - 1;
        dock.items[last].set_leaving(true);
        step(&mut dock, 6);
        save(&dock, "5-app-closing");
        step(&mut dock, 40);
        dock.items.push(Item::new(Kind::App, "test".into(), "Test".into(), String::new(), false, true));
        step(&mut dock, 14);
        save(&dock, "6-app-opening");
        step(&mut dock, 40);

        // Dragging the second pin past the next one: it follows the mouse,
        // the next pin slides over into its place.
        if let Some(i) = dock.items.iter().position(|it| it.pinned && it.kind == Kind::App).map(|i| i + 1) {
            let layout = dock.layout(true);
            let slot = &layout.slots[i];
            let x = slot.x + slot.size / 2.0;
            let key = dock.items[i].key.clone();
            dock.mouse = Some((x, layout.icon_bottom - 10.0));
            dock.drag = Some(Drag { key, start_x: x, x, grab: 0.5, active: true });
            let pitch = (TILE + GAP) * dock.scale;
            for k in 1..=6 {
                let x = x + pitch * 1.4 * k as f32 / 6.0;
                dock.mouse = Some((x, layout.icon_bottom - 10.0));
                dock.drag.as_mut().unwrap().x = x;
                dock.reorder();
                dock.update_targets();
                step(&mut dock, 1);
            }
            save(&dock, "8-drag-1");
            step(&mut dock, 30);
            save(&dock, "8-drag-2");
            dock.drag = None;
            dock.mouse = None;
            dock.update_targets();
            step(&mut dock, 40);
        }

        // Fixed mode with a maximized window: solid full width.
        dock.widen.target = 1.0;
        step(&mut dock, 50);
        save(&dock, "7-full-width");
        crate::preview::snapshot(dir)?;
        crate::notch::Notch::snapshot(dir)
    }
}

/// `POLLEN_PROFILE=1`: logs each animation frame (interval since the previous
/// one, render time) to %TEMP%\pollen-profile.txt, one burst per animation.
mod profile {
    use std::sync::Mutex;
    static FRAMES: Mutex<Vec<(f32, f32)>> = Mutex::new(Vec::new());

    fn enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("POLLEN_PROFILE").is_some())
    }

    pub fn frame(interval: Option<f32>, render: f32, last: bool) {
        if !enabled() {
            return;
        }
        let Ok(mut frames) = FRAMES.lock() else { return };
        if let Some(i) = interval {
            frames.push((i, render));
        }
        if last && !frames.is_empty() {
            let n = frames.len() as f32;
            let avg_i = frames.iter().map(|f| f.0).sum::<f32>() / n;
            let max_i = frames.iter().map(|f| f.0).fold(0.0, f32::max);
            let avg_r = frames.iter().map(|f| f.1).sum::<f32>() / n;
            let max_r = frames.iter().map(|f| f.1).fold(0.0, f32::max);
            let list: Vec<String> = frames.iter().map(|f| format!("{:.0}", f.0)).collect();
            let line = format!(
                "{} frames: interval avg {:.1} ms (max {:.1}) = {:.0} fps, render avg {:.2} ms (max {:.2}), refresh {:.2} ms
  intervals: {}
",
                frames.len(), avg_i, max_i, 1000.0 / avg_i, avg_r, max_r, crate::pacer::refresh_ms(), list.join(" ")
            );
            let (wakes, skips, wait) = crate::pacer::take_stats();
            let line = format!("{line}  pacer: {wakes} wake-ups, {skips} skipped (frame still pending), avg wait {wait:.2} ms
");
            if let Some(dir) = std::env::var_os("TEMP") {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(std::path::Path::new(&dir).join("pollen-profile.txt"))
                {
                    let _ = f.write_all(line.as_bytes());
                }
            }
            frames.clear();
        }
    }
}

fn text_format(dwrite: &IDWriteFactory, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<IDWriteTextFormat> {
    unsafe {
        dwrite.CreateTextFormat(
            w!("Segoe UI Variable Text"),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("tr-tr"),
        )
    }
}

/// Draws an icon centered in its box so its visible part fills ICON_FILL of it,
/// whatever padding the app's icon file has.
fn draw_icon(rt: &ID2D1DCRenderTarget, icon: &IconBitmap, r: D2D_RECT_F, opacity: f32) {
    let (bw, bh) = (r.right - r.left, r.bottom - r.top);
    let [x0, y0, x1, y1] = icon.content;
    let content = (x1 - x0).max(y1 - y0).max(1.0);
    let (kx, ky) = (bw * ICON_FILL / content, bh * ICON_FILL / content);
    let cx = (r.left + r.right) / 2.0;
    let cy = (r.top + r.bottom) / 2.0;
    let left = cx - (x0 + x1) / 2.0 * kx;
    let top = cy - (y0 + y1) / 2.0 * ky;
    let dest = rect(left, top, left + icon.width * kx, top + icon.height * ky);
    unsafe {
        rt.DrawBitmap(&icon.bitmap, Some(&dest), opacity, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
    }
}

fn is_fullscreen(hwnd: HWND, monitor: &RECT) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    unsafe {
        let mut class = [0u16; 64];
        let len = GetClassNameW(hwnd, &mut class).max(0) as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        // The desktop also covers the whole monitor.
        if matches!(class.as_str(), "Progman" | "WorkerW") {
            return false;
        }
        // A maximized app window spills a few pixels past the monitor when nothing
        // reserves the bottom strip (Smart, Peek); that is not fullscreen. Games in
        // borderless "fullscreen" are maximized too, but have neither a caption
        // nor a sizing border.
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let framed = style & (WS_CAPTION.0 | WS_THICKFRAME.0) != 0;
        if IsZoomed(hwnd).as_bool() && framed {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return false;
        }
        r.left <= monitor.left && r.top <= monitor.top && r.right >= monitor.right && r.bottom >= monitor.bottom
    }
}

/// Store apps are hosted by ApplicationFrameHost.exe, so they are told apart by AUMID.
fn window_key(window: &AppWindow) -> String {
    if window.exe.ends_with("\\applicationframehost.exe") || window.exe.is_empty() {
        if let Some(aumid) = &window.aumid {
            return format!("aumid:{}", aumid.to_lowercase());
        }
    }
    if window.exe.is_empty() {
        format!("hwnd:{}", window.hwnd.0 as usize)
    } else {
        format!("exe:{}", window.exe)
    }
}

fn mouse_point(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as u16 as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as f32;
    (x, y)
}

/// A top-level window moved, was maximized, restored or minimized: re-check after
/// things settle. Called a lot (every cursor move), so it stays tiny.
unsafe extern "system" fn win_event(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object != 0 || id_child != 0 || hwnd.0.is_null() || GetAncestor(hwnd, GA_ROOT) != hwnd {
        return;
    }
    let dock = HWND(DOCK_HWND.load(Ordering::Relaxed) as *mut _);
    if !dock.0.is_null() {
        SetTimer(Some(dock), TIMER_WINDOWS_MOVED, 80, None);
    }
}

unsafe fn show_menu(dock: *mut Dock, hit: Option<Hit>) {
    crate::preview::hide_now();
    let hwnd = (*dock).hwnd;
    let mode = (*dock).settings.dock_mode;
    let index = match hit {
        Some(Hit::Item(i)) if (&(*dock).items)[i].kind == Kind::App => Some(i),
        _ => None,
    };
    let (can_open, running) = match index {
        Some(i) => {
            let item = &(&(*dock).items)[i];
            (item.launch.is_some(), !item.windows.is_empty())
        }
        None => (false, false),
    };

    let Ok(menu) = CreatePopupMenu() else { return };
    if can_open {
        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, if running { w!("Yeni pencere") } else { w!("Aç") });
    }
    if running {
        let _ = AppendMenuW(menu, MF_STRING, CMD_CLOSE, w!("Pencereleri kapat"));
    }
    if can_open || running {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    }
    let check = |m: DockMode| if mode == m { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, check(DockMode::Fixed), CMD_MODE_FIXED, w!("Sabit"));
    let _ = AppendMenuW(menu, check(DockMode::Smart), CMD_MODE_SMART, w!("Akıllı (pencere gelince gizlen)"));
    let _ = AppendMenuW(menu, check(DockMode::Peek), CMD_MODE_PEEK, w!("Göz at (kenara gelince göster)"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("Pollen'dan çık"));

    let mut cursor = POINT::default();
    let _ = GetCursorPos(&mut cursor);
    let _ = SetForegroundWindow(hwnd);
    let command = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_CENTERALIGN,
        cursor.x,
        cursor.y,
        None,
        hwnd,
        None,
    );
    let _ = DestroyMenu(menu);
    let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));

    // The menu ran its own message loop; take a fresh reference.
    let dock = &mut *dock;
    match (command.0 as usize, index) {
        (CMD_OPEN, Some(i)) if i < dock.items.len() => dock.launch(i),
        (CMD_CLOSE, Some(i)) if i < dock.items.len() => {
            let windows = dock.items[i].windows.clone();
            shell::close(&windows);
            dock.schedule_refresh();
        }
        (CMD_MODE_FIXED, _) => dock.set_mode(DockMode::Fixed),
        (CMD_MODE_SMART, _) => dock.set_mode(DockMode::Smart),
        (CMD_MODE_PEEK, _) => dock.set_mode(DockMode::Peek),
        (CMD_QUIT, _) => {
            let _ = DestroyWindow(hwnd);
        }
        _ => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Dock;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    if msg == (*ptr).shell_message && msg != 0 {
        let dock = &mut *ptr;
        if wparam.0 & 0xFFFF == HSHELL_GETMINRECT && lparam.0 != 0 {
            // lparam points at a SHELLHOOKINFO { hwnd, rc }.
            #[repr(C)]
            struct ShellHookInfo {
                hwnd: HWND,
                rc: RECT,
            }
            let info = &mut *(lparam.0 as *mut ShellHookInfo);
            if let Some(rect) = dock.minimize_target(info.hwnd) {
                info.rc = rect;
                return LRESULT(1);
            }
            return LRESULT(0);
        }
        match wparam.0 & 0xFFFF {
            HSHELL_WINDOWACTIVATED | HSHELL_RUDEAPPACTIVATED => {
                dock.foreground = HWND(lparam.0 as *mut _);
                dock.update_fullscreen();
                dock.render();
                dock.schedule_refresh();
            }
            HSHELL_WINDOWCREATED | HSHELL_WINDOWDESTROYED | HSHELL_WINDOWREPLACED | HSHELL_REDRAW => {
                dock.schedule_refresh();
            }
            _ => {}
        }
        return LRESULT(0);
    }

    if msg == (*ptr).taskbar_created_message && msg != 0 {
        // Explorer restarted: its taskbar is back and our reservation is gone.
        let dock = &mut *ptr;
        taskbar::hide();
        taskbar::watch();
        dock.appbar_registered = false;
        dock.apply_mode();
        dock.reload_pins();
        dock.refresh_windows();
        dock.render();
        return LRESULT(0);
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
        WM_CAPTURECHANGED => {
            let dock = &mut *ptr;
            if dock.drag.as_ref().is_some_and(|d| d.active) && HWND(lparam.0 as *mut _) != hwnd {
                dock.end_drag();
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = mouse_point(lparam);
            let dock = &mut *ptr;
            if dock.end_drag() {
                return LRESULT(0);
            }
            let hit = dock.hit_test(x, y);
            let pressed = dock.pressed.take();
            if let Some(hit) = hit.filter(|h| Some(*h) == pressed) {
                dock.activate(hit);
            }
            (*ptr).start_animation();
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = mouse_point(lparam);
            let hit = (*ptr).hit_test(x, y);
            show_menu(ptr, hit);
            LRESULT(0)
        }
        WM_TIMER => {
            let dock = &mut *ptr;
            match wparam.0 {
                TIMER_REFRESH => {
                    let _ = KillTimer(Some(hwnd), TIMER_REFRESH);
                    dock.refresh_windows();
                    dock.update_targets();
                }
                TIMER_WINDOWS_MOVED => {
                    let _ = KillTimer(Some(hwnd), TIMER_WINDOWS_MOVED);
                    dock.update_fullscreen();
                    dock.update_window_state();
                }
                TIMER_PREVIEW => {
                    let _ = KillTimer(Some(hwnd), TIMER_PREVIEW);
                    if let Some(key) = dock.preview_for.clone() {
                        dock.show_preview(&key);
                    }
                }
                TIMER_HIDE => {
                    let _ = KillTimer(Some(hwnd), TIMER_HIDE);
                    dock.lingering = false;
                    dock.update_visibility();
                }
                TIMER_CLOCK => {
                    dock.update_clock();
                    dock.render();
                }
                TIMER_POLL => dock.on_poll(),
                _ => {}
            }
            LRESULT(0)
        }
        pacer::WM_FRAME => {
            pacer::frame_done(hwnd);
            (*ptr).tick();
            LRESULT(0)
        }
        WM_PRESS_TRAY => {
            (*ptr).activate(Hit::Tray);
            LRESULT(0)
        }
        WM_PREVIEW_LEFT => {
            let dock = &mut *ptr;
            if dock.mouse.is_none() && dock.settings.dock_mode != DockMode::Fixed {
                dock.lingering = true;
                SetTimer(Some(hwnd), TIMER_HIDE, HIDE_DELAY_MS, None);
            }
            LRESULT(0)
        }
        WM_SETTINGS_CHANGED => {
            (*ptr).reload_settings();
            LRESULT(0)
        }
        WM_APPBAR => {
            let dock = &mut *ptr;
            match wparam.0 {
                ABN_POSCHANGED => dock.update_appbar(true),
                ABN_FULLSCREENAPP => {
                    dock.foreground = GetForegroundWindow();
                    dock.update_fullscreen();
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE | WM_DPICHANGED => {
            let dock = &mut *ptr;
            if msg == WM_DISPLAYCHANGE {
                taskbar::keep_hidden();
            }
            dock.place();
            dock.update_window_state();
            dock.render();
            if msg == WM_DPICHANGED {
                LRESULT(0)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                (*ptr).shutdown();
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            (*ptr).shutdown();
            DOCK_HWND.store(0, Ordering::Relaxed);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
