//! Calendar: a month grid (dots under days with events) on the left; on the right the selected day's agenda
//! (own events, subscribed events and to-dos due that day) as a timeline, with a composer pill
//! (time | title | add) at the bottom. The clock button (or clicking the time box) opens a clock-face time
//! picker over the agenda: pick the hour on the dial, then the minutes; AM/PM; Set time / All day / ✕.

use std::time::Instant;

use crate::gfx::{with_alpha, Align, BOLD, LIGHT, REGULAR, SEMIBOLD};
use crate::i18n::{day_name, month_name};
use crate::services::{calendar, todos};
use crate::ui::{id, Key, Rect, Ui};

use super::picker;
use crate::i18n::tr;

const TIME_ID: u64 = id("calendar.time");
const TITLE_ID: u64 = id("calendar.title");
const DIAL_ID: u64 = id("picker.dial");
/// While the picker is open it poses as an open dropdown, so Esc closes the picker instead of the notch.
const PICKER_ID: u64 = id("calendar.picker");
const LIST_ID: u64 = id("calendar.list");

const ROW_H: f32 = 36.0;

pub struct State {
    /// First day of the shown month (days since 1970-01-01).
    month: i64,
    selected: i64,
    /// The day the view was last drawn on (rolls the selection over at midnight).
    rendered_today: i64,
    picker: bool,
    dial: picker::Dial,
    /// The time box text that failed to parse (red until edited).
    time_err: Option<String>,
    /// Month change: when, and from which side (-1 / 1).
    slide: Option<(Instant, f32)>,
    /// Month dot counts and the selected day's agenda, rebuilt at most every half second (or on change).
    cache: Option<Cache>,
    /// Focus the title box on the next frame (the click that closed the picker would otherwise unfocus it).
    focus_title: bool,
}

struct Cache {
    at: Instant,
    month: i64,
    selected: i64,
    counts: std::rc::Rc<std::collections::HashMap<i64, usize>>,
    items: std::rc::Rc<Vec<Item>>,
}

impl Default for State {
    fn default() -> Self {
        let t = today();
        Self {
            month: first_of_month(t),
            selected: t,
            rendered_today: t,
            picker: false,
            dial: picker::Dial::new(9, 0, picker::Mode::Clock, false),
            time_err: None,
            slide: None,
            cache: None,
            focus_title: false,
        }
    }
}

fn today() -> i64 {
    let (y, m, d) = calendar::today();
    calendar::days_from_civil(y, m, d)
}

fn first_of_month(day: i64) -> i64 {
    let (y, m, _) = calendar::civil_from_days(day);
    calendar::days_from_civil(y, m, 1)
}

fn add_months(first: i64, n: i32) -> i64 {
    let (y, m, _) = calendar::civil_from_days(first);
    let total = y * 12 + m as i32 - 1 + n;
    calendar::days_from_civil(total.div_euclid(12), total.rem_euclid(12) as u32 + 1, 1)
}

pub fn shown(s: &mut State) {
    let t = today();
    if s.rendered_today != t && s.selected == s.rendered_today {
        s.selected = t;
        s.month = first_of_month(t);
    }
    s.rendered_today = t;
}

/// One agenda row.
struct Item {
    time: Option<(u32, u32)>,
    title: String,
    /// Own events can be deleted here (subscribed events and to-dos can't).
    delete_id: Option<String>,
}

fn items_on(day: i64) -> Vec<Item> {
    let (y, m, d) = calendar::civil_from_days(day);
    let mut items: Vec<Item> = calendar::on_day(y, m, d)
        .into_iter()
        .map(|e| Item { time: e.time, title: e.title.clone(), delete_id: (!e.read_only).then(|| e.id.clone()) })
        .collect();
    for t in todos::all().into_iter().filter(|t| !t.done) {
        if let Some((ty, tm, td, h, mi)) = t.due_local() {
            if (ty, tm, td) == (y, m, d) {
                items.push(Item { time: (!t.due_all_day).then_some((h, mi)), title: format!("To-do · {}", t.text), delete_id: None });
            }
        }
    }
    // All day first, then by time (stable, like the C# OrderBy).
    items.sort_by_key(|i| i.time.map_or(-1, |(h, m)| (h * 60 + m) as i64));
    items
}

