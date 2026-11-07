use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::PCWSTR;

static DEMO: AtomicBool = AtomicBool::new(false);

/// `pollen --demo-shots`: made-up data and transparent backgrounds, for
/// screenshots that show nothing of the user's own.
pub fn demo() -> bool {
    DEMO.load(Ordering::Relaxed)
}

pub fn set_demo() {
    DEMO.store(true, Ordering::Relaxed);
}

/// A NUL-terminated UTF-16 copy of `s`.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// UTF-16 from a fixed buffer, stopping at the first NUL.
pub fn from_wide_buf(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

pub fn pcwstr(buf: &[u16]) -> PCWSTR {
    PCWSTR(buf.as_ptr())
}
