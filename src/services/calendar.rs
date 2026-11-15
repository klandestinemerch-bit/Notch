//! Calendar events, file-compatible with the .NET build: the user's own events in `events.json` plus
//! read-only events from subscribed .ics links in `events-feed.json` (written by `refresh_feeds`; the links
//! themselves are DPAPI-protected in `calendar-feeds.bin`). To-dos with due dates are merged by the UI.
//!
//! Stored shape (System.Text.Json, PascalCase): `{"Date":"2026-11-14T00:00:00","Time":"09:30"|"",
//! "Title":"…","Feed":null|"<12 hex>"}`.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use serde_json::{json, Value};
use windows::core::{s, HSTRING};
use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Time::{
    EnumDynamicTimeZoneInformation, SystemTimeToTzSpecificLocalTimeEx, TzSpecificLocalTimeToSystemTimeEx, DYNAMIC_TIME_ZONE_INFORMATION,
};

use super::store;

const EVENTS: &str = "events.json";
const FEED_EVENTS: &str = "events-feed.json";
const LINKS: &str = "calendar-feeds.bin";
const LINKS_ENTROPY: &[u8] = b"FathNotch.CalendarFeeds";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// Stable while the event is unchanged; pass to `delete`.
    pub id: String,
    pub title: String,
    pub date: (i32, u32, u32),
    /// None = all day.
    pub time: Option<(u32, u32)>,
    /// From a subscribed calendar (can't be deleted here).
    pub read_only: bool,
    /// The subscribed calendar's id (a hash, not the link).
    pub feed: Option<String>,
}

impl Event {
    /// "09:30", or "All day".
    pub fn time_label(&self) -> String {
        match self.time {
            Some((h, m)) => format!("{h:02}:{m:02}"),
            None => crate::i18n::tr("All day").into(),
        }
    }
}

// ---------------------------------------------------------------- reading

/// (file, parsed) with the modification times they were read at, so redraws don't re-read the files.
static CACHE: Mutex<Option<(Option<SystemTime>, Option<SystemTime>, Vec<Event>)>> = Mutex::new(None);

fn mtime(name: &str) -> Option<SystemTime> {
    std::fs::metadata(store::path(name)).and_then(|m| m.modified()).ok()
}

fn load_raw(name: &str) -> Vec<Value> {
    store::load::<Vec<Value>>(name)
}

/// "2026-11-14T00:00:00+07:00" → (2026, 11, 14): the date as written (local, like the .NET build reads it).
fn parse_date(s: &str) -> Option<(i32, u32, u32)> {
    let y = s.get(0..4)?.parse().ok()?;
    let m = s.get(5..7)?.parse().ok()?;
    let d = s.get(8..10)?.parse().ok()?;
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then_some((y, m, d))
}

