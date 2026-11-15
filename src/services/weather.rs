//! Weather from Open-Meteo (free, no key), like the .NET build: city → coordinates via its geocoding API,
//! then the forecast endpoint. Results are cached for 10 minutes per (city, unit); when a fetch fails the
//! last good value is returned. Blocking: call from `bg::spawn`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

const FRESH: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug, Default)]
pub struct Current {
    pub temp: f64,
    pub code: u32,
    pub is_day: bool,
    /// "Jakarta, Indonesia" (the geocoded place, as the Weather tab shows it).
    pub city: String,
    // Only filled by `forecast` (the Weather tab's detail row); 0 from `current`.
    pub feels: f64,
    pub humidity: f64,
    /// km/h, or mph with `fahrenheit`.
    pub wind: f64,
}

#[derive(Clone, Debug)]
pub struct Day {
    /// "2026-11-14"
    pub date: String,
    pub code: u32,
    pub max: f64,
    pub min: f64,
}

#[derive(Clone, Debug)]
pub struct Hour {
    /// Local hour of day at the place, 0–23.
    pub hour: u32,
    pub temp: f64,
    pub code: u32,
}

type Forecast = (Current, Vec<Hour>, Vec<Day>);

#[derive(Default)]
struct Cache {
    places: HashMap<String, (f64, f64, String)>,
    current: HashMap<(String, bool), (Instant, Current)>,
    forecast: HashMap<(String, bool), (Instant, Forecast)>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

fn with_cache<R>(f: impl FnOnce(&mut Cache) -> R) -> R {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Cache::default))
}

fn key(city: &str, fahrenheit: bool) -> (String, bool) {
    (city.trim().to_lowercase(), fahrenheit)
}

/// Current conditions (shared by the idle notch and Home). None when the city is unknown, or offline with
/// nothing fetched yet.
pub fn current(city: &str, fahrenheit: bool) -> Option<Current> {
    let k = key(city, fahrenheit);
    let fresh = with_cache(|c| {
        let a = c.current.get(&k).filter(|(at, _)| at.elapsed() < FRESH).map(|(_, v)| v.clone());
        let b = c.forecast.get(&k).filter(|(at, _)| at.elapsed() < FRESH).map(|(_, v)| v.0.clone());
        a.or(b)
    });
    if fresh.is_some() {
        return fresh;
    }
    match fetch_current(city, fahrenheit) {
        Some(cur) => {
            with_cache(|c| c.current.insert(k, (Instant::now(), cur.clone())));
            Some(cur)
        }
        None => with_cache(|c| c.current.get(&k).map(|(_, v)| v.clone()).or_else(|| c.forecast.get(&k).map(|(_, v)| v.0.clone()))),
    }
}

/// Current conditions, the next 24 hours and 7 days (today first).
pub fn forecast(city: &str, fahrenheit: bool) -> Option<(Current, Vec<Hour>, Vec<Day>)> {
    let k = key(city, fahrenheit);
    if let Some(f) = with_cache(|c| c.forecast.get(&k).filter(|(at, _)| at.elapsed() < FRESH).map(|(_, v)| v.clone())) {
        return Some(f);
    }
    match fetch_forecast(city, fahrenheit) {
        Some(f) => {
            with_cache(|c| c.forecast.insert(k, (Instant::now(), f.clone())));
            Some(f)
        }
        None => with_cache(|c| c.forecast.get(&k).map(|(_, v)| v.clone())),
    }
}

/// Drops cached readings so the next call fetches (the Weather tab's refresh button; a new place).
pub fn invalidate() {
    with_cache(|c| {
        c.current.clear();
        c.forecast.clear();
        c.places.clear();
    });
}

/// The coordinates picked in Settings → Weather, when `city` is still the place they were picked for.
pub fn picked(city: &str) -> Option<(f64, f64, String)> {
    let s = super::settings::shared();
    let same = !city.trim().is_empty() && city.trim().eq_ignore_ascii_case(s.weather_city.trim());
    match (same, s.weather_lat, s.weather_lon) {
        (true, Some(lat), Some(lon)) => Some((lat, lon, s.weather_city.trim().to_string())),
        _ => None,
    }
}

/// A search result (Settings → Weather).
#[derive(Clone, Debug)]
pub struct Place {
    pub name: String,
    /// "Jawa Barat, Indonesia"
    pub area: String,
    pub lat: f64,
    pub lon: f64,
}

impl Place {
    /// "Bekasi, Jawa Barat, Indonesia": what's saved as the weather city.
    pub fn label(&self) -> String {
        if self.area.is_empty() { self.name.clone() } else { format!("{}, {}", self.name, self.area) }
    }
}

