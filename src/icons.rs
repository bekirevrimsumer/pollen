//! Loads app icons from the shell at a requested pixel size, and keeps them
//! on disk: asking the shell for an icon loads a good part of Explorer's
//! machinery into Pollen (several MB that stay for good), and most starts need
//! only icons seen before.

use std::ffi::c_void;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// A kept icon is asked for again after this long (an app's update may have
/// changed it; files are also checked by their write time).
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAGIC: &[u8; 4] = b"PIC1";

/// The icon of a file, shortcut or `shell:AppsFolder\<AUMID>` item.
pub fn load(parsing_name: &str, size: i32) -> Option<Icon> {
    let stamp = source_stamp(parsing_name);
    let path = cache_path(parsing_name, size);
    if let Some(icon) = path.as_ref().and_then(|p| read_cached(p, parsing_name, stamp)) {
        return Some(icon);
    }
    let icon = load_from_shell(parsing_name, size)?;
    if let Some(p) = path {
        write_cached(&p, parsing_name, stamp, &icon);
    }
    Some(icon)
}

/// `%LOCALAPPDATA%\Pollen\icons\<hash of the name>-<size>.icon`.
fn cache_path(parsing_name: &str, size: i32) -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Pollen").join("icons");
    // FNV-1a over the name, case-folded: paths come in either case.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in parsing_name.to_lowercase().bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    Some(dir.join(format!("{hash:016x}-{size}.icon")))
}

/// A file's write time and size, so a changed file's icon is read again; 0
/// for shell items (`shell:AppsFolder\...`), which only age out.
fn source_stamp(parsing_name: &str) -> u64 {
    if parsing_name.starts_with("shell:") {
        return 0;
    }
    std::fs::metadata(parsing_name)
        .ok()
        .map(|m| {
            let written = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
            written ^ m.len().rotate_left(32)
        })
        .unwrap_or(0)
}

/// File layout: magic, stamp (u64), width, height (u32), content box (4 x f32),
/// name length (u32) and name (to rule out hash collisions), then the pixels.
fn read_cached(path: &PathBuf, parsing_name: &str, stamp: u64) -> Option<Icon> {
    let age = std::fs::metadata(path).ok()?.modified().ok()?;
    if SystemTime::now().duration_since(age).map_or(true, |a| a > MAX_AGE) {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    let mut at = 0usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        let part = data.get(at..at + n)?;
        at += n;
        Some(part)
    };
    if take(4)? != MAGIC {
        return None;
    }
    let u64_at = |b: &[u8]| u64::from_le_bytes(b.try_into().unwrap_or([0; 8]));
    let u32_at = |b: &[u8]| u32::from_le_bytes(b.try_into().unwrap_or([0; 4]));
    if u64_at(take(8)?) != stamp {
        return None;
    }
    let width = u32_at(take(4)?);
    let height = u32_at(take(4)?);
    let mut content = [0f32; 4];
    for c in &mut content {
        *c = f32::from_le_bytes(take(4)?.try_into().ok()?);
    }
    let name_len = u32_at(take(4)?) as usize;
    if take(name_len)? != parsing_name.as_bytes() {
        return None;
    }
    if width == 0 || height == 0 || width > 1024 || height > 1024 {
        return None;
    }
    let pixels = take((width * height * 4) as usize)?.to_vec();
    Some(Icon { width, height, pixels, content })
}

fn write_cached(path: &PathBuf, parsing_name: &str, stamp: u64, icon: &Icon) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut data = Vec::with_capacity(48 + parsing_name.len() + icon.pixels.len());
    data.extend_from_slice(MAGIC);
    data.extend_from_slice(&stamp.to_le_bytes());
    data.extend_from_slice(&icon.width.to_le_bytes());
    data.extend_from_slice(&icon.height.to_le_bytes());
    for c in icon.content {
        data.extend_from_slice(&c.to_le_bytes());
    }
    data.extend_from_slice(&(parsing_name.len() as u32).to_le_bytes());
    data.extend_from_slice(parsing_name.as_bytes());
    data.extend_from_slice(&icon.pixels);
    // Written aside and moved in, so a reader never sees half a file.
    let partial = path.with_extension("part");
    if std::fs::write(&partial, &data).is_ok() && std::fs::rename(&partial, path).is_err() {
        let _ = std::fs::remove_file(&partial);
    }
}

fn load_from_shell(parsing_name: &str, size: i32) -> Option<Icon> {
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
