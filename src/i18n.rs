//! Turkish and English. The language is the Settings tab's choice; "Auto"
//! follows Windows' display language (Turkish there, English otherwise).
//!
//! Texts sit next to their use as `t("Türkçe", "English")` pairs.

use std::sync::OnceLock;

use crate::settings::{self, Language};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Lang {
    Turkish,
    English,
}

fn windows_language() -> Lang {
    static SYSTEM: OnceLock<Lang> = OnceLock::new();
    *SYSTEM.get_or_init(|| {
        let id = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
        // The primary language is the low 10 bits; LANG_TURKISH is 0x1f.
        if id & 0x3ff == 0x1f {
            Lang::Turkish
        } else {
            Lang::English
        }
    })
}

pub fn lang() -> Lang {
    match settings::get().language {
        Language::Turkish => Lang::Turkish,
        Language::English => Lang::English,
        Language::Auto => windows_language(),
    }
}

/// The text in the current language.
pub fn t(turkish: &'static str, english: &'static str) -> &'static str {
    match lang() {
        Lang::Turkish => turkish,
        Lang::English => english,
    }
}

pub fn is_turkish() -> bool {
    lang() == Lang::Turkish
}

/// "%45" in Turkish, "45%" in English; `fraction` is 0..1.
pub fn percent(fraction: f32) -> String {
    let n = (fraction * 100.0).round() as i64;
    if is_turkish() {
        format!("%{n}")
    } else {
        format!("{n}%")
    }
}

/// A formatted number with Turkish's decimal comma where it applies.
pub fn decimal(text: String) -> String {
    if is_turkish() {
        text.replace('.', ",")
    } else {
        text
    }
}

/// Month names, January first.
pub fn months() -> [&'static str; 12] {
    if is_turkish() {
        ["Ocak", "Şubat", "Mart", "Nisan", "Mayıs", "Haziran", "Temmuz", "Ağustos", "Eylül", "Ekim", "Kasım", "Aralık"]
    } else {
        ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"]
    }
}

pub fn short_months() -> [&'static str; 12] {
    if is_turkish() {
        ["Oca", "Şub", "Mar", "Nis", "May", "Haz", "Tem", "Ağu", "Eyl", "Eki", "Kas", "Ara"]
    } else {
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
    }
}

/// Short weekday names, Monday first.
pub fn weekdays() -> [&'static str; 7] {
    if is_turkish() {
        ["Pzt", "Sal", "Çar", "Per", "Cum", "Cmt", "Paz"]
    } else {
        ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
    }
}

/// The closed island's date: "Cmt 7 Kas" / "Sat Nov 7". `weekday` is
/// Sunday-first (as Windows gives it), `month` 1..=12.
pub fn short_date(weekday: usize, day: u32, month: u32) -> String {
    let name = weekdays()[(weekday + 6) % 7];
    let month = short_months()[(month as usize).saturating_sub(1) % 12];
    if is_turkish() {
        format!("{name} {day} {month}")
    } else {
        format!("{name} {month} {day}")
    }
}
