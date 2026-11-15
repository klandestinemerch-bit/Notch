//! Daily prayer times computed locally from latitude/longitude (the praytimes.org sun-position formulas, as
//! the .NET build did). Only the city → coordinates lookup goes online (Open-Meteo geocoding).

use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::Time::{GetDynamicTimeZoneInformation, TzSpecificLocalTimeToSystemTimeEx, DYNAMIC_TIME_ZONE_INFORMATION};

/// Display names, in `Times` order (Indonesian, like the .NET build). Index 1 (sunrise) is not a prayer.
pub const NAMES: [&str; 6] = ["Subuh", "Terbit", "Dzuhur", "Ashar", "Maghrib", "Isya"];
/// Stable ids in the same order.
pub const IDS: [&str; 6] = ["fajr", "sunrise", "dhuhr", "asr", "maghrib", "isha"];

/// A calculation method: twilight angles, optional fixed Isha delay (minutes after Maghrib) and ihtiyat.
pub struct Method {
    pub id: &'static str,
    pub name: &'static str,
    pub fajr_angle: f64,
    pub isha_angle: f64,
    pub isha_minutes: Option<f64>,
    pub ihtiyat: f64,
}

/// Same table (and ids, as stored in settings) as the .NET build; the first is the default.
pub const METHODS: [Method; 6] = [
    Method { id: "kemenag", name: "Kemenag RI (Indonesia)", fajr_angle: 20.0, isha_angle: 18.0, isha_minutes: None, ihtiyat: 2.0 },
    Method { id: "mwl", name: "Muslim World League", fajr_angle: 18.0, isha_angle: 17.0, isha_minutes: None, ihtiyat: 0.0 },
    Method { id: "isna", name: "ISNA (North America)", fajr_angle: 15.0, isha_angle: 15.0, isha_minutes: None, ihtiyat: 0.0 },
    Method { id: "egypt", name: "Egyptian Authority", fajr_angle: 19.5, isha_angle: 17.5, isha_minutes: None, ihtiyat: 0.0 },
    Method { id: "makkah", name: "Umm al-Qura (Makkah)", fajr_angle: 18.5, isha_angle: 0.0, isha_minutes: Some(90.0), ihtiyat: 0.0 },
    Method { id: "jakim", name: "JAKIM (Malaysia)", fajr_angle: 20.0, isha_angle: 18.0, isha_minutes: None, ihtiyat: 2.0 },
];

pub fn method(id: &str) -> &'static Method {
    METHODS.iter().find(|m| m.id == id).unwrap_or(&METHODS[0])
}

/// Local clock hours (4.25 = 04:15), already rounded up to the whole minute as printed timetables do.
/// NaN when the sun never reaches the angle (high latitudes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Times {
    pub fajr: f64,
    pub sunrise: f64,
    pub dhuhr: f64,
    pub asr: f64,
    pub maghrib: f64,
    pub isha: f64,
}

impl Times {
    pub fn get(&self, i: usize) -> f64 {
        [self.fajr, self.sunrise, self.dhuhr, self.asr, self.maghrib, self.isha][i]
    }
}

/// Times with angle-based Isha and the Shafi'i Asr (shadow factor 1).
#[allow(clippy::too_many_arguments)]
pub fn compute(year: i32, month: u32, day: u32, lat: f64, lon: f64, tz_hours: f64, fajr_angle: f64, isha_angle: f64, ihtiyat_min: f64) -> Times {
    let m = Method { id: "", name: "", fajr_angle, isha_angle, isha_minutes: None, ihtiyat: ihtiyat_min };
    compute_with(year, month, day, lat, lon, tz_hours, &m, 1.0)
}

