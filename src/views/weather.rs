//! Weather: city field (Enter saves `weather_city`), status and refresh; current conditions (glyph, big
//! temperature, description, place, feels/humidity/wind/range); a 7-day strip.
//! Data from Open-Meteo through `services::weather::forecast`, fetched on a background thread; a skeleton
//! shows while the first load (or a new city) is on its way, a plain refresh keeps the old data on screen.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::bg;
use crate::gfx::{color, Align, BOLD, LIGHT, MEDIUM, SEMIBOLD};
use crate::services::{settings, weather};
use crate::ui::{id, Rect, Ui};
use crate::i18n::tr;

type Forecast = (weather::Current, Vec<weather::Hour>, Vec<weather::Day>);

#[derive(Default)]
struct Data {
    loading: bool,
    forecast: Option<Arc<Forecast>>,
    status: String,
    /// (city, fahrenheit) the forecast belongs to.
    key: Option<(String, bool)>,
}

#[derive(Default)]
pub struct State {
    data: Arc<Mutex<Data>>,
    last_fetch: Option<Instant>,
    field_ready: bool,
}

const FIELD: &str = "weather.city";

fn key() -> (String, bool) {
    let s = settings::shared();
    (s.weather_city.trim().to_string(), s.use_fahrenheit)
}

fn load(s: &mut State, new_place: bool) {
    {
        let mut d = s.data.lock().unwrap();
        if d.loading {
            return;
        }
        d.loading = true;
        if new_place {
            d.forecast = None;
        }
        d.status = if d.forecast.is_none() { tr("Loading…").into() } else { tr("Updating…").into() };
    }
    s.last_fetch = Some(Instant::now());
    let (city, f) = key();
    let data = s.data.clone();
    bg::spawn(move || {
        let res = if city.is_empty() { None } else { weather::forecast(&city, f) };
        let mut d = data.lock().unwrap();
        d.loading = false;
        d.key = Some((city.clone(), f));
        match res {
            Some(fc) => {
                d.forecast = Some(Arc::new(fc));
                let n = crate::notch::now();
                d.status = crate::i18n::tf!("Updated {:02}:{:02}", "Diperbarui {:02}:{:02}", n.hour, n.minute);
            }
            None if city.is_empty() => d.status = tr("Type a city and press Enter").into(),
            None => d.status = crate::i18n::tf!("Couldn't load weather for \"{city}\"", "Tidak bisa memuat cuaca untuk \"{city}\""),
        }
    });
}

pub fn shown(s: &mut State) {
    s.field_ready = false;
    let stale = s.last_fetch.map_or(true, |t| t.elapsed().as_secs() > 15 * 60);
    let changed = s.data.lock().unwrap().key.as_ref() != Some(&key());
    if stale || changed {
        load(s, changed);
    }
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, _go: &mut Option<&'static str>) {
    let t = ui.t;
    // Settings may change the city or unit while the tab is open.
    {
        let changed = {
            let d = s.data.lock().unwrap();
            !d.loading && d.key.is_some() && d.key.as_ref() != Some(&key())
        };
        if changed {
            load(s, true);
        }
    }
    if !s.field_ready && ui.st.focus != id(FIELD) {
        ui.set_field_text(id(FIELD), &settings::shared().weather_city);
        s.field_ready = true;
    }

    // Top row: city field, status, refresh.
    let (top, rest) = r.take_top(34.0, 0.0);
    let field = Rect::new(top.x, top.y, 170.0, top.h);
    if ui.st.focus == id(FIELD) {
        ui.g.stroke_round(field.inset(-1.5, -1.5).d2d(), 10.5, ui.c(t.accent_soft), 3.0);
    }
    ui.fill(field, 9.0, t.surface2);
    ui.g.stroke_round(field.inset(0.5, 0.5).d2d(), 8.5, ui.c(t.stroke), 1.0);
    ui.icon("MapPin", field.x + 18.5, field.cy(), 15.0, t.subtext);
    if let Some(city) = ui.text_field(id(FIELD), field.pad(36.0, 0.0, 10.0, 0.0), tr("City"), 13.0) {
        ui.set_field_text(id(FIELD), &city);
        ui.st.focus = 0;
        settings::update_quiet(|x| x.weather_city = city);
        load(s, true);
    }
    let (status, loading, fc) = {
        let d = s.data.lock().unwrap();
        (d.status.clone(), d.loading, d.forecast.clone())
    };
    let refresh = Rect::new(top.right() - 30.0, top.cy() - 15.0, 30.0, 30.0);
    ui.text(&status, Rect::new(field.right() + 10.0, top.y, refresh.x - field.right() - 20.0, top.h), 12.0, SEMIBOLD, t.subtext, Align::Left);
    if ui.icon_button(refresh, "Refresh", 14.0, t.subtext) {
        weather::invalidate();
        load(s, false);
    }

    let (strip, cur) = rest.take_bottom(STRIP_H, 0.0);
    match fc.as_deref() {
        Some((now, _, days)) => {
            current(ui, cur, now, days.first());
            daily_strip(ui, strip, days);
        }
        None if loading => skeleton(ui, cur, strip),
        None => {}
    }
}