/// Accepts "9", "14", "9:30", "09:30" (the Calendar tab's time box).
pub fn parse_time(raw: &str) -> Option<(u32, u32)> {
    let raw = raw.trim();
    if let Ok(h) = raw.parse::<u32>() {
        return (h < 24).then_some((h, 0));
    }
    let mut it = raw.split(':');
    let h: u32 = it.next()?.trim().parse().ok()?;
    let m: u32 = it.next()?.trim().parse().ok()?;
    (h < 24 && m < 60).then_some((h, m))
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

fn to_events(raw: &[Value], read_only: bool, seen: &mut HashMap<u64, u32>) -> Vec<Event> {
    raw.iter()
        .filter_map(|v| {
            let date = parse_date(v.get("Date")?.as_str()?)?;
            let time = v.get("Time").and_then(Value::as_str).and_then(parse_time);
            let title = v.get("Title").and_then(Value::as_str).unwrap_or("").to_string();
            let feed = v.get("Feed").and_then(Value::as_str).map(str::to_string);
            let h = fnv(&format!("{date:?}|{time:?}|{title}|{feed:?}"));
            let n = seen.entry(h).or_insert(0);
            *n += 1;
            Some(Event { id: format!("{h:016x}-{n}"), title, date, time, read_only: read_only || feed.is_some(), feed })
        })
        .collect()
}

/// Everything (own + subscribed), in file order. Mirrors `CalendarEvent.LoadAll()` minus the to-dos.
pub fn load_all() -> Vec<Event> {
    let (a, b) = (mtime(EVENTS), mtime(FEED_EVENTS));
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((ca, cb, list)) = cache.as_ref() {
        if *ca == a && *cb == b && a.is_some() | b.is_some() {
            return list.clone();
        }
    }
    let mut seen = HashMap::new();
    let mut list = to_events(&load_raw(EVENTS), false, &mut seen);
    list.extend(to_events(&load_raw(FEED_EVENTS), true, &mut seen));
    *cache = Some((a, b, list.clone()));
    list
}

fn invalidate() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// One day's events: all-day first, then by time.
pub fn on_day(y: i32, m: u32, d: u32) -> Vec<Event> {
    let mut list: Vec<Event> = load_all().into_iter().filter(|e| e.date == (y, m, d)).collect();
    list.sort_by_key(|e| e.time.map_or(-1, |(h, m)| (h * 60 + m) as i64));
    list
}

/// Days of the month that have events: (day, count), ascending.
pub fn month(y: i32, m: u32) -> Vec<(u32, usize)> {
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for e in load_all() {
        if e.date.0 == y && e.date.1 == m {
            *counts.entry(e.date.2).or_default() += 1;
        }
    }
    let mut v: Vec<_> = counts.into_iter().collect();
    v.sort();
    v
}

/// The first timed event today that starts at or after now (the idle notch's "next event").
pub fn next_today() -> Option<Event> {
    let (y, m, d, h, min) = now_local();
    let now = h * 60 + min;
    on_day(y, m, d).into_iter().find(|e| e.time.is_some_and(|(eh, em)| eh * 60 + em >= now))
}

// ---------------------------------------------------------------- editing (events.json only)

/// Adds one of the user's own events and returns it.
pub fn add(y: i32, m: u32, d: u32, time: Option<(u32, u32)>, title: &str) -> Option<Event> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    let mut raw = load_raw(EVENTS);
    raw.push(json!({
        "Date": format!("{y:04}-{m:02}-{d:02}T00:00:00"),
        "Time": time.map(|(h, m)| format!("{h:02}:{m:02}")).unwrap_or_default(),
        "Title": title,
        "Feed": null,
    }));
    store::save(EVENTS, &raw);
    invalidate();
    load_all().into_iter().rev().find(|e| !e.read_only && e.date == (y, m, d) && e.title == title && e.time == time)
}

/// Deletes one of the user's own events by id. False for subscribed events or unknown ids.
pub fn delete(id: &str) -> bool {
    let mut raw = load_raw(EVENTS);
    let own = to_events(&raw, false, &mut HashMap::new());
    // Ids of own events are computed the same way as in load_all (own events come first there).
    let Some(pos) = own.iter().position(|e| e.id == id && e.feed.is_none()) else { return false };
    // `to_events` skips unparsable entries: map back to the raw index.
    let mut seen = 0;
    let Some(raw_index) = raw.iter().position(|v| {
        let ok = v.get("Date").and_then(Value::as_str).and_then(parse_date).is_some();
        if ok {
            seen += 1;
        }
        ok && seen == pos + 1
    }) else {
        return false;
    };
    raw.remove(raw_index);
    store::save(EVENTS, &raw);
    invalidate();
    true
}

// ---------------------------------------------------------------- subscribed calendars

/// The subscribed links (secret: anyone with one can read the calendar).
pub fn feeds() -> Vec<String> {
    let Ok(bytes) = std::fs::read(store::path(LINKS)) else { return Vec::new() };
    let Some(plain) = store::unprotect(&bytes, LINKS_ENTROPY) else { return Vec::new() };
    String::from_utf8_lossy(&plain).split('\n').filter(|s| !s.is_empty()).map(str::to_string).collect()
}

fn save_feeds(feeds: &[String]) {
    let path = store::path(LINKS);
    if feeds.is_empty() {
        let _ = std::fs::remove_file(path);
    } else if let Some(cipher) = store::protect(feeds.join("\n").as_bytes(), LINKS_ENTROPY) {
        let _ = std::fs::write(path, cipher);
    }
}

/// Subscribes to a link (call `refresh_feeds` afterwards, on a background thread).
pub fn add_feed(url: &str) -> bool {
    let url = url.trim();
    if !is_valid_url(url) {
        return false;
    }
    let mut list = feeds();
    if !list.iter().any(|f| f == url) {
        list.push(url.to_string());
        save_feeds(&list);
    }
    true
}

pub fn remove_feed(url: &str) {
    let mut list = feeds();
    list.retain(|f| f != url);
    save_feeds(&list);
}

