//! The one time picker every tool uses (To-Dos, Calendar, Timer, Pomodoro, Alarm):
//!
//! * `field`: the time as text ("07:30", or "25 min" / "1h 30m" for a length). Two fingers or the wheel over
//!   the hour or the minutes change that part by one; a click opens the dial on that part.
//! * `dial`: the clock face. Pick the hour, then any minute (not just the labelled fives; there's a tick for
//!   each). Time of day has AM / PM; a length has hours 0–11 instead. Set, an optional "clear" button
//!   (All day…), ✕.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f32::consts::TAU;

use crate::gfx::{with_alpha, Align, LIGHT, MEDIUM, REGULAR, SEMIBOLD};
use crate::ui::{id, Rect, Ui};
use crate::i18n::tr;

/// What's being picked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// A time of day, 00:00–23:59.
    Clock,
    /// A length, 0:01–11:59 (timers, pomodoro).
    Length,
}

#[derive(Clone, Copy, Debug)]
pub struct Dial {
    pub hour: u32,
    pub minute: u32,
    /// On the minutes (else the hour).
    pub minutes: bool,
    pub mode: Mode,
    dragging: bool,
}

impl Dial {
    pub fn new(hour: u32, minute: u32, mode: Mode, minutes: bool) -> Self {
        let hour = if mode == Mode::Clock { hour % 24 } else { hour.min(11) };
        Self { hour, minute: minute % 60, minutes, mode, dragging: false }
    }
}

pub enum Picked {
    None,
    Cancel,
    Set(u32, u32),
    /// The optional extra button (All day).
    Clear,
}

