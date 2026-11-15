//! Timers: Timer | Stopwatch | Pomodoro | Alarm switch on top (Alarm draws `alarm::draw`); below, one card with the round progress ring and the
//! time, what the mode is doing (session dots, preset, laps), and the controls: start/pause, a secondary
//! button (+1 min for timers, lap for the stopwatch), reset, plus the length and sound pickers.

use std::time::Duration;

use crate::gfx::{color, with_alpha, Align, BOLD, MEDIUM, SEMIBOLD};
use crate::services::{settings, timers};
use crate::services::timers::Kind;
use crate::ui::{id, Rect, Ui};

use super::picker::{self, Dial, Field, Mode, Picked};
use crate::i18n::tr;

const SOUNDS: [&str; 4] = ["No Sound", "Soft Chime", "Bell", "Ping"];
/// Longest timer / pomodoro phase the picker allows (11h 59m).
const MAX_MIN: u64 = 11 * 60 + 59;

/// Which length the dial is picking.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Countdown,
    Focus,
    Break,
}
/// C# "GoodBrush" (the break arc).
const GOOD: u32 = 0x32D74B;
const ROW_H: f32 = 30.0;

/// An open picker list. It opens upward when there's no room below (the content area ends just under
/// the boxes) and scrolls when it's taller than the room.
struct Popup {
    id: u64,
    anchor: Rect,
    options: Vec<String>,
    selected: usize,
    scroll: f32,
}

pub struct State {
    mode: Kind,
    /// Stopwatch laps (elapsed at each lap).
    laps: Vec<Duration>,
    popup: Option<Popup>,
    /// Picked this frame: (box id, option).
    picked: Option<(u64, usize)>,
    /// The Alarm segment is open (alarms live in this tab too).
    pub show_alarm: bool,
    pub alarm: super::alarm::State,
    /// The shared time picker, open on a length.
    dial: Option<(Target, Dial)>,
    /// The switch highlight as drawn: x, width, set (0 until the first frame).
    seg: (f32, f32, f32),
    /// Switching segments: when, and from which side.
    seg_anim: Option<(std::time::Instant, f32)>,
}

impl Default for State {
    fn default() -> Self {
        Self { mode: Kind::Pomodoro, laps: Vec::new(), popup: None, picked: None, show_alarm: false, alarm: Default::default(), dial: None, seg: (0.0, 0.0, 0.0), seg_anim: None }
    }
}

pub fn shown(s: &mut State) {
    s.popup = None;
    s.dial = None;
    let st = settings::get();
    timers::set_config(st.pomodoro_work_min.max(1) as u64, st.pomodoro_break_min.max(1) as u64);
    // Open on whatever is running.
    if let Some(t) = timers::current() {
        s.mode = t.kind;
        s.show_alarm = false;
    }
    super::alarm::shown(&mut s.alarm);
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, _go: &mut Option<&'static str>) {
    // The open list takes clicks and the wheel before anything under it.
    popup_input(ui, s, r);
    let (seg, rest) = r.take_top(36.0, 12.0);
    // A ringing alarm shows its Snooze / Stop here.
    if crate::services::alarms::ringing().is_some() {
        s.show_alarm = true;
    }
    segments(ui, s, seg);
    let base_alpha = ui.alpha;
    let rest = seg_transition(ui, s, rest);
    body(ui, s, r, rest);
    ui.alpha = base_alpha;
}

