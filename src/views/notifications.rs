//! Notifications: everything other apps showed, newest first, grouped by day (Today / Yesterday / weekday /
//! "3 November"). Click a card to read a long message in full; × hides one, Clear all empties the history.
//! When Windows refuses access, the reason from `notifications::problem()` is shown instead.

use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::gfx::{Align, REGULAR, SEMIBOLD};
use crate::notch::now;
use crate::services::appicons;
use crate::services::notifications::{self, Notification};
use crate::services::settings;
use crate::ui::{id, Rect, Ui};
use crate::i18n::tr;

const BODY_LINE: f32 = 17.0;
const COLLAPSED_LINES: usize = 3;

#[derive(Default)]
pub struct State {
    expanded: HashSet<u32>,
    /// Cards deleted with ×.
    hidden: HashSet<u32>,
}

pub fn shown(_s: &mut State) {}

fn items(s: &State) -> Vec<Notification> {
    let mut v = notifications::history();
    if std::env::var("FATHNOTCH_DEMO_NOTIFS").is_ok() && v.is_empty() {
        v = demo();
    }
    v.retain(|n| !s.hidden.contains(&n.id));
    v
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, _go: &mut Option<&'static str>) {
    let t = ui.t;
    let list = items(s);
    let (head, body) = r.take_top(26.0, 8.0);

    let count = match list.len() {
        0 => String::new(),
        1 => tr("1 notification").into(),
        n => crate::i18n::tf!("{n} notifications", "{n} notifikasi"),
    };
    ui.text(&count, head, 12.0, REGULAR, t.subtext, Align::Left);
    if !list.is_empty() {
        let w = ui.measure(tr("Clear all"), 11.0, REGULAR) + 34.0;
        let b = Rect::new(head.right() - 10.0 - w, head.y, w, head.h);
        if ui.hit(b, 8.0) {
            notifications::clear();
            s.hidden.clear();
            s.expanded.clear();
        }
        ui.icon("Trash", b.x + 14.0, b.cy(), 12.0, t.subtext);
        ui.text(tr("Clear all"), Rect::new(b.x + 25.0, b.y, w - 25.0, b.h), 11.0, REGULAR, t.subtext, Align::Left);
    }

    if list.is_empty() {
        empty(ui, body);
        return;
    }

    // Lay out day headers and cards, then draw what's visible.
    let width = body.w - 10.0;
    let text_w = width - 24.0 - 41.0;
    let offset = local_offset();
    enum Item<'a> {
        Day(String),
        /// Card, height, body lines shown, whether the body is longer than the collapsed card shows.
        Card(&'a Notification, f32, usize, bool),
    }
    let mut layout: Vec<(f32, Item)> = Vec::new();
    let mut y = 0.0;
    let mut day: Option<i64> = None;
    for n in &list {
        let d = local_day(n.arrived, offset);
        if day != Some(d) {
            if day.is_some() {
                y += 8.0;
            }
            layout.push((y, Item::Day(day_label(d, offset))));
            y += 20.0;
            day = Some(d);
        }
        let est = if n.body.trim().is_empty() {
            0
        } else {
            (ui.measure(n.body.trim(), 12.0, REGULAR) / text_w.max(1.0)).ceil().max(1.0) as usize
        };
        let lines = if s.expanded.contains(&n.id) { est } else { est.min(COLLAPSED_LINES) };
        let h = 9.0 + 15.0 + 2.0 + 18.0 + if lines > 0 { 2.0 + lines as f32 * BODY_LINE } else { 0.0 } + 10.0;
        layout.push((y, Item::Card(n, h, lines, est > COLLAPSED_LINES)));
        y += h + 6.0;
    }

    let mut toggle = None;
    let mut delete = None;
    ui.scroll(id("notif.list"), body, y, |ui, top| {
        let t = ui.t;
        for (y, item) in &layout {
            match item {
                Item::Day(label) => {
                    let r = Rect::new(body.x + 2.0, top + y, 300.0, 16.0);
                    ui.text(label, r, 11.0, SEMIBOLD, t.subtext, Align::Left);
                }
                Item::Card(n, h, lines, long) => {
                    let card = Rect::new(body.x, top + y, width, *h);
                    if card.bottom() < body.y || card.y > body.bottom() {
                        continue;
                    }
                    let hov = ui.hovered(card);
                    ui.fill(card, 16.0, if hov { t.surface2 } else { t.surface });
                    let inner = card.pad(12.0, 9.0, 12.0, 10.0);
                    // App tile: its icon, else the first letter.
                    let tile = Rect::new(inner.x, inner.y + 1.0, 30.0, 30.0);
                    if n.icon.is_empty() {
                        app_tile(ui, &n.app_id, &n.app, tile, 8.0, 24.0, 13.0);
                    } else {
                        ui.fill(tile, 8.0, t.accent);
                        ui.icon(own_icon(&n.icon), tile.cx(), tile.cy(), 14.0, t.on_accent);
                    }
                    let x = tile.right() + 11.0;
                    let w = inner.right() - x;
                    let meta = format!("{} · {}", n.app, age(n.arrived, offset));
                    ui.text(&meta, Rect::new(x, inner.y, w - 26.0, 15.0), 11.0, REGULAR, t.subtext, Align::Left);
                    ui.text(&n.title, Rect::new(x, inner.y + 17.0, w, 18.0), 13.0, SEMIBOLD, t.text, Align::Left);
                    if *lines > 0 {
                        let br = Rect::new(x, inner.y + 37.0, w, *lines as f32 * BODY_LINE);
                        ui.push_clip(Rect::new(br.x, br.y, br.w, br.h + 1.0));
                        ui.wrap(n.body.trim(), br, 12.0, REGULAR, t.subtext);
                        ui.pop_clip();
                    }
                    if hov {
                        let del = Rect::new(inner.right() - 18.0, inner.y - 4.0, 22.0, 22.0);
                        if ui.icon_button(del, "XMark", 10.0, t.subtext) {
                            delete = Some(n.id);
                        }
                        // A long message: the chevron shows it in full (or folds it back).
                        if *long {
                            let more = Rect::new(del.x - 24.0, del.y, 22.0, 22.0);
                            let open = s.expanded.contains(&n.id);
                            if ui.icon_button(more, if open { "ChevronUp" } else { "ChevronDown" }, 11.0, t.subtext) {
                                toggle = Some(n.id);
                            }
                        }
                    }
                    // Clicking the card opens the app it came from (WhatsApp…); FathNotch's own entries
                    // (and long ones without an app) unfold instead.
                    if ui.clicked(card) {
                        if n.icon.is_empty() {
                            let (id, name) = (n.app_id.clone(), n.app.clone());
                            crate::bg::spawn(move || crate::shell::open_app(&id, &name));
                        } else if *long {
                            toggle = Some(n.id);
                        }
                    }
                }
            }
        }
    });
    if let Some(i) = delete {
        s.hidden.insert(i);
        crate::services::notifications::remove(i);
        s.expanded.remove(&i);
    }
    if let Some(i) = toggle {
        if !s.expanded.remove(&i) {
            s.expanded.insert(i);
        }
    }
}