/// Event counts per day of the shown month, plus open to-dos due then.
fn counts(first: i64) -> std::collections::HashMap<i64, usize> {
    let mut map = std::collections::HashMap::new();
    for e in calendar::load_all() {
        let day = calendar::days_from_civil(e.date.0, e.date.1, e.date.2);
        if (day - first).abs() < 45 {
            *map.entry(day).or_insert(0) += 1;
        }
    }
    for t in todos::all().into_iter().filter(|t| !t.done) {
        if let Some((ty, tm, td, _, _)) = t.due_local() {
            *map.entry(calendar::days_from_civil(ty, tm, td)).or_insert(0) += 1;
        }
    }
    map
}

/// The cached month counts and agenda (reading events and to-dos isn't free, and draw runs every frame while animating).
fn data(s: &mut State) -> (std::rc::Rc<std::collections::HashMap<i64, usize>>, std::rc::Rc<Vec<Item>>) {
    let fresh = s.cache.as_ref().is_some_and(|c| c.month == s.month && c.selected == s.selected && c.at.elapsed().as_millis() < 500);
    if !fresh {
        s.cache = Some(Cache {
            at: Instant::now(),
            month: s.month,
            selected: s.selected,
            counts: std::rc::Rc::new(counts(s.month)),
            items: std::rc::Rc::new(items_on(s.selected)),
        });
    }
    let c = s.cache.as_ref().unwrap();
    (c.counts.clone(), c.items.clone())
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, _go: &mut Option<&'static str>) {
    let t = today();
    if s.rendered_today != t {
        shown(s);
    }
    // The shell closed our "dropdown" (tool switched / panel hidden): the picker goes with it.
    if s.picker && ui.st.dropdown != PICKER_ID {
        close_picker(ui, s);
    }
    let (left, right) = r.take_left(250.0, 18.0);
    month_card(ui, s, left);
    day_panel(ui, s, right.pad(0.0, 2.0, 0.0, 0.0));
}

// ---------------------------------------------------------------- month card