// webcal:// is just "this is a calendar" on top of http(s).
fn fetch_url(url: &str) -> String {
    match url.get(..9) {
        Some(p) if p.eq_ignore_ascii_case("webcal://") => format!("https://{}", &url[9..]),
        _ => url.to_string(),
    }
}

pub fn is_valid_url(url: &str) -> bool {
    let u = fetch_url(url).to_ascii_lowercase();
    (u.starts_with("https://") || u.starts_with("http://")) && u.len() > 10
}

/// A short, safe label for a link: its host.
pub fn label(url: &str) -> String {
    let u = fetch_url(url);
    u.split("://").nth(1).and_then(|r| r.split(['/', '?', '#']).next()).filter(|h| !h.is_empty()).unwrap_or("Calendar").to_string()
}

/// Stable id for a link that doesn't reveal it: first 12 hex digits (upper case) of its SHA-256.
pub fn feed_id(url: &str) -> String {
    let mut hash = [0u8; 32];
    unsafe {
        let _ = BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, url.as_bytes(), &mut hash);
    }
    hash[..6].iter().map(|b| format!("{b:02X}")).collect()
}

/// Fetches every subscribed calendar (last month through a year ahead) into events-feed.json. A feed that
/// fails keeps its previous events. Returns a status line for Settings ("" with no feeds). Blocking.
pub fn refresh_feeds() -> String {
    let links = feeds();
    let previous = load_raw(FEED_EVENTS);
    let (y, m, d, _, _) = now_local();
    let today = days_from_civil(y, m, d);
    let from = add_months(today, -1);
    let to = add_months(today, 12);
    let mut out: Vec<Value> = Vec::new();
    let mut failed = 0;
    for link in &links {
        let id = feed_id(link);
        // gzip, not the default deflate some servers pick, which WinRT's HttpClient fails to decode.
        match crate::http::send("GET", &fetch_url(link), &[("Accept-Encoding", "gzip")], None).ok().filter(|r| r.ok()) {
            Some(r) => {
                let text = r.body;
                for o in parse_ics(&text, from, to) {
                    let (y, m, d) = civil_from_days(o.day);
                    out.push(json!({
                        "Date": format!("{y:04}-{m:02}-{d:02}T00:00:00"),
                        "Time": o.time.map(|(h, m)| format!("{h:02}:{m:02}")).unwrap_or_default(),
                        "Title": o.title,
                        "Feed": id,
                    }));
                }
            }
            None => {
                failed += 1;
                out.extend(previous.iter().filter(|v| v.get("Feed").and_then(Value::as_str) == Some(id.as_str())).cloned());
            }
        }
    }
    store::save(FEED_EVENTS, &out);
    invalidate();
    if links.is_empty() {
        String::new()
    } else if failed > 0 {
        format!("{failed} of {} failed · using last copy", links.len())
    } else {
        let upcoming = out
            .iter()
            .filter_map(|v| parse_date(v.get("Date")?.as_str()?))
            .filter(|&(y, m, d)| days_from_civil(y, m, d) >= today)
            .count();
        let (_, _, _, h, min) = now_local();
        format!("Synced {h:02}:{min:02} · {upcoming} upcoming")
    }
}

// ---------------------------------------------------------------- dates (days since 1970-01-01)

fn now_local() -> (i32, u32, u32, u32, u32) {
    let t = unsafe { GetLocalTime() };
    (t.wYear as i32, t.wMonth as u32, t.wDay as u32, t.wHour as u32, t.wMinute as u32)
}

/// Today's local date.
pub fn today() -> (i32, u32, u32) {
    let (y, m, d, _, _) = now_local();
    (y, m, d)
}

pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + if m <= 2 { 1 } else { 0 }) as i32;
    (y, m, d)
}

/// 0 = Monday … 6 = Sunday.
pub fn weekday(days: i64) -> u32 {
    (days + 3).rem_euclid(7) as u32
}

pub fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn add_months(days: i64, n: i32) -> i64 {
    let (y, m, d) = civil_from_days(days);
    let total = y * 12 + m as i32 - 1 + n;
    let (y, m) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    days_from_civil(y, m, d.min(days_in_month(y, m)))
}

// ---------------------------------------------------------------- iCalendar

/// One occurrence in local time.
#[derive(Clone, Debug, PartialEq)]
pub struct Occurrence {
    pub day: i64,
    pub time: Option<(u32, u32)>,
    pub title: String,
}

