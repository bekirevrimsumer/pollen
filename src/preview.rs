//! Live window previews above a dock icon, like Nectar's: a card per window
//! with a DWM live thumbnail, the window's title and a close button.
//!
//! The previews are DWM thumbnails: the compositor draws the windows straight
//! into this layered window, so they stay live and cost Pollen nothing to
//! draw. Minimized windows have no live image; they show the app's icon.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Instant;

use windows::core::{w, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_MEDIUM,
};
use windows::Win32::Graphics::Dwm::{
    DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_SOURCECLIENTAREAONLY,
    DWM_TNP_VISIBLE,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetWindowLongPtrW, IsIconic, IsWindow, KillTimer, LoadCursorW,
    RegisterClassExW, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWLP_USERDATA, HWND_TOPMOST, IDC_ARROW,
    MA_NOACTIVATE, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows_numerics::Vector2;

use crate::render::Surface;
use crate::{apps, icons, pacer, settings, shell};

// Nectar's preview card (DIPs).
const ITEM_WIDTH: f32 = 208.0;
const ITEM_PAD: f32 = 8.0;
const ITEM_GAP: f32 = 6.0;
const CARD_PAD: f32 = 8.0;
const THUMB_HEIGHT: f32 = (ITEM_WIDTH - ITEM_PAD * 2.0) * 10.0 / 16.0;
const LABEL_HEIGHT: f32 = 18.0;
const LABEL_GAP: f32 = 6.0;
const ITEM_HEIGHT: f32 = ITEM_PAD + THUMB_HEIGHT + LABEL_GAP + LABEL_HEIGHT + ITEM_PAD;
const RADIUS: f32 = 18.0;
const CLOSE: f32 = 20.0;
/// An invisible strip under the card, so the mouse can cross from the icon to
/// the card without the card closing (Nectar's `::after`).
const BRIDGE: f32 = 24.0;
/// How far the card rises while fading in.
const RISE: f32 = 10.0;
const FADE_IN_MS: f32 = 150.0;
const FADE_OUT_MS: f32 = 100.0;
const HIDE_DELAY_MS: u32 = 150;

const TIMER_HIDE: usize = 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const ICON_CLOSE: char = '\u{E8BB}';


static PREVIEW_HWND: AtomicIsize = AtomicIsize::new(0);

/// What the dock asks to preview.
pub struct Request {
    /// The dock item's key.
    pub key: String,
    pub name: String,
    pub icon_source: String,
    pub windows: Vec<HWND>,
    /// Screen point the card centers on, horizontally, and sits above.
    pub anchor: (i32, i32),
    /// The monitor's work area the card must stay inside.
    pub monitor: RECT,
}

struct Entry {
    hwnd: HWND,
    title: String,
    /// The DWM thumbnail, 0 when there is none (minimized).
    thumb: isize,
    /// The source window's size, for fitting the thumbnail.
    source: (f32, f32),
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Item(usize),
    Close(usize),
}

struct Preview {
    hwnd: HWND,
    surface: Surface,
    brush: ID2D1SolidColorBrush,
    dwrite: IDWriteFactory,
    /// Text layouts kept between frames.
    texts: crate::text::TextCache,
    title_format: IDWriteTextFormat,
    close_format: IDWriteTextFormat,
    scale: f32,
    key: Option<String>,
    name: String,
    icon_source: String,
    icon: Option<ID2D1Bitmap>,
    entries: Vec<Entry>,
    origin: (i32, i32),
    /// 0..1, and where it is heading.
    fade: f32,
    fade_from: (f32, Instant),
    fade_target: f32,
    animating: bool,
    mouse: Option<(f32, f32)>,
    pressed: Option<Hit>,
}

fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
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

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F { left, top, right, bottom }
}

