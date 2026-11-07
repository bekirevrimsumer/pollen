//! Text layouts kept from one frame to the next. Laying a line out (shaping,
//! font fallback, glyph positions) is most of what drawing text costs, and an
//! animation redraws the same lines every frame.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::core::Interface;
use windows::Win32::Graphics::DirectWrite::{
    IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout, DWRITE_TEXT_METRICS, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};

/// Past this many layouts the cache starts over: a ticking clock or a level
/// in percent makes new strings all the time.
const LIMIT: usize = 256;
/// How wide a measured (unbounded) line may be.
const FREE_WIDTH: f32 = 4000.0;
/// Key width of an unbounded line (bounded ones are at least 1).
const FREE: u32 = 0;

#[derive(Default)]
pub struct TextCache {
    /// (text, format, max width in whole DIPs) → the layout. Each entry holds
    /// its format too, so a format's address can't be reused by another one
    /// while an entry still names it.
    layouts: RefCell<HashMap<(String, usize, u32), (IDWriteTextFormat, IDWriteTextLayout)>>,
}

impl TextCache {
    fn get(
        &self,
        dwrite: &IDWriteFactory,
        text: &str,
        format: &IDWriteTextFormat,
        width: u32,
    ) -> Option<IDWriteTextLayout> {
        let key = (text.to_owned(), format.as_raw() as usize, width);
        if let Some((_, layout)) = self.layouts.borrow().get(&key) {
            return Some(layout.clone());
        }
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let layout = unsafe {
            if width == FREE {
                dwrite.CreateTextLayout(&utf16, format, FREE_WIDTH, 200.0).ok()?
            } else {
                let layout = dwrite.CreateTextLayout(&utf16, format, width as f32, 200.0).ok()?;
                let _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                if let Ok(sign) = dwrite.CreateEllipsisTrimmingSign(format) {
                    let trimming = DWRITE_TRIMMING {
                        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                        delimiter: 0,
                        delimiterCount: 0,
                    };
                    let _ = layout.SetTrimming(&trimming, &sign);
                }
                layout
            }
        };
        let mut layouts = self.layouts.borrow_mut();
        if layouts.len() >= LIMIT {
            layouts.clear();
        }
        layouts.insert(key, (format.clone(), layout.clone()));
        Some(layout)
    }

    /// `text` laid out as wide as it needs (lines break only at line breaks).
    pub fn free(&self, dwrite: &IDWriteFactory, text: &str, format: &IDWriteTextFormat) -> Option<IDWriteTextLayout> {
        self.get(dwrite, text, format, FREE)
    }

    /// `text` on one line, cut off with an ellipsis past `max_width`.
    pub fn line(
        &self,
        dwrite: &IDWriteFactory,
        text: &str,
        format: &IDWriteTextFormat,
        max_width: f32,
    ) -> Option<IDWriteTextLayout> {
        self.get(dwrite, text, format, max_width.max(1.0).ceil() as u32)
    }

    /// The metrics of `text` laid out as wide as it needs.
    pub fn metrics(&self, dwrite: &IDWriteFactory, text: &str, format: &IDWriteTextFormat) -> DWRITE_TEXT_METRICS {
        let mut m = DWRITE_TEXT_METRICS::default();
        if let Some(layout) = self.free(dwrite, text, format) {
            unsafe {
                let _ = layout.GetMetrics(&mut m);
            }
        }
        m
    }

    /// Width and height of `text` laid out as wide as it needs.
    pub fn size(&self, dwrite: &IDWriteFactory, text: &str, format: &IDWriteTextFormat) -> (f32, f32) {
        let m = self.metrics(dwrite, text, format);
        (m.width, m.height)
    }

    /// Drops every layout (when a window closes or goes idle).
    pub fn clear(&self) {
        self.layouts.borrow_mut().clear();
    }
}