const DAY: i64 = 86_400;
const MAX_PERIODS: i64 = 5000; // a runaway rule stops here

#[derive(Clone, Copy)]
enum Zone {
    Utc,
    Local,
    Win(DYNAMIC_TIME_ZONE_INFORMATION),
}

/// A start as written: wall-clock seconds (since 1970, in its own zone).
#[derive(Clone, Copy)]
struct Stamp {
    wall: i64,
    zone: Zone,
    all_day: bool,
}

struct Prop {
    name: String,
    params: HashMap<String, String>,
    value: String,
}

fn systemtime(secs: i64) -> SYSTEMTIME {
    let (y, m, d) = civil_from_days(secs.div_euclid(DAY));
    let s = secs.rem_euclid(DAY);
    SYSTEMTIME {
        wYear: y as u16,
        wMonth: m as u16,
        wDay: d as u16,
        wHour: (s / 3600) as u16,
        wMinute: (s / 60 % 60) as u16,
        wSecond: (s % 60) as u16,
        ..Default::default()
    }
}

fn secs(t: &SYSTEMTIME) -> i64 {
    days_from_civil(t.wYear as i32, t.wMonth as u32, t.wDay as u32) * DAY + t.wHour as i64 * 3600 + t.wMinute as i64 * 60 + t.wSecond as i64
}

fn to_utc(s: &Stamp) -> i64 {
    if s.all_day {
        return s.wall.div_euclid(DAY) * DAY; // compared as dates
    }
    let tz = match &s.zone {
        Zone::Utc => return s.wall,
        Zone::Local => None,
        Zone::Win(z) => Some(z as *const _),
    };
    let mut out = SYSTEMTIME::default();
    match unsafe { TzSpecificLocalTimeToSystemTimeEx(tz, &systemtime(s.wall), &mut out) } {
        Ok(()) => secs(&out),
        Err(_) => s.wall,
    }
}

/// UTC seconds → wall-clock seconds in `zone`.
fn from_utc(utc: i64, zone: &Zone) -> i64 {
    let tz = match zone {
        Zone::Utc => return utc,
        Zone::Local => None,
        Zone::Win(z) => Some(z as *const _),
    };
    let mut out = SYSTEMTIME::default();
    match unsafe { SystemTimeToTzSpecificLocalTimeEx(tz, &systemtime(utc), &mut out) } {
        Ok(()) => secs(&out),
        Err(_) => utc,
    }
}

/// Windows time zones by key name ("SE Asia Standard Time").
fn windows_zones() -> &'static Vec<(String, DYNAMIC_TIME_ZONE_INFORMATION)> {
    static ZONES: OnceLock<Vec<(String, DYNAMIC_TIME_ZONE_INFORMATION)>> = OnceLock::new();
    ZONES.get_or_init(|| {
        let mut v = Vec::new();
        for i in 0.. {
            let mut z = DYNAMIC_TIME_ZONE_INFORMATION::default();
            if unsafe { EnumDynamicTimeZoneInformation(i, &mut z) } != 0 {
                break;
            }
            let end = z.TimeZoneKeyName.iter().position(|&c| c == 0).unwrap_or(128);
            v.push((String::from_utf16_lossy(&z.TimeZoneKeyName[..end]), z));
        }
        v
    })
}

/// IANA id ("Asia/Jakarta") → Windows key name, through the ICU that ships with Windows 10 1903+.
fn iana_to_windows(id: &str) -> Option<String> {
    type F = unsafe extern "C" fn(*const u16, i32, *mut u16, i32, *mut i32) -> i32;
    static FUNC: OnceLock<Option<F>> = OnceLock::new();
    let f = (*FUNC.get_or_init(|| unsafe {
        let lib = LoadLibraryW(&HSTRING::from("icu.dll")).ok()?;
        GetProcAddress(lib, s!("ucal_getWindowsTimeZoneID")).map(|p| std::mem::transmute::<_, F>(p))
    }))?;
    let wide: Vec<u16> = id.encode_utf16().collect();
    let mut buf = [0u16; 128];
    let mut status = 0i32;
    let n = unsafe { f(wide.as_ptr(), wide.len() as i32, buf.as_mut_ptr(), buf.len() as i32, &mut status) };
    (status <= 0 && n > 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

/// A TZID as written (Windows names from Outlook, IANA ids from Google/iCloud); local time when unknown.
fn find_zone(tzid: &str, cache: &mut HashMap<String, Zone>) -> Zone {
    let tzid = tzid.trim_matches('"');
    if let Some(z) = cache.get(tzid) {
        return *z;
    }
    let lookup = |name: &str| windows_zones().iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, z)| *z);
    let zone = match tzid {
        "UTC" | "Etc/UTC" | "GMT" | "Etc/GMT" | "Z" => Zone::Utc,
        _ => lookup(tzid).or_else(|| iana_to_windows(tzid).and_then(|w| lookup(&w))).map_or(Zone::Local, Zone::Win),
    };
    cache.insert(tzid.to_string(), zone);
    zone
}

