//! Alarm (the Timers tab's Alarm segment): set a time (click it for the To-Dos clock face), the days it repeats on (none: once)
//! and an optional label, then Add. The list on the right shows each alarm with when it rings next, a switch
//! and × to delete. Ringing is done by `model` (sound + a pop-up that opens this tab).

use crate::gfx::{with_alpha, Align, LIGHT, REGULAR, SEMIBOLD};
use crate::notch::now;
use crate::services::alarms::{self, Alarm};
use crate::ui::{id, ButtonStyle, Rect, Ui};

use super::picker::{self, Dial, Field, Mode, Picked};
use crate::i18n::tr;

const LABEL_ID: u64 = id("alarm.label");
/// Days in Monday-first order (0 = Sunday … 6 = Saturday) and their chip letters.
const WEEK: [(u8, &str); 7] = [(1, "M"), (2, "T"), (3, "W"), (4, "T"), (5, "F"), (6, "S"), (0, "S")];

#[derive(Default)]
pub struct State {
    ready: bool,
    hour: u32,
    minute: u32,
    days: [bool; 7],
    /// The clock face, while picking the time.
    clock: Option<Dial>,
}

pub fn shown(s: &mut State) {
    if !s.ready {
        // Start at the next full hour.
        s.hour = (now().hour as u32 + 1) % 24;
        s.minute = 0;
        s.ready = true;
    }
}

/// Drawn by the Timers tab under its switch (the Alarm segment).
pub fn draw(ui: &mut Ui, s: &mut State, r: Rect) {
    shown(s);
    if let Some(a) = alarms::ringing() {
        ringing(ui, &a, r);
        return;
    }
    if let Some(mut c) = s.clock {
        ui.card(r, 10.0);
        match picker::dial(ui, &mut c, r.pad(16.0, 6.0, 16.0, 6.0), None) {
            Picked::None => s.clock = Some(c),
            Picked::Cancel | Picked::Clear => s.clock = None,
            Picked::Set(h, m) => {
                s.hour = h;
                s.minute = m;
                s.clock = None;
            }
        }
        return;
    }
    let (left, right) = r.take_left(272.0, 12.0);
    composer(ui, s, left);
    list(ui, right);
}

// ---------------------------------------------------------------------- new alarm

fn composer(ui: &mut Ui, s: &mut State, r: Rect) {
    let t = ui.t;
    ui.card(r, 10.0);
    let inner = r.pad(16.0, 8.0, 16.0, 10.0);

    // Time: the shared picker field (scroll the hour or the minutes; a click opens the dial on that part).
    let ty = inner.y + 2.0;
    let time_r = Rect::new(inner.x + 20.0, ty, inner.w - 40.0, 52.0);
    if let Field::Open(minutes) = picker::field(ui, id("alarm.time"), time_r, &mut s.hour, &mut s.minute, Mode::Clock, 34.0, true) {
        s.clock = Some(Dial::new(s.hour, s.minute, Mode::Clock, minutes));
    }

    // Repeat days, midway between the time and the label row.
    let row_y = inner.bottom() - 30.0;
    let d = 24.0;
    let chips_y = ((ty + 52.0 + row_y - d) / 2.0).round();
    let gap = ((inner.w - 7.0 * d) / 6.0).min(8.0);
    let cx0 = inner.cx() - (7.0 * d + 6.0 * gap) / 2.0;
    for (i, (_, letter)) in WEEK.iter().enumerate() {
        let c = Rect::new(cx0 + i as f32 * (d + gap), chips_y, d, d);
        let on = s.days[i];
        let hov = ui.hovered(c);
        ui.fill(c, d / 2.0, if on { t.accent } else if hov { t.hover } else { t.surface2 });
        ui.text(letter, c, 11.0, SEMIBOLD, if on { t.on_accent } else { t.subtext }, Align::Center);
        if ui.clicked(c) {
            s.days[i] = !s.days[i];
        }
    }

    // Label and Add (Enter in the field adds too).
    let add = Rect::new(inner.right() - 64.0, row_y, 64.0, 30.0);
    let field = Rect::new(inner.x, row_y, add.x - 8.0 - inner.x, 30.0);
    ui.fill(field, 8.0, t.surface2);
    let submitted = ui.text_field(LABEL_ID, field.pad(10.0, 0.0, 10.0, 0.0), tr("Label (optional)"), 13.0).is_some();
    if ui.button(add, tr("Add"), ButtonStyle::Accent) || submitted {
        let days: Vec<u8> = WEEK.iter().zip(s.days).filter(|(_, on)| *on).map(|((d, _), _)| *d).collect();
        let label = ui.field_text(LABEL_ID);
        alarms::add(s.hour, s.minute, &label, days);
        ui.set_field_text(LABEL_ID, "");
        ui.st.focus = 0;
        s.days = [false; 7];
    }
}

// ---------------------------------------------------------------------- the alarms