fn empty(ui: &mut Ui, r: Rect) {
    let t = ui.t;
    let problem = notifications::problem();
    let cy = r.cy() - 14.0;
    ui.g.fill_circle(r.cx(), cy - 18.0, 17.0, ui.c(t.surface2));
    ui.icon("Bell", r.cx(), cy - 18.0, 16.0, t.subtext);
    let title = if problem.is_some() { tr("Can't read notifications") } else { tr("No notifications yet") };
    ui.text(title, Rect::new(r.x, cy + 4.0, r.w, 22.0), 14.0, SEMIBOLD, t.text, Align::Center);
    let st = settings::shared();
    let sub = match &problem {
        Some(p) => p.clone(),
        None if !st.notify_enabled => tr("Turn on notifications in Settings → Alerts to collect them.").into(),
        None if !st.notify_keep_history => tr("History is off. Turn on Keep history in Settings to save them here.").into(),
        None => tr("Notifications that pop out of the notch are kept here.").into(),
    };
    ui.text(&sub, Rect::new(r.x, cy + 27.0, r.w, 18.0), 12.0, REGULAR, t.subtext, Align::Center);
    if problem.is_some() {
        let w = 150.0;
        let b = Rect::new(r.cx() - w / 2.0, cy + 52.0, w, 28.0);
        if ui.button(b, tr("Open privacy settings"), crate::ui::ButtonStyle::Normal) {
            crate::shell::open_settings_page("ms-settings:privacy-notifications");
        }
    }
}