fn parse_stamp(value: &str, params: &HashMap<String, String>, zones: &mut HashMap<String, Zone>) -> Option<Stamp> {
    let v = value.trim();
    let date = |s: &str| -> Option<i64> {
        let y = s.get(0..4)?.parse().ok()?;
        let m = s.get(4..6)?.parse().ok()?;
        let d = s.get(6..8)?.parse().ok()?;
        ((1..=12).contains(&m) && (1..=31).contains(&d)).then(|| days_from_civil(y, m, d))
    };
    let all_day = params.get("VALUE").is_some_and(|k| k.eq_ignore_ascii_case("DATE")) || v.len() == 8;
    if all_day {
        return Some(Stamp { wall: date(v)? * DAY, zone: Zone::Local, all_day: true });
    }
    let utc = v.ends_with('Z');
    let v = v.trim_end_matches('Z');
    if v.len() != 15 || v.as_bytes()[8] != b'T' {
        return None;
    }
    let (h, m, s): (i64, i64, i64) = (v.get(9..11)?.parse().ok()?, v.get(11..13)?.parse().ok()?, v.get(13..15)?.parse().ok()?);
    let zone = if utc {
        Zone::Utc
    } else if let Some(tzid) = params.get("TZID") {
        find_zone(tzid, zones)
    } else {
        Zone::Local
    };
    Some(Stamp { wall: date(v)? * DAY + h * 3600 + m * 60 + s, zone, all_day: false })
}

/// Joins folded lines (a continuation starts with a space or tab).
fn unfold(ics: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in ics.split('\n') {
        let line = raw.trim_end_matches('\r');
        if (line.starts_with(' ') || line.starts_with('\t')) && !out.is_empty() {
            out.last_mut().unwrap().push_str(&line[1..]);
        } else {
            out.push(line.to_string());
        }
    }
    out
}

/// NAME;PARAM=x;PARAM="y:z":value → name, params, value (the first ':' outside quotes splits).
fn parse_line(line: &str) -> Option<Prop> {
    let mut quoted = false;
    let colon = line.char_indices().find(|&(_, c)| {
        if c == '"' {
            quoted = !quoted;
        }
        c == ':' && !quoted
    })?.0;
    if colon == 0 {
        return None;
    }
    let mut head = line[..colon].split(';');
    let name = head.next()?.to_ascii_uppercase();
    let params = head.filter_map(|p| p.split_once('=')).filter(|(k, _)| !k.is_empty()).map(|(k, v)| (k.to_ascii_uppercase(), v.to_string())).collect();
    Some(Prop { name, params, value: line[colon + 1..].to_string() })
}

fn read_events(ics: &str) -> Vec<Vec<Prop>> {
    let mut events = Vec::new();
    let mut current: Option<Vec<Prop>> = None;
    let mut depth = 0; // nested blocks inside a VEVENT (VALARM) are skipped
    for line in unfold(ics) {
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("BEGIN:") {
            if current.is_some() {
                depth += 1;
            } else if upper == "BEGIN:VEVENT" {
                current = Some(Vec::new());
            }
            continue;
        }
        if upper.starts_with("END:") {
            if current.is_some() {
                if depth > 0 {
                    depth -= 1;
                } else {
                    events.push(current.take().unwrap());
                }
            }
            continue;
        }
        if depth > 0 {
            continue;
        }
        if let (Some(cur), Some(p)) = (current.as_mut(), parse_line(&line)) {
            cur.push(p);
        }
    }
    events
}

fn get<'a>(e: &'a [Prop], name: &str) -> Option<&'a Prop> {
    e.iter().find(|p| p.name == name)
}

