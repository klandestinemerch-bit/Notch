//! Quick Note: note cards (title, preview, time) on the left, the open note on the right in a card with a
//! footer (save state, word count, copy, delete). The first line is the title; it saves as you type.
//! The editor is a small multi-line text box written here (caret, typing, Enter, Backspace/Delete, arrows,
//! Home/End, Ctrl+A / C / X / V).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::gfx::{with_alpha, Align, BOLD, REGULAR, SEMIBOLD};
use crate::services::{notes, settings};
use crate::ui::{id, Key, Rect, TextEdit, Ui};
use crate::i18n::tr;

const EDITOR: u64 = id("note.editor");
const SIZE: f32 = 13.0;
const LINE_H: f32 = 18.6;
/// Autosave this long after the last keystroke (ms).
const SAVE_AFTER_MS: u64 = 600;

#[derive(Default)]
pub struct State {
    current: Option<String>,
    /// The text last loaded into / saved from the editor.
    saved_text: String,
    /// Unsaved edits; a background save runs SAVE_AFTER_MS after the last one (`gen` bumps per edit,
    /// `saved_gen` is the edit a save last covered).
    dirty: bool,
    gen: Arc<AtomicU64>,
    saved_gen: Arc<AtomicU64>,
    status: String,
    /// Everything selected (Ctrl+A): the next edit replaces it.
    all_selected: bool,
    /// Wrapped layout cache: (text, width) → rows as (start char, end char) ranges.
    layout_key: (String, f32),
    rows: Vec<(usize, usize)>,
    scroll: f32,
    /// Caret column to keep while moving up / down (x in DIP).
    goal_x: Option<f32>,
}

pub fn shown(_s: &mut State) {}

fn load_into(ui: &mut Ui, s: &mut State, note: &notes::Note) {
    flush(ui, s);
    // Don't leave empty notes behind when moving to another one.
    if let Some(prev) = s.current.clone() {
        if prev != note.id && notes::get(&prev).is_some_and(|n| n.is_blank()) && notes::all().len() > 1 {
            notes::delete(&prev);
        }
    }
    s.current = Some(note.id.clone());
    if settings::get().last_note_id.as_deref() != Some(note.id.as_str()) {
        let nid = note.id.clone();
        settings::update_quiet(|st| st.last_note_id = Some(nid));
    }
    let e = ui.st.edits.entry(EDITOR).or_default();
    e.text = note.text.replace("\r\n", "\n");
    e.caret = e.text.chars().count();
    s.saved_text = e.text.clone();
    s.status.clear();
    s.all_selected = false;
    s.scroll = f32::MAX; // scroll to the end
    s.goal_x = None;
}

/// Saves pending edits now.
fn flush(ui: &mut Ui, s: &mut State) {
    let Some(cur) = s.current.clone() else { return };
    if !std::mem::take(&mut s.dirty) {
        return;
    }
    s.gen.fetch_add(1, Ordering::SeqCst); // cancels the pending background save
    let text = ui.st.edits.get(&EDITOR).map(|e| e.text.clone()).unwrap_or_default();
    if text != s.saved_text {
        notes::update(&cur, &text);
        s.saved_text = text;
    }
    s.status = tr("Saved").into();
}

/// Saves the open note SAVE_AFTER_MS after this edit unless another edit comes first (runs even if the tool
/// is switched away meanwhile).
fn schedule_save(ui: &mut Ui, s: &mut State) {
    let Some(cur) = s.current.clone() else { return };
    s.dirty = true;
    let g = s.gen.fetch_add(1, Ordering::SeqCst) + 1;
    let text = ui.st.edits.get(&EDITOR).map(|e| e.text.clone()).unwrap_or_default();
    let (gen, saved) = (s.gen.clone(), s.saved_gen.clone());
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(SAVE_AFTER_MS));
        if gen.load(Ordering::SeqCst) == g {
            notes::update(&cur, &text);
            saved.store(g, Ordering::SeqCst);
            crate::bg::refresh();
        }
    });
}

fn new_note(ui: &mut Ui, s: &mut State) {
    let blank = ui.st.edits.get(&EDITOR).map_or(true, |e| e.text.trim().is_empty());
    if !blank || s.current.is_none() {
        flush(ui, s);
        let n = notes::add("");
        load_into(ui, s, &n);
    }
    ui.st.focus = EDITOR;
    ui.wants_keyboard = true;
}