fn body(ui: &mut Ui, s: &mut State, r: Rect, rest: Rect) {
    let t = ui.t;
    if s.show_alarm {
        s.popup = None;
        super::alarm::draw(ui, &mut s.alarm, rest.pad(18.0, 0.0, 18.0, 6.0));
        return;
    }

    let card = rest.pad(18.0, 0.0, 18.0, 6.0);
    ui.card(card, 18.0);
    let inner = card.pad(18.0, 10.0, 18.0, 10.0);

    // The shared time picker, over the card, while a length is being picked.
    if let Some((target, mut c)) = s.dial {
        s.popup = None;
        match picker::dial(ui, &mut c, card.pad(18.0, 6.0, 18.0, 6.0), None) {
            Picked::None => s.dial = Some((target, c)),
            Picked::Cancel | Picked::Clear => s.dial = None,
            Picked::Set(h, m) => {
                set_length(target, (h * 60 + m) as u64);
                s.dial = None;
            }
        }
        return;
    }

    let st = settings::get();
    let cur = timers::current().filter(|x| x.kind == s.mode);
    let (focus_min, break_min) = timers::config();
    let timer_min = (st.timer_minutes.max(1) as u64).min(MAX_MIN);
    let clock = |m: u64| if m >= 60 { format!("{}:{:02}:00", m / 60, m % 60) } else { format!("{m:02}:00") };
    let text = match (&cur, s.mode) {
        (Some(c), _) => c.text(),
        (None, Kind::Pomodoro) => clock(focus_min),
        (None, Kind::Countdown) => clock(timer_min),
        (None, Kind::Stopwatch) => "00:00".into(),
    };
    let progress = match (&cur, s.mode) {
        (Some(c), Kind::Pomodoro | Kind::Countdown) => c.progress(),
        _ => 0.0,
    };
    let is_break = cur.is_some_and(|c| c.phase == timers::Phase::Break);

    // Ring.
    let ring = Rect::new(inner.x, inner.cy() - 64.0, 128.0, 128.0);
    let (cx, cy) = (ring.cx(), ring.cy());
    ui.g.stroke_circle(cx, cy, 57.5, ui.c(t.track), 7.0);
    ui.g.fill_circle(cx, cy, 46.0, ui.c(with_alpha(t.accent_soft, 0.55)));
    let p = progress.clamp(0.0, 0.9999);
    if p > 0.001 {
        let arc_c = if is_break { color(GOOD, 1.0) } else { t.accent };
        ui.g.arc(cx, cy, 57.5, 0.0, p * std::f32::consts::TAU, ui.c(arc_c), 7.0);
    }
    ui.text(&text, Rect::new(ring.x - 10.0, ring.y, ring.w + 20.0, ring.h), if text.len() > 5 { 21.0 } else { 26.0 }, BOLD, t.text, Align::Center);

    // Controls (right): buttons row, then the length(s) and the sound.
    let ctrl_w = 264.0;
    let ctrl = Rect::new(inner.right() - ctrl_w, inner.cy() - 45.0, ctrl_w, 90.0);
    let running = cur.is_some_and(|c| c.running());
    let reset = Rect::new(ctrl.right() - 46.0, ctrl.y + 2.0, 46.0, 46.0);
    let second = Rect::new(reset.x - 10.0 - 46.0, reset.y, 46.0, 46.0);
    let primary = Rect::new(second.x - 10.0 - 50.0, ctrl.y, 50.0, 50.0);
    let hov = ui.hovered(primary);
    ui.g.fill_circle(primary.cx(), primary.cy(), 25.0, ui.c(with_alpha(t.accent, if hov { 0.85 } else { 1.0 })));
    ui.icon(if running { "Pause" } else { "Play" }, primary.cx() + if running { 0.0 } else { 1.5 }, primary.cy(), 19.0, t.on_accent);
    if ui.clicked(primary) {
        match cur {
            Some(_) => timers::toggle_pause(),
            None => {
                s.laps.clear();
                timers::start(s.mode, timer_min);
            }
        }
    }
    // C#: Skip (pomodoro), +1 min (timer), Lap (stopwatch).
    let second_icon = match s.mode {
        Kind::Stopwatch => "Flag",
        Kind::Pomodoro => "Forward",
        Kind::Countdown => "Plus",
    };
    // Each one only shows as a button when it does something: Lap while the stopwatch runs, Skip while a
    // pomodoro is on, +1 min always (before the start it lengthens the timer).
    let second_on = match s.mode {
        Kind::Stopwatch => running,
        Kind::Pomodoro => cur.is_some(),
        Kind::Countdown => true,
    };
    if circle_button(ui, second, second_icon, second_on) {
        match (s.mode, &cur) {
            (Kind::Stopwatch, Some(c)) => s.laps.push(c.elapsed()),
            // Skip: straight to the next phase.
            (Kind::Pomodoro, Some(_)) => timers::skip(),
            (Kind::Countdown, Some(_)) => timers::add_minute(),
            (Kind::Countdown, None) => set_length(Target::Countdown, timer_min + 1),
            _ => {}
        }
    }
    let reset_on = cur.is_some() || !s.laps.is_empty();
    if circle_button(ui, reset, "Refresh", reset_on) {
        if cur.is_some() {
            timers::stop();
        }
        s.laps.clear();
    }

    let dur = Rect::new(ctrl.x, ctrl.bottom() - 28.0, 146.0, 28.0);
    let snd = Rect::new(dur.right() + 8.0, dur.y, ctrl.right() - dur.right() - 8.0, 28.0);
    let sounds: Vec<String> = SOUNDS.iter().map(|x| x.to_string()).collect();
    match s.mode {
        Kind::Pomodoro => {
            // Focus and break lengths, side by side: scroll either, or click for the dial.
            let half = (dur.w - 4.0) / 2.0;
            for (i, (target, icon, len)) in [(Target::Focus, "Bolt", focus_min), (Target::Break, "Moon", break_min)].into_iter().enumerate() {
                let b = Rect::new(dur.x + i as f32 * (half + 4.0), dur.y, half, dur.h);
                if let Some(d) = length_box(ui, s, b, target, Some(icon), len) {
                    s.dial = Some((target, d));
                }
            }
            let sel = SOUNDS.iter().position(|x| *x == st.pomodoro_sound).unwrap_or(1);
            if let Some(i) = combo(ui, s, id("timer.sound.pomodoro"), snd, &sounds, sel) {
                settings::update(|x| x.pomodoro_sound = SOUNDS[i].to_string());
            }
        }
        Kind::Countdown => {
            if let Some(d) = length_box(ui, s, dur, Target::Countdown, None, timer_min) {
                s.dial = Some((Target::Countdown, d));
            }
            let sel = SOUNDS.iter().position(|x| *x == st.timer_sound).unwrap_or(1);
            if let Some(i) = combo(ui, s, id("timer.sound.countdown"), snd, &sounds, sel) {
                settings::update(|x| x.timer_sound = SOUNDS[i].to_string());
            }
        }
        Kind::Stopwatch => {
            // No length or sound to pick: the latest laps go here instead.
            let row = Rect::new(dur.x, dur.y, ctrl.right() - dur.x, dur.h);
            let mut x = row.x;
            for (i, lap) in s.laps.iter().enumerate().rev().take(3) {
                let prev = if i == 0 { Duration::ZERO } else { s.laps[i - 1] };
                let label = format!("#{} {}", i + 1, lap_text(*lap - prev));
                let w = ui.measure(&label, 12.0, SEMIBOLD) + 16.0;
                if x + w > row.right() {
                    break;
                }
                let chip = Rect::new(x, row.y + 2.0, w, row.h - 4.0);
                ui.fill(chip, 8.0, t.surface2);
                ui.text(&label, chip, 12.0, SEMIBOLD, if i + 1 == s.laps.len() { t.text } else { t.subtext }, Align::Center);
                x += w + 6.0;
            }
            if s.laps.is_empty() {
                let hint = if running { tr("Press the flag for a lap") } else { tr("Start, then press the flag for laps") };
                ui.text(hint, row, 12.0, MEDIUM, t.subtext, Align::Left);
            }
        }
    }

    // Mode info (middle).
    let ix = ring.right() + 18.0;
    let iw = (ctrl.x - 12.0 - ix).max(40.0);
    match s.mode {
        Kind::Pomodoro => {
            let rounds = cur.map_or(0, |c| c.rounds);
            // C# stack (margin-top 10, centred): title 18, +3 session 15, +7 dots 7, +8 today 14.5.
            let y = inner.cy() - 31.0;
            title_row(ui, ix, y, iw, if is_break { "Moon" } else { "Bolt" }, if is_break { tr("Break") } else { tr("Focus") });
            ui.text(&crate::i18n::tf!("Session {} of 4", "Sesi {} dari 4", rounds % 4 + 1), Rect::new(ix, y + 20.0, iw, 17.0), 12.0, SEMIBOLD, t.subtext, Align::Left);
            let mut filled = rounds % 4;
            if rounds > 0 && filled == 0 {
                filled = 4;
            }
            for i in 0..4 {
                let c = if i < filled { t.accent } else { t.track };
                ui.g.fill_circle(ix + 3.5 + i as f32 * 13.0, y + 46.5, 3.5, ui.c(c));
            }
            let today = match rounds {
                0 => tr("No sessions yet today").to_string(),
                1 => tr("1 session today").to_string(),
                n => crate::i18n::tf!("{n} sessions today", "{n} sesi hari ini"),
            };
            ui.text(&today, Rect::new(ix, y + 57.0, iw, 16.0), 12.0, SEMIBOLD, t.subtext, Align::Left);
        }
        Kind::Countdown => {
            let y = inner.cy() - 24.0;
            title_row(ui, ix, y, iw, "Clock", tr("Timer"));
            ui.text(&crate::i18n::tf!("{} countdown", "Hitung mundur {}", picker::text((timer_min / 60) as u32, (timer_min % 60) as u32, Mode::Length)), Rect::new(ix, y + 20.0, iw, 17.0), 12.0, SEMIBOLD, t.subtext, Align::Left);
            ui.text(tr("Stays in the closed notch."), Rect::new(ix, y + 43.0, iw, 16.0), 12.0, SEMIBOLD, t.subtext, Align::Left);
        }
        Kind::Stopwatch => {
            let y = inner.cy() - 24.0;
            title_row(ui, ix, y, iw, "Clock", tr("Stopwatch"));
            let laps = match s.laps.len() {
                0 => tr("No laps yet").to_string(),
                1 => tr("1 lap").to_string(),
                n => crate::i18n::tf!("{n} laps", "{n} putaran"),
            };
            ui.text(&laps, Rect::new(ix, y + 20.0, iw, 17.0), 12.0, SEMIBOLD, t.subtext, Align::Left);
            ui.text(tr("Counts up from zero."), Rect::new(ix, y + 43.0, iw, 16.0), 12.0, SEMIBOLD, t.subtext, Align::Left);
        }
    }
    s.picked = None;
    draw_popup(ui, s, r);
}