fn month_card(ui: &mut Ui, s: &mut State, card: Rect) {
    let t = ui.t;
    ui.fill(card, 14.0, t.surface);
    ui.g.stroke_round(card.inset(0.5, 0.5).d2d(), 14.0, ui.c(t.stroke), 1.0);
    let inner = card.pad(10.0, 8.0, 10.0, 6.0);

    // Header: "November 2026", prev · today · next.
    let (head, rest) = inner.take_top(24.0, 4.0);
    let (y, m, _) = calendar::civil_from_days(s.month);
    let month_name = month_name(m as usize);
    let hx = head.x + 2.0;
    ui.text(month_name, Rect::new(hx, head.y, 160.0, head.h), 14.0, SEMIBOLD, t.text, Align::Left);
    let mw = ui.measure(month_name, 14.0, SEMIBOLD);
    ui.text(&y.to_string(), Rect::new(hx + mw + 5.0, head.y, 60.0, head.h), 14.0, SEMIBOLD, t.accent, Align::Left);
    let next_r = Rect::new(head.right() - 24.0, head.y, 24.0, 24.0);
    let today_r = Rect::new(next_r.x - 24.0, head.y, 24.0, 24.0);
    let prev_r = Rect::new(today_r.x - 24.0, head.y, 24.0, 24.0);
    if ui.icon_button(prev_r, "ChevronLeft", 13.0, t.subtext) {
        s.month = add_months(s.month, -1);
        s.slide = Some((Instant::now(), -1.0));
    }
    let today_click = ui.hit(today_r, 8.0);
    ui.g.fill_circle(today_r.cx(), today_r.cy(), 3.0, ui.c(t.accent));
    if today_click {
        go_today(s);
    }
    if ui.icon_button(next_r, "ChevronRight", 13.0, t.subtext) {
        s.month = add_months(s.month, 1);
        s.slide = Some((Instant::now(), 1.0));
    }

    // Weekdays, Monday first.
    let (wk, grid) = rest.take_top(14.0, 2.0);
    let cw = wk.w / 7.0;
    for (i, name) in [tr("MO"), tr("TU"), tr("WE"), tr("TH"), tr("FR"), tr("SA"), tr("SU")].iter().enumerate() {
        let c = if i >= 5 { with_alpha(t.subtext, 0.6) } else { t.subtext };
        ui.text(name, Rect::new(wk.x + i as f32 * cw, wk.y, cw, wk.h), 10.0, BOLD, c, Align::Center);
    }

    // The month glides in from the side it comes from.
    let (mut dx, mut fade) = (0.0, 1.0);
    if let Some((at, dir)) = s.slide {
        let p = ease_out(at.elapsed().as_secs_f32() / 0.24);
        if p >= 1.0 {
            s.slide = None;
        } else {
            dx = 14.0 * dir * (1.0 - p);
            fade = p;
            ui.animate = true;
        }
    }

    let counts = data(s).0;
    let today = today();
    let offset = calendar::weekday(s.month) as i64;
    let start = s.month - offset;
    let ch = grid.h / 6.0;
    let base_alpha = ui.alpha;
    ui.push_clip(card);
    for i in 0..42 {
        let day = start + i as i64;
        let (_, dm, dd) = calendar::civil_from_days(day);
        let cell = Rect::new(grid.x + (i % 7) as f32 * cw + dx, grid.y + (i / 7) as f32 * ch, cw, ch);
        let selected = day == s.selected;
        let is_today = day == today;
        let in_month = dm == m;
        let weekend = i % 7 >= 5;
        ui.alpha = base_alpha * fade * if in_month { 1.0 } else { 0.35 };

        // Circle + dots, centred as one block (26 + 1.5 + 3.5).
        let cy = cell.cy() - 2.5;
        let circle = Rect::new(cell.cx() - 13.0, cy - 13.0, 26.0, 26.0);
        if selected {
            for k in 0..6 {
                // Soft glow around the selected day.
                let a = 0.06 * (6 - k) as f32 / 6.0;
                ui.g.fill_circle(circle.cx(), circle.cy(), 13.0 + 1.2 * (k + 1) as f32, ui.c(with_alpha(t.accent, a)));
            }
            ui.g.fill_circle(circle.cx(), circle.cy(), 13.0, ui.c(t.accent));
        } else if ui.hovered(cell) {
            ui.g.fill_circle(circle.cx(), circle.cy(), 13.0, ui.c(t.hover));
        }
        if is_today && !selected {
            ui.g.stroke_circle(circle.cx(), circle.cy(), 12.25, ui.c(t.accent), 1.5);
        }
        let fg = if selected {
            t.on_accent
        } else if is_today {
            t.accent
        } else if weekend {
            t.subtext
        } else {
            t.text
        };
        let weight = if is_today || selected { BOLD } else { REGULAR };
        ui.text(&dd.to_string(), Rect::new(circle.x - 4.0, circle.y + 1.0, circle.w + 8.0, circle.h), 12.5, weight, fg, Align::Center);

        let n = counts.get(&day).copied().unwrap_or(0).min(3);
        if n > 0 {
            let dy = circle.bottom() + 1.5 + 1.75;
            let total = n as f32 * 5.0;
            for k in 0..n {
                ui.g.fill_circle(cell.cx() - total / 2.0 + 2.5 + k as f32 * 5.0, dy, 1.75, ui.c(t.accent));
            }
        }

        if ui.clicked(cell) {
            s.selected = day;
            if !in_month {
                let dir = if day < s.month { -1.0 } else { 1.0 };
                s.month = first_of_month(day);
                s.slide = Some((Instant::now(), dir));
            }
        }
    }
    ui.alpha = base_alpha;
    ui.pop_clip();
}

fn go_today(s: &mut State) {
    let t = today();
    let first = first_of_month(t);
    let dir = (first - s.month).signum() as f32;
    s.selected = t;
    s.month = first;
    if dir != 0.0 {
        s.slide = Some((Instant::now(), dir));
    }
}

// ---------------------------------------------------------------- day panel

