//! A wallpaper made from a theme's colors: its background with a few large,
//! soft fields of the accent and two neighboring hues. Drawn once per theme
//! with Direct2D and saved as a PNG next to Pollen's settings.

use std::path::Path;

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAMMA_2_2,
    D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES,
};
use windows_numerics::Vector2;

use crate::render::Surface;
use crate::settings::Palette;

const WIDTH: i32 = 2560;
const HEIGHT: i32 = 1440;

fn rgba((r, g, b): (f32, f32, f32), a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

/// The accent turned around the color wheel by `degrees`.
fn turn((r, g, b): (f32, f32, f32), degrees: f32) -> (f32, f32, f32) {
    // Rotate in YIQ-like space: keeps lightness, turns the hue.
    let (c, s) = (degrees.to_radians().cos(), degrees.to_radians().sin());
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let i = 0.596 * r - 0.274 * g - 0.322 * b;
    let q = 0.211 * r - 0.523 * g + 0.312 * b;
    let (i, q) = (i * c - q * s, i * s + q * c);
    (
        (y + 0.956 * i + 0.621 * q).clamp(0.0, 1.0),
        (y - 0.272 * i - 0.647 * q).clamp(0.0, 1.0),
        (y - 1.106 * i + 1.703 * q).clamp(0.0, 1.0),
    )
}

pub fn render(path: &Path, palette: Palette) -> Result<()> {
    let factory: ID2D1Factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
    let surface = Surface::new(&factory, WIDTH, HEIGHT)?;
    let rt = &surface.rt;
    let (w, h) = (WIDTH as f32, HEIGHT as f32);
    let strength = if palette.light { 0.32 } else { 0.42 };
    // (center, radius, color, opacity), as fractions of the screen.
    let fields = [
        ((0.22, 0.28), 0.55, palette.accent, strength),
        ((0.80, 0.22), 0.50, turn(palette.accent, 40.0), strength * 0.8),
        ((0.62, 0.86), 0.62, turn(palette.accent, -70.0), strength * 0.7),
        ((0.10, 0.92), 0.40, turn(palette.accent, 140.0), strength * 0.45),
    ];
    unsafe {
        rt.BeginDraw();
        rt.Clear(Some(&rgba(palette.surface, 1.0)));
        for ((cx, cy), radius, color, opacity) in fields {
            let stops = [
                D2D1_GRADIENT_STOP { position: 0.0, color: rgba(color, opacity) },
                D2D1_GRADIENT_STOP { position: 0.55, color: rgba(color, opacity * 0.35) },
                D2D1_GRADIENT_STOP { position: 1.0, color: rgba(color, 0.0) },
            ];
            let collection = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
                center: Vector2 { X: cx * w, Y: cy * h },
                gradientOriginOffset: Vector2 { X: 0.0, Y: 0.0 },
                radiusX: radius * w,
                radiusY: radius * w,
            };
            let brush = rt.CreateRadialGradientBrush(&props, None, &collection)?;
            rt.FillRectangle(&D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: h }, &brush);
        }
        rt.EndDraw(None, None)?;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    surface.save_png(&path.display().to_string())
}
