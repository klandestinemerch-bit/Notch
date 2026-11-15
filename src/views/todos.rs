//! To-Dos: a sidebar with today's progress ring, the filters (To do, Important, Completed) and the TickTick
//! sync line; on the right the composer ("Add a to-do, press Enter", its due chip and +) and the list as
//! cards (check ring coloured by priority, text, due chip, star, delete on hover). A due chip opens the due
//! panel over the list: quick picks, the next 14 days, a time (clock-face picker) and the priority.

use crate::gfx::{color, mix, with_alpha, Align, BOLD, REGULAR, SEMIBOLD};
use crate::services::todos::{self, civil_from_days, days_from_civil, local_now_secs, DueInput, Todo};
use crate::services::{settings, ticktick};
use crate::ui::{id, Key, Rect, Ui};

use super::picker;
use crate::i18n::tr;

const RED: u32 = 0xFF5A5F;
const AMBER: u32 = 0xFFB800;
const BLUE: u32 = 0x4C8DFF;

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Filter {
    #[default]
    Open,
    Starred,
    Done,
}

/// Which to-do the due panel edits: the one being typed, or an item by id.
#[derive(Clone, PartialEq, Eq)]
enum Editing {
    New,
    Item(String),
}

#[derive(Default)]
pub struct State {
    filter: Filter,
    /// What the next new to-do gets.
    new_due: Option<DueInput>,
    new_priority: i32,
    new_ready: bool,
    panel: Option<Editing>,
    clock: Option<picker::Dial>,
}

fn reset_new(s: &mut State) {
    s.new_due = if settings::get().todo_default_due == "today" { Some(today_input()) } else { None };
    s.new_priority = if s.filter == Filter::Starred { 5 } else { 0 };
    s.new_ready = true;
}