/// The clock face and its read-out, laid out in `r` (the dial is as tall as `r`, up to 170).
pub fn dial(ui: &mut Ui, c: &mut Dial, r: Rect, clear: Option<&str>) -> Picked {
    let t = ui.t;
    let size = r.h.min(170.0);
    let radius = size * 64.0 / 170.0;
    let face = Rect::new(r.x, r.y + (r.h - size) / 2.0, size, size);
    let (cx, cy) = (face.cx(), face.cy());

    // Press on the face, drag, release: the hour moves on to the minutes.
    let pick = |c: &mut Dial, p: (f32, f32)| {
        let mut a = (p.0 - cx).atan2(cy - p.1);
        if a < 0.0 {
            a += TAU;
        }
        if c.minutes {
            c.minute = ((a / TAU * 60.0).round() as u32) % 60;
        } else {
            let slot = ((a / TAU * 12.0).round() as u32) % 12;
            c.hour = match c.mode {
                Mode::Clock => slot + if c.hour >= 12 { 12 } else { 0 },
                Mode::Length => slot,
            };
        }
    };
    let dial_id = id("picker.dial");
    if ui.clicked(face) {
        c.dragging = true;
        ui.st.active = dial_id;
        pick(c, ui.mouse());
    } else if c.dragging {
        if ui.input.down {
            pick(c, ui.mouse());
            ui.animate = true;
        } else {
            c.dragging = false;
            if ui.st.active == dial_id {
                ui.st.active = 0;
            }
            c.minutes = true;
        }
    }

    // Face, minute ticks, hand, numbers.
    ui.g.fill_circle(cx, cy, size / 2.0, ui.c(t.surface2));
    if c.minutes {
        let tr = size / 2.0 - 5.0;
        for m in (0..60).filter(|m| m % 5 != 0) {
            let a = m as f32 / 60.0 * TAU;
            ui.g.fill_circle(cx + tr * a.sin(), cy - tr * a.cos(), 1.0, ui.c(with_alpha(t.text, 0.3)));
        }
    }
    let slot = if c.minutes { c.minute as f32 / 5.0 } else { (c.hour % 12) as f32 };
    let a = slot / 12.0 * TAU;
    let (hx, hy) = (cx + radius * a.sin(), cy - radius * a.cos());
    ui.g.line(cx, cy, hx, hy, ui.c(t.accent), 1.5);
    ui.g.fill_circle(cx, cy, 3.0, ui.c(t.accent));
    let between = c.minutes && c.minute % 5 != 0;
    ui.g.fill_circle(hx, hy, if between { 13.0 } else { 15.0 }, ui.c(t.accent));
    for i in 0..12 {
        let d = (i as f32 - slot).abs();
        if between && d.min(12.0 - d) < 0.5 {
            continue; // under the knob
        }
        let selected = d < 0.01;
        let label = match (c.minutes, c.mode) {
            (true, _) => format!("{:02}", i * 5),
            (false, Mode::Clock) => (if i == 0 { 12 } else { i }).to_string(),
            (false, Mode::Length) => i.to_string(),
        };
        let an = i as f32 / 12.0 * TAU;
        let (lx, ly) = (cx + radius * an.sin(), cy - radius * an.cos());
        ui.text(
            &label,
            Rect::new(lx - 15.0, ly - 9.0, 30.0, 18.0),
            12.5,
            if selected { SEMIBOLD } else { REGULAR },
            if selected { t.on_accent } else { t.text },
            Align::Center,
        );
    }
    if between {
        ui.text(&format!("{:02}", c.minute), Rect::new(hx - 15.0, hy - 9.0, 30.0, 18.0), 11.0, SEMIBOLD, t.on_accent, Align::Center);
    }

    // Read-out: HH:MM (tap a part to go back to it), AM / PM or the units, hint, buttons.
    let sx = face.right() + 22.0;
    let big_h = 46.0;
    let top = r.y + (r.h - (big_h + 30.0 + 30.0)) / 2.0;
    let hs = match c.mode {
        Mode::Clock => format!("{:02}", if c.hour % 12 == 0 { 12 } else { c.hour % 12 }),
        Mode::Length => c.hour.to_string(),
    };
    let ms = format!("{:02}", c.minute);
    let hour_r = Rect::new(sx, top, ui.measure(&hs, 34.0, LIGHT), big_h);
    let colon_r = Rect::new(hour_r.right() + 2.0, top, ui.measure(":", 34.0, LIGHT), big_h);
    let min_r = Rect::new(colon_r.right() + 2.0, top, ui.measure(&ms, 34.0, LIGHT), big_h);
    ui.text(&hs, hour_r, 34.0, LIGHT, if c.minutes { t.subtext } else { t.accent }, Align::Left);
    ui.text(":", colon_r, 34.0, LIGHT, t.subtext, Align::Left);
    ui.text(&ms, min_r, 34.0, LIGHT, if c.minutes { t.accent } else { t.subtext }, Align::Left);
    if ui.clicked(hour_r) {
        c.minutes = false;
    }
    if ui.clicked(min_r) {
        c.minutes = true;
    }
    let a1 = Rect::new(min_r.right() + 12.0, top + big_h / 2.0 - 18.0, 30.0, 16.0);
    let a2 = Rect::new(a1.x, a1.bottom() + 4.0, 30.0, 16.0);
    match c.mode {
        Mode::Clock => {
            ui.text(tr("AM"), a1, 12.0, SEMIBOLD, if c.hour < 12 { t.accent } else { t.subtext }, Align::Left);
            ui.text(tr("PM"), a2, 12.0, SEMIBOLD, if c.hour >= 12 { t.accent } else { t.subtext }, Align::Left);
            if ui.clicked(a1) && c.hour >= 12 {
                c.hour -= 12;
            }
            if ui.clicked(a2) && c.hour < 12 {
                c.hour += 12;
            }
        }
        Mode::Length => {
            ui.text("hr", a1, 12.0, SEMIBOLD, if c.minutes { t.subtext } else { t.accent }, Align::Left);
            ui.text("min", a2, 12.0, SEMIBOLD, if c.minutes { t.accent } else { t.subtext }, Align::Left);
        }
    }
    let hint = match (c.minutes, c.mode) {
        (true, _) => tr("Pick the minutes"),
        (false, Mode::Clock) => tr("Pick the hour"),
        (false, Mode::Length) => tr("Pick the hours"),
    };
    ui.text(hint, Rect::new(sx + 1.0, top + big_h + 2.0, 200.0, 16.0), 12.0, REGULAR, t.subtext, Align::Left);

    let by = top + big_h + 30.0;
    let set_label = if c.mode == Mode::Clock { tr("Set time") } else { tr("Set") };
    let set = Rect::new(sx, by, ui.measure(set_label, 13.0, SEMIBOLD) + 28.0, 30.0);
    let hov = ui.hovered(set);
    ui.fill(set, 8.0, with_alpha(t.accent, if hov { 0.85 } else { 1.0 }));
    ui.text(set_label, set, 13.0, SEMIBOLD, t.on_accent, Align::Center);
    let mut x = set.right() + 4.0;
    let mut cleared = false;
    if let Some(label) = clear {
        let b = Rect::new(set.right() + 6.0, by, ui.measure(label, 13.0, MEDIUM) + 28.0, 30.0);
        let hov = ui.hovered(b);
        ui.fill(b, 8.0, with_alpha(t.surface2, if hov { 0.85 } else { 1.0 }));
        ui.text(label, b, 13.0, MEDIUM, t.text, Align::Center);
        cleared = ui.clicked(b);
        x = b.right() + 4.0;
    }
    let cancel = Rect::new(x, by + 1.0, 28.0, 28.0);
    let x_clicked = ui.icon_button(cancel, "XMark", 13.0, t.subtext);
    let escape = ui.input.keys.iter().position(|k| *k == crate::ui::Key::Escape);

    let out = if ui.clicked(set) {
        Picked::Set(c.hour, c.minute)
    } else if cleared {
        Picked::Clear
    } else if x_clicked {
        Picked::Cancel
    } else if let Some(i) = escape {
        ui.input.keys.remove(i);
        Picked::Cancel
    } else {
        Picked::None
    };
    if !matches!(out, Picked::None) && ui.st.active == dial_id {
        ui.st.active = 0;
    }
    // Swallow other clicks so what's underneath doesn't get them.
    let _ = ui.clicked(r);
    out
}