/// Full form: any method, `asr_factor` 1 (Shafi'i) or 2 (Hanafi).
#[allow(clippy::too_many_arguments)]
pub fn compute_with(year: i32, month: u32, day: u32, lat: f64, lon: f64, tz_hours: f64, m: &Method, asr_factor: f64) -> Times {
    let jd = julian_date(year, month as i32, day as i32) - lon / (15.0 * 24.0);

    // Day-fraction first guesses, refined once with the sun's position at that moment.
    let mut t = [5.0, 6.0, 12.0, 13.0, 18.0, 18.0, 18.0];
    for _ in 0..2 {
        let f = t.map(|x| x / 24.0);
        t = [
            sun_angle_time(jd, lat, m.fajr_angle, f[0], true),
            sun_angle_time(jd, lat, 0.833, f[1], true),
            mid_day(jd, f[2]),
            asr_time(jd, lat, asr_factor, f[3]),
            sun_angle_time(jd, lat, 0.833, f[4], false),
            sun_angle_time(jd, lat, 0.833, f[5], false),
            if m.isha_minutes.is_none() { sun_angle_time(jd, lat, m.isha_angle, f[6], false) } else { f[6] * 24.0 },
        ];
    }

    let shift = tz_hours - lon / 15.0; // solar → local clock time
    let ih = m.ihtiyat / 60.0;
    let isha = match m.isha_minutes {
        Some(min) => t[4] + shift + min / 60.0,
        None => t[6] + shift + ih,
    };
    Times {
        fajr: round_up(t[0] + shift + ih),
        sunrise: round_up(t[1] + shift - ih),
        dhuhr: round_up(t[2] + shift + if m.ihtiyat > 0.0 { ih } else { 1.0 / 60.0 }), // just past the meridian
        asr: round_up(t[3] + shift + ih),
        maghrib: round_up(t[4] + shift + ih),
        isha: round_up(isha),
    }
}

/// Up to the next whole minute (same tolerance as the .NET build).
fn round_up(hours: f64) -> f64 {
    if !hours.is_finite() {
        return f64::NAN;
    }
    (hours * 60.0 - 0.0001).ceil() / 60.0
}

/// The next prayer (sunrise skipped) after `now_hours`: (index into `NAMES`, hours until it).
/// After Isha it is tomorrow's Fajr, estimated from today's (within a minute); `index` is then 0
/// and the hours exceed the time left today.
pub fn next(t: &Times, now_hours: f64) -> (usize, f64) {
    for i in [0, 2, 3, 4, 5] {
        let at = t.get(i);
        if at.is_finite() && at > now_hours {
            return (i, at - now_hours);
        }
    }
    (0, 24.0 - now_hours + t.fajr)
}

/// "4.25" → "04:15".
pub fn hhmm(hours: f64) -> String {
    if !hours.is_finite() {
        return "--:--".into();
    }
    let m = (hours * 60.0).round() as i64;
    format!("{:02}:{:02}", m.div_euclid(60).rem_euclid(24), m.rem_euclid(60))
}

// ------------------------------------------------------------------ location and clock

/// City → (latitude, longitude, place name) via Open-Meteo geocoding, the same lookup the .NET build used.
pub fn geocode(city: &str) -> Option<(f64, f64, String)> {
    let city = city.trim();
    if city.is_empty() {
        return None;
    }
    // The weather place picked in Settings → Weather (the prayer city falls back to it).
    if let Some(p) = super::weather::picked(city) {
        return Some(p);
    }
    let url = format!("https://geocoding-api.open-meteo.com/v1/search?count=1&language=en&name={}", crate::http::escape(city));
    let v = super::weather::get_json(&url)?;
    let place = v.get("results")?.get(0)?;
    let lat = place.get("latitude")?.as_f64()?;
    let lon = place.get("longitude")?.as_f64()?;
    let name = place.get("name").and_then(|n| n.as_str()).unwrap_or(city).to_string();
    Some((lat, lon, name))
}

/// The PC's current UTC offset in hours, daylight saving included.
pub fn local_tz_hours() -> f64 {
    unsafe {
        let mut tz = DYNAMIC_TIME_ZONE_INFORMATION::default();
        let id = GetDynamicTimeZoneInformation(&mut tz);
        let bias = tz.Bias
            + match id {
                1 => tz.StandardBias,
                2 => tz.DaylightBias,
                _ => 0,
            };
        -(bias as f64) / 60.0
    }
}

/// The PC's UTC offset at local noon on a given day (what the .NET build used for that day's times).
pub fn tz_hours_on(year: i32, month: u32, day: u32) -> f64 {
    let local = SYSTEMTIME { wYear: year as u16, wMonth: month as u16, wDay: day as u16, wHour: 12, ..Default::default() };
    let mut utc = SYSTEMTIME::default();
    unsafe {
        if TzSpecificLocalTimeToSystemTimeEx(None, &local, &mut utc).is_err() {
            return local_tz_hours();
        }
    }
    let jd = |s: &SYSTEMTIME| julian_date(s.wYear as i32, s.wMonth as i32, s.wDay as i32);
    let utc_h = (jd(&utc) - jd(&local)) * 24.0 + utc.wHour as f64 + utc.wMinute as f64 / 60.0;
    12.0 - utc_h
}

// ------------------------------------------------------------------ astronomy

fn julian_date(mut year: i32, mut month: i32, day: i32) -> f64 {
    if month <= 2 {
        year -= 1;
        month += 12;
    }
    let a = (year as f64 / 100.0).floor();
    let b = 2.0 - a + (a / 4.0).floor();
    (365.25 * (year as f64 + 4716.0)).floor() + (30.6001 * (month as f64 + 1.0)).floor() + day as f64 + b - 1524.5
}

