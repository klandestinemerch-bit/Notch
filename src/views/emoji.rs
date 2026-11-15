//! Emoji: a search field, category chips (Recent + the nine Unicode groups) and a grid. Clicking an emoji
//! copies it and shows "Copied: name"; recent picks are kept in `emoji-recent.json` (a JSON list of strings, the
//! same file as the .NET build, newest first, at most 48). Glyphs come from Segoe UI Emoji through font
//! fallback and draw monochrome.

use std::sync::OnceLock;
use std::time::Instant;

use crate::gfx::{Align, REGULAR, SEMIBOLD};
use crate::services::store;
use crate::ui::{id, Rect, Ui};
use crate::i18n::tr;

const PER_ROW: usize = 16;
const RECENT_FILE: &str = "emoji-recent.json";
const RECENT: &str = "Recent";

const GROUPS: [(&str, &str, &str); 9] = [
    ("Smileys & Emotion", "Smileys", "FaceSmile"),
    ("People & Body", "People", "User"),
    ("Animals & Nature", "Nature", "Globe"),
    ("Food & Drink", "Food", "Cake"),
    ("Travel & Places", "Travel", "Map"),
    ("Activities", "Activities", "Trophy"),
    ("Objects", "Objects", "LightBulb"),
    ("Symbols", "Symbols", "Heart"),
    ("Flags", "Flags", "Flag"),
];

struct Emoji {
    ch: &'static str,
    name: &'static str,
    group: &'static str,
}

/// Unicode's emoji-test.txt, reduced at build time to "group\tchar\tname" (fully-qualified, no skin tones,
/// no ZWJ sequences, Emoji ≤ 15.0 so Segoe UI Emoji has them).
fn all() -> &'static [Emoji] {
    static ALL: OnceLock<Vec<Emoji>> = OnceLock::new();
    ALL.get_or_init(|| {
        include_str!("emoji.tsv")
            .lines()
            .filter_map(|l| {
                let mut p = l.splitn(3, '\t');
                Some(Emoji { group: p.next()?, ch: p.next()?, name: p.next()? })
            })
            .collect()
    })
}

