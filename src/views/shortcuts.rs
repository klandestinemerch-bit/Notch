//! Shortcuts: a grid of one-click launchers (apps, folders, URLs, Windows settings pages) with search,
//! an inline editor (+ to add, right-click → Edit / Delete) and a status line after running one.
//! Sizes follow the .NET ShortcutsView.xaml.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::bg;
use crate::gfx::{color, with_alpha, Align, MEDIUM, REGULAR, SEMIBOLD};
use crate::services::shortcuts::{self, Shortcut};
use crate::ui::{id, Key, Rect, Ui};
use crate::i18n::tr;

const SEARCH: &str = "shortcuts.search";
const NAME: &str = "shortcuts.name";
const TARGET: &str = "shortcuts.target";

#[derive(Default)]
pub struct State {
    items: Vec<Shortcut>,
    loaded: bool,
    /// Editor open: Some(None) = new, Some(Some(i)) = editing item i.
    editing: Option<Option<usize>>,
    /// Field that failed validation (red border until edited).
    invalid: Option<&'static str>,
    status: Option<(String, bool, Instant)>,
    /// Right-click menu: item index and where it opens.
    menu: Option<(usize, f32, f32)>,
    /// A file picked with Browse (filled in by a background thread).
    picked: Arc<Mutex<Option<String>>>,
    /// Give the Name field focus once this frame's click is through (opened from the menu).
    refocus: bool,
}

pub fn shown(s: &mut State) {
    s.items = shortcuts::all();
    s.loaded = true;
    s.menu = None;
}

fn flash(s: &mut State, text: String, bad: bool) {
    s.status = Some((text, bad, Instant::now()));
}

/// Text field box: Surface2, 1 px border (Track on hover, Accent with a soft glow while focused, red on error).
fn field_chrome(ui: &mut Ui, r: Rect, focused: bool, error: bool) {
    let t = ui.t;
    if focused && !error {
        ui.g.stroke_round(r.inset(-1.5, -1.5).d2d(), 10.5, ui.c(t.accent_soft), 3.0);
    }
    ui.fill(r, 9.0, t.surface2);
    let border = if error {
        t.danger
    } else if focused {
        t.accent
    } else if ui.hovered(r) {
        t.track
    } else {
        t.stroke
    };
    ui.g.stroke_round(r.inset(0.5, 0.5).d2d(), 8.5, ui.c(border), 1.0);
}

/// Default or accent button (13 px, 14 padding, 8 radius); hover dims it.
fn button(ui: &mut Ui, r: Rect, label: &str, accent: bool) -> bool {
    let t = ui.t;
    let hov = ui.hovered(r);
    let (bg, fg) = if accent { (t.accent, t.on_accent) } else { (t.surface2, t.text) };
    ui.fill(r, 8.0, with_alpha(bg, if hov { 0.85 } else { 1.0 }));
    ui.text(label, r, 13.0, if accent { SEMIBOLD } else { MEDIUM }, fg, Align::Center);
    ui.clicked(r)
}