/// Where the open list goes: below the box if it fits in `area`, else above (or whichever side is bigger),
/// clipped to the room there.
fn popup_rect(p: &Popup, area: Rect) -> Rect {
    let full = ROW_H * p.options.len() as f32 + 8.0;
    let below = area.bottom() - p.anchor.bottom() - 4.0;
    let above = p.anchor.y - 4.0 - area.y;
    if full <= below || below >= above {
        Rect::new(p.anchor.x, p.anchor.bottom() + 4.0, p.anchor.w, full.min(below))
    } else {
        let h = full.min(above);
        Rect::new(p.anchor.x, p.anchor.y - 4.0 - h, p.anchor.w, h)
    }
}

fn popup_input(ui: &mut Ui, s: &mut State, area: Rect) {
    s.picked = None;
    let Some(p) = s.popup.as_mut() else { return };
    let list = popup_rect(p, area);
    let max = (ROW_H * p.options.len() as f32 + 8.0 - list.h).max(0.0);
    if ui.input.wheel != 0.0 && list.contains(ui.mouse()) {
        p.scroll = (p.scroll - ui.input.wheel * ROW_H).clamp(0.0, max);
        ui.input.wheel = 0.0;
    }
    if ui.input.keys.contains(&crate::ui::Key::Escape) {
        s.popup = None;
        return;
    }
    if !ui.input.pressed || ui.input.consumed {
        return;
    }
    let m = ui.mouse();
    if list.contains(m) {
        let i = ((m.1 - list.y - 4.0 + p.scroll) / ROW_H).floor();
        if i >= 0.0 && (i as usize) < p.options.len() {
            s.picked = Some((p.id, i as usize));
        }
        ui.input.consumed = true;
    } else if p.anchor.contains(m) {
        ui.input.consumed = true; // clicking the box again closes it
    }
    s.popup = None;
}