pub struct State {
    group: &'static str,
    recent: Vec<String>,
    loaded: bool,
    copied: Option<(String, Instant)>,
    last_query: String,
    /// Bumped when the recents change (invalidates `items`).
    recent_version: u32,
    /// The shown emoji (char, name) for (query, group, recent_version).
    items: Option<((String, &'static str, u32), std::rc::Rc<Vec<(String, String)>>)>,
}

impl Default for State {
    fn default() -> Self {
        Self { group: GROUPS[0].0, recent: Vec::new(), loaded: false, copied: None, last_query: String::new(), recent_version: 0, items: None }
    }
}

pub fn shown(s: &mut State) {
    if !s.loaded {
        s.recent = store::load::<Vec<String>>(RECENT_FILE);
        s.loaded = true;
        if !s.recent.is_empty() {
            s.group = RECENT;
        }
    }
}

const FIELD: &str = "emoji.search";

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, _go: &mut Option<&'static str>) {
    let t = ui.t;
    if !s.loaded {
        shown(s);
    }
    // Search row: the field fills the width; "Copied: name" (at most 160 wide) sits on its right.
    let (top, rest) = r.take_top(34.0, 6.0);
    let status_w = s.copied.as_ref().map_or(0.0, |(n, _)| (ui.measure(&crate::i18n::tf!("Copied: {n}", "Disalin: {n}"), 12.0, SEMIBOLD) + 2.0).min(160.0));
    let field = Rect::new(top.x, top.y, top.w - if status_w > 0.0 { status_w + 12.0 } else { 0.0 }, top.h);
    let fid = id(FIELD);
    if ui.st.focus == fid {
        ui.g.stroke_round(field.inset(-1.5, -1.5).d2d(), 10.5, ui.c(t.accent_soft), 3.0);
    }
    ui.fill(field, 9.0, t.surface2);
    let border = if ui.st.focus == fid { t.accent } else if ui.hovered(field) { t.track } else { t.stroke };
    ui.g.stroke_round(field.inset(0.5, 0.5).d2d(), 8.5, ui.c(border), 1.0);
    ui.icon("Magnifier", field.x + 18.5, field.cy(), 15.0, t.subtext);
    let _ = ui.text_field(fid, field.pad(36.0, 0.0, 34.0, 0.0), tr("Search emoji…"), 13.0);
    let query = ui.field_text(fid).trim().to_lowercase();
    if !query.is_empty() && ui.icon_button(Rect::new(field.right() - 28.0, field.cy() - 11.0, 22.0, 22.0), "XMark", 12.0, t.subtext) {
        ui.set_field_text(fid, "");
    }
    if query != s.last_query {
        ui.st.scroll.insert(id("emoji.grid"), 0.0);
        s.last_query = query.clone();
    }
    if let Some((name, _)) = &s.copied {
        let sr = Rect::new(field.right() + 10.0, top.y, status_w, top.h);
        ui.text(&crate::i18n::tf!("Copied: {name}", "Disalin: {name}"), sr, 12.0, SEMIBOLD, t.subtext, Align::Left);
    }

    // Category chips: 34×24, 4 apart; the selected one in the accent colour.
    let (chips, grid) = rest.take_top(24.0, 6.0);
    let mut x = chips.x;
    let mut entries: Vec<(&'static str, &'static str)> = vec![(RECENT, "Clock")];
    entries.extend(GROUPS.iter().map(|(g, _, i)| (*g, *i)));
    for (g, icon) in entries {
        let c = Rect::new(x, chips.y, 34.0, chips.h);
        let on = s.group == g && query.is_empty();
        let hov = ui.hovered(c);
        ui.fill(c, 10.0, if on { t.accent } else { t.surface2 });
        ui.icon(icon, c.cx(), c.cy(), 14.0, if on { t.on_accent } else if hov { t.text } else { t.subtext });
        if ui.clicked(c) {
            s.group = g;
            ui.set_field_text(fid, "");
            ui.st.scroll.insert(id("emoji.grid"), 0.0);
        }
        x += 38.0;
    }

    // The emoji to show (rebuilt only when the search, group or recents change).
    let key = (query.clone(), s.group, s.recent_version);
    if s.items.as_ref().map_or(true, |(k, _)| *k != key) {
        let items: Vec<(String, String)> = if !query.is_empty() {
            all().iter().filter(|e| e.name.contains(&query)).map(|e| (e.ch.to_string(), e.name.to_string())).collect()
        } else if s.group == RECENT {
            s.recent.iter().map(|c| (c.clone(), all().iter().find(|e| e.ch == c).map_or(c.clone(), |e| e.name.to_string()))).collect()
        } else {
            all().iter().filter(|e| e.group == s.group).map(|e| (e.ch.to_string(), e.name.to_string())).collect()
        };
        s.items = Some((key, std::rc::Rc::new(items)));
    }
    let items = s.items.as_ref().unwrap().1.clone();
    if items.is_empty() {
        if !query.is_empty() || s.group == RECENT {
            let msg = if query.is_empty() { tr("Emoji you copy show up here") } else { tr("No emoji found") };
            ui.text(msg, Rect::new(grid.x, grid.y + 10.0, grid.w, 20.0), 12.0, REGULAR, t.subtext, Align::Center);
        }
        return;
    }

    // 16 buttons of 38×37 per row, left-aligned (the list reaches 4 px past the content on each side).
    let (cell_w, cell_h) = (38.0, 37.0);
    let gx = grid.x - 4.0;
    let rows = items.len().div_ceil(PER_ROW);
    let mut picked: Option<(String, String)> = None;
    ui.scroll(id("emoji.grid"), Rect::new(gx, grid.y, grid.w + 8.0, grid.h), rows as f32 * cell_h, |ui, top| {
        let t = ui.t;
        let first = (((grid.y - top) / cell_h).floor().max(0.0) as usize) * PER_ROW;
        for (i, (ch, name)) in items.iter().enumerate().skip(first) {
            let (row, col) = (i / PER_ROW, i % PER_ROW);
            let cell = Rect::new(gx + col as f32 * cell_w, top + row as f32 * cell_h, cell_w, cell_h);
            if cell.y > grid.bottom() {
                break;
            }
            if ui.hit(cell, 8.0) {
                picked = Some((ch.clone(), name.clone()));
            }
            ui.text(ch, cell, 21.0, REGULAR, t.text, Align::Center);
        }
    });
    if let Some((ch, name)) = picked {
        crate::clipboard::set_text(&ch);
        s.copied = Some((name, Instant::now()));
        s.recent.retain(|c| c != &ch);
        s.recent.insert(0, ch);
        s.recent.truncate(PER_ROW * 3);
        s.recent_version += 1;
        store::save(RECENT_FILE, &s.recent);
    }
}