fn button_w(ui: &Ui, label: &str, accent: bool) -> f32 {
    ui.measure(label, 13.0, if accent { SEMIBOLD } else { MEDIUM }).ceil() + 28.0
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, _go: &mut Option<&'static str>) {
    let t = ui.t;
    if !s.loaded {
        shown(s);
    }
    let menu_hit = menu_input(ui, s, r);

    // Search row: field, status (8 right of it), "+" (34).
    let (top, mut rest) = r.take_top(34.0, 8.0);
    let (plus, field_row) = top.take_right(34.0, 8.0);
    let mut field = field_row;
    if let Some((text, bad, at)) = s.status.clone() {
        if at.elapsed().as_secs_f32() < 2.0 {
            let w = (ui.measure(&text, 12.0, SEMIBOLD).ceil() + 2.0).min(300.0);
            let (st, f) = field_row.take_right(w + 8.0, 8.0);
            field = f;
            let c = if bad { t.danger } else { color(0x34C759, 1.0) };
            ui.text(&text, Rect::new(st.x, st.y, w, st.h), 12.0, SEMIBOLD, c, Align::Left);
            ui.animate = true;
        } else {
            s.status = None;
        }
    }
    let sid = id(SEARCH);
    field_chrome(ui, field, ui.st.focus == sid, false);
    ui.icon("Magnifier", field.x + 18.5, field.cy(), 15.0, t.subtext);
    let has_text = !ui.field_text(sid).is_empty();
    let _ = ui.text_field(sid, field.pad(36.0, 0.0, if has_text { 30.0 } else { 10.0 }, 0.0), tr("Search shortcuts…"), 13.0);
    let query = ui.field_text(sid).trim().to_lowercase();
    if has_text && ui.icon_button(Rect::new(field.right() - 28.0, field.cy() - 11.0, 22.0, 22.0), "XMark", 12.0, t.subtext) {
        ui.set_field_text(sid, "");
    }
    if ui.icon_button(plus, "Plus", 14.0, t.subtext) {
        open_editor(ui, s, None);
    }

    // Inline editor: 8 padding around 34 tall fields, 8 below.
    if s.editing.is_some() {
        let (ed, below) = rest.take_top(50.0, 8.0);
        rest = below;
        editor(ui, s, ed);
    }

    // Grid: 4 columns (UniformGrid), each button 26 tall with a 6 margin right and below.
    let visible: Vec<usize> =
        (0..s.items.len()).filter(|&i| query.is_empty() || s.items[i].name.to_lowercase().contains(&query)).collect();
    let cols = 4usize;
    let cell_w = rest.w / cols as f32;
    let (row_h, gap) = (26.0, 6.0);
    let rows = visible.len().div_ceil(cols);
    let content_h = rows as f32 * (row_h + gap);
    let mut run: Option<usize> = None;
    let mut open_menu: Option<(usize, f32, f32)> = None;
    let items = &s.items;
    let menu_open = s.menu.is_some();
    ui.scroll(id("shortcuts.grid"), rest, content_h, |ui, top| {
        for (k, &i) in visible.iter().enumerate() {
            let (row, col) = (k / cols, k % cols);
            let b = Rect::new(rest.x + col as f32 * cell_w, top + row as f32 * (row_h + gap), cell_w - gap, row_h);
            if b.bottom() < rest.y || b.y > rest.bottom() {
                continue;
            }
            let hov = ui.hovered(b) && !menu_open;
            let pressed = hov && ui.input.down;
            ui.fill(b, 8.0, with_alpha(if hov { t.surface2 } else { t.surface }, if pressed { 0.7 } else { 1.0 }));
            let cx = b.x + 7.0 + 6.5;
            ui.g.fill_circle(cx, b.cy(), 6.5, ui.c(t.accent));
            ui.icon("Play", cx + 0.5, b.cy(), 8.0, t.on_accent);
            let tw = (b.w - 27.0 - 7.0).min(96.0);
            ui.text(&items[i].name, Rect::new(b.x + 27.0, b.y, tw, b.h), 12.0, MEDIUM, t.text, Align::Left);
            if ui.right_clicked(b) {
                let (mx, my) = ui.mouse();
                open_menu = Some((i, mx, my));
            } else if ui.clicked(b) {
                run = Some(i);
            }
        }
    });
    if !menu_hit {
        if let Some(m) = open_menu {
            s.menu = Some(m);
        }
        if let Some(i) = run {
            let item = s.items[i].clone();
            match shortcuts::run(&item) {
                Ok(()) => flash(s, crate::i18n::tf!("Ran {}", "Membuka {}", tr(&item.name)), false),
                Err(e) => flash(s, tr(&e).to_string(), true),
            }
        }
    }
    draw_menu(ui, s, r);
}

fn open_editor(ui: &mut Ui, s: &mut State, index: Option<usize>) {
    s.editing = Some(index);
    s.invalid = None;
    let (name, cmd) = match index.and_then(|i| s.items.get(i)) {
        Some(it) => (it.name.clone(), shortcuts::join_command(it)),
        None => (String::new(), String::new()),
    };
    ui.set_field_text(id(NAME), &name);
    ui.set_field_text(id(TARGET), &cmd);
    ui.st.focus = id(NAME);
    ui.wants_keyboard = true;
    s.refocus = true;
}