fn contains(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn text_format(dwrite: &IDWriteFactory, family: windows::core::PCWSTR, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<IDWriteTextFormat> {
    unsafe {
        dwrite.CreateTextFormat(
            family,
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("tr-TR"),
        )
    }
}

fn instance() -> Option<&'static mut Preview> {
    let hwnd = PREVIEW_HWND.load(Ordering::Relaxed);
    if hwnd == 0 {
        return None;
    }
    unsafe {
        let ptr = GetWindowLongPtrW(HWND(hwnd as *mut _), GWLP_USERDATA) as *mut Preview;
        ptr.as_mut()
    }
}

/// Shows the previews for an app, or switches to it if another app's are up.
/// The same app again just keeps them up.
pub fn show(request: Request) {
    if instance().is_none() && create().is_err() {
        return;
    }
    let Some(p) = instance() else { return };
    p.show(request);
}

/// Hides after a short grace period, unless the mouse reaches the card (or the
/// icon asks again) first.
pub fn hide_soon() {
    if let Some(p) = instance() {
        if p.key.is_some() && p.mouse.is_none() {
            unsafe { SetTimer(Some(p.hwnd), TIMER_HIDE, HIDE_DELAY_MS, None) };
        }
    }
}

pub fn hide_now() {
    if let Some(p) = instance() {
        p.hide();
    }
}

/// The dock item whose previews are up.
pub fn shown_key() -> Option<String> {
    instance().and_then(|p| if p.fade_target > 0.5 { p.key.clone() } else { None })
}

/// Whether the mouse is over the card (the dock stays up meanwhile).
pub fn is_hovered() -> bool {
    instance().is_some_and(|p| p.mouse.is_some())
}

fn create() -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: w!("PollenPreview"),
            ..Default::default()
        };
        RegisterClassExW(&class);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("PollenPreview"),
            w!("Pollen previews"),
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
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let surface = Surface::new(&factory, 1, 1)?;
        surface.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        surface.rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let brush = surface.rt.CreateSolidColorBrush(&rgba(1.0, 1.0, 1.0, 1.0), None)?;
        let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let preview = Box::new(Preview {
            hwnd,
            surface,
            brush,
            title_format: text_format(&dwrite, w!("Segoe UI Variable Text"), 12.0, DWRITE_FONT_WEIGHT_MEDIUM)?,
            close_format: text_format(&dwrite, w!("Segoe Fluent Icons"), 8.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            dwrite,
            texts: Default::default(),
            scale: 0.0,
            key: None,
            name: String::new(),
            icon_source: String::new(),
            icon: None,
            entries: Vec::new(),
            origin: (0, 0),
            fade: 0.0,
            fade_from: (0.0, Instant::now()),
            fade_target: 0.0,
            animating: false,
            mouse: None,
            pressed: None,
        });
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(preview) as isize);
        PREVIEW_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
    }
    Ok(())
}

