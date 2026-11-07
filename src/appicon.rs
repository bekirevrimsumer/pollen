//! Pollen's own icon in the notification area: left click opens the notch's
//! Settings tab, right click a small menu. The icon (a little flower) is drawn
//! here, so the program needs no image files.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::{CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION, NOTIFYICONDATAW,
    NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos, GetSystemMetrics,
    PostMessageW, SetForegroundWindow, TrackPopupMenu, HICON, ICONINFO, MF_CHECKED, MF_SEPARATOR, MF_STRING,
    SM_CXSMICON, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_NULL,
};

const ID: u32 = 1;

pub const CMD_SETTINGS: usize = 1;
pub const CMD_AUTOSTART: usize = 2;
pub const CMD_QUIT: usize = 3;
pub const CMD_LAUNCHER: usize = 4;
pub const CMD_UPDATE: usize = 5;

fn data(owner: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: owner, uID: ID, ..Default::default() }
}

/// Adds the icon; `message` comes back to `owner` with the clicks (version 4:
/// the event in lparam's low word).
pub fn add(owner: HWND, message: u32) {
    let mut nid = data(owner);
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
    nid.uCallbackMessage = message;
    nid.hIcon = flower(unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16));
    let tip: Vec<u16> = format!("Pollen {}", crate::install::VERSION).encode_utf16().collect();
    nid.szTip[..tip.len()].copy_from_slice(&tip);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
        if !nid.hIcon.is_invalid() {
            let _ = DestroyIcon(nid.hIcon);
        }
    }
}

pub fn remove(owner: HWND) {
    let nid = data(owner);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

/// A notification from the icon (a click on it comes back as
/// NIN_BALLOONUSERCLICK).
pub fn notify(owner: HWND, title: &str, text: &str) {
    let mut nid = data(owner);
    nid.uFlags = windows::Win32::UI::Shell::NIF_INFO;
    let title: Vec<u16> = title.encode_utf16().take(63).collect();
    let text: Vec<u16> = text.encode_utf16().take(255).collect();
    nid.szInfoTitle[..title.len()].copy_from_slice(&title);
    nid.szInfo[..text.len()].copy_from_slice(&text);
    nid.dwInfoFlags = windows::Win32::UI::Shell::NIIF_INFO;
    unsafe {
        let _ = Shell_NotifyIconW(windows::Win32::UI::Shell::NIM_MODIFY, &nid);
    }
}

/// The right-click menu at the mouse; returns the chosen command (0 for none).
pub fn menu(owner: HWND, autostart: bool) -> usize {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return 0 };
        let text = |tr: &'static str, en: &'static str| windows::core::HSTRING::from(crate::i18n::t(tr, en));
        if let Some(release) = crate::update::available() {
            let label = windows::core::HSTRING::from(format!("{} {}", crate::i18n::t("Güncelle:", "Update to"), release.version));
            let _ = AppendMenuW(menu, MF_STRING, CMD_UPDATE, &label);
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        }
        let _ = AppendMenuW(menu, MF_STRING, CMD_LAUNCHER, &text("Başlatıcı\tAlt+Boşluk", "Launcher\tAlt+Space"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_SETTINGS, &text("Ayarlar", "Settings"));
        let check = if autostart { MF_STRING | MF_CHECKED } else { MF_STRING };
        let _ = AppendMenuW(menu, check, CMD_AUTOSTART, &text("Windows ile başlat", "Start with Windows"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, &text("Pollen'dan çık", "Quit Pollen"));
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let _ = SetForegroundWindow(owner);
        let command = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, cursor.x, cursor.y, None, owner, None);
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(Some(owner), WM_NULL, Default::default(), Default::default());
        command.0 as usize
    }
}

/// A small flower: six blue petals around a warm center, antialiased.
fn flower(size: i32) -> HICON {
    let n = size as usize;
    let mut pixels = vec![0u8; n * n * 4];
    let c = size as f32 / 2.0;
    let unit = size as f32 / 16.0;
    let petal_r = 2.5 * unit;
    let ring = 4.9 * unit;
    let center_r = 2.5 * unit;
    let petals: Vec<(f32, f32)> = (0..6)
        .map(|i| {
            let a = i as f32 * std::f32::consts::PI / 3.0 - std::f32::consts::FRAC_PI_2;
            (c + ring * a.cos(), c + ring * a.sin())
        })
        .collect();
    // Coverage of a disc at a pixel center, with a one-pixel soft edge.
    let cover = |x: f32, y: f32, cx: f32, cy: f32, r: f32| ((r - ((x - cx).powi(2) + (y - cy).powi(2)).sqrt()) + 0.5).clamp(0.0, 1.0);
    for py in 0..n {
        for px in 0..n {
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            let petal = petals.iter().map(|&(cx, cy)| cover(x, y, cx, cy, petal_r)).fold(0.0f32, f32::max);
            let center = cover(x, y, c, c, center_r);
            // Petals #7aa2ff, center #ffd36b painted over them.
            let (pr, pg, pb) = (0x7a as f32, 0xa2 as f32, 0xff as f32);
            let (cr, cg, cb) = (0xff as f32, 0xd3 as f32, 0x6b as f32);
            let alpha = petal.max(center);
            if alpha <= 0.0 {
                continue;
            }
            // Premultiplied: petal color under the center's.
            let r = pr * petal * (1.0 - center) + cr * center;
            let g = pg * petal * (1.0 - center) + cg * center;
            let b = pb * petal * (1.0 - center) + cb * center;
            let i = (py * n + px) * 4;
            pixels[i] = b.round() as u8;
            pixels[i + 1] = g.round() as u8;
            pixels[i + 2] = r.round() as u8;
            pixels[i + 3] = (alpha * 255.0).round() as u8;
        }
    }
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) else { return HICON::default() };
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
        let mask = CreateBitmap(size, size, 1, 1, None);
        let icon = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let hicon = CreateIconIndirect(&icon).unwrap_or_default();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        hicon
    }
}