fn draw_popup(ui: &mut Ui, s: &mut State, area: Rect) {
    let Some(p) = s.popup.as_ref() else { return };
    let t = ui.t;
    let list = popup_rect(p, area);
    ui.fill(list, 8.0, t.surface2);
    ui.g.stroke_round(list.inset(0.5, 0.5).d2d(), 7.5, ui.c(t.stroke), 1.0);
    ui.push_clip(list.inset(1.0, 4.0));
    for (i, o) in p.options.iter().enumerate() {
        let row = Rect::new(list.x + 4.0, list.y + 4.0 + i as f32 * ROW_H - p.scroll, list.w - 8.0, ROW_H);
        if row.bottom() < list.y || row.y > list.bottom() {
            continue;
        }
        if row.contains(ui.mouse()) && list.contains(ui.mouse()) {
            ui.fill(row, 6.0, t.hover);
        }
        let sel = i == p.selected;
        ui.text(o, row.pad(10.0, 0.0, 26.0, 0.0), 13.0, if sel { SEMIBOLD } else { MEDIUM }, if sel { t.text } else { t.subtext }, Align::Left);
        if sel {
            ui.icon("Check", row.right() - 14.0, row.cy(), 12.0, t.text);
        }
    }
    ui.pop_clip();
    let full = ROW_H * p.options.len() as f32 + 8.0;
    if full > list.h {
        let bar_h = (list.h * list.h / full).max(18.0);
        let y = list.y + (list.h - bar_h) * (p.scroll / (full - list.h));
        ui.fill(Rect::new(list.right() - 4.0, y, 2.0, bar_h), 1.0, with_alpha(t.text, 0.25));
    }
}