impl Preview {
    fn show(&mut self, request: Request) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
        }
        let same = self.key.as_deref() == Some(request.key.as_str()) && self.fade_target > 0.5;
        if same {
            return;
        }
        let was_up = self.fade_target > 0.5;
        self.set_scale(request.anchor);
        self.clear_thumbnails();
        if self.icon_source != request.icon_source {
            self.icon = None;
            self.icon_source = request.icon_source.clone();
        }
        self.key = Some(request.key);
        self.name = request.name;
        self.entries = request
            .windows
            .iter()
            .map(|&hwnd| Entry { hwnd, title: apps::window_title(hwnd), thumb: 0, source: (16.0, 10.0) })
            .collect();
        if self.icon.is_none() {
            self.icon = self.load_icon();
        }

        // Size and place the window: the card, centered over the anchor and
        // kept inside the monitor, with the bridge below it.
        let s = self.scale;
        let m = request.monitor;
        let fit = (((m.right - m.left) as f32 - 16.0 * s - CARD_PAD * 2.0 * s + ITEM_GAP * s) / ((ITEM_WIDTH + ITEM_GAP) * s))
            .floor()
            .max(1.0) as usize;
        self.entries.truncate(fit);
        let (w, h) = self.size();
        let x = (request.anchor.0 - w / 2).min(m.right - 8 - w).max(m.left + 8);
        let y = request.anchor.1 - h;
        self.origin = (x, y);
        let _ = self.surface.resize(w, h);
        self.register_thumbnails();

        if !was_up {
            self.fade_from = (self.fade, Instant::now());
        }
        self.fade_target = 1.0;
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        self.render();
        self.start_animation();
    }

    fn hide(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
        }
        if self.fade_target == 0.0 {
            return;
        }
        self.fade_target = 0.0;
        self.fade_from = (self.fade, Instant::now());
        self.mouse = None;
        self.pressed = None;
        self.start_animation();
    }

    fn set_scale(&mut self, anchor: (i32, i32)) {
        let mut dpi = (96u32, 96u32);
        unsafe {
            let monitor = MonitorFromPoint(POINT { x: anchor.0, y: anchor.1 }, MONITOR_DEFAULTTONEAREST);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi.0, &mut dpi.1);
        }
        let scale = dpi.0 as f32 / 96.0;
        if (scale - self.scale).abs() > f32::EPSILON {
            self.scale = scale;
            self.icon = None;
            if let (Ok(title), Ok(close)) = (
                text_format(&self.dwrite, w!("Segoe UI Variable Text"), 12.0 * scale, DWRITE_FONT_WEIGHT_MEDIUM),
                text_format(&self.dwrite, w!("Segoe Fluent Icons"), 8.0 * scale, DWRITE_FONT_WEIGHT_NORMAL),
            ) {
                self.title_format = title;
                self.close_format = close;
            }
        }
    }

    fn load_icon(&self) -> Option<ID2D1Bitmap> {
        let icon = icons::load(&self.icon_source, (48.0 * self.scale).ceil() as i32)?;
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        unsafe {
            self.surface
                .rt
                .CreateBitmap(
                    D2D_SIZE_U { width: icon.width, height: icon.height },
                    Some(icon.pixels.as_ptr() as *const _),
                    icon.width * 4,
                    &props,
                )
                .ok()
        }
    }

    /// The window: the card plus the bridge under it.
    fn size(&self) -> (i32, i32) {
        let s = self.scale;
        let n = self.entries.len().max(1) as f32;
        let w = (n * ITEM_WIDTH + (n - 1.0) * ITEM_GAP + CARD_PAD * 2.0) * s;
        let h = (ITEM_HEIGHT + CARD_PAD * 2.0 + BRIDGE) * s;
        (w.ceil() as i32, h.ceil() as i32)
    }

    /// How far the card is lowered by the fade-in.
    fn offset(&self) -> f32 {
        if self.fade_target > 0.5 {
            (1.0 - self.fade) * RISE * self.scale
        } else {
            0.0
        }
    }

    fn card(&self) -> D2D_RECT_F {
        let (w, _) = self.size();
        let s = self.scale;
        let dy = self.offset();
        rect(0.0, dy, w as f32, dy + (ITEM_HEIGHT + CARD_PAD * 2.0) * s)
    }

    fn item_rect(&self, i: usize) -> D2D_RECT_F {
        let s = self.scale;
        let card = self.card();
        let x = card.left + CARD_PAD * s + i as f32 * (ITEM_WIDTH + ITEM_GAP) * s;
        let y = card.top + CARD_PAD * s;
        rect(x, y, x + ITEM_WIDTH * s, y + ITEM_HEIGHT * s)
    }

    fn thumb_rect(&self, i: usize) -> D2D_RECT_F {
        let s = self.scale;
        let r = self.item_rect(i);
        rect(r.left + ITEM_PAD * s, r.top + ITEM_PAD * s, r.right - ITEM_PAD * s, r.top + (ITEM_PAD + THUMB_HEIGHT) * s)
    }

    /// At the end of the title row: Windows draws the live thumbnail over the
    /// card, so nothing of ours may sit on the thumbnail.
    fn close_rect(&self, i: usize) -> D2D_RECT_F {
        let s = self.scale;
        let r = self.item_rect(i);
        let cy = self.thumb_rect(i).bottom + (LABEL_GAP + LABEL_HEIGHT / 2.0) * s;
        let right = r.right - (ITEM_PAD - 2.0) * s;
        rect(right - CLOSE * s, cy - CLOSE * s / 2.0, right, cy + CLOSE * s / 2.0)
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        (0..self.entries.len()).find_map(|i| {
            if contains(&self.close_rect(i), x, y) {
                Some(Hit::Close(i))
            } else if contains(&self.item_rect(i), x, y) {
                Some(Hit::Item(i))
            } else {
                None
            }
        })
    }

    // ---- thumbnails ------------------------------------------------------------

    fn register_thumbnails(&mut self) {
        for entry in &mut self.entries {
            unsafe {
                if !IsWindow(Some(entry.hwnd)).as_bool() || IsIconic(entry.hwnd).as_bool() {
                    continue;
                }
                if let Ok(thumb) = DwmRegisterThumbnail(self.hwnd, entry.hwnd) {
                    entry.thumb = thumb;
                    if let Ok(size) = DwmQueryThumbnailSourceSize(thumb) {
                        if size.cx > 0 && size.cy > 0 {
                            entry.source = (size.cx as f32, size.cy as f32);
                        }
                    }
                }
            }
        }
        self.update_thumbnails();
    }

    /// Places the thumbnails (fitted inside their boxes) at the current fade.
    fn update_thumbnails(&self) {
        let opacity = (self.fade.clamp(0.0, 1.0) * 255.0).round() as u8;
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.thumb == 0 {
                continue;
            }
            let b = self.thumb_rect(i);
            let (bw, bh) = (b.right - b.left, b.bottom - b.top);
            let k = (bw / entry.source.0).min(bh / entry.source.1);
            let (w, h) = (entry.source.0 * k, entry.source.1 * k);
            let left = (b.left + (bw - w) / 2.0).round() as i32;
            let top = (b.top + (bh - h) / 2.0).round() as i32;
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
                rcDestination: RECT { left, top, right: left + w.round() as i32, bottom: top + h.round() as i32 },
                opacity,
                fVisible: (opacity > 0).into(),
                fSourceClientAreaOnly: false.into(),
                ..Default::default()
            };
            unsafe {
                let _ = DwmUpdateThumbnailProperties(entry.thumb, &props);
            }
        }
    }

    fn clear_thumbnails(&mut self) {
        for entry in &mut self.entries {
            if entry.thumb != 0 {
                unsafe {
                    let _ = DwmUnregisterThumbnail(entry.thumb);
                }
                entry.thumb = 0;
            }
        }
    }

    // ---- motion ------------------------------------------------------------------

    fn start_animation(&mut self) {
        if !self.animating {
            self.animating = true;
            pacer::start(self.hwnd);
        }
    }

    fn tick(&mut self) {
        let (from, at) = self.fade_from;
        let duration = if self.fade_target > 0.5 { FADE_IN_MS } else { FADE_OUT_MS };
        let t = at.elapsed().as_secs_f32() * 1000.0 / duration;
        self.fade = from + (self.fade_target - from) * ease_out(t);
        let done = t >= 1.0;
        if done {
            self.fade = self.fade_target;
        }
        self.render();
        self.update_thumbnails();
        if done {
            self.animating = false;
            pacer::stop(self.hwnd);
            if self.fade_target == 0.0 {
                self.clear_thumbnails();
                self.entries.clear();
                self.key = None;
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
            }
        }
    }

    // ---- drawing -------------------------------------------------------------------

    fn fill_rounded(&self, r: D2D_RECT_F, radius: f32, color: D2D1_COLOR_F) {
        unsafe {
            self.brush.SetColor(&color);
            self.surface.rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &self.brush);
        }
    }

    fn text_size(&self, text: &str, format: &IDWriteTextFormat) -> (f32, f32) {
        self.texts.size(&self.dwrite, text, format)
    }

    fn text(&self, text: &str, format: &IDWriteTextFormat, x: f32, y: f32, color: D2D1_COLOR_F, max_width: f32) {
        let Some(layout) = self.texts.line(&self.dwrite, text, format, max_width) else { return };
        unsafe {
            self.brush.SetColor(&color);
            self.surface.rt.DrawTextLayout(Vector2 { X: x, Y: y }, &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
    }

    fn draw_icon(&self, r: D2D_RECT_F, opacity: f32) {
        if let Some(icon) = &self.icon {
            unsafe {
                self.surface.rt.DrawBitmap(icon, Some(&r), opacity, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            }
        }
    }

    fn render_only(&mut self) {
        let s = self.scale;
        let a = self.fade.clamp(0.0, 1.0);
        let ink = |o: f32| ink_color(o * a);
        let hovered = self.mouse.and_then(|(x, y)| self.hit(x, y));
        let rt = self.surface.rt.clone();
        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&rgba(0.0, 0.0, 0.0, 0.0)));
            // The bridge: invisible, but it keeps the mouse "inside".
            let (w, h) = (self.surface.width as f32, self.surface.height as f32);
            let card = self.card();
            self.brush.SetColor(&rgba(0.0, 0.0, 0.0, 1.0 / 255.0));
            rt.FillRectangle(&rect(w * 0.3, card.bottom, w * 0.7, h), &self.brush);

            let (r, g, b) = surface();
            let opacity = (settings::get().opacity + 0.05).min(1.0);
            self.fill_rounded(card, RADIUS * s, rgba(r, g, b, opacity * a));
            self.brush.SetColor(&ink(0.12));
            let edge = rect(card.left + 0.5, card.top + 0.5, card.right - 0.5, card.bottom - 0.5);
            rt.DrawRoundedRectangle(
                &D2D1_ROUNDED_RECT { rect: edge, radiusX: RADIUS * s, radiusY: RADIUS * s },
                &self.brush,
                1.0,
                None,
            );

            for (i, entry) in self.entries.iter().enumerate() {
                let item = self.item_rect(i);
                let item_hot = matches!(hovered, Some(Hit::Item(j) | Hit::Close(j)) if j == i);
                let pressed = self.pressed == Some(Hit::Item(i));
                if item_hot {
                    self.fill_rounded(item, 12.0 * s, ink(if pressed { 0.16 } else { 0.10 }));
                }
                let thumb = self.thumb_rect(i);
                self.fill_rounded(thumb, 8.0 * s, rgba(0.0, 0.0, 0.0, 0.45 * a));
                if entry.thumb == 0 {
                    let size = 40.0 * s;
                    let (cx, cy) = ((thumb.left + thumb.right) / 2.0, (thumb.top + thumb.bottom) / 2.0);
                    self.draw_icon(rect(cx - size / 2.0, cy - size / 2.0, cx + size / 2.0, cy + size / 2.0), a);
                }

                // The label: the app's icon and the window's title.
                let ly = thumb.bottom + LABEL_GAP * s;
                let icon = 16.0 * s;
                self.draw_icon(rect(item.left + ITEM_PAD * s, ly + 1.0 * s, item.left + ITEM_PAD * s + icon, ly + 1.0 * s + icon), a);
                let title = if entry.title.is_empty() { &self.name } else { &entry.title };
                let tx = item.left + ITEM_PAD * s + icon + 6.0 * s;
                let (_, th) = self.text_size(title, &self.title_format);
                let room = self.close_rect(i).left - 4.0 * s - tx;
                self.text(title, &self.title_format, tx, ly + (LABEL_HEIGHT * s - th) / 2.0, ink(0.9), room);

                // The close button shows on the hovered card.
                if item_hot {
                    let c = self.close_rect(i);
                    let close_hot = hovered == Some(Hit::Close(i));
                    let bg = if close_hot { rgba(239.0 / 255.0, 68.0 / 255.0, 68.0 / 255.0, a) } else { ink(0.14) };
                    self.fill_rounded(c, CLOSE * s / 2.0, bg);
                    let glyph = ICON_CLOSE.to_string();
                    let (gw, gh) = self.text_size(&glyph, &self.close_format);
                    self.text(
                        &glyph,
                        &self.close_format,
                        (c.left + c.right - gw) / 2.0,
                        (c.top + c.bottom - gh) / 2.0,
                        if close_hot { pure_white(a) } else { ink(0.8) },
                        gw + 4.0,
                    );
                }
            }
            let _ = rt.EndDraw(None, None);
        }
    }

    fn render(&mut self) {
        self.render_only();
        let _ = self.surface.present(self.hwnd, self.origin.0, self.origin.1, None);
    }

    // ---- input ---------------------------------------------------------------------

    fn on_mouse_move(&mut self, x: f32, y: f32) {
        if self.fade_target == 0.0 {
            return;
        }
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
        }
        let before = self.mouse.and_then(|(x, y)| self.hit(x, y));
        self.mouse = Some((x, y));
        if self.hit(x, y) != before && !self.animating {
            self.render();
        }
    }

    fn on_mouse_leave(&mut self) {
        self.mouse = None;
        self.pressed = None;
        if !self.animating && self.fade_target > 0.5 {
            self.render();
        }
        hide_soon();
        crate::dock::preview_left();
    }

    fn on_up(&mut self, x: f32, y: f32, middle: bool) {
        let hit = self.hit(x, y);
        let pressed = self.pressed.take();
        if !middle && (hit.is_none() || hit != pressed) {
            self.render();
            return;
        }
        match hit {
            Some(Hit::Item(i)) if middle => self.close(i),
            Some(Hit::Item(i)) => {
                let hwnd = self.entries[i].hwnd;
                self.hide();
                shell::focus(hwnd);
            }
            Some(Hit::Close(i)) => self.close(i),
            None => {}
        }
    }

    /// Asks the window to close and drops its card.
    fn close(&mut self, i: usize) {
        let entry = self.entries.remove(i);
        shell::close(&[entry.hwnd]);
        if entry.thumb != 0 {
            unsafe {
                let _ = DwmUnregisterThumbnail(entry.thumb);
            }
        }
        if self.entries.is_empty() {
            self.hide();
            return;
        }
        // Shrink around the same center.
        let (old_w, _) = (self.surface.width, self.surface.height);
        let (w, h) = self.size();
        self.origin.0 += (old_w - w) / 2;
        let _ = self.surface.resize(w, h);
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), self.origin.0, self.origin.1, w, h, SWP_NOACTIVATE);
        }
        self.update_thumbnails();
        self.render();
    }
}