/// Places matching `query` (up to 8, names in the interface language). None when offline.
pub fn search(query: &str) -> Option<Vec<Place>> {
    let q = query.trim();
    if q.is_empty() {
        return Some(Vec::new());
    }
    let lang = if crate::i18n::is_id() { "id" } else { "en" };
    let url = format!("https://geocoding-api.open-meteo.com/v1/search?count=8&language={lang}&name={}", crate::http::escape(q));
    Some(places(&get_json(&url)?))
}

/// The geocoding answer's results as places.
fn places(v: &Value) -> Vec<Place> {
    let Some(list) = v.get("results").and_then(Value::as_array) else { return Vec::new() };
    let text = |r: &Value, k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    list.iter()
            .filter_map(|r| {
                let name = text(r, "name");
                let mut parts: Vec<String> = Vec::new();
                for k in ["admin2", "admin1", "country"] {
                    let p = text(r, k);
                    if !p.is_empty() && p != name && !parts.contains(&p) {
                        parts.push(p);
                    }
                }
                // The district (admin2) only where it adds something: "Bekasi, Jawa Barat" not "Bekasi, Bekasi".
                if parts.len() > 2 {
                    parts.remove(0);
                }
            Some(Place { name, area: parts.join(", "), lat: r.get("latitude")?.as_f64()?, lon: r.get("longitude")?.as_f64()? })
        })
        .collect()
}

/// (lat, lon, "Name, Country"), remembered for the session.
fn place(city: &str) -> Option<(f64, f64, String)> {
    let k = city.trim().to_lowercase();
    if k.is_empty() {
        return None;
    }
    if let Some(p) = picked(city) {
        return Some(p);
    }
    if let Some(p) = with_cache(|c| c.places.get(&k).cloned()) {
        return Some(p);
    }
    let url = format!("https://geocoding-api.open-meteo.com/v1/search?count=1&language=en&name={}", crate::http::escape(city.trim()));
    let v = get_json(&url)?;
    let r = v.get("results")?.get(0)?;
    let name = r.get("name").and_then(Value::as_str).unwrap_or(city.trim());
    let label = match r.get("country").and_then(Value::as_str) {
        Some(country) => format!("{name}, {country}"),
        None => name.to_string(),
    };
    let p = (r.get("latitude")?.as_f64()?, r.get("longitude")?.as_f64()?, label);
    with_cache(|c| c.places.insert(k, p.clone()));
    Some(p)
}

/// GET + parse for Open-Meteo. It answers with `Content-Encoding: deflate` by default, which WinRT's HttpClient
/// fails to decode (0x80072F8F), so ask for gzip.
pub(crate) fn get_json(url: &str) -> Option<Value> {
    let r = crate::http::send("GET", url, &[("Accept-Encoding", "gzip")], None).ok()?;
    if !r.ok() {
        return None;
    }
    serde_json::from_str(&r.body).ok()
}

fn num(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

fn parse_current(v: &Value, city: String) -> Option<Current> {
    let cur = v.get("current")?;
    Some(Current {
        temp: cur.get("temperature_2m")?.as_f64()?,
        code: cur.get("weather_code")?.as_u64()? as u32,
        is_day: cur.get("is_day").and_then(Value::as_i64) == Some(1),
        city,
        feels: num(cur, "apparent_temperature"),
        humidity: num(cur, "relative_humidity_2m"),
        wind: num(cur, "wind_speed_10m"),
    })
}

fn fetch_current(city: &str, fahrenheit: bool) -> Option<Current> {
    let (lat, lon, name) = place(city)?;
    let unit = if fahrenheit { "&temperature_unit=fahrenheit" } else { "" };
    let v = get_json(&format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,weather_code,is_day&timezone=auto{unit}"
    ))?;
    parse_current(&v, name)
}