/// (declination, equation of time)
fn sun_position(jd: f64) -> (f64, f64) {
    let d = jd - 2451545.0;
    let g = fix_angle(357.529 + 0.98560028 * d);
    let q = fix_angle(280.459 + 0.98564736 * d);
    let l = fix_angle(q + 1.915 * sin(g) + 0.020 * sin(2.0 * g));
    let e = 23.439 - 0.00000036 * d;
    let ra = fix_hour(atan2(cos(e) * sin(l), cos(l)) / 15.0);
    let eqt = q / 15.0 - ra;
    let decl = asin(sin(e) * sin(l));
    (decl, eqt)
}

fn mid_day(jd: f64, fraction: f64) -> f64 {
    fix_hour(12.0 - sun_position(jd + fraction).1)
}

/// When the sun is `angle` degrees below the horizon, before (ccw) or after noon.
fn sun_angle_time(jd: f64, lat: f64, angle: f64, fraction: f64, ccw: bool) -> f64 {
    let decl = sun_position(jd + fraction).0;
    let noon = mid_day(jd, fraction);
    let c = (-sin(angle) - sin(decl) * sin(lat)) / (cos(decl) * cos(lat));
    if !(-1.0..=1.0).contains(&c) {
        return f64::NAN;
    }
    let t = acos(c) / 15.0;
    noon + if ccw { -t } else { t }
}

fn asr_time(jd: f64, lat: f64, factor: f64, fraction: f64) -> f64 {
    let decl = sun_position(jd + fraction).0;
    let angle = -acot(factor + tan((lat - decl).abs()));
    sun_angle_time(jd, lat, angle, fraction, false)
}

fn sin(d: f64) -> f64 { d.to_radians().sin() }
fn cos(d: f64) -> f64 { d.to_radians().cos() }
fn tan(d: f64) -> f64 { d.to_radians().tan() }
fn asin(x: f64) -> f64 { x.asin().to_degrees() }
fn acos(x: f64) -> f64 { x.acos().to_degrees() }
fn atan2(y: f64, x: f64) -> f64 { y.atan2(x).to_degrees() }
fn acot(x: f64) -> f64 { (1.0 / x).atan().to_degrees() }
fn fix_angle(a: f64) -> f64 { a - 360.0 * (a / 360.0).floor() }
fn fix_hour(h: f64) -> f64 { h - 24.0 * (h / 24.0).floor() }

#[cfg(test)]
mod tests {
    use super::*;

    fn line(t: &Times) -> String {
        (0..6).map(|i| format!("{} {}", NAMES[i], hhmm(t.get(i)))).collect::<Vec<_>>().join("  ")
    }

    #[test]
    #[ignore]
    fn jakarta() {
        // Keep one MTA alive for the whole test binary: windows-rs caches WinRT factories process-wide, and a
        // torn-down MTA (last MTA thread gone) leaves them dangling (access violation on the next HTTP call).
        unsafe { let _ = windows::Win32::System::Com::CoIncrementMTAUsage(); }
        let (lat, lon) = (-6.21462, 106.84513);
        // Expected values: the .NET build's PrayerTimeService.Compute (Kemenag) run on the same inputs.
        let cases = [
            ((2026, 11, 14), "04:06 05:24 11:39 15:01 17:51 19:05"),
            ((2026, 1, 1), "04:19 05:40 11:59 15:25 18:13 19:29"),
            ((2026, 6, 21), "04:40 06:00 11:57 15:19 17:50 19:05"),
        ];
        for ((y, m, d), expected) in cases {
            let t = compute(y, m, d, lat, lon, 7.0, 20.0, 18.0, 2.0);
            let got = (0..6).map(|i| hhmm(t.get(i))).collect::<Vec<_>>().join(" ");
            println!("{y}-{m:02}-{d:02}  rust: {got}\n            C#:   {expected}\n            {}", line(&t));
            assert_eq!(got, expected);
        }
        println!("local tz now {} / on 2026-07-01 {}", local_tz_hours(), tz_hours_on(2026, 7, 1));
        let t = compute(2026, 11, 14, lat, lon, 7.0, 20.0, 18.0, 2.0);
        let (i, left) = next(&t, 12.0);
        println!("next after 12:00: {} in {:.0} min", NAMES[i], left * 60.0);
        assert_eq!(i, 3);
        println!("geocode Jakarta: {:?}", geocode("Jakarta"));
    }
}