/// The forecast row's height (caps label, glyph, high, low).
const STRIP_H: f32 = 108.0;

/// Big glyph, temperature + description, place; the 2×2 details on the right.
fn current(ui: &mut Ui, r: Rect, w: &weather::Current, today: Option<&weather::Day>) {
    let t = ui.t;
    glyph(ui, r.x + 40.0, r.y + 55.0, 64.0, w.code, w.is_day);
    let x = r.x + 92.0;
    let temp = weather::degrees(w.temp);
    let tw = ui.measure(&temp, 40.0, LIGHT);
    ui.text(&temp, Rect::new(x, r.y + 9.0, tw + 6.0, 54.0), 40.0, LIGHT, t.text, Align::Left);
    ui.text(crate::i18n::weather(weather::describe(w.code)), Rect::new(x + tw + 10.0, r.y + 36.0, 220.0, 20.0), 14.0, MEDIUM, t.text, Align::Left);
    ui.text(&w.city, Rect::new(x, r.y + 69.0, 340.0, 18.0), 12.0, SEMIBOLD, t.subtext, Align::Left);

    let unit = if settings::shared().use_fahrenheit { "mph" } else { "km/h" };
    let range = today.map_or("–".to_string(), |d| format!("{} / {}", weather::degrees(d.max), weather::degrees(d.min)));
    let items: [(&'static str, String); 4] = [
        ("Fire", crate::i18n::tf!("Feels {}", "Terasa {}", weather::degrees(w.feels))),
        ("Drop", format!("{:.0}%", w.humidity)),
        ("Cloud", format!("{:.0} {unit}", w.wind)),
        ("ArrowsUpDown", range),
    ];
    let m = |ui: &Ui, s: &str| ui.measure(s, 12.0, SEMIBOLD);
    let col_w = [m(ui, &items[0].1).max(m(ui, &items[2].1)), m(ui, &items[1].1).max(m(ui, &items[3].1))];
    // Each cell: 13 px icon, 5 px gap, text; 14 px between the columns; 4 px from the right edge.
    let right = r.right() - 4.0;
    let x1 = right - col_w[1] - 18.0;
    let x0 = x1 - 14.0 - col_w[0] - 18.0;
    for (i, (icon, label)) in items.iter().enumerate() {
        let x = if i % 2 == 0 { x0 } else { x1 };
        let cy = r.y + 42.0 + (i / 2) as f32 * 19.0;
        ui.icon(icon, x + 6.5, cy, 13.0, t.subtext);
        ui.text(label, Rect::new(x + 18.0, cy - 9.0, 140.0, 18.0), 12.0, SEMIBOLD, t.subtext, Align::Left);
    }
}

fn daily_strip(ui: &mut Ui, r: Rect, days: &[weather::Day]) {
    let t = ui.t;
    let n = days.len().clamp(1, 7);
    for (i, (d, c)) in days.iter().zip(r.columns(n, 4.0)).enumerate() {
        let label = if i == 0 { tr("TODAY").to_string() } else { day_name(&d.date).to_uppercase() };
        ui.text(&label, Rect::new(c.x, c.y + 7.0, c.w, 18.0), 11.0, BOLD, t.subtext, Align::Center);
        glyph(ui, c.cx(), c.y + 45.0, 30.0, d.code, true);
        ui.text(&weather::degrees(d.max), Rect::new(c.x, c.y + 64.0, c.w, 20.0), 14.0, SEMIBOLD, t.text, Align::Center);
        ui.text(&weather::degrees(d.min), Rect::new(c.x, c.y + 81.0, c.w, 18.0), 13.0, SEMIBOLD, t.subtext, Align::Center);
    }
}

/// A weather glyph drawn from the icon set in the colours of the emoji the C# version shows
/// (☀️ 🌤️ ⛅ ☁️ 🌫️ 🌦️ 🌧️ 🌨️ ⛈️): a pale cloud with a grey rim, a sun peeking out, slanted blue drops.
fn glyph(ui: &mut Ui, cx: f32, cy: f32, size: f32, code: u32, day: bool) {
    let sun = color(0xFFB02E, 1.0);
    let cloud = color(0xF3EEF8, 1.0);
    let rim = color(0x9B9B9B, 1.0);
    let storm = color(0x2D4FC1, 1.0);
    let drop = color(0x3F5AE0, 1.0);
    let moon = color(0xFCD53F, 1.0);
    // A cloud with an outline: the same shape a touch larger in the rim colour behind it.
    let cloud_at = |ui: &mut Ui, x: f32, y: f32, s: f32, fill: crate::gfx::Color, edge: crate::gfx::Color| {
        ui.icon("Cloud", x, y, s * 1.08, edge);
        ui.icon("Cloud", x, y, s, fill);
    };
    let drops = |ui: &mut Ui, x: f32, y: f32, s: f32, c: crate::gfx::Color| {
        let w = (s * 0.075).max(1.6);
        for k in -1..=1 {
            let dx = x + k as f32 * s * 0.2;
            ui.g.line(dx + s * 0.04, y, dx - s * 0.04, y + s * 0.11, ui.c(c), w);
        }
    };
    match code {
        0 if day => ui.icon("Sun", cx, cy, size * 0.9, sun),
        0 | 1 if !day => ui.icon("Moon", cx, cy, size * 0.75, moon),
        1 => {
            ui.icon("Sun", cx - size * 0.1, cy - size * 0.1, size * 0.8, sun);
            cloud_at(ui, cx + size * 0.18, cy + size * 0.18, size * 0.5, cloud, rim);
        }
        2 => {
            ui.icon("Sun", cx - size * 0.2, cy - size * 0.2, size * 0.6, sun);
            cloud_at(ui, cx + size * 0.06, cy + size * 0.06, size * 0.8, cloud, rim);
        }
        45 | 48 => {
            cloud_at(ui, cx, cy - size * 0.1, size * 0.8, cloud, rim);
            for k in 0..2 {
                let y = cy + size * (0.22 + 0.12 * k as f32);
                ui.g.line(cx - size * 0.32, y, cx + size * 0.32, y, ui.c(rim), (size * 0.06).max(1.4));
            }
        }
        51..=57 | 80..=82 | 61..=67 => {
            if !matches!(code, 61..=67) {
                ui.icon("Sun", cx - size * 0.2, cy - size * 0.24, size * 0.55, sun);
            }
            cloud_at(ui, cx + size * 0.04, cy - size * 0.06, size * 0.8, cloud, rim);
            drops(ui, cx, cy + size * 0.24, size, drop);
        }
        71..=77 | 85 | 86 => {
            cloud_at(ui, cx, cy - size * 0.08, size * 0.8, cloud, rim);
            for k in -1..=1 {
                ui.g.fill_circle(cx + k as f32 * size * 0.2, cy + size * 0.3, (size * 0.05).max(1.3), ui.c(color(0x9FD7F5, 1.0)));
            }
        }
        95.. => {
            cloud_at(ui, cx, cy - size * 0.1, size * 0.8, storm, color(0x1C3A9E, 1.0));
            ui.icon("Bolt", cx, cy + size * 0.08, size * 0.32, color(0xFF8A2B, 1.0));
            drops(ui, cx, cy + size * 0.26, size, drop);
        }
        _ => cloud_at(ui, cx, cy, size * 0.85, cloud, rim),
    }
}

/// Placeholder blocks shaped like the content.
fn skeleton(ui: &mut Ui, cur: Rect, strip: Rect) {
    let c = ui.t.surface;
    ui.g.fill_circle(cur.x + 40.0, cur.y + 55.0, 28.0, ui.c(c));
    ui.fill(Rect::new(cur.x + 92.0, cur.y + 20.0, 76.0, 44.0), 8.0, c);
    ui.fill(Rect::new(cur.x + 178.0, cur.y + 39.0, 92.0, 14.0), 4.0, c);
    ui.fill(Rect::new(cur.x + 92.0, cur.y + 73.0, 150.0, 11.0), 4.0, c);
    for i in 0..4 {
        let x = cur.right() - 160.0 + (i % 2) as f32 * 82.0;
        let y = cur.y + 36.0 + (i / 2) as f32 * 19.0;
        ui.fill(Rect::new(x, y, 62.0, 11.0), 4.0, c);
    }
    for col in strip.columns(7, 4.0) {
        ui.fill(Rect::new(col.cx() - 16.0, col.y + 11.0, 32.0, 10.0), 3.0, c);
        ui.g.fill_circle(col.cx(), col.y + 45.0, 13.0, ui.c(c));
        ui.fill(Rect::new(col.cx() - 14.0, col.y + 67.0, 28.0, 13.0), 4.0, c);
        ui.fill(Rect::new(col.cx() - 11.0, col.y + 85.0, 22.0, 11.0), 4.0, c);
    }
    ui.animate = true;
}

/// "2026-11-15" → "Sun".
fn day_name(date: &str) -> &'static str {
    let mut p = date.split('-').filter_map(|x| x.parse::<i32>().ok());
    let (Some(mut y), Some(m), Some(d)) = (p.next(), p.next(), p.next()) else { return "" };
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    if m < 3 {
        y -= 1;
    }
    let dow = (y + y / 4 - y / 100 + y / 400 + T[((m - 1) as usize).min(11)] + d).rem_euclid(7);
    crate::i18n::day3(dow as usize)
}