fn fetch_forecast(city: &str, fahrenheit: bool) -> Option<Forecast> {
    let (lat, lon, name) = place(city)?;
    let unit = if fahrenheit { "&temperature_unit=fahrenheit&wind_speed_unit=mph" } else { "" };
    let v = get_json(&format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}\
         &current=temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,wind_speed_10m,is_day\
         &hourly=temperature_2m,weather_code&forecast_hours=24\
         &daily=weather_code,temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=7{unit}"
    ))?;
    let current = parse_current(&v, name)?;

    let arr = |block: &str, k: &str| -> Vec<Value> {
        v.get(block).and_then(|b| b.get(k)).and_then(Value::as_array).cloned().unwrap_or_default()
    };
    let (times, temps, codes) = (arr("hourly", "time"), arr("hourly", "temperature_2m"), arr("hourly", "weather_code"));
    let hours = times
        .iter()
        .zip(temps.iter().zip(codes.iter()))
        .filter_map(|(t, (temp, code))| {
            // "2026-11-14T13:00"
            let hour = t.as_str()?.get(11..13)?.parse().ok()?;
            Some(Hour { hour, temp: temp.as_f64()?, code: code.as_u64()? as u32 })
        })
        .collect();

    let (dates, codes, max, min) =
        (arr("daily", "time"), arr("daily", "weather_code"), arr("daily", "temperature_2m_max"), arr("daily", "temperature_2m_min"));
    let days = (0..dates.len())
        .filter_map(|i| {
            Some(Day {
                date: dates[i].as_str()?.to_string(),
                code: codes.get(i)?.as_u64()? as u32,
                max: max.get(i)?.as_f64()?,
                min: min.get(i)?.as_f64()?,
            })
        })
        .collect();
    Some((current, hours, days))
}

/// WMO weather interpretation code → short text.
pub fn describe(code: u32) -> &'static str {
    match code {
        0 => "Clear",
        1 => "Mainly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51..=57 => "Drizzle",
        61..=67 => "Rain",
        71..=77 => "Snow",
        80..=82 => "Rain showers",
        85 | 86 => "Snow showers",
        95.. => "Thunderstorm",
        _ => "Unknown",
    }
}

/// WMO code → icon name for the UI: "sun", "moon", "cloud", "cloud-sun", "rain", "snow", "storm", "fog".
pub fn icon_kind(code: u32, is_day: bool) -> &'static str {
    match code {
        0 | 1 => if is_day { "sun" } else { "moon" },
        2 => if is_day { "cloud-sun" } else { "cloud" },
        3 => "cloud",
        45 | 48 => "fog",
        51..=67 | 80..=82 => "rain",
        71..=77 | 85 | 86 => "snow",
        95.. => "storm",
        _ => "cloud",
    }
}

/// "31°"
pub fn degrees(v: f64) -> String {
    format!("{}°", v.round() as i64)
}

#[cfg(test)]
mod place_tests {
    use super::*;

    #[test]
    fn search_results_name_the_region() {
        let v: Value = serde_json::from_str(
            r#"{"results":[
                {"name":"Bekasi","latitude":-6.2349,"longitude":106.9896,"country":"Indonesia","admin1":"Jawa Barat","admin2":"Kota Bekasi"},
                {"name":"Bekasi","latitude":-6.25,"longitude":107.15,"country":"Indonesia","admin1":"Jawa Barat","admin2":"Bekasi"},
                {"name":"Singapore","latitude":1.29,"longitude":103.85,"country":"Singapore"},
                {"name":"no coordinates","country":"X"}
            ]}"#,
        )
        .unwrap();
        let p = places(&v);
        assert_eq!(p.len(), 3, "results without coordinates are dropped");
        assert_eq!(p[0].label(), "Bekasi, Jawa Barat, Indonesia");
        assert_eq!(p[1].area, "Jawa Barat, Indonesia", "the district only when it differs from the name");
        assert_eq!(p[2].label(), "Singapore", "a city-state is just its name");
        assert!(places(&serde_json::json!({})).is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn jakarta() {
        // Keep one MTA alive for the whole test binary: windows-rs caches WinRT factories process-wide, and a
        // torn-down MTA (last MTA thread gone) leaves them dangling (access violation on the next HTTP call).
        unsafe { let _ = windows::Win32::System::Com::CoIncrementMTAUsage(); }
        let c = current("Jakarta", false).expect("current");
        println!("current: {} {} {} ({}) in {}", degrees(c.temp), describe(c.code), icon_kind(c.code, c.is_day), c.code, c.city);
        let t = Instant::now();
        let (c, hours, days) = forecast("Jakarta", false).expect("forecast");
        println!("forecast fetched in {:?}: now {} feels {} hum {}% wind {} km/h", t.elapsed(), degrees(c.temp), degrees(c.feels), c.humidity, c.wind);
        for h in hours.iter().take(6) {
            println!("  {:02}:00 {} {}", h.hour, degrees(h.temp), describe(h.code));
        }
        for d in &days {
            println!("  {} {} {} / {}", d.date, describe(d.code), degrees(d.max), degrees(d.min));
        }
        assert_eq!(hours.len(), 24);
        assert_eq!(days.len(), 7);
        let t = Instant::now();
        assert!(current("jakarta ", false).is_some());
        assert!(t.elapsed() < Duration::from_millis(5), "second call should come from the cache");
        let f = current("Jakarta", true).expect("fahrenheit");
        println!("fahrenheit: {}", degrees(f.temp));
    }
}