fn segments(ui: &mut Ui, s: &mut State, r: Rect) {
    let t = ui.t;
    let labels = [(tr("Timer"), Some(Kind::Countdown)), (tr("Stopwatch"), Some(Kind::Stopwatch)), (tr("Pomodoro"), Some(Kind::Pomodoro)), ("Alarm", None)];
    let widths: Vec<f32> = labels.iter().map(|(l, _)| (ui.measure(l, 13.0, SEMIBOLD) + 28.0).max(96.0)).collect();
    let total: f32 = widths.iter().sum::<f32>() + 6.0;
    let bar = Rect::new(r.cx() - total / 2.0, r.y, total, 36.0);
    ui.fill(bar, 11.0, t.surface);
    let mut rects = Vec::new();
    let mut x = bar.x + 3.0;
    for w in &widths {
        rects.push(Rect::new(x, bar.y + 3.0, *w, 30.0));
        x += w;
    }
    let selected = labels
        .iter()
        .position(|(_, kind)| match kind {
            Some(k) => !s.show_alarm && s.mode == *k,
            None => s.show_alarm,
        })
        .unwrap_or(0);

    // The highlight glides to the picked option.
    let target = rects[selected];
    if s.seg.2 <= 0.0 {
        s.seg = (target.x, target.w, 1.0);
    }
    let sc = settings::anim_scale();
    let k = if sc <= 0.0 { 1.0 } else { (0.3 / sc).min(1.0) };
    s.seg.0 += (target.x - s.seg.0) * k;
    s.seg.1 += (target.w - s.seg.1) * k;
    if (target.x - s.seg.0).abs() > 0.3 || (target.w - s.seg.1).abs() > 0.3 {
        ui.animate = true;
    } else {
        s.seg.0 = target.x;
        s.seg.1 = target.w;
    }
    ui.fill(Rect::new(s.seg.0, target.y, s.seg.1, target.h), 8.0, t.accent_soft);

    for (i, ((label, kind), b)) in labels.iter().zip(rects).enumerate() {
        let on = i == selected;
        let hov = ui.hovered(b);
        let fg = if on || hov { t.text } else { t.subtext };
        ui.text(label, b, 13.5, if on { SEMIBOLD } else { MEDIUM }, fg, Align::Center);
        if ui.clicked(b) && !on {
            s.seg_anim = Some((std::time::Instant::now(), if i > selected { 1.0 } else { -1.0 }));
            s.dial = None;
            match kind {
                Some(k) => {
                    s.mode = *k;
                    s.show_alarm = false;
                }
                None => s.show_alarm = true,
            }
        }
    }
}

/// Switching segments: the card under the switch fades in and slides the last few pixels from the side of
/// the picked option. Returns the shifted rect; sets `ui.alpha` (the caller restores it).
fn seg_transition(ui: &mut Ui, s: &mut State, r: Rect) -> Rect {
    let Some((at, dir)) = s.seg_anim else { return r };
    let k = (settings::anim_secs(at.elapsed().as_secs_f32()) / 0.26).clamp(0.0, 1.0);
    if k >= 1.0 {
        s.seg_anim = None;
        return r;
    }
    ui.animate = true;
    let e = 1.0 - (1.0 - k).powi(3);
    ui.alpha *= 0.15 + 0.85 * e;
    Rect::new(r.x + dir * 18.0 * (1.0 - e), r.y, r.w, r.h)
}


fn title_row(ui: &mut Ui, x: f32, y: f32, w: f32, icon: &'static str, label: &str) {
    let t = ui.t;
    ui.icon(icon, x + 8.5, y + 10.0, 17.0, t.text);
    ui.text(label, Rect::new(x + 24.0, y, w - 24.0, 20.0), 14.0, SEMIBOLD, t.text, Align::Left);
}