fn editor(ui: &mut Ui, s: &mut State, r: Rect) {
    let t = ui.t;
    ui.fill(r, 9.0, t.surface);
    let inner = r.inset(8.0, 8.0);
    let (name_r, rest) = inner.take_left(120.0, 6.0);
    let (close_r, rest) = rest.take_right(26.0, 2.0);
    let (save_r, rest) = rest.take_right(button_w(ui, tr("Save"), true), 6.0);
    let (browse_r, target_r) = rest.take_right(button_w(ui, tr("Browse"), false), 6.0);

    // A file chosen with Browse arrives from the dialog thread.
    if let Some(p) = s.picked.lock().unwrap().take() {
        ui.set_field_text(id(TARGET), &p);
        if ui.field_text(id(NAME)).trim().is_empty() {
            let stem = std::path::Path::new(&p).file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
            ui.set_field_text(id(NAME), &stem);
        }
    }

    let mut submit = false;
    for (fid, fr, ph, key) in [(id(NAME), name_r, tr("Name"), "name"), (id(TARGET), target_r, tr("App, file, folder, URL or ms-settings:…"), "target")] {
        let bad = s.invalid == Some(key);
        field_chrome(ui, fr, ui.st.focus == fid, bad);
        // Enter saves instead of clearing the field (the kit's field clears on Enter).
        if ui.st.focus == fid && ui.input.keys.contains(&Key::Enter) {
            ui.input.keys.retain(|k| *k != Key::Enter);
            submit = true;
        }
        let before = ui.field_text(fid);
        let _ = ui.text_field(fid, fr.pad(12.0, 0.0, 10.0, 0.0), ph, 13.0);
        if bad && ui.field_text(fid) != before {
            s.invalid = None;
        }
    }
    if s.refocus {
        s.refocus = false;
        ui.st.focus = id(NAME);
        ui.wants_keyboard = true;
    }
    if button(ui, browse_r, tr("Browse"), false) {
        let slot = s.picked.clone();
        bg::spawn(move || {
            if let Some(p) = shortcuts::pick_files(tr("Choose an app or file"), false).into_iter().next() {
                *slot.lock().unwrap() = Some(p);
            }
        });
    }
    if button(ui, save_r, tr("Save"), true) {
        submit = true;
    }
    if ui.icon_button(Rect::new(close_r.x, close_r.cy() - 13.0, 26.0, 26.0), "XMark", 13.0, t.subtext) {
        s.editing = None;
        return;
    }
    if ui.input.keys.contains(&Key::Escape) {
        ui.input.keys.retain(|k| *k != Key::Escape);
        s.editing = None;
        return;
    }
    if submit {
        let name = ui.field_text(id(NAME)).trim().to_string();
        let (target, args) = shortcuts::split_command(&ui.field_text(id(TARGET)));
        if name.is_empty() || target.is_empty() {
            s.invalid = Some(if name.is_empty() { "name" } else { "target" });
            return;
        }
        match s.editing.flatten() {
            Some(i) => shortcuts::update(i, &name, &target, &args),
            None => shortcuts::add(&name, &target, &args),
        }
        s.items = shortcuts::all();
        s.editing = None;
        ui.st.focus = 0;
    }
}

// ---------------------------------------------------------------- right-click menu (.NET ContextMenu look)

const MENU_ROW: f32 = 27.0;
const MENU_ITEMS: [&str; 2] = ["Edit", "Delete"];

fn menu_rect(s: &State, r: Rect) -> Option<Rect> {
    let (_, x, y) = s.menu?;
    let w = 130.0 + 8.0;
    let h = MENU_ROW * MENU_ITEMS.len() as f32 + 8.0;
    let x = x.min(r.right() - w);
    let y = if y + h > r.bottom() + 8.0 { (y - h).max(r.y - 40.0) } else { y };
    Some(Rect::new(x, y, w, h))
}

/// Handles a click on (or away from) the open menu before anything else sees it. True when it took the click.
fn menu_input(ui: &mut Ui, s: &mut State, r: Rect) -> bool {
    let Some((i, _, _)) = s.menu else { return false };
    if !(ui.input.pressed || ui.input.right_pressed) || ui.input.consumed {
        return false;
    }
    let m = menu_rect(s, r).unwrap();
    let (mx, my) = ui.mouse();
    s.menu = None;
    if m.contains((mx, my)) && ui.input.pressed {
        match ((my - m.y - 4.0) / MENU_ROW).floor() as i32 {
            0 => {
                open_editor(ui, s, Some(i));
                s.refocus = true;
            }
            1 => {
                shortcuts::remove(i);
                s.items = shortcuts::all();
                s.editing = None;
            }
            _ => {}
        }
    }
    ui.input.consumed = true;
    true
}

fn draw_menu(ui: &mut Ui, s: &State, r: Rect) {
    let Some(m) = menu_rect(s, r) else { return };
    let t = ui.t;
    ui.g.fill_round(m.d2d(), 8.0, ui.c(t.surface2));
    ui.g.stroke_round(m.inset(0.5, 0.5).d2d(), 7.5, ui.c(t.stroke), 1.0);
    for (k, label) in MENU_ITEMS.iter().enumerate() {
        let row = Rect::new(m.x + 4.0, m.y + 4.0 + k as f32 * MENU_ROW, m.w - 8.0, MENU_ROW);
        if row.contains(ui.mouse()) {
            ui.fill(row, 5.0, t.hover);
        }
        ui.text(label, row.pad(10.0, 0.0, 6.0, 0.0), 13.0, REGULAR, t.text, Align::Left);
    }
    ui.animate = true;
}