fn unescape(s: &str) -> String {
    s.replace("\\n", " ").replace("\\N", " ").replace("\\,", ",").replace("\\;", ";").replace("\\\\", "\\").trim().to_string()
}

/// Every VEVENT occurrence whose local date falls in `from..=to` (days since 1970), sorted. Covers UTC /
/// TZID / floating / all-day starts, RRULE (DAILY, WEEKLY, MONTHLY, YEARLY with INTERVAL, COUNT, UNTIL,
/// BYDAY, BYMONTHDAY), EXDATE, RECURRENCE-ID overrides and STATUS:CANCELLED, like the .NET IcsParser.
pub fn parse_ics(ics: &str, from: i64, to: i64) -> Vec<Occurrence> {
    let events = read_events(ics);
    let mut zones = HashMap::new();
    let mut result = Vec::new();

    // Instances moved or cancelled by a RECURRENCE-ID entry don't come from the master's rule.
    let mut overridden: HashSet<(String, i64)> = HashSet::new();
    for e in &events {
        if let (Some(rid), Some(uid)) = (get(e, "RECURRENCE-ID"), get(e, "UID")) {
            if let Some(st) = parse_stamp(&rid.value, &rid.params, &mut zones) {
                overridden.insert((uid.value.clone(), to_utc(&st)));
            }
        }
    }

    for e in &events {
        let Some(dt) = get(e, "DTSTART") else { continue };
        let Some(start) = parse_stamp(&dt.value, &dt.params, &mut zones) else { continue };
        if get(e, "STATUS").is_some_and(|s| s.value.eq_ignore_ascii_case("CANCELLED")) {
            continue;
        }
        let mut title = unescape(get(e, "SUMMARY").map_or("", |p| p.value.as_str()));
        if title.is_empty() {
            title = "(No title)".into();
        }
        let uid = get(e, "UID").map_or(String::new(), |p| p.value.clone());

        let walls: Vec<i64> = match get(e, "RRULE") {
            Some(rrule) if get(e, "RECURRENCE-ID").is_none() => {
                let mut excluded = HashSet::new();
                for p in e.iter().filter(|p| p.name == "EXDATE") {
                    for v in p.value.split(',') {
                        if let Some(st) = parse_stamp(v, &p.params, &mut zones) {
                            excluded.insert(to_utc(&st));
                        }
                    }
                }
                // Window in the event's wall clock: a couple of days' slack covers any zone offset.
                expand(&start, &rrule.value, (from - 2) * DAY, (to + 2) * DAY, &mut zones)
                    .into_iter()
                    .filter(|&w| {
                        let utc = to_utc(&Stamp { wall: w, ..start });
                        !excluded.contains(&utc) && !overridden.contains(&(uid.clone(), utc))
                    })
                    .collect()
            }
            _ => vec![start.wall],
        };

        for wall in walls {
            let local = if start.all_day { wall } else { from_utc(to_utc(&Stamp { wall, ..start }), &Zone::Local) };
            let day = local.div_euclid(DAY);
            if day >= from && day <= to {
                let s = local.rem_euclid(DAY);
                let time = (!start.all_day).then(|| ((s / 3600) as u32, (s / 60 % 60) as u32));
                result.push((local, Occurrence { day, time, title: title.clone() }));
            }
        }
    }
    result.sort_by_key(|(t, _)| *t);
    result.into_iter().map(|(_, o)| o).collect()
}

/// "MO", "2TU", "-1FR" → (ordinal, weekday 0=Mon); ordinal 0 means every such day.
fn parse_by_day(s: &str) -> Option<(i32, u32)> {
    let s = s.trim();
    if s.len() < 2 {
        return None;
    }
    let (n, d) = s.split_at(s.len() - 2);
    let day = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"].iter().position(|x| x.eq_ignore_ascii_case(d))? as u32;
    Some((if n.is_empty() { 0 } else { n.parse().unwrap_or(0) }, day))
}