fn delete_note(ui: &mut Ui, s: &mut State, nid: &str) {
    let list = notes::all();
    let Some(index) = list.iter().position(|n| n.id == nid) else { return };
    if s.current.as_deref() == Some(nid) {
        s.dirty = false;
        s.gen.fetch_add(1, Ordering::SeqCst);
        s.current = None;
    }
    notes::delete(nid);
    let mut list = notes::all();
    if list.is_empty() {
        list.push(notes::add(""));
    }
    if s.current.is_none() {
        let next = list[index.min(list.len() - 1)].clone();
        load_into(ui, s, &next);
    }
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, _go: &mut Option<&'static str>) {
    let t = ui.t;
    // Select the last note (or make one) the first time.
    if s.current.as_ref().map_or(true, |c| notes::get(c).is_none()) {
        let mut list = notes::all();
        if list.is_empty() {
            list.push(notes::add(""));
        }
        let last = settings::get().last_note_id;
        let pick = list.iter().find(|n| Some(&n.id) == last.as_ref()).unwrap_or(&list[0]).clone();
        s.dirty = false;
        load_into(ui, s, &pick);
    }
    // A background save covered the latest edit.
    if s.dirty && s.saved_gen.load(Ordering::SeqCst) == s.gen.load(Ordering::SeqCst) {
        s.dirty = false;
        s.saved_text = ui.st.edits.get(&EDITOR).map(|e| e.text.clone()).unwrap_or_default();
        s.status = tr("Saved").into();
    }

    let area = r.pad(18.0, 0.0, 18.0, 10.0);
    let (left, rest) = area.take_left(196.0, 12.0);
    note_list(ui, s, left);
    editor_card(ui, s, rest);

    // Grab handle (the panel height is fixed in this build; it is drawn for the look).
    ui.fill(Rect::new(r.cx() - 22.0, r.bottom() - 2.0, 44.0, 4.0), 2.0, t.track);
}

fn note_list(ui: &mut Ui, s: &mut State, r: Rect) {
    let t = ui.t;
    let list = notes::all();
    // Show the open note's live text (it may not be saved yet).
    let live = ui.st.edits.get(&EDITOR).map(|e| e.text.clone()).unwrap_or_default();

    let head = Rect::new(r.x, r.y, r.w, 30.0);
    ui.text(tr("NOTES"), Rect::new(head.x, head.y, 60.0, head.h), 11.0, BOLD, t.subtext, Align::Left);
    let count = list.iter().filter(|n| if Some(&n.id) == s.current.as_ref() { !live.trim().is_empty() } else { !n.is_blank() }).count();
    if count > 0 {
        let x = head.x + ui.measure(tr("NOTES"), 11.0, BOLD) + 6.0;
        ui.text(&count.to_string(), Rect::new(x, head.y, 40.0, head.h), 11.0, REGULAR, t.subtext, Align::Left);
    }
    let plus = Rect::new(head.right() - 26.0, head.cy() - 13.0, 26.0, 26.0);
    ui.fill(plus, 8.0, if ui.hovered(plus) { t.hover } else { t.accent_soft });
    ui.icon("Plus", plus.cx(), plus.cy(), 13.0, t.accent);
    if ui.clicked(plus) {
        new_note(ui, s);
    }

    let body = Rect::new(r.x, head.bottom() + 6.0, r.w, r.bottom() - head.bottom() - 6.0);
    let row_h = 58.0;
    let mut select: Option<notes::Note> = None;
    let mut delete: Option<String> = None;
    let cur = s.current.clone();
    ui.scroll(id("note.list"), body, list.len() as f32 * (row_h + 5.0), |ui, top| {
        for (i, n) in list.iter().enumerate() {
            let row = Rect::new(body.x, top + i as f32 * (row_h + 5.0), body.w - 4.0, row_h);
            if row.bottom() < body.y || row.y > body.bottom() {
                continue;
            }
            let selected = cur.as_ref() == Some(&n.id);
            let hov = ui.hovered(row);
            ui.fill(row, 10.0, if selected || hov { t.surface2 } else { t.surface });
            if selected {
                ui.fill(Rect::new(row.x, row.y + 8.0, 3.0, row.h - 16.0), 1.5, t.accent);
            }
            let note = if selected { notes::Note { text: live.clone(), ..n.clone() } } else { n.clone() };
            let tx = row.x + 11.0;
            let tw = row.w - 11.0 - 4.0 - if hov { 24.0 } else { 0.0 };
            ui.text(note.title(), Rect::new(tx, row.y + 5.0, tw, 17.0), 12.0, SEMIBOLD, t.text, Align::Left);
            ui.text(&note.preview(), Rect::new(tx, row.y + 21.5, tw, 15.0), 11.0, REGULAR, t.subtext, Align::Left);
            ui.text(&n.when(), Rect::new(tx, row.y + 36.5, tw, 13.5), 10.0, REGULAR, with_alpha(t.subtext, 0.75), Align::Left);
            if hov {
                let del = Rect::new(row.right() - 4.0 - 22.0, row.y + 7.0, 22.0, 22.0);
                if ui.icon_button(del, "Trash", 11.0, t.subtext) {
                    delete = Some(n.id.clone());
                }
            }
            if ui.clicked(row) {
                select = Some(n.clone());
            }
        }
    });
    if let Some(nid) = delete {
        delete_note(ui, s, &nid);
    } else if let Some(n) = select {
        if s.current.as_ref() != Some(&n.id) {
            load_into(ui, s, &n);
        }
        ui.st.focus = EDITOR;
        ui.wants_keyboard = true;
    }
}