fn list(ui: &mut Ui, r: Rect) {
    let t = ui.t;
    let items = alarms::all();
    let (head, body) = r.take_top(22.0, 6.0);
    ui.text(tr("ALARMS"), Rect::new(head.x + 2.0, head.y, 100.0, head.h), 11.0, SEMIBOLD, t.subtext, Align::Left);
    if !items.is_empty() {
        let c = items.len().to_string();
        let cw = (ui.measure(&c, 10.0, SEMIBOLD) + 12.0).max(18.0);
        let x = head.x + 2.0 + ui.measure(tr("ALARMS"), 11.0, SEMIBOLD) + 8.0;
        let cr = Rect::new(x, head.cy() - 8.0, cw, 16.0);
        ui.fill(cr, 8.0, t.accent_soft);
        ui.text(&c, cr, 10.0, SEMIBOLD, t.accent, Align::Center);
    }
    if items.is_empty() {
        ui.card(body, 10.0);
        let cy = body.cy() - 14.0;
        ui.g.fill_circle(body.cx(), cy - 14.0, 17.0, ui.c(t.surface2));
        ui.icon("Alarm", body.cx(), cy - 14.0, 16.0, t.subtext);
        ui.text(tr("No alarms yet"), Rect::new(body.x, cy + 8.0, body.w, 20.0), 14.0, SEMIBOLD, t.text, Align::Center);
        ui.text(tr("Pick a time on the left and press Add."), Rect::new(body.x, cy + 30.0, body.w, 16.0), 12.0, REGULAR, t.subtext, Align::Center);
        return;
    }
    const ROW_H: f32 = 52.0;
    let n = now();
    let mut toggle = None;
    let mut delete = None;
    ui.scroll(id("alarm.list"), body, items.len() as f32 * (ROW_H + 6.0), |ui, top| {
        let t = ui.t;
        for (i, a) in items.iter().enumerate() {
            let row = Rect::new(body.x, top + i as f32 * (ROW_H + 6.0), body.w - 8.0, ROW_H);
            let hov = ui.hovered(row);
            ui.fill(row, 10.0, if hov { t.surface2 } else { t.surface });
            let inner = row.pad(14.0, 6.0, 12.0, 6.0);
            let time_c = if a.enabled { t.text } else { t.subtext };
            let tw = ui.measure(&a.time(), 24.0, LIGHT).ceil() + 4.0;
            ui.text(&a.time(), Rect::new(inner.x, inner.y, tw, inner.h), 24.0, LIGHT, time_c, Align::Left);
            let x = inner.x + tw + 12.0;
            let sw = Rect::new(inner.right() - 36.0, row.cy() - 10.0, 36.0, 20.0);
            let text_w = sw.x - 16.0 - x;
            let title = if a.label.is_empty() { tr("Alarm").to_string() } else { a.label.clone() };
            ui.text(&title, Rect::new(x, inner.y + 4.0, text_w, 18.0), 13.0, SEMIBOLD, time_c, Align::Left);
            let sub = match alarms::snoozed_for(a.id) {
                Some(m) => crate::i18n::tf!("Snoozed · rings again in {}m", "Ditunda · berbunyi lagi dalam {} mnt", m.max(1)),
                None if a.enabled => format!("{} · {}", a.repeat_text(), until(a, &n)),
                None => a.repeat_text(),
            };
            ui.text(&sub, Rect::new(x, inner.y + 22.0, text_w, 16.0), 11.0, REGULAR, t.subtext, Align::Left);
            if hov {
                let del = Rect::new(sw.x - 30.0, row.cy() - 11.0, 22.0, 22.0);
                if ui.icon_button(del, "XMark", 10.0, t.subtext) {
                    delete = Some(a.id);
                }
            }
            if ui.toggle(sw, a.enabled) {
                toggle = Some((a.id, !a.enabled));
            }
        }
    });
    if let Some((aid, on)) = toggle {
        alarms::set_enabled(aid, on);
    }
    if let Some(aid) = delete {
        alarms::remove(aid);
    }
}

/// The alarm ringing now: it keeps ringing until Snooze or Stop.
fn ringing(ui: &mut Ui, a: &Alarm, r: Rect) {
    let t = ui.t;
    ui.card(r, 10.0);
    // A gentle pulse around the bell while it rings.
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| (d.as_millis() % 60_000) as f32 / 1000.0);
    let pulse = (secs * 3.0).sin() * 0.5 + 0.5;
    ui.animate = true;
    let (cx, cy) = (r.x + 70.0, r.cy());
    ui.g.fill_circle(cx, cy, 34.0 + 6.0 * pulse, ui.c(with_alpha(t.accent, 0.12 + 0.10 * pulse)));
    ui.g.fill_circle(cx, cy, 30.0, ui.c(t.accent));
    ui.icon("Alarm", cx, cy, 26.0, t.on_accent);
    let x = cx + 56.0;
    let title = if a.label.is_empty() { "Alarm".to_string() } else { a.label.clone() };
    ui.text(&a.time(), Rect::new(x, cy - 40.0, 200.0, 44.0), 34.0, LIGHT, t.text, Align::Left);
    ui.text(&title, Rect::new(x, cy + 4.0, r.right() - x - 300.0, 20.0), 14.0, SEMIBOLD, t.text, Align::Left);
    ui.text(&a.repeat_text(), Rect::new(x, cy + 24.0, 200.0, 16.0), 12.0, REGULAR, t.subtext, Align::Left);
    let stop = Rect::new(r.right() - 20.0 - 110.0, cy - 20.0, 110.0, 40.0);
    let snooze = Rect::new(stop.x - 10.0 - 150.0, cy - 20.0, 150.0, 40.0);
    if ui.button(snooze, &crate::i18n::tf!("Snooze {} min", "Tunda {} mnt", alarms::SNOOZE_MIN), ButtonStyle::Normal) {
        alarms::snooze();
    }
    if ui.button(stop, "Stop", ButtonStyle::Accent) {
        alarms::stop();
    }
}

/// "in 45m", "in 7h 20m", "in 2d 3h".
fn until(a: &Alarm, n: &crate::notch::Now) -> String {
    let m = a.minutes_until(n.dow, n.hour as u32, n.minute as u32);
    let (d, h, mi) = (m / 1440, m % 1440 / 60, m % 60);
    if d > 0 {
        format!("in {d}d {h}h")
    } else if h > 0 {
        format!("in {h}h {mi}m")
    } else {
        format!("in {}m", mi.max(1))
    }
}