fn day_panel(ui: &mut Ui, s: &mut State, r: Rect) {
    let t = ui.t;
    let (composer, top) = r.take_bottom(40.0, 6.0);
    let (head, list) = top.take_top(44.0, 8.0);
    let today = today();
    let is_today = s.selected == today;
    let items = data(s).1;

    if s.picker {
        picker(ui, s, top);
    } else {
        // Header: big day number, "Today"/weekday, date · count; a Today chip when elsewhere.
        let (y, m, d) = calendar::civil_from_days(s.selected);
        let num = d.to_string();
        let nw = ui.measure(&num, 34.0, LIGHT).max(40.0);
        ui.text(&num, Rect::new(head.x, head.y - 2.0, nw + 4.0, head.h), 34.0, LIGHT, if is_today { t.accent } else { t.text }, Align::Left);
        let tx = head.x + nw + 12.0;
        let dow = day_name((calendar::weekday(s.selected) as usize + 1) % 7);
        let month = month_name(m as usize);
        ui.text(if is_today { tr("Today") } else { dow }, Rect::new(tx, head.y + 2.0, 300.0, 20.0), 14.0, SEMIBOLD, t.text, Align::Left);
        let count = match items.len() {
            0 => tr("No events").to_string(),
            1 => tr("1 event").to_string(),
            n => crate::i18n::tf!("{n} events", "{n} acara"),
        };
        let sub = if is_today { crate::i18n::tf!("{dow}, {month} {d} · {count}", "{dow}, {d} {month} · {count}") } else { crate::i18n::tf!("{month} {d}, {y} · {count}", "{d} {month} {y} · {count}") };
        ui.text(&sub, Rect::new(tx, head.y + 22.0, 320.0, 18.0), 12.0, REGULAR, t.subtext, Align::Left);
        if !is_today {
            let cw = ui.measure(tr("Today"), 11.0, SEMIBOLD) + 35.0;
            let chip = Rect::new(head.right() - cw, head.cy() - 11.0, cw, 22.0);
            let hov = ui.hovered(chip);
            ui.fill(chip, 11.0, if hov { with_alpha(t.accent_soft, 0.8) } else { t.accent_soft });
            ui.icon("ChevronLeft", chip.x + 15.0, chip.cy(), 10.0, t.accent);
            ui.text(tr("Today"), Rect::new(chip.x + 23.0, chip.y, cw - 23.0, chip.h), 11.0, SEMIBOLD, t.accent, Align::Left);
            if ui.clicked(chip) {
                go_today(s);
            }
        }

        if items.is_empty() {
            let cy = list.cy() - 6.0;
            ui.g.fill_circle(list.cx(), cy - 18.0, 18.0, ui.c(t.accent_soft));
            ui.icon("CalendarDays", list.cx(), cy - 18.0, 16.0, t.accent);
            ui.text(tr("No events"), Rect::new(list.x, cy + 7.0, list.w, 18.0), 13.0, SEMIBOLD, t.text, Align::Center);
            ui.text(tr("Add one below"), Rect::new(list.x, cy + 24.0, list.w, 16.0), 11.0, REGULAR, t.subtext, Align::Center);
        } else {
            agenda(ui, s, list, &items, is_today);
        }
    }
    composer_pill(ui, s, composer);
}