/// The icon a FathNotch alert was raised with (the names `push_alert` callers use).
fn own_icon(name: &str) -> &'static str {
    const KNOWN: &[&str] = &["Alarm", "ArrowDown", "ArrowUp", "Clock", "Moon", "CalendarDays", "Bell", "Bolt", "DevicePhone"];
    KNOWN.iter().copied().find(|k| *k == name).unwrap_or("Bell")
}

/// A rounded tile with the app's icon (`icon` px, centred), or its first letter until / unless one is found.
fn app_tile(ui: &mut Ui, app_id: &str, app: &str, tile: Rect, radius: f32, icon: f32, letter_size: f32) {
    let t = ui.t;
    ui.fill(tile, radius, t.hover);
    let px = (icon * ui.g.scale).round() as u32;
    let r = Rect::new(tile.cx() - icon / 2.0, tile.cy() - icon / 2.0, icon, icon);
    let drawn = appicons::app(app_id, app, px).is_some_and(|(k, png)| {
        ui.g.load_image(k, &png) && {
            ui.g.image(k, r.d2d(), 0.0, ui.alpha);
            true
        }
    });
    if !drawn {
        let letter = app.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or("?".into());
        ui.text(&letter, tile, letter_size, SEMIBOLD, t.text, Align::Center);
    }
}

// ------------------------------------------------------------------ time

/// Days since 1970-01-01 for a civil date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

fn unix(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// Local time minus UTC, in seconds (rounded to minutes).
fn local_offset() -> i64 {
    let n = now();
    let local = days_from_civil(n.year as i64, n.month as i64, n.day as i64) * 86_400 + n.hour as i64 * 3600 + n.minute as i64 * 60 + n.second as i64;
    let diff = local - unix(SystemTime::now());
    (diff as f64 / 60.0).round() as i64 * 60
}

fn local_day(t: SystemTime, offset: i64) -> i64 {
    (unix(t) + offset).div_euclid(86_400)
}

fn day_label(day: i64, offset: i64) -> String {
    let today = local_day(SystemTime::now(), offset);
    if day == today {
        return tr("Today").into();
    }
    if day == today - 1 {
        return tr("Yesterday").into();
    }
    if day > today - 7 {
        return crate::i18n::day_name((day + 4).rem_euclid(7) as usize).into();
    }
    let (_, m, d) = civil_from_days(day);
    format!("{d} {}", crate::i18n::month_name(m as usize))
}

/// "now", "5m ago", "2h ago" (today, under 6 h); the clock time otherwise.
fn age(at: SystemTime, offset: i64) -> String {
    let secs = SystemTime::now().duration_since(at).unwrap_or(Duration::ZERO).as_secs();
    if secs < 60 {
        return tr("now").into();
    }
    if secs < 3600 {
        return crate::i18n::tf!("{}m ago", "{} mnt lalu", secs / 60);
    }
    let today = local_day(SystemTime::now(), offset) == local_day(at, offset);
    if today && secs < 6 * 3600 {
        return crate::i18n::tf!("{}h ago", "{} jam lalu", secs / 3600);
    }
    let local = (unix(at) + offset).rem_euclid(86_400);
    format!("{:02}:{:02}", local / 3600, local / 60 % 60)
}

/// Sample cards for screenshots (FATHNOTCH_DEMO_NOTIFS=1, only while the real history is empty).
fn demo() -> Vec<Notification> {
    let at = |mins: u64| SystemTime::now() - Duration::from_secs(mins * 60);
    let n = |id, app: &str, title: &str, body: &str, mins| Notification { id, app: app.into(), app_id: String::new(), title: title.into(), body: body.into(), arrived: at(mins), icon: String::new() };
    vec![
        n(1, "WhatsApp", tr("Aulia UM"), tr("btw, did you see the Microsoft thing? · 2 unread messages"), 40),
        n(2, "WhatsApp", tr("Aulia UM"), tr("On my way, see you in ten minutes · 3 unread messages"), 49),
        n(3, tr("Outlook"), tr("Weekly sync moved"), tr("The weekly sync has been moved to Thursday at 10:00. Please update your calendars and let me know if this clashes with anything else you have planned that day, otherwise see you there."), 300),
        n(4, tr("Slack"), "#general", tr("Deploy finished"), 60 * 26),
    ]
}