/// Wall-clock starts of a recurring event, roughly within the window (callers filter).
fn expand(start: &Stamp, rrule: &str, window_start: i64, window_end: i64, zones: &mut HashMap<String, Zone>) -> Vec<i64> {
    let parts: HashMap<String, String> =
        rrule.split(';').filter_map(|p| p.split_once('=')).map(|(k, v)| (k.trim().to_ascii_uppercase(), v.trim().to_string())).collect();
    let freq = parts.get("FREQ").map(|f| f.to_ascii_uppercase()).unwrap_or_default();
    let interval = parts.get("INTERVAL").and_then(|v| v.parse::<i64>().ok()).filter(|&n| n > 0).unwrap_or(1);
    let count = parts.get("COUNT").and_then(|v| v.parse::<usize>().ok());
    let until = parts.get("UNTIL").and_then(|u| parse_stamp(u, &HashMap::new(), zones)).map(|us| {
        if matches!(us.zone, Zone::Utc) && !matches!(start.zone, Zone::Utc) { from_utc(us.wall, &start.zone) } else { us.wall }
    });
    let by_day: Vec<(i32, u32)> = parts.get("BYDAY").map(|v| v.split(',').filter_map(parse_by_day).collect()).unwrap_or_default();
    let by_month_day: Vec<i32> =
        parts.get("BYMONTHDAY").map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).filter(|&d| d != 0).collect()).unwrap_or_default();
    let mut end = match until {
        Some(u) if u < window_end => u,
        _ => window_end,
    };
    if start.all_day {
        if let Some(u) = until {
            end = (u.div_euclid(DAY) * DAY).min(window_end);
        }
    }

    if !matches!(freq.as_str(), "DAILY" | "WEEKLY" | "MONTHLY" | "YEARLY") {
        return vec![start.wall]; // unsupported rule (e.g. HOURLY): just the first one
    }

    let first_day = start.wall.div_euclid(DAY);
    let tod = start.wall.rem_euclid(DAY);
    let (fy, fm, fd) = civil_from_days(first_day);
    let week_start = first_day - weekday(first_day) as i64; // Monday-based weeks
    let month_index = fy as i64 * 12 + fm as i64 - 1;

    // Without COUNT, jump straight to the window: a daily event from years ago mustn't spend the period budget.
    let mut skip = 0i64;
    if count.is_none() && window_start > start.wall {
        let gap_days = (window_start - start.wall) as f64 / DAY as f64;
        let (wy, wm, _) = civil_from_days(window_start.div_euclid(DAY));
        let months = (wy - fy) as i64 * 12 + wm as i64 - fm as i64;
        let n = match freq.as_str() {
            "DAILY" => (gap_days / interval as f64) as i64,
            "WEEKLY" => (gap_days / 7.0 / interval as f64) as i64,
            "MONTHLY" => months / interval,
            _ => months / 12 / interval,
        };
        skip = (n - 1).max(0);
    }

    let mut out = Vec::new();
    for k in skip..skip + MAX_PERIODS {
        let step = k * interval;
        let (period_start, mut candidates): (i64, Vec<i64>) = match freq.as_str() {
            "DAILY" => {
                let d = start.wall + step * DAY;
                let c = if by_day.is_empty() || by_day.iter().any(|b| b.1 == weekday(d.div_euclid(DAY))) { vec![d] } else { vec![] };
                (d, c)
            }
            "WEEKLY" => {
                let week = week_start + 7 * step;
                let c = if by_day.is_empty() {
                    vec![start.wall + 7 * step * DAY]
                } else {
                    by_day.iter().map(|b| (week + b.1 as i64) * DAY + tod).collect()
                };
                (week * DAY, c)
            }
            "MONTHLY" => {
                let mi = month_index + step;
                let (y, m) = (mi.div_euclid(12) as i32, mi.rem_euclid(12) as u32 + 1);
                let first = days_from_civil(y, m, 1);
                let n = days_in_month(y, m) as i64;
                let mut c = Vec::new();
                for &md in &by_month_day {
                    let day = if md > 0 { md as i64 } else { n + md as i64 + 1 };
                    if (1..=n).contains(&day) {
                        c.push((first + day - 1) * DAY + tod);
                    }
                }
                for &(nth, dow) in &by_day {
                    let matches: Vec<i64> = (0..n).map(|i| first + i).filter(|&d| weekday(d) == dow).collect();
                    if nth == 0 {
                        c.extend(matches.iter().map(|d| d * DAY + tod));
                    } else if (nth.unsigned_abs() as usize) <= matches.len() {
                        let d = if nth > 0 { matches[nth as usize - 1] } else { matches[matches.len() - nth.unsigned_abs() as usize] };
                        c.push(d * DAY + tod);
                    }
                }
                if by_month_day.is_empty() && by_day.is_empty() && fd as i64 <= n {
                    c.push((first + fd as i64 - 1) * DAY + tod); // the 31st skips short months
                }
                (first * DAY, c)
            }
            _ => {
                let y = fy + step as i32;
                let c = if fm == 2 && fd == 29 && days_in_month(y, 2) == 28 { vec![] } else { vec![days_from_civil(y, fm, fd) * DAY + tod] };
                (days_from_civil(y, 1, 1) * DAY, c)
            }
        };
        if period_start > end {
            break;
        }
        candidates.sort();
        for d in candidates {
            if d < start.wall {
                continue;
            }
            if d > end || count.is_some_and(|max| out.len() >= max) {
                return out;
            }
            out.push(d);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART;TZID=Asia/Jakarta:20261102T090000\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE\r\nEXDATE;TZID=Asia/Jakarta:20261104T090000\r\nSUMMARY:Stand\r\n up\\, daily\r\nBEGIN:VALARM\r\nSUMMARY:x\r\nEND:VALARM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:a\r\nRECURRENCE-ID;TZID=Asia/Jakarta:20261111T090000\r\nDTSTART;TZID=Asia/Jakarta:20261111T150000\r\nSUMMARY:Stand up (moved)\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:b\r\nDTSTART;VALUE=DATE:20261117\r\nSUMMARY:Holiday\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:c\r\nDTSTART:20261120T020000Z\r\nSUMMARY:UTC call\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:d\r\nDTSTART;TZID=W. Europe Standard Time:20261101T100000\r\nRRULE:FREQ=MONTHLY;BYDAY=-1FR;COUNT=3\r\nSUMMARY:Last Friday\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    #[ignore]
    fn ics_and_files() {
        // Keep one MTA alive for the whole test binary: windows-rs caches WinRT factories process-wide, and a
        // torn-down MTA (last MTA thread gone) leaves them dangling (access violation on the next HTTP call).
        unsafe { let _ = windows::Win32::System::Com::CoIncrementMTAUsage(); }
        let from = days_from_civil(2026, 11, 1);
        let to = days_from_civil(2027, 2, 28);
        println!("local tz now: {:+} h", super::super::prayer::local_tz_hours());
        for o in parse_ics(SAMPLE, from, days_from_civil(2026, 11, 30)).iter().chain(parse_ics(SAMPLE, days_from_civil(2027, 1, 1), to).iter()) {
            let (y, m, d) = civil_from_days(o.day);
            println!("  {y}-{m:02}-{d:02} {:?} {}", o.time, o.title);
        }
        let all = parse_ics(SAMPLE, from, to);
        assert!(all.iter().any(|o| o.title == "Standup, daily"));
        assert!(!all.iter().any(|o| o.day == days_from_civil(2026, 11, 4) && o.title.starts_with("Standup,")), "EXDATE");
        assert!(!all.iter().any(|o| o.day == days_from_civil(2026, 11, 11) && o.title == "Standup, daily"), "RECURRENCE-ID");
        assert_eq!(all.iter().filter(|o| o.title == "Last Friday").count(), 3);
        assert_eq!(feed_id("https://example.com/a.ics").len(), 12);
        println!("feed id: {}", feed_id("https://example.com/a.ics"));

        // Files: a scratch copy of the real data folder.
        let dir = std::env::temp_dir().join("fathnotch-calendar-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let real = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap()).join("FathNotch");
        for f in [EVENTS, FEED_EVENTS, LINKS] {
            let _ = std::fs::copy(real.join(f), dir.join(f));
        }
        std::env::set_var("FATHNOTCH_DATA_DIR", &dir);
        println!("copied user's events: {} entries", load_all().len());
        println!("feeds subscribed: {}", feeds().len());
        let (y, m, d) = today();
        let e = add(y, m, d, parse_time("9:30"), "Test event").unwrap();
        add(y, m, d, None, "All-day test").unwrap();
        println!("today: {:?}", on_day(y, m, d).iter().map(|e| format!("{} {}", e.time_label(), e.title)).collect::<Vec<_>>());
        println!("month: {:?}", month(y, m));
        println!("events.json now:\n{}", std::fs::read_to_string(dir.join(EVENTS)).unwrap());
        assert!(delete(&e.id));
        assert!(!on_day(y, m, d).iter().any(|x| x.title == "Test event"));
        println!("refresh_feeds: {:?}", refresh_feeds());
        println!("after refresh: {} events", load_all().len());
    }
}