fn agenda(ui: &mut Ui, s: &mut State, list: Rect, items: &[Item], is_today: bool) {
    let t = ui.t;
    let n = crate::notch::now();
    let now_min = n.hour as i64 * 60 + n.minute as i64;
    let start = |i: &Item| i.time.map(|(h, m)| (h * 60 + m) as i64);
    // "Up next" only for today: the first timed event that hasn't started yet.
    let next = if is_today { items.iter().position(|i| start(i).is_some_and(|m| m >= now_min - 1)) } else { None };
    let selected = s.selected;
    let today = today();
    let mut delete: Option<String> = None;
    let count = items.len();
    ui.scroll(LIST_ID, list, count as f32 * ROW_H, |ui, top| {
        for (k, it) in items.iter().enumerate() {
            let row = Rect::new(list.x, top + k as f32 * ROW_H, list.w - 6.0, ROW_H);
            let is_next = next == Some(k);
            let past = if is_today { start(it).is_some_and(|m| m < now_min) && !is_next } else { selected < today };
            let base = ui.alpha;
            if past {
                ui.alpha = base * 0.5;
            }
            let hov = ui.hovered(row);
            ui.text(&it.time.map_or(tr("All day").into(), |(h, m)| format!("{h:02}:{m:02}")), Rect::new(row.x, row.y + 2.0, 46.0, 28.0), 12.0, SEMIBOLD, t.subtext, Align::Left);
            // Node and the connector down to the next one.
            let nx = row.x + 46.0 + 9.0;
            if k + 1 < count {
                ui.fill(Rect::new(nx - 0.75, row.y + 22.0, 1.5, ROW_H - 22.0), 0.0, t.stroke);
            }
            let node = if past { t.subtext } else { t.accent };
            ui.g.fill_circle(nx, row.y + 16.5, 4.5, ui.c(if is_next { t.accent } else { t.pill }));
            ui.g.stroke_circle(nx, row.y + 16.5, 3.7, ui.c(node), 1.6);
            // Card.
            let card = Rect::new(row.x + 68.0, row.y + 2.0, row.w - 68.0, ROW_H - 6.0);
            ui.fill(card, 15.0, t.surface);
            let border = if is_next { t.accent } else if hov { t.track } else { t.stroke };
            ui.g.stroke_round(card.inset(0.5, 0.5).d2d(), 14.5, ui.c(border), 1.0);
            let mut right = card.right() - 6.0;
            if let Some(idv) = &it.delete_id {
                let del = Rect::new(right - 22.0, card.cy() - 11.0, 22.0, 22.0);
                right = del.x;
                if hov && ui.icon_button(del, "Trash", 12.0, t.subtext) {
                    delete = Some(idv.clone());
                }
            }
            if is_next {
                let (h, m) = it.time.unwrap_or((0, 0));
                let label = crate::i18n::tf!("UP NEXT · {}", "BERIKUTNYA · {}", next_label((h * 60 + m) as i64 - now_min));
                let w = ui.measure(&label, 11.0, BOLD) + 14.0;
                let chip = Rect::new(right - 2.0 - w, card.cy() - 9.0, w, 18.0);
                right = chip.x - 8.0;
                ui.fill(chip, 8.0, t.accent_soft);
                ui.text(&label, chip, 11.0, BOLD, t.accent, Align::Center);
            }
            ui.text(&it.title, Rect::new(card.x + 11.0, card.y, right - card.x - 11.0, card.h), 12.0, REGULAR, t.text, Align::Left);
            ui.alpha = base;
        }
    });
    if let Some(idv) = delete {
        calendar::delete(&idv);
        s.cache = None;
    }
}

fn next_label(mins: i64) -> String {
    if mins < 1 {
        tr("NOW").into()
    } else if mins < 60 {
        crate::i18n::tf!("IN {mins}M", "{mins} MNT LAGI")
    } else if mins % 60 == 0 {
        crate::i18n::tf!("IN {}H", "{} JAM LAGI", mins / 60)
    } else {
        format!("IN {}H {}M", mins / 60, mins % 60)
    }
}

// ---------------------------------------------------------------- composer