/// The C# combo box (Surface2, radius 7, 13 SemiBold, a thin chevron; hover dims to the hover brush) over
/// the kit dropdown, which still does the picking and draws the open list.
fn combo(ui: &mut Ui, s: &mut State, cid: u64, r: Rect, options: &[String], selected: usize) -> Option<usize> {
    let picked = s.picked.filter(|(p, _)| *p == cid).map(|(_, i)| i);
    let open = s.popup.as_ref().is_some_and(|p| p.id == cid);
    if !open && ui.clicked(r) {
        s.popup = Some(Popup { id: cid, anchor: r, options: options.to_vec(), selected, scroll: 0.0 });
        ui.animate = true;
    }
    let hov = ui.hovered(r) || open;
    combo_face(ui, r, options.get(selected).map(String::as_str).unwrap_or(""), hov);
    picked
}

/// The box itself; hover swaps Surface2 for the (translucent) hover brush over the card.
fn combo_face(ui: &mut Ui, r: Rect, label: &str, hov: bool) {
    let t = ui.t;
    if hov {
        ui.fill(r, 7.0, t.surface);
        ui.fill(r, 7.0, t.hover);
    } else {
        ui.fill(r, 7.0, t.surface2);
    }
    ui.text(label, r.pad(9.0, 0.0, 26.0, 0.0), 13.0, SEMIBOLD, t.text, Align::Left);
    let (cx, cy) = (r.right() - 10.0 - 4.0, r.cy());
    let c = ui.c(t.subtext);
    ui.g.line(cx - 4.0, cy - 2.0, cx, cy + 2.0, c, 1.4);
    ui.g.line(cx, cy + 2.0, cx + 4.0, cy - 2.0, c, 1.4);
}

/// A round button; dimmed (and not clickable) when it would do nothing right now.
fn circle_button(ui: &mut Ui, r: Rect, icon: &'static str, enabled: bool) -> bool {
    let t = ui.t;
    let hov = enabled && ui.hovered(r);
    ui.g.fill_circle(r.cx(), r.cy(), r.w / 2.0, ui.c(if hov { t.hover } else { t.surface2 }));
    ui.icon(icon, r.cx(), r.cy(), 18.0, if enabled { t.text } else { with_alpha(t.text, 0.3) });
    let clicked = ui.clicked(r);
    enabled && clicked
}

/// Saves a picked length (minutes, 1 … 11h 59m).
fn set_length(target: Target, minutes: u64) {
    let m = minutes.clamp(1, MAX_MIN);
    match target {
        Target::Countdown => settings::update(|x| x.timer_minutes = m as i32),
        Target::Focus | Target::Break => {
            let (f, b) = timers::config();
            let (f, b) = if target == Target::Focus { (m, b) } else { (f, m) };
            settings::update(|x| {
                x.pomodoro_work_min = f as i32;
                x.pomodoro_break_min = b as i32;
            });
            timers::set_config(f, b);
        }
    }
}

/// A length in a box with the shared picker field: scroll the hours or minutes, click for the dial
/// (returned, to open).
fn length_box(ui: &mut Ui, _s: &mut State, r: Rect, target: Target, icon: Option<&'static str>, minutes: u64) -> Option<Dial> {
    let t = ui.t;
    ui.fill(r, 7.0, t.surface2);
    let mut fr = r;
    if let Some(i) = icon {
        ui.icon(i, r.x + 13.0, r.cy(), 11.0, t.subtext);
        fr = r.pad(18.0, 0.0, 0.0, 0.0);
    }
    let (mut h, mut m) = ((minutes / 60) as u32, (minutes % 60) as u32);
    let key = id("timer.length") ^ target as u64;
    match picker::field(ui, key, fr, &mut h, &mut m, Mode::Length, 13.0, icon.is_none()) {
        Field::Changed => {
            set_length(target, (h * 60 + m) as u64);
            None
        }
        Field::Open(on_minutes) => Some(Dial::new(h, m, Mode::Length, on_minutes)),
        Field::None => None,
    }
}

/// A lap's length: "12.4s", "1:02.4".
fn lap_text(d: Duration) -> String {
    let tenths = d.as_millis() / 100;
    let (m, s, t) = (tenths / 600, tenths / 10 % 60, tenths % 10);
    if m > 0 {
        format!("{m}:{s:02}.{t}")
    } else {
        format!("{s}.{t}s")
    }
}