fn mouse_point(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as u16 as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as f32;
    (x, y)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Preview;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let p = &mut *ptr;
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            let (x, y) = mouse_point(lparam);
            p.on_mouse_move(x, y);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            p.on_mouse_leave();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = mouse_point(lparam);
            p.pressed = p.hit(x, y);
            p.render();
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = mouse_point(lparam);
            p.on_up(x, y, false);
            LRESULT(0)
        }
        WM_MBUTTONUP => {
            let (x, y) = mouse_point(lparam);
            p.on_up(x, y, true);
            LRESULT(0)
        }
        pacer::WM_FRAME => {
            pacer::frame_done(hwnd);
            p.tick();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_HIDE => {
            let _ = KillTimer(Some(hwnd), TIMER_HIDE);
            if p.mouse.is_none() {
                p.hide();
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// `pollen --snapshot`: the card for the first app with windows, drawn to a
/// PNG. The live thumbnails are composed by DWM on screen, so the PNG shows
/// their empty boxes.
pub fn snapshot(dir: &str) -> Result<()> {
    let windows = apps::app_windows();
    let Some(first) = windows.first() else { return Ok(()) };
    let same: Vec<HWND> = windows.iter().filter(|w| w.exe == first.exe).map(|w| w.hwnd).take(3).collect();
    create()?;
    let Some(p) = instance() else { return Ok(()) };
    p.set_scale((0, 0));
    p.icon_source = first.exe.clone();
    p.icon = p.load_icon();
    p.name = "App".into();
    p.entries = same.iter().map(|&hwnd| Entry { hwnd, title: apps::window_title(hwnd), thumb: 0, source: (16.0, 10.0) }).collect();
    let (w, h) = p.size();
    p.surface.resize(w, h)?;
    p.fade = 1.0;
    p.fade_target = 1.0;
    p.mouse = Some((p.close_rect(0).left + 2.0, p.close_rect(0).top + 2.0));
    p.render_only();
    p.surface.save_png(&format!("{dir}/preview.png"))
}