pub fn shown(s: &mut State) {
    if !s.new_ready {
        reset_new(s);
    }
    ticktick::sync_soon(); // fresh from TickTick when you look
    // Dev aid for screenshots: FATHNOTCH_TODOS_DEMO=due|clock opens the panel for the new to-do.
    match std::env::var("FATHNOTCH_TODOS_DEMO").as_deref() {
        Ok("due") => s.panel = Some(Editing::New),
        Ok("clock") => {
            s.panel = Some(Editing::New);
            s.clock = Some(new_clock(None));
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ dates

fn today_days() -> i64 {
    local_now_secs().div_euclid(86_400)
}

fn input_for_days(days: i64, time: Option<(u32, u32)>) -> DueInput {
    let (y, m, d) = civil_from_days(days);
    (y, m, d, time)
}

fn today_input() -> DueInput {
    input_for_days(today_days(), None)
}

fn days_of(due: &DueInput) -> i64 {
    days_from_civil(due.0, due.1, due.2)
}

fn dow(days: i64) -> usize {
    (days + 4).rem_euclid(7) as usize
}

/// "Today", "Tomorrow", "Mon 16 Nov", with " 09:30" when timed; "No date" without one.
fn describe(due: Option<DueInput>) -> String {
    let Some(d) = due else { return tr("No date").into() };
    let days = days_of(&d);
    let today = today_days();
    let day = if days == today {
        tr("Today").to_string()
    } else if days == today + 1 {
        tr("Tomorrow").to_string()
    } else {
        format!("{} {} {}", crate::i18n::day3(dow(days)), d.2, crate::i18n::month3(d.1 as usize))
    };
    match d.3 {
        Some((h, m)) => format!("{day} {h:02}:{m:02}"),
        None => day,
    }
}

fn item_due(t: &Todo) -> Option<DueInput> {
    let (y, m, d, h, min) = t.due_local()?;
    Some((y, m, d, if t.due_all_day { None } else { Some((h, min)) }))
}

fn new_clock(current: Option<(u32, u32)>) -> picker::Dial {
    let (hour, minute) = current.unwrap_or_else(|| {
        // The next half hour.
        let n = crate::notch::now();
        let total = n.hour as u32 * 60 + n.minute as u32;
        let next = (total + 30 - total % 30) % (24 * 60);
        (next / 60, next % 60)
    });
    picker::Dial::new(hour, minute, picker::Mode::Clock, false)
}

fn priority_color(p: i32) -> Option<u32> {
    match p {
        5 => Some(RED),
        3 => Some(AMBER),
        1 => Some(BLUE),
        _ => None,
    }
}

// ------------------------------------------------------------------ the view

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, _go: &mut Option<&'static str>) {
    if !s.new_ready {
        reset_new(s);
    }
    let area = r.pad(18.0, 0.0, 18.0, 4.0);
    let (side, rest) = area.take_left(168.0, 14.0);
    let items = todos::all();

    // Escape closes the clock, then the panel (before the composer sees it).
    if s.panel.is_some() && ui.input.keys.contains(&Key::Escape) {
        ui.input.keys.retain(|k| *k != Key::Escape);
        if s.clock.is_some() {
            s.clock = None;
        } else {
            s.panel = None;
        }
    }

    sidebar(ui, s, side, &items);

    // The due panel covers the whole main column while open.
    if s.panel.is_some() {
        due_panel(ui, s, rest, &items);
        if ui.input.pressed && rest.contains(ui.mouse()) {
            ui.input.consumed = true; // a press on the panel's empty space
        }
    } else {
        main_column(ui, s, rest, &items);
    }
}

fn sidebar(ui: &mut Ui, s: &mut State, r: Rect, items: &[Todo]) {
    let t = ui.t;
    let done = items.iter().filter(|i| i.done).count();
    let open = items.len() - done;
    let starred = items.iter().filter(|i| i.starred && !i.done).count();

    // Progress ring and "N to go".
    let (cx, cy) = (r.x + 2.0 + 16.0, r.y + 16.0);
    ui.g.stroke_circle(cx, cy, 14.5, ui.c(t.track), 3.0);
    let frac = if items.is_empty() { 0.0 } else { done as f32 / items.len() as f32 };
    if frac > 0.001 {
        ui.g.arc(cx, cy, 14.5, 0.0, frac.min(0.9999) * std::f32::consts::TAU, ui.c(t.accent), 3.0);
    }
    let title = if items.is_empty() {
        tr("No to-dos").to_string()
    } else if open == 0 {
        tr("All done").to_string()
    } else {
        crate::i18n::tf!("{open} to go", "{open} lagi")
    };
    let sub = if items.is_empty() { tr("Add one on the right").to_string() } else { crate::i18n::tf!("{done} of {} completed", "{done} dari {} selesai", items.len()) };
    let tx = r.x + 2.0 + 34.0 + 9.0;
    ui.text(&title, Rect::new(tx, r.y, r.right() - tx, 18.0), 13.0, SEMIBOLD, t.text, Align::Left);
    ui.text(&sub, Rect::new(tx, r.y + 17.0, r.right() - tx, 15.0), 11.0, REGULAR, t.subtext, Align::Left);

    // Filters.
    let rows = [
        (Filter::Open, "ListBullet", tr("To do"), open),
        (Filter::Starred, "Star", tr("Important"), starred),
        (Filter::Done, "Check", tr("Completed"), done),
    ];
    for (i, (f, icon, label, count)) in rows.into_iter().enumerate() {
        let row = Rect::new(r.x, r.y + 42.0 + i as f32 * 35.0, r.w, 32.0);
        let selected = s.filter == f;
        let hov = ui.hovered(row);
        if selected {
            ui.fill(row, 8.0, t.accent_soft);
        } else if hov {
            ui.fill(row, 8.0, t.surface2);
        }
        let fg = if selected || hov { t.text } else { t.subtext };
        ui.icon(icon, row.x + 17.0, row.cy(), 14.0, fg);
        ui.text(label, Rect::new(row.x + 33.0, row.y, row.w - 70.0, row.h), 12.0, SEMIBOLD, fg, Align::Left);
        ui.text(&count.to_string(), Rect::new(row.right() - 50.0, row.y, 40.0, row.h), 11.0, REGULAR, t.subtext, Align::Right);
        if ui.clicked(row) && s.filter != f {
            s.filter = f;
            if f == Filter::Starred && s.new_priority < 5 {
                s.new_priority = 5;
            } else if f != Filter::Starred && s.new_priority == 5 && s.panel.is_none() {
                s.new_priority = 0;
            }
        }
    }

    // TickTick sync status (click to sync now).
    if ticktick::enabled() {
        let status = ticktick::status();
        let label = if status.is_empty() { "TickTick".to_string() } else { format!("TickTick · {status}") };
        let w = (ui.measure(&label, 11.0, REGULAR).min(138.0) + 12.0 + 6.0 + 16.0).min(r.w);
        let row = Rect::new(r.x, r.bottom() - 24.0, w, 24.0);
        if ui.hovered(row) {
            ui.fill(row, 10.0, t.surface2);
        }
        let icon_c = if ticktick::failed() { t.danger } else { t.subtext };
        ui.icon("Refresh", row.x + 14.0, row.cy(), 12.0, icon_c);
        ui.text(&label, Rect::new(row.x + 26.0, row.y, row.w - 30.0, row.h), 11.0, REGULAR, t.subtext, Align::Left);
        if ui.clicked(row) {
            ticktick::sync_soon();
        }
    }
}

/// A small chip (hover-tinted pill, 10.5 px text) as the due chips in the composer and rows.
fn due_chip(ui: &mut Ui, right: f32, cy: f32, label: &str, icon: bool, fg: crate::gfx::Color) -> Rect {
    let t = ui.t;
    let tw = ui.measure(label, 11.0, REGULAR);
    let w = tw + 14.0 + if icon { 16.0 } else { 0.0 };
    let r = Rect::new(right - w, cy - 9.5, w, 19.0);
    ui.fill(r, 6.0, t.hover);
    let mut x = r.x + 7.0;
    if icon {
        ui.icon("CalendarDays", x + 5.5, r.cy(), 11.0, t.subtext);
        x += 16.0;
    }
    ui.text(label, Rect::new(x, r.y, tw + 2.0, r.h), 11.0, REGULAR, fg, Align::Left);
    r
}

fn main_column(ui: &mut Ui, s: &mut State, r: Rect, items: &[Todo]) {
    let t = ui.t;

    // Composer.
    let comp = Rect::new(r.x, r.y, r.w, 38.0);
    ui.fill(comp, 10.0, t.surface);
    let plus = Rect::new(comp.right() - 4.0 - 30.0, comp.cy() - 15.0, 30.0, 30.0);
    let label = describe(s.new_due)
        + match s.new_priority {
            5 => tr(" · High"),
            3 => tr(" · Medium"),
            1 => tr(" · Low"),
            _ => "",
        };
    // The chip sits in a row button (8, 5 padding, Surface2 on hover).
    let chip_w = ui.measure(&label, 11.0, REGULAR) + 30.0;
    let chip_bg = Rect::new(plus.x - chip_w - 16.0, comp.cy() - 14.5, chip_w + 16.0, 29.0);
    if ui.hovered(chip_bg) {
        ui.fill(chip_bg, 10.0, t.surface2);
    }
    let chip = due_chip(ui, plus.x - 8.0, comp.cy(), &label, true, t.subtext);
    if ui.clicked(chip_bg) {
        s.panel = Some(Editing::New);
        s.clock = None;
    }
    let field_id = id("todos.input");
    let field = Rect::new(comp.x + 12.0, comp.y, chip.x - comp.x - 24.0, comp.h);
    let submitted = ui.text_field(field_id, field, tr("Add a to-do, press Enter"), 13.0);
    let mut add_text = submitted;
    if ui.icon_button(plus, "Plus", 15.0, t.accent) {
        let typed = ui.field_text(field_id);
        if !typed.trim().is_empty() {
            add_text = Some(typed.trim().to_string());
            ui.set_field_text(field_id, "");
        }
    }
    if let Some(text) = add_text {
        if s.filter == Filter::Starred && s.new_priority < 5 {
            s.new_priority = 5; // adding under Important makes it important
        }
        if s.filter == Filter::Done {
            s.filter = Filter::Open; // a new item wouldn't show under Completed
        }
        todos::add(&text, s.new_priority, s.new_due);
        s.panel = None;
        reset_new(s);
    }

    // List: open first, important on top, newest first.
    let mut shown: Vec<&Todo> = items
        .iter()
        .filter(|i| match s.filter {
            Filter::Open => !i.done,
            Filter::Starred => i.starred && !i.done,
            Filter::Done => i.done,
        })
        .collect();
    let created = |x: &Todo| todos::parse_local(&x.created).unwrap_or((0, 0));
    shown.sort_by(|a, b| a.done.cmp(&b.done).then(b.starred.cmp(&a.starred)).then(created(b).cmp(&created(a))));

    let list = Rect::new(r.x, comp.bottom() + 8.0, r.w, r.bottom() - comp.bottom() - 8.0);
    if shown.is_empty() {
        let (icon, text) = match s.filter {
            Filter::Starred => ("Star", tr("Nothing marked important")),
            Filter::Done => ("Check", tr("Nothing completed yet")),
            Filter::Open if items.is_empty() => ("ListBullet", tr("Nothing to do yet")),
            Filter::Open => ("Check", tr("All done. Nice!")),
        };
        let cy = list.cy();
        ui.icon(icon, list.cx(), cy - 12.0, 22.0, with_alpha(t.subtext, 0.6));
        ui.text(text, Rect::new(list.x, cy + 2.0, list.w, 20.0), 12.0, REGULAR, t.subtext, Align::Center);
        return;
    }
    let row_h = 38.0;
    let content_h = shown.len() as f32 * (row_h + 5.0);
    let mut open_panel: Option<String> = None;
    ui.scroll(id("todos.list"), list, content_h, |ui, top| {
        for (i, item) in shown.iter().enumerate() {
            let card = Rect::new(list.x, top + i as f32 * (row_h + 5.0), list.w - 4.0, row_h);
            if card.bottom() < list.y || card.y > list.bottom() {
                continue;
            }
            if let Some(id) = todo_row(ui, card, item) {
                open_panel = Some(id);
            }
        }
    });
    if let Some(item) = open_panel {
        s.panel = Some(Editing::Item(item));
        s.clock = None;
    }
}

/// One to-do card; returns the id when its due chip was clicked.
fn todo_row(ui: &mut Ui, card: Rect, item: &Todo) -> Option<String> {
    let t = ui.t;
    let hov = ui.hovered(card);
    let alpha = if item.done { 0.7 } else { 1.0 };
    ui.fill(card, 10.0, with_alpha(if hov { t.surface2 } else { t.surface }, alpha));
    let mut clicked_due = None;

    // Check ring (coloured by priority); filled with the accent when done.
    let ring = Rect::new(card.x + 10.0, card.cy() - 9.0, 18.0, 18.0);
    let ring_hov = ui.hovered(ring);
    if item.done {
        ui.g.fill_circle(ring.cx(), ring.cy(), 9.0, ui.c(with_alpha(t.accent, alpha)));
        ui.icon("Check", ring.cx(), ring.cy(), 10.0, t.on_accent);
    } else {
        let c = if ring_hov { t.accent } else { priority_color(item.tick_priority).map_or(t.subtext, |c| color(c, 1.0)) };
        ui.g.stroke_circle(ring.cx(), ring.cy(), 8.2, ui.c(with_alpha(c, if ring_hov { 1.0 } else { 0.85 })), 1.6);
    }
    if ui.clicked(Rect::new(ring.x - 4.0, ring.y - 4.0, 26.0, 26.0)) {
        todos::set_done(&item.id, !item.done);
    }

    // Right side: delete, star (26 each, shown on hover / when starred), the due chip.
    let del = Rect::new(card.right() - 4.0 - 26.0, card.cy() - 13.0, 26.0, 26.0);
    let star = Rect::new(del.x - 26.0, del.y, 26.0, 26.0);
    if hov {
        if ui.icon_button(del, "Trash", 12.0, t.subtext) {
            todos::delete(&item.id);
        }
    }
    if hov || item.starred {
        let c = if item.starred { color(AMBER, 1.0) } else { t.subtext };
        if ui.icon_button(star, "Star", 13.0, c) {
            todos::set_starred(&item.id, !item.starred);
        }
    }
    let due = item.due_text();
    let chip_right = star.x - 10.0;
    let text_right;
    if !due.is_empty() {
        let fg = if item.overdue() { t.danger } else { t.subtext };
        let chip = due_chip(ui, chip_right, card.cy(), &due, false, fg);
        let hit = chip.inset(-6.0, -4.0);
        text_right = hit.x;
        if ui.clicked(hit) {
            clicked_due = Some(item.id.clone());
        }
    } else {
        let hit = Rect::new(chip_right - 26.0, card.cy() - 13.0, 26.0, 26.0);
        text_right = hit.x;
        if hov {
            if ui.icon_button(hit, "CalendarDays", 13.0, t.subtext) {
                clicked_due = Some(item.id.clone());
            }
        }
    }

    // The text (struck through when done).
    let tx = card.x + 10.0 + 28.0;
    let tr = Rect::new(tx, card.y, (text_right - 8.0 - tx).max(0.0), card.h);
    let fg = if item.done { t.subtext } else { t.text };
    ui.text(&item.text, tr, 12.0, REGULAR, with_alpha(fg, alpha), Align::Left);
    if item.done {
        let w = ui.measure(&item.text, 12.0, REGULAR).min(tr.w);
        ui.g.line(tx, card.cy() + 0.5, tx + w, card.cy() + 0.5, ui.c(with_alpha(fg, alpha)), 1.0);
    }
    clicked_due
}

// ------------------------------------------------------------------ due panel

/// A selectable pill (11.5 px semibold), optionally with a coloured dot; returns (rect, clicked).
fn pill(ui: &mut Ui, x: f32, y: f32, label: &str, selected: bool, dot: Option<u32>) -> (Rect, bool) {
    let t = ui.t;
    let tw = ui.measure(label, 11.0, SEMIBOLD);
    let w = tw + 18.0 + if dot.is_some() { 12.0 } else { 0.0 };
    let r = Rect::new(x, y, w, 23.0);
    let hov = ui.hovered(r);
    let bg = if selected { t.accent_soft } else if hov { mix(t.hover, t.text, 0.06) } else { t.hover };
    ui.fill(r, 8.0, bg);
    let mut tx = r.x + 9.0;
    if let Some(c) = dot {
        ui.g.fill_circle(tx + 3.5, r.cy(), 3.5, ui.c(color(c, 1.0)));
        tx += 12.0;
    }
    ui.text(label, Rect::new(tx, r.y, tw + 2.0, r.h), 11.0, SEMIBOLD, if selected { t.text } else { t.subtext }, Align::Left);
    let clicked = ui.clicked(r);
    (r, clicked)
}

/// Applies a choice to the item being edited or to the new to-do.
fn apply(s: &mut State, due: Option<DueInput>, priority: Option<i32>) {
    match s.panel.clone() {
        Some(Editing::Item(id)) => {
            todos::set_due(&id, due);
            if let Some(p) = priority {
                todos::set_priority(&id, p);
            }
        }
        _ => {
            s.new_due = due;
            if let Some(p) = priority {
                s.new_priority = p;
            }
        }
    }
}

fn due_panel(ui: &mut Ui, s: &mut State, r: Rect, items: &[Todo]) {
    let t = ui.t;
    let editing = s.panel.clone().unwrap_or(Editing::New);
    let item = match &editing {
        Editing::Item(id) => items.iter().find(|i| &i.id == id).cloned(),
        Editing::New => None,
    };
    if matches!(editing, Editing::Item(_)) && item.is_none() {
        s.panel = None;
        return;
    }
    let (due, priority, title) = match &item {
        Some(i) => (item_due(i), i.tick_priority, i.text.clone()),
        None => (s.new_due, s.new_priority, tr("New to-do").to_string()),
    };

    ui.fill(r, 12.0, t.surface);
    ui.g.stroke_round(r.inset(0.5, 0.5).d2d(), 12.0, ui.c(t.stroke), 1.0);
    let inner = r.pad(12.0, 8.0, 12.0, 10.0);

    if let Some(mut c) = s.clock {
        let picked = picker::dial(ui, &mut c, Rect::new(inner.x, inner.y + 4.0, inner.w, inner.h - 4.0), Some(tr("All day")));
        let time = match picked {
            picker::Picked::None => {
                s.clock = Some(c);
                return;
            }
            picker::Picked::Cancel => {
                s.clock = None;
                return;
            }
            picker::Picked::Set(h, m) => Some((h, m)),
            picker::Picked::Clear => None,
        };
        let day = due.map_or(today_days(), |d| days_of(&d));
        apply(s, Some(input_for_days(day, time)), None);
        s.clock = None;
        return;
    }

    // Header: what's being dated, and Done.
    let done_w = ui.measure(tr("Done"), 11.0, SEMIBOLD) + 24.0;
    let done_r = Rect::new(inner.right() - done_w, inner.y, done_w, 26.0);
    let hov = ui.hovered(done_r);
    ui.fill(done_r, 8.0, with_alpha(t.accent, if hov { 0.85 } else { 1.0 }));
    ui.text(tr("Done"), done_r, 11.0, SEMIBOLD, t.on_accent, Align::Center);
    if ui.clicked(done_r) {
        s.panel = None;
        s.clock = None;
        return;
    }
    ui.text(&title, Rect::new(inner.x, inner.y, done_r.x - inner.x - 8.0, 26.0), 13.0, SEMIBOLD, t.text, Align::Left);
    let mut y = inner.y + 26.0 + 6.0;

    // Quick picks.
    let today = today_days();
    let to_monday = (1 - dow(today) as i64).rem_euclid(7);
    let next_week = today + if to_monday == 0 { 7 } else { to_monday };
    let time = due.and_then(|d| d.3);
    let mut x = inner.x;
    for (label, day) in [(tr("Today"), Some(today)), (tr("Tomorrow"), Some(today + 1)), (tr("Next week"), Some(next_week)), (tr("No date"), None)] {
        let selected = match day {
            None => due.is_none(),
            Some(d) => due.is_some_and(|x| days_of(&x) == d),
        };
        let (pr, clicked) = pill(ui, x, y, label, selected, None);
        if clicked {
            apply(s, day.map(|d| input_for_days(d, time)), None);
        }
        x = pr.right() + 4.0;
    }
    y += 23.0 + 6.0;

    // The next 14 days.
    let cell_w = inner.w / 7.0;
    for i in 0..14 {
        let day = today + i as i64;
        let cell = Rect::new(inner.x + (i % 7) as f32 * cell_w + 1.0, y + (i / 7) as f32 * 36.0 + 1.0, cell_w - 2.0, 34.0);
        let selected = due.is_some_and(|d| days_of(&d) == day);
        if selected {
            ui.fill(cell, 8.0, t.accent);
        } else if ui.hovered(cell) {
            ui.fill(cell, 8.0, t.surface2);
        }
        let (_, _, dnum) = civil_from_days(day);
        let name = crate::i18n::day2(dow(day));
        ui.text(name, Rect::new(cell.x, cell.y + 3.0, cell.w, 13.0), 10.0, REGULAR, if selected { t.on_accent } else { t.subtext }, Align::Center);
        ui.text(
            &dnum.to_string(),
            Rect::new(cell.x, cell.y + 15.0, cell.w, 16.0),
            12.0,
            if i == 0 { BOLD } else { SEMIBOLD },
            if selected { t.on_accent } else { t.text },
            Align::Center,
        );
        if ui.clicked(cell) {
            apply(s, Some(input_for_days(day, time)), None);
        }
    }
    y += 72.0 + 8.0;

    // Time: "Pick a time" (or the time) and All day.
    ui.icon("Clock", inner.x + 2.0 + 6.5, y + 11.5, 13.0, t.subtext);
    let tx = inner.x + 23.0;
    let label = time.map_or(tr("Pick a time").to_string(), |(h, m)| format!("{h:02}:{m:02}"));
    let (pr, clicked) = pill(ui, tx, y, &label, time.is_some(), None);
    if clicked {
        s.clock = Some(new_clock(time));
    }
    if time.is_some() {
        let (_, clicked) = pill(ui, pr.right() + 4.0, y, tr("All day"), false, None);
        if clicked {
            let day = due.map_or(today, |d| days_of(&d));
            apply(s, Some(input_for_days(day, None)), None);
        }
    }
    y += 23.0 + 6.0;

    // Priority, as in TickTick.
    ui.icon("Flag", inner.x + 2.0 + 6.5, y + 11.5, 13.0, t.subtext);
    let mut x = tx;
    for (label, value, dot) in [(tr("None"), 0, None), (tr("Low"), 1, Some(BLUE)), (tr("Medium"), 3, Some(AMBER)), (tr("High"), 5, Some(RED))] {
        let (pr, clicked) = pill(ui, x, y, label, priority == value, dot);
        if clicked {
            apply(s, due, Some(value));
        }
        x = pr.right() + 4.0;
    }
}