fn composer_pill(ui: &mut Ui, s: &mut State, r: Rect) {
    let t = ui.t;
    if s.focus_title && !ui.input.pressed {
        s.focus_title = false;
        ui.st.focus = TITLE_ID;
        ui.wants_keyboard = true;
    }
    let focused = ui.st.focus == TIME_ID || ui.st.focus == TITLE_ID;
    ui.fill(r, 20.0, t.surface);
    let border = if focused { t.accent } else if ui.hovered(r) { t.track } else { t.stroke };
    ui.g.stroke_round(r.inset(0.5, 0.5).d2d(), 19.5, ui.c(border), 1.0);

    let clock = Rect::new(r.x + 5.0, r.cy() - 13.0, 26.0, 26.0);
    if ui.icon_button(clock, "Clock", 14.0, if s.picker { t.accent } else { t.subtext }) {
        if s.picker {
            close_picker(ui, s);
        } else {
            open_picker(ui, s);
        }
    }

    // Time box: clicking it opens the dial too.
    let time_r = Rect::new(clock.right() + 6.0, r.y, 50.0, r.h);
    if !s.picker && ui.input.pressed && !ui.input.consumed && time_r.contains(ui.mouse()) {
        open_picker(ui, s);
    }
    let raw = ui.field_text(TIME_ID);
    if s.time_err.as_deref().is_some_and(|e| e != raw) {
        s.time_err = None;
    }
    if s.time_err.is_some() {
        ui.fill(Rect::new(time_r.x - 2.0, r.cy() - 12.0, time_r.w, 24.0), 6.0, with_alpha(t.danger, 0.22));
    }
    let mut add = false;
    if let Some(v) = ui.text_field(TIME_ID, time_r, tr("Time"), 13.0) {
        ui.set_field_text(TIME_ID, &v);
        add = true;
    }
    ui.fill(Rect::new(time_r.right() + 2.0, r.cy() - 9.0, 1.0, 18.0), 0.0, t.stroke);

    let plus = Rect::new(r.right() - 35.0, r.cy() - 15.0, 30.0, 30.0);
    let title_r = Rect::new(time_r.right() + 15.0, r.y, plus.x - time_r.right() - 23.0, r.h);
    if let Some(v) = ui.text_field(TITLE_ID, title_r, tr("New event…"), 13.0) {
        ui.set_field_text(TITLE_ID, &v);
        add = true;
    }
    let hov = ui.hovered(plus);
    ui.g.fill_circle(plus.cx(), plus.cy(), 15.0, ui.c(with_alpha(t.accent, if hov { 0.85 } else { 1.0 })));
    ui.icon("Plus", plus.cx(), plus.cy(), 14.0, t.on_accent);
    if ui.clicked(plus) {
        add = true;
    }
    if add {
        add_event(ui, s);
    }
}

fn add_event(ui: &mut Ui, s: &mut State) {
    let title = ui.field_text(TITLE_ID).trim().to_string();
    if title.is_empty() {
        return;
    }
    let raw = ui.field_text(TIME_ID).trim().to_string();
    let time = if raw.is_empty() {
        None
    } else {
        match calendar::parse_time(&raw) {
            Some(tm) => Some(tm),
            None => {
                s.time_err = Some(ui.field_text(TIME_ID));
                return;
            }
        }
    };
    let (y, m, d) = calendar::civil_from_days(s.selected);
    calendar::add(y, m, d, time, &title);
    s.cache = None;
    ui.set_field_text(TITLE_ID, "");
    ui.set_field_text(TIME_ID, "");
}

// ---------------------------------------------------------------- time picker

fn open_picker(ui: &mut Ui, s: &mut State) {
    let (h, m) = if let Some((h, m)) = calendar::parse_time(&ui.field_text(TIME_ID)) {
        (h, m)
    } else {
        // The next half hour.
        let n = crate::notch::now();
        let total = n.hour as u32 * 60 + n.minute as u32;
        let next = (total + 30 - total % 30) % (24 * 60);
        (next / 60, next % 60)
    };
    s.dial = picker::Dial::new(h, m, picker::Mode::Clock, false);
    s.picker = true;
    ui.st.focus = 0;
    ui.st.dropdown = PICKER_ID;
    ui.wants_keyboard = true;
}

fn close_picker(ui: &mut Ui, s: &mut State) {
    s.picker = false;
    if ui.st.dropdown == PICKER_ID {
        ui.st.dropdown = 0;
    }
    if ui.st.active == DIAL_ID {
        ui.st.active = 0;
    }
}

fn picker(ui: &mut Ui, s: &mut State, area: Rect) {
    let t = ui.t;
    if let Some(i) = ui.input.keys.iter().position(|k| *k == Key::Escape) {
        ui.input.keys.remove(i);
        close_picker(ui, s);
        return;
    }
    ui.fill(area, 0.0, t.pill);
    let mut c = s.dial;
    let picked = picker::dial(ui, &mut c, area, Some(tr("All day")));
    s.dial = c;
    match picked {
        picker::Picked::None => {}
        picker::Picked::Cancel => close_picker(ui, s),
        picker::Picked::Set(h, m) => {
            ui.set_field_text(TIME_ID, &format!("{h:02}:{m:02}"));
            close_picker(ui, s);
            s.focus_title = true;
            ui.animate = true;
        }
        picker::Picked::Clear => {
            ui.set_field_text(TIME_ID, "");
            close_picker(ui, s);
            s.focus_title = true;
            ui.animate = true;
        }
    }
}

