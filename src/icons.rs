//! Loads app icons from the shell at a requested pixel size.

use std::ffi::c_void;

use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
};

use crate::util::{pcwstr, wide};

/// Premultiplied BGRA pixels, top-down.
pub struct Icon {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    /// Bounding box of the visible pixels: left, top, right, bottom.
    pub content: [f32; 4],
}

/// The icon of a file, shortcut or `shell:AppsFolder\<AUMID>` item.
pub fn load(parsing_name: &str, size: i32) -> Option<Icon> {
    unsafe {
        let name = wide(parsing_name);
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(pcwstr(&name), None::<&IBindCtx>).ok()?;
        let hbmp = factory
            .GetImage(SIZE { cx: size, cy: size }, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK)
            .ok()?;
        let icon = read_bitmap(hbmp);
        let _ = DeleteObject(hbmp.into());
        icon
    }
}

unsafe fn read_bitmap(hbmp: HBITMAP) -> Option<Icon> {
    let mut bm = BITMAP::default();
    if GetObjectW(hbmp.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut c_void)) == 0 {
        return None;
    }
    let (width, height) = (bm.bmWidth, bm.bmHeight.abs());
    if width <= 0 || height <= 0 {
        return None;
    }

    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let hdc = GetDC(None);
    let lines = GetDIBits(
        hdc,
        hbmp,
        0,
        height as u32,
        Some(pixels.as_mut_ptr() as *mut c_void),
        &mut info,
        DIB_RGB_COLORS,
    );
    ReleaseDC(None, hdc);
    if lines == 0 {
        return None;
    }

    // The shell hands back premultiplied or straight alpha depending on the source,
    // and no alpha at all for old 24-bit icons. Premultiplied data never has a
    // color channel brighter than its alpha.
    let mut has_alpha = false;
    let mut straight = false;
    for px in pixels.chunks_exact(4) {
        let a = px[3];
        has_alpha |= a != 0;
        straight |= px[0] > a || px[1] > a || px[2] > a;
    }
    for px in pixels.chunks_exact_mut(4) {
        if !has_alpha {
            px[3] = 255;
        } else if straight {
            let a = px[3] as u32;
            px[0] = ((px[0] as u32 * a + 127) / 255) as u8;
            px[1] = ((px[1] as u32 * a + 127) / 255) as u8;
            px[2] = ((px[2] as u32 * a + 127) / 255) as u8;
        }
    }

    let content = content_box(&pixels, width as usize, height as usize);
    Some(Icon { width: width as u32, height: height as u32, pixels, content })
}

/// Bounding box of the pixels that are clearly visible, so icons with different
/// built-in padding can be drawn at the same apparent size.
fn content_box(pixels: &[u8], width: usize, height: usize) -> [f32; 4] {
    let (mut x0, mut y0, mut x1, mut y1) = (width, height, 0usize, 0usize);
    for y in 0..height {
        for x in 0..width {
            if pixels[(y * width + x) * 4 + 3] > 24 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x1 <= x0 || y1 <= y0 {
        return [0.0, 0.0, width as f32, height as f32];
    }
    [x0 as f32, y0 as f32, x1 as f32, y1 as f32]
}