fn editor_card(ui: &mut Ui, s: &mut State, r: Rect) {
    let t = ui.t;
    ui.fill(r, 14.0, t.surface);
    let (foot, body) = r.take_bottom(28.0 + 6.0, 0.0);
    let foot = foot.pad(16.0, 0.0, 8.0, 6.0);

    let text_box = body.pad(18.0, 14.0, 16.0, 6.0);
    if editor(ui, s, text_box, body) {
        s.status = tr("Editing…").into();
        schedule_save(ui, s);
    }

    // Footer: a hairline, status · words, copy and delete.
    ui.fill(Rect::new(foot.x, foot.y - 2.0, foot.w - 8.0, 1.0), 0.0, t.stroke);
    let text = ui.st.edits.get(&EDITOR).map(|e| e.text.clone()).unwrap_or_default();
    let words = text.split_whitespace().count();
    let status = if s.status.is_empty() {
        let when = s.current.as_ref().and_then(|c| notes::get(c)).map(|n| n.when()).unwrap_or_default();
        crate::i18n::tf!("Edited {when}", "Diubah {when}")
    } else {
        s.status.clone()
    };
    let sw = ui.measure(&status, 11.0, REGULAR);
    ui.text(&status, Rect::new(foot.x, foot.y, sw + 2.0, foot.h), 11.0, REGULAR, t.subtext, Align::Left);
    let dot_x = foot.x + sw + 6.0;
    ui.text("·", Rect::new(dot_x, foot.y, 8.0, foot.h), 11.0, SEMIBOLD, t.subtext, Align::Left);
    let wl = if words == 1 { tr("1 word").to_string() } else { crate::i18n::tf!("{words} words", "{words} kata") };
    ui.text(&wl, Rect::new(dot_x + ui.measure("·", 11.0, SEMIBOLD) + 6.0, foot.y, 120.0, foot.h), 11.0, REGULAR, t.subtext, Align::Left);
    let del = Rect::new(foot.right() - 26.0, foot.cy() - 13.0, 26.0, 26.0);
    let copy = Rect::new(del.x - 2.0 - 26.0, del.y, 26.0, 26.0);
    if ui.icon_button(copy, "Copy", 13.0, t.subtext) {
        crate::clipboard::set_text(&text.replace('\n', "\r\n"));
    }
    if ui.icon_button(del, "Trash", 13.0, t.subtext) {
        if let Some(cur) = s.current.clone() {
            delete_note(ui, s, &cur);
        }
    }
}

// ------------------------------------------------------------------ multi-line editor

fn byte_at(text: &str, ch: usize) -> usize {
    text.char_indices().nth(ch).map_or(text.len(), |(b, _)| b)
}

/// Wraps `text` to `width`: rows as char ranges (end exclusive, without the newline). Breaks after spaces,
/// or mid-word when one word is wider than the row.
fn layout(ui: &Ui, text: &str, width: f32) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut rows = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i <= chars.len() {
        // A hard line from `start` to the next newline.
        let end = (i..chars.len()).find(|&k| chars[k] == '\n').unwrap_or(chars.len());
        let mut s = start;
        loop {
            let line: String = chars[s..end].iter().collect();
            if ui.measure(&line, SIZE, REGULAR) <= width || end - s <= 1 {
                rows.push((s, end));
                break;
            }
            // Longest prefix that fits, by binary search on measured width.
            let (mut lo, mut hi) = (s + 1, end);
            while lo < hi {
                let mid = (lo + hi + 1) / 2;
                let part: String = chars[s..mid].iter().collect();
                if ui.measure(&part, SIZE, REGULAR) <= width {
                    lo = mid;
                } else {
                    hi = mid - 1;
                }
            }
            let mut cut = lo;
            // Prefer breaking after the last space in the fitting part.
            if let Some(sp) = (s + 1..=lo).rev().find(|&k| chars[k - 1] == ' ') {
                cut = sp;
            }
            rows.push((s, cut));
            s = cut;
        }
        start = end + 1;
        i = start;
        if end == chars.len() {
            break;
        }
    }
    if rows.is_empty() {
        rows.push((0, 0));
    }
    rows
}

