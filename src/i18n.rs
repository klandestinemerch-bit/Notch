//! Interface language: English (the strings in the code) or Indonesian (`id.rs`).
//!
//! Settings → General → Language picks one; "Windows" (the default) follows Windows' display language.
//! `tr("…")` translates a fixed UI string (anything not in the table stays as it is), and `tf!` picks between
//! an English and an Indonesian format string for text with values in it. What people write or receive
//! (to-dos, notes, songs, notifications, app and city names) is never passed through either.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

#[path = "i18n_id.rs"]
mod id;

/// The setting's values (stored in settings.json as `Language`).
pub const SYSTEM: &str = "";
pub const ENGLISH: &str = "en";
pub const INDONESIAN: &str = "id";

const EN: u8 = 1;
const ID: u8 = 2;

/// 0 = not worked out yet.
static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Re-reads the setting (call after settings change).
pub fn refresh() {
    let lang = match crate::services::settings::shared().language.as_str() {
        ENGLISH => EN,
        INDONESIAN => ID,
        _ => {
            if windows_is_indonesian() {
                ID
            } else {
                EN
            }
        }
    };
    CURRENT.store(lang, Ordering::Relaxed);
}

fn windows_is_indonesian() -> bool {
    // LANG_INDONESIAN = 0x21 (the primary language in the low 10 bits of the LANGID).
    let lang = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
    lang & 0x3FF == 0x21
}

/// The interface is in Indonesian.
pub fn is_id() -> bool {
    let mut c = CURRENT.load(Ordering::Relaxed);
    if c == 0 {
        refresh();
        c = CURRENT.load(Ordering::Relaxed);
    }
    c == ID
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static T: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    T.get_or_init(|| id::TABLE.iter().copied().collect())
}

/// A fixed UI string in the interface language.
pub fn tr(s: &str) -> &str {
    if !is_id() {
        return s;
    }
    table().get(s).copied().unwrap_or(s)
}

/// Whether `s` has a translation (tests).
#[cfg(test)]
pub fn known(s: &str) -> bool {
    table().contains_key(s)
}

/// Three-letter weekday (0 = Sunday) and month (1 = January) names, as the interface shows them.
pub fn day3(dow: usize) -> &'static str {
    const EN3: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const ID3: [&str; 7] = ["Min", "Sen", "Sel", "Rab", "Kam", "Jum", "Sab"];
    if is_id() { ID3[dow % 7] } else { EN3[dow % 7] }
}

pub fn day_name(dow: usize) -> &'static str {
    const ID: [&str; 7] = ["Minggu", "Senin", "Selasa", "Rabu", "Kamis", "Jumat", "Sabtu"];
    if is_id() { ID[dow % 7] } else { crate::notch::DAYS[dow % 7] }
}

/// Two-letter weekday for compact pickers (Su Mo … / Mg Sn …).
pub fn day2(dow: usize) -> &'static str {
    const EN2: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];
    const ID2: [&str; 7] = ["Mg", "Sn", "Sl", "Rb", "Km", "Jm", "Sb"];
    if is_id() { ID2[dow % 7] } else { EN2[dow % 7] }
}

pub fn month3(month: usize) -> &'static str {
    const EN3: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    const ID3: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "Mei", "Jun", "Jul", "Agu", "Sep", "Okt", "Nov", "Des"];
    let i = (month.max(1) - 1) % 12;
    if is_id() { ID3[i] } else { EN3[i] }
}

pub fn month_name(month: usize) -> &'static str {
    const ID: [&str; 12] =
        ["Januari", "Februari", "Maret", "April", "Mei", "Juni", "Juli", "Agustus", "September", "Oktober", "November", "Desember"];
    let i = (month.max(1) - 1) % 12;
    if is_id() { ID[i] } else { crate::notch::MONTHS[i] }
}

/// A weather condition (`weather::describe`) in the interface language (kept apart from the main table:
/// "Clear" the sky is not "Clear" the button).
pub fn weather(s: &str) -> &str {
    if !is_id() {
        return s;
    }
    match s {
        "Clear" => "Cerah",
        "Mainly clear" => "Cerah berawan",
        "Partly cloudy" => "Berawan sebagian",
        "Overcast" => "Mendung",
        "Fog" => "Berkabut",
        "Drizzle" => "Gerimis",
        "Rain" => "Hujan",
        "Snow" => "Salju",
        "Rain showers" => "Hujan lokal",
        "Snow showers" => "Hujan salju",
        "Thunderstorm" => "Badai petir",
        "Unknown" => "Tidak diketahui",
        _ => s,
    }
}

/// `tf!("English {x}", "Indonesian {x}", args…)`: `format!` with the string for the interface language.
macro_rules! tf {
    ($en:literal, $id:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::is_id() { format!($id $(, $arg)*) } else { format!($en $(, $arg)*) }
    };
}
pub(crate) use tf;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names() {
        CURRENT.store(ID, Ordering::Relaxed);
        assert_eq!(day3(0), "Min");
        assert_eq!(month3(8), "Agu");
        assert_eq!(month_name(12), "Desember");
        assert_eq!(tr("Today"), "Hari ini");
        assert_eq!(tr("some text nobody translated"), "some text nobody translated");
        let n = 3;
        assert_eq!(tf!("{n} events", "{n} acara"), "3 acara");
        CURRENT.store(EN, Ordering::Relaxed);
        assert_eq!(tr("Today"), "Today");
        assert_eq!(tf!("{n} events", "{n} acara"), "3 events");
    }

    #[test]
    fn no_duplicate_keys() {
        let mut seen = std::collections::HashSet::new();
        for (k, _) in id::TABLE {
            assert!(seen.insert(*k), "\"{k}\" is in the table twice");
        }
    }
}