// ---------------------------------------------------------------------- the field

pub enum Field {
    None,
    /// Clicked: open the dial, on the minutes (true) or the hour.
    Open(bool),
    /// Scrolled to a new value.
    Changed,
}

thread_local! {
    /// Built-up wheel per field (touchpads send fractions): (amount, over the minutes).
    static WHEEL: RefCell<HashMap<u64, (f32, bool)>> = RefCell::new(HashMap::new());
}

/// Wheel notches per step.
const STEP: f32 = 1.0;

/// Text pieces of a value and the part each belongs to (None: punctuation / unit).
fn pieces(hour: u32, minute: u32, mode: Mode) -> Vec<(String, Option<bool>)> {
    match mode {
        Mode::Clock => vec![(format!("{hour:02}"), Some(false)), (":".into(), None), (format!("{minute:02}"), Some(true))],
        Mode::Length if hour > 0 => vec![(crate::i18n::tf!("{hour}h", "{hour}j"), Some(false)), (" ".into(), None), (format!("{minute:02}m"), Some(true))],
        Mode::Length => vec![(minute.to_string(), Some(true)), (tr(" min").into(), None)],
    }
}

/// The value as text: "07:30", "25 min", "1h 30m".
pub fn text(hour: u32, minute: u32, mode: Mode) -> String {
    pieces(hour, minute, mode).into_iter().map(|(s, _)| s).collect()
}

/// The time as text in `r` (centred, `size` DIP, an optional clock icon in front), with a hover background.
/// The wheel changes the part under the pointer (a length's minutes carry into its hours; at least 1 min).
pub fn field(ui: &mut Ui, key: u64, r: Rect, hour: &mut u32, minute: &mut u32, mode: Mode, size: f32, icon: bool) -> Field {
    let t = ui.t;
    let weight = if size >= 24.0 { LIGHT } else { SEMIBOLD };
    let parts = pieces(*hour, *minute, mode);
    let widths: Vec<f32> = parts.iter().map(|(s, _)| ui.measure(s, size, weight)).collect();
    let icon_w = if icon { size * 0.45 + 8.0 } else { 0.0 };
    let total = icon_w + widths.iter().sum::<f32>();
    let hov = ui.hovered(r);
    if hov {
        ui.fill(r, (r.h / 2.0).min(10.0), t.hover);
    }
    let mut x = r.cx() - total / 2.0;
    if icon {
        let s = (size * 0.45).max(12.0);
        ui.icon("Clock", x + s / 2.0, r.cy(), s, t.subtext);
        x += icon_w;
    }
    // Which part the pointer is over: the nearest one with a part.
    let mx = ui.mouse().0;
    let mut over = None;
    let mut best = f32::MAX;
    let mut px = x;
    for ((s, part), w) in parts.iter().zip(&widths) {
        ui.text(s, Rect::new(px, r.y, w + 2.0, r.h), size, weight, if part.is_some() { t.text } else { t.subtext }, Align::Left);
        if let Some(p) = part {
            let d = (mx - (px + w / 2.0)).abs();
            if d < best {
                best = d;
                over = Some(*p);
            }
        }
        px += w;
    }
    let on_minutes = over.unwrap_or(true);

    let mut out = Field::None;
    if hov && ui.input.wheel != 0.0 {
        let wheel = std::mem::take(&mut ui.input.wheel);
        let steps = WHEEL.with(|w| {
            let mut w = w.borrow_mut();
            let e = w.entry(key).or_insert((0.0, on_minutes));
            if e.1 != on_minutes {
                *e = (0.0, on_minutes);
            }
            e.0 += wheel;
            let n = (e.0 / STEP).trunc();
            e.0 -= n * STEP;
            n as i32
        });
        if steps != 0 {
            match mode {
                Mode::Clock if on_minutes => *minute = (*minute as i32 + steps).rem_euclid(60) as u32,
                Mode::Clock => *hour = (*hour as i32 + steps).rem_euclid(24) as u32,
                Mode::Length => {
                    let by = if on_minutes { steps } else { steps * 60 };
                    let total = (*hour * 60 + *minute) as i32 + by;
                    let total = total.clamp(1, 11 * 60 + 59) as u32;
                    *hour = total / 60;
                    *minute = total % 60;
                }
            }
            out = Field::Changed;
        }
    }
    if ui.clicked(r) {
        out = Field::Open(on_minutes);
    }
    out
}