/// The visual row holding `caret` (a caret at a soft wrap belongs to the next row).
fn row_of(rows: &[(usize, usize)], caret: usize) -> usize {
    for (i, &(a, b)) in rows.iter().enumerate() {
        let next_starts_here = rows.get(i + 1).is_some_and(|n| n.0 == b);
        if caret >= a && (caret < b || (caret == b && !next_starts_here)) {
            return i;
        }
    }
    rows.len() - 1
}

/// The char index in row `row` closest to x offset `x`.
fn index_at_x(ui: &Ui, text: &[char], row: (usize, usize), x: f32) -> usize {
    let mut best = row.0;
    let mut best_d = f32::MAX;
    for k in row.0..=row.1 {
        let s: String = text[row.0..k].iter().collect();
        let d = (ui.measure(&s, SIZE, REGULAR) - x).abs();
        if d < best_d {
            best_d = d;
            best = k;
        }
    }
    best
}

/// Draws and edits the note. Returns true when the text changed.
fn editor(ui: &mut Ui, s: &mut State, r: Rect, hit_area: Rect) -> bool {
    let t = ui.t;
    let mut changed = false;

    // Focus: click inside to type, outside to stop.
    let focused_before = ui.st.focus == EDITOR;
    let click = ui.clicked(hit_area);
    if click {
        ui.st.focus = EDITOR;
        ui.wants_keyboard = true;
        s.all_selected = false;
    } else if ui.input.pressed && !ui.input.consumed && !hit_area.contains(ui.mouse()) && focused_before {
        // A press elsewhere that no widget took (a widget like + or a note card keeps the editor focused, as in C#).
        ui.st.focus = 0;
        s.all_selected = false;
    }
    let focused = ui.st.focus == EDITOR;

    let mut e: TextEdit = ui.st.edits.get(&EDITOR).cloned().unwrap_or_default();
    // Layout before the keys (arrows need rows).
    if s.layout_key.0 != e.text || s.layout_key.1 != r.w {
        s.rows = layout(ui, &e.text, r.w);
        s.layout_key = (e.text.clone(), r.w);
    }

    if focused {
        let replace_all = |e: &mut TextEdit, s: &mut State| {
            if s.all_selected {
                e.text.clear();
                e.caret = 0;
                s.all_selected = false;
            }
        };
        for ch in ui.input.chars.clone().chars() {
            if ch.is_control() {
                continue;
            }
            replace_all(&mut e, s);
            let b = byte_at(&e.text, e.caret);
            e.text.insert(b, ch);
            e.caret += 1;
            changed = true;
            s.goal_x = None;
        }
        for key in ui.input.keys.clone() {
            let len = e.text.chars().count();
            let mut vertical = false;
            match key {
                Key::Enter | Key::Tab => {
                    replace_all(&mut e, s);
                    let b = byte_at(&e.text, e.caret);
                    e.text.insert(b, if key == Key::Enter { '\n' } else { '\t' });
                    e.caret += 1;
                    changed = true;
                }
                Key::Backspace => {
                    if s.all_selected {
                        replace_all(&mut e, s);
                        changed = true;
                    } else if e.caret > 0 {
                        let b = byte_at(&e.text, e.caret - 1);
                        e.text.remove(b);
                        e.caret -= 1;
                        changed = true;
                    }
                }
                Key::Delete => {
                    if s.all_selected {
                        replace_all(&mut e, s);
                        changed = true;
                    } else if e.caret < len {
                        let b = byte_at(&e.text, e.caret);
                        e.text.remove(b);
                        changed = true;
                    }
                }
                Key::Left => e.caret = e.caret.saturating_sub(1),
                Key::Right => e.caret = (e.caret + 1).min(len),
                Key::Home | Key::End => {
                    let rows = layout(ui, &e.text, r.w);
                    let row = rows[row_of(&rows, e.caret)];
                    e.caret = if key == Key::Home { row.0 } else { row.1 };
                }
                Key::Up | Key::Down => {
                    vertical = true;
                    let rows = layout(ui, &e.text, r.w);
                    let chars: Vec<char> = e.text.chars().collect();
                    let ri = row_of(&rows, e.caret);
                    let row = rows[ri];
                    let x = s.goal_x.unwrap_or_else(|| {
                        let before: String = chars[row.0..e.caret.min(row.1)].iter().collect();
                        ui.measure(&before, SIZE, REGULAR)
                    });
                    s.goal_x = Some(x);
                    if key == Key::Up {
                        e.caret = if ri == 0 { 0 } else { index_at_x(ui, &chars, rows[ri - 1], x) };
                    } else {
                        e.caret = if ri + 1 >= rows.len() { len } else { index_at_x(ui, &chars, rows[ri + 1], x) };
                    }
                }
                Key::SelectAll => s.all_selected = true,
                Key::Copy | Key::Cut => {
                    if s.all_selected {
                        crate::clipboard::set_text(&e.text.replace('\n', "\r\n"));
                        if key == Key::Cut {
                            replace_all(&mut e, s);
                            changed = true;
                        }
                    }
                }
                Key::Paste => {
                    if let Some(p) = crate::clipboard::get_text() {
                        replace_all(&mut e, s);
                        let p = p.replace("\r\n", "\n").replace('\r', "\n");
                        let b = byte_at(&e.text, e.caret);
                        e.text.insert_str(b, &p);
                        e.caret += p.chars().count();
                        changed = true;
                    }
                }
                Key::Escape => {
                    ui.st.focus = 0;
                    s.all_selected = false;
                }
                // Tab switching is the shell's (it takes these keys before the tool draws).
                Key::TabPrev | Key::TabNext | Key::TabAt(_) => {}
            }
            if !vertical {
                s.goal_x = None;
            }
            if !matches!(key, Key::SelectAll) && key != Key::Copy {
                s.all_selected = false;
            }
        }
        if changed || !ui.input.keys.is_empty() {
            s.rows = layout(ui, &e.text, r.w);
            s.layout_key = (e.text.clone(), r.w);
        }
    }

    let chars: Vec<char> = e.text.chars().collect();
    let content_h = s.rows.len() as f32 * LINE_H;
    let max = (content_h - r.h).max(0.0);
    let caret_row = row_of(&s.rows, e.caret.min(chars.len()));
    // Keep the caret in view after typing or moving.
    if focused && (changed || !ui.input.keys.is_empty() || !ui.input.chars.is_empty()) {
        let cy = caret_row as f32 * LINE_H;
        if cy < s.scroll {
            s.scroll = cy;
        } else if cy + LINE_H > s.scroll + r.h {
            s.scroll = cy + LINE_H - r.h;
        }
    }
    if ui.input.wheel != 0.0 && hit_area.contains(ui.mouse()) {
        s.scroll -= ui.input.wheel * 48.0;
        ui.input.wheel = 0.0;
    }
    s.scroll = s.scroll.clamp(0.0, max);

    // Click: put the caret under the pointer.
    if click {
        let (mx, my) = ui.mouse();
        let ri = (((my - r.y + s.scroll) / LINE_H).floor().max(0.0) as usize).min(s.rows.len() - 1);
        e.caret = index_at_x(ui, &chars, s.rows[ri], mx - r.x);
        s.goal_x = None;
    }

    // Draw.
    let clip = Rect::new(r.x - 4.0, r.y - 2.0, r.w + 8.0, r.h + 4.0);
    ui.push_clip(clip);
    if e.text.is_empty() {
        ui.wrap(tr("Jot something down. The first line is the title; it saves as you type."), Rect::new(r.x, r.y, r.w, r.h), SIZE, REGULAR, t.subtext);
    } else {
        for (i, &(a, b)) in s.rows.iter().enumerate() {
            let y = r.y + i as f32 * LINE_H - s.scroll;
            if y + LINE_H < r.y || y > r.bottom() {
                continue;
            }
            let line: String = chars[a..b].iter().collect();
            if s.all_selected && focused {
                let w = ui.measure(&line, SIZE, REGULAR).max(4.0);
                ui.fill(Rect::new(r.x, y, w, LINE_H), 2.0, with_alpha(t.accent, 0.3));
            }
            ui.text(&line, Rect::new(r.x, y, r.w + 4.0, LINE_H), SIZE, REGULAR, t.text, Align::Left);
        }
    }
    if focused {
        ui.animate = true;
        if ui.st.caret_on {
            let row = s.rows[caret_row];
            let before: String = chars[row.0..e.caret.clamp(row.0, row.1)].iter().collect();
            let x = r.x + ui.measure(&before, SIZE, REGULAR);
            let y = r.y + caret_row as f32 * LINE_H - s.scroll;
            ui.g.line(x + 0.5, y + 2.0, x + 0.5, y + LINE_H - 2.0, ui.c(t.text), 1.2);
        }
    }
    ui.pop_clip();

    ui.st.edits.insert(EDITOR, e);
    changed
}
