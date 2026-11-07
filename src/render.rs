//! A software Direct2D target drawn into a DIB and shown with UpdateLayeredWindow.
//! Per-pixel alpha without a GPU device: fully transparent pixels let clicks through.

use windows::core::Result;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DCRenderTarget, ID2D1Factory, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindowIndirect, ULW_ALPHA, UPDATELAYEREDWINDOWINFO};

pub struct Surface {
    pub rt: ID2D1DCRenderTarget,
    dc: HDC,
    bitmap: HBITMAP,
    original: HGDIOBJ,
    bits: *mut u8,
    pub width: i32,
    pub height: i32,
}

impl Surface {
    pub fn new(factory: &ID2D1Factory, width: i32, height: i32) -> Result<Self> {
        unsafe {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = factory.CreateDCRenderTarget(&props)?;
            let dc = CreateCompatibleDC(None);
            let mut surface =
                Self {
                rt,
                dc,
                bitmap: HBITMAP::default(),
                original: HGDIOBJ::default(),
                bits: std::ptr::null_mut(),
                width: 0,
                height: 0,
            };
            surface.resize(width, height)?;
            Ok(surface)
        }
    }

    pub fn resize(&mut self, width: i32, height: i32) -> Result<()> {
        if width == self.width && height == self.height {
            return Ok(());
        }
        unsafe {
            let info = BITMAPINFO {
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
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(self.dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let previous = SelectObject(self.dc, bitmap.into());
            if self.bitmap.is_invalid() {
                self.original = previous;
            } else {
                let _ = DeleteObject(self.bitmap.into());
            }
            self.bitmap = bitmap;
            self.bits = bits as *mut u8;
            self.width = width;
            self.height = height;
            self.rt.BindDC(self.dc, &RECT { left: 0, top: 0, right: width, bottom: height })?;
        }
        Ok(())
    }

    /// Shows the frame. With `dirty`, only that part is sent to the compositor:
    /// everything outside it must be unchanged since the previous frame.
    pub fn present(&self, hwnd: HWND, x: i32, y: i32, dirty: Option<RECT>) -> Result<()> {
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let position = POINT { x, y };
        let size = SIZE { cx: self.width, cy: self.height };
        let source = POINT { x: 0, y: 0 };
        let dirty = dirty.map(|r| RECT {
            left: r.left.clamp(0, self.width),
            top: r.top.clamp(0, self.height),
            right: r.right.clamp(0, self.width),
            bottom: r.bottom.clamp(0, self.height),
        });
        let info = UPDATELAYEREDWINDOWINFO {
            cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
            hdcDst: HDC::default(),
            pptDst: &position,
            psize: &size,
            hdcSrc: self.dc,
            pptSrc: &source,
            crKey: COLORREF(0),
            pblend: &blend,
            dwFlags: ULW_ALPHA,
            prcDirty: dirty.as_ref().map_or(std::ptr::null(), |r| r as *const RECT),
        };
        unsafe { UpdateLayeredWindowIndirect(hwnd, &info).ok() }
    }
}

impl Surface {
    /// Writes the current frame to a PNG file (used by `pollen --snapshot`).
    pub fn save_png(&self, path: &str) -> Result<()> {
        use windows::Win32::Graphics::Gdi::GdiFlush;
        use windows::Win32::Graphics::Imaging::{
            CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
            WICBitmapEncoderNoCache,
        };
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
        unsafe {
            let _ = GdiFlush();
            let stride = self.width as u32 * 4;
            let pixels = std::slice::from_raw_parts(self.bits, (stride * self.height as u32) as usize);
            let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let bitmap = factory.CreateBitmapFromMemory(
                self.width as u32,
                self.height as u32,
                &GUID_WICPixelFormat32bppPBGRA,
                stride,
                pixels,
            )?;
            let stream = factory.CreateStream()?;
            let path_w = crate::util::wide(path);
            stream.InitializeFromFilename(crate::util::pcwstr(&path_w), 0x4000_0000)?;
            let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let mut frame = None;
            encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
            let frame = frame.ok_or_else(windows::core::Error::empty)?;
            frame.Initialize(None)?;
            frame.SetSize(self.width as u32, self.height as u32)?;
            let mut format = GUID_WICPixelFormat32bppPBGRA;
            frame.SetPixelFormat(&mut format)?;
            frame.WriteSource(&bitmap, std::ptr::null())?;
            frame.Commit()?;
            encoder.Commit()?;
        }
        Ok(())
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            if !self.original.is_invalid() {
                SelectObject(self.dc, self.original);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(self.bitmap.into());
            }
            let _ = DeleteDC(self.dc);
        }
    }
}
