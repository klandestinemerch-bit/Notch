//! Clipboard history as a horizontal strip of cards with search and type filters. Clicking a card copies it
//! again; hovering shows pin and delete; "…" clears everything that isn't pinned. The wheel glides the strip;
//! resting the pointer near either edge scrolls that way (as the .NET EdgeScroll).
//!
//! Recording runs in `services::cliphistory`; it starts when the views are created (app start).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::bg;
use crate::gfx::{color, with_alpha, Align, MEDIUM, REGULAR, SEMIBOLD};
use crate::services::cliphistory::{self, Clip, Kind};
use crate::services::shelf;
use crate::ui::{id, Rect, Ui};
use crate::i18n::tr;

const SEARCH: &str = "clipboard.search";
const CARD_W: f32 = 148.0;
const CARD_H: f32 = 150.0;
const CARD_GAP: f32 = 13.0;
const FILTERS: [(&str, Option<Kind>); 5] =
    [("All", None), ("Text", Some(Kind::Text)), ("Links", Some(Kind::Link)), ("Images", Some(Kind::Image)), ("Files", Some(Kind::Files))];

// EdgeScroll (Shell/EdgeScroll.cs).
const ZONE: f32 = 56.0;
const MAX_SPEED: f32 = 900.0;
const SPEED_EASE: f32 = 0.12;
const FOLLOW_EASE: f32 = 0.09;
const WHEEL_STEP: f32 = 0.8 * 120.0;

pub struct State {
    filter: Option<Kind>,
    /// Strip scroll: where it is and where it's gliding to; edge-scroll speed (eased).
    offset: f32,
    target: f32,
    speed: f32,
    last_frame: Option<Instant>,
    status: Option<Instant>,
    /// "…" menu open at this point.
    menu: Option<(f32, f32)>,
    thumbs: Arc<Mutex<HashMap<String, Option<Vec<u8>>>>>,
    loading: Arc<Mutex<Vec<String>>>,
}

impl Default for State {
    fn default() -> Self {
        // The views are created once at startup: begin recording the clipboard then.
        cliphistory::start();
        Self {
            filter: None,
            offset: 0.0,
            target: 0.0,
            speed: 0.0,
            last_frame: None,
            status: None,
            menu: None,
            thumbs: Default::default(),
            loading: Default::default(),
        }
    }
}

pub fn shown(s: &mut State) {
    cliphistory::start();
    s.offset = 0.0;
    s.target = 0.0;
    s.speed = 0.0;
    s.menu = None;
}

fn matches(c: &Clip, filter: Option<Kind>, q: &str) -> bool {
    if filter.is_some_and(|k| c.kind() != k) {
        return false;
    }
    q.is_empty() || c.preview().to_lowercase().contains(q) || c.text.as_deref().is_some_and(|t| t.to_lowercase().contains(q))
}

fn lh(size: f32) -> f32 {
    (size * 1.33).round()
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, _go: &mut Option<&'static str>) {
    let t = ui.t;
    let good = color(0x34C759, 1.0);

    // "…" menu: one item, "Clear unpinned" (ContextMenu at the pointer).
    let (top, strip) = r.take_top(34.0, 14.0);
    let menu_btn = Rect::new(top.right() - 30.0, top.cy() - 15.0, 30.0, 30.0);
    let menu_r = s.menu.map(|(x, y)| {
        let w = (ui.measure(tr("Clear unpinned"), 13.0, REGULAR).ceil() + 20.0).max(130.0) + 8.0;
        Rect::new(x.min(r.right() - w), y, w, 27.0 + 8.0)
    });
    if let Some(m) = menu_r {
        if (ui.input.pressed || ui.input.right_pressed) && !ui.input.consumed {
            if m.contains(ui.mouse()) && ui.input.pressed {
                bg::spawn(cliphistory::clear);
            }
            s.menu = None;
            ui.input.consumed = true;
        }
    }

    // Search (203) + filter chips + status + menu.
    let field = Rect::new(top.x, top.y, 203.0, top.h);
    let sid = id(SEARCH);
    field_chrome(ui, field, ui.st.focus == sid);
    ui.icon("Magnifier", field.x + 18.5, field.cy(), 15.0, t.subtext);
    let has_text = !ui.field_text(sid).is_empty();
    let _ = ui.text_field(sid, field.pad(36.0, 0.0, if has_text { 30.0 } else { 10.0 }, 0.0), tr("Search"), 13.0);
    let query = ui.field_text(sid).trim().to_lowercase();
    if has_text && ui.icon_button(Rect::new(field.right() - 28.0, field.cy() - 11.0, 22.0, 22.0), "XMark", 12.0, t.subtext) {
        ui.set_field_text(sid, "");
    }
    let mut x = field.right() + 12.0;
    let chip_h = lh(12.0) + 8.0;
    for (label, kind) in FILTERS {
        let w = ui.measure(label, 12.0, MEDIUM).ceil() + 24.0;
        let chip = Rect::new(x, top.cy() - chip_h / 2.0, w, chip_h);
        let selected = s.filter == kind;
        let hov = ui.hovered(chip) && s.menu.is_none();
        ui.fill(chip, 10.0, if selected { t.accent } else { t.surface2 });
        let fg = if selected { t.on_accent } else if hov { t.text } else { t.subtext };
        ui.text(label, chip, 12.0, MEDIUM, fg, Align::Center);
        if ui.clicked(chip) && s.filter != kind {
            s.filter = kind;
            s.target = 0.0;
        }
        x += w + 4.0;
    }
    if ui.icon_button(menu_btn, "Ellipsis", 17.0, t.subtext) {
        let (mx, my) = ui.mouse();
        s.menu = Some((mx, my));
    }
    if let Some(at) = s.status {
        if at.elapsed().as_secs_f32() < 1.5 {
            ui.text(tr("Copied"), Rect::new(menu_btn.x - 8.0 - 100.0, top.y, 100.0, top.h), 12.0, SEMIBOLD, good, Align::Right);
            ui.animate = true;
        } else {
            s.status = None;
        }
    }

    // Cards.
    let items: Vec<Clip> = cliphistory::all().into_iter().filter(|c| matches(c, s.filter, &query)).collect();
    if items.is_empty() {
        ui.text(tr("Copy something — it will show up here."), strip, 12.0, SEMIBOLD, t.subtext, Align::Center);
        draw_menu(ui, menu_r);
        return;
    }
    let content_w = items.len() as f32 * (CARD_W + CARD_GAP);
    let max = (content_w - strip.w).max(0.0);

    // Motion on the frame clock: wheel and edge zones move the target; the view follows it.
    let now = Instant::now();
    let dt = s.last_frame.map_or(1.0 / 60.0, |f| (now - f).as_secs_f32()).clamp(0.0, 0.1);
    s.last_frame = Some(now);
    let (mx, my) = ui.mouse();
    if ui.input.wheel != 0.0 && strip.contains((mx, my)) {
        s.target -= ui.input.wheel * WHEEL_STEP;
        ui.input.wheel = 0.0;
    }
    let mut edge = 0.0;
    if strip.contains((mx, my)) && s.menu.is_none() && max > 0.0 {
        let zone = ZONE.min(strip.w * 0.15);
        let x = mx - strip.x;
        if x > strip.w - zone {
            edge = MAX_SPEED * ((x - (strip.w - zone)) / zone).powi(2);
        } else if x < zone {
            edge = -MAX_SPEED * ((zone - x) / zone).powi(2);
        }
    }
    if (edge > 0.0 && s.target >= max) || (edge < 0.0 && s.target <= 0.0) {
        edge = 0.0;
    }
    s.speed += (edge - s.speed) * (1.0 - (-dt / SPEED_EASE).exp());
    s.target = (s.target + s.speed * dt).clamp(0.0, max);
    s.offset += (s.target - s.offset) * (1.0 - (-dt / FOLLOW_EASE).exp());
    if (s.target - s.offset).abs() < 0.3 && s.speed.abs() < 0.5 && edge == 0.0 {
        s.offset = s.target;
        s.speed = 0.0;
        s.last_frame = None;
    } else {
        ui.animate = true;
    }
    s.offset = s.offset.clamp(0.0, max);

    let card_y = strip.y + ((strip.h - CARD_H) / 2.0).max(0.0).floor();
    let card_h = CARD_H.min(strip.h);
    let visible_paths: Vec<String> = items
        .iter()
        .enumerate()
        .filter(|(i, c)| {
            let cx = strip.x + *i as f32 * (CARD_W + CARD_GAP) - s.offset;
            c.kind() == Kind::Image && cx + CARD_W > strip.x && cx < strip.right()
        })
        .filter_map(|(_, c)| c.image_path.clone())
        .collect();
    load_thumbs(s, &visible_paths);

    let mut copy: Option<String> = None;
    let mut pin: Option<String> = None;
    let mut delete: Option<String> = None;
    ui.push_clip(strip);
    for (i, c) in items.iter().enumerate() {
        let card = Rect::new(strip.x + i as f32 * (CARD_W + CARD_GAP) - s.offset, card_y, CARD_W, card_h);
        if card.right() < strip.x || card.x > strip.right() {
            continue;
        }
        let hov = ui.hovered(card) && s.menu.is_none();
        ui.fill(card, 16.0, if hov { t.surface2 } else { t.surface });
        let inner = card.pad(11.0, 10.0, 11.0, 10.0);
        let head = Rect::new(inner.x, inner.y, inner.w, 20.0);
        ui.icon(c.icon(), head.x + 7.5, head.cy(), 15.0, t.subtext);
        let tt = c.time_text();
        let tw = ui.measure(&tt, 13.0, SEMIBOLD);
        ui.text(&tt, Rect::new(head.x + 22.0, head.y, tw + 2.0, head.h), 13.0, SEMIBOLD, t.subtext, Align::Left);
        if c.pinned {
            ui.icon("Pin", head.x + 22.0 + tw + 4.0 + 5.5, head.cy(), 11.0, t.accent);
        }
        if hov {
            let del = Rect::new(head.right() - 22.0, head.cy() - 11.0, 22.0, 22.0);
            let pn = Rect::new(del.x - 22.0, del.y, 22.0, 22.0);
            if ui.icon_button(pn, "Pin", 11.0, t.subtext) {
                pin = Some(c.id.clone());
            }
            if ui.icon_button(del, "XMark", 11.0, t.subtext) {
                delete = Some(c.id.clone());
            }
        }
        let body = Rect::new(inner.x, head.bottom() + 8.0, inner.w, inner.bottom() - head.bottom() - 8.0);
        match c.kind() {
            Kind::Link => {
                let (host, path) = c.link_parts();
                ui.text(&host, Rect::new(body.x, body.y, body.w, lh(13.0)), 13.0, SEMIBOLD, t.text, Align::Left);
                lines(ui, &path, Rect::new(body.x, body.y + lh(13.0) + 2.0, body.w, 64.0), 12.0, REGULAR, lh(12.0), t.subtext);
            }
            Kind::Image => {
                let k = id("clip.thumb") ^ crate::ui::id(&c.id).rotate_left(5);
                // Decoded once; the bytes are only copied out while the renderer doesn't have the image.
                let shown = ui.g.has_image(k)
                    || c.image_path.as_ref().and_then(|p| s.thumbs.lock().unwrap().get(p).cloned().flatten()).is_some_and(|b| ui.g.load_image(k, &b));
                if shown {
                    ui.g.image(k, Rect::new(body.x, body.y, body.w, body.h.min(98.0)).d2d(), 6.0, ui.alpha);
                } else {
                    lines(ui, &c.preview(), body, 13.0, REGULAR, 17.0, t.text);
                }
            }
            _ => lines(ui, &c.preview(), body, 13.0, REGULAR, 17.0, t.text),
        }
        if ui.clicked(card) {
            copy = Some(c.id.clone());
        }
    }
    ui.pop_clip();

    // "More this way" hints at the edges (visual only).
    let pill = t.pill;
    if s.offset > 1.0 {
        edge_hint(ui, strip, true, pill);
    }
    if s.offset < max - 1.0 {
        edge_hint(ui, strip, false, pill);
    }

    if let Some(i) = pin {
        cliphistory::toggle_pin(&i);
    } else if let Some(i) = delete {
        cliphistory::remove(&i);
    } else if let Some(i) = copy {
        s.status = Some(Instant::now());
        bg::spawn(move || {
            cliphistory::recopy(&i);
        });
    }
    draw_menu(ui, menu_r);
}

/// Wrapped text with a fixed line pitch, as many lines as fit; the last one ends in "…" when cut.
fn lines(ui: &mut Ui, s: &str, r: Rect, size: f32, weight: u16, pitch: f32, c: crate::gfx::Color) {
    let max = ((r.h + 0.5) / pitch).floor().max(1.0) as usize;
    let all = wrap(ui, s, size, weight, r.w);
    for (i, l) in all.iter().take(max).enumerate() {
        let cut = i + 1 == max && all.len() > max;
        let rect = Rect::new(r.x, r.y + i as f32 * pitch, r.w, pitch);
        if cut {
            // Character ellipsis: the plain text renderer trims what doesn't fit.
            let more = format!("{l} {}", all[max]);
            ui.text(&more, rect, size, weight, c, Align::Left);
        } else {
            ui.text(l, rect, size, weight, c, Align::Left);
        }
    }
}

thread_local! {
    static WRAPS: std::cell::RefCell<HashMap<(String, u32, u32), Vec<String>>> = Default::default();
}

/// Word wrap (long words break by characters) with the real metrics; cached per text and width.
fn wrap(ui: &Ui, s: &str, size: f32, weight: u16, w: f32) -> Vec<String> {
    let key = (s.chars().take(600).collect::<String>(), (size * 10.0) as u32 ^ ((weight as u32) << 16), w.round() as u32);
    if let Some(v) = WRAPS.with(|c| c.borrow().get(&key).cloned()) {
        return v;
    }
    let mut out: Vec<String> = Vec::new();
    for para in key.0.split('\n') {
        let para = para.trim_end_matches('\r');
        let mut line = String::new();
        for word in para.split(' ') {
            let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if ui.measure(&candidate, size, weight) <= w {
                line = candidate;
                continue;
            }
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            // A word wider than the line breaks by characters.
            let mut rest = word.to_string();
            while ui.measure(&rest, size, weight) > w && rest.chars().count() > 1 {
                let chars: Vec<char> = rest.chars().collect();
                let mut n = 1;
                while n < chars.len() && ui.measure(&chars[..n + 1].iter().collect::<String>(), size, weight) <= w {
                    n += 1;
                }
                out.push(chars[..n].iter().collect());
                rest = chars[n..].iter().collect();
            }
            line = rest;
        }
        out.push(line);
        if out.len() > 12 {
            break;
        }
    }
    while out.last().is_some_and(|l| l.is_empty()) && out.len() > 1 {
        out.pop();
    }
    WRAPS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 600 {
            c.clear();
        }
        c.insert(key, out.clone());
    });
    out
}

/// Text field box: Surface2, 1 px border (Track on hover, Accent with a soft glow while focused).
fn field_chrome(ui: &mut Ui, r: Rect, focused: bool) {
    let t = ui.t;
    if focused {
        ui.g.stroke_round(r.inset(-1.5, -1.5).d2d(), 10.5, ui.c(t.accent_soft), 3.0);
    }
    ui.fill(r, 9.0, t.surface2);
    let border = if focused { t.accent } else if ui.hovered(r) { t.track } else { t.stroke };
    ui.g.stroke_round(r.inset(0.5, 0.5).d2d(), 8.5, ui.c(border), 1.0);
}

/// Fade in the notch colour over 48 DIP plus a 22 px chevron button, 2 from the edge.
fn edge_hint(ui: &mut Ui, strip: Rect, left: bool, pill: crate::gfx::Color) {
    let t = ui.t;
    let w = 48.0;
    let steps = 16;
    for k in 0..steps {
        let f = k as f32 / steps as f32;
        let a = if left { 1.0 - f } else { f + 1.0 / steps as f32 };
        let x = if left { strip.x + f * w } else { strip.right() - w + f * w };
        ui.g.fill_rect(Rect::new(x, strip.y, w / steps as f32 + 0.5, strip.h).d2d(), ui.c(with_alpha(pill, a)));
    }
    let cx = if left { strip.x + 2.0 + 11.0 } else { strip.right() - 2.0 - 11.0 };
    ui.g.fill_circle(cx, strip.cy(), 11.0, ui.c(t.surface2));
    ui.icon(if left { "ChevronLeft" } else { "ChevronRight" }, cx, strip.cy(), 12.0, t.text);
}

fn load_thumbs(s: &State, paths: &[String]) {
    let missing: Vec<String> = {
        let th = s.thumbs.lock().unwrap();
        let loading = s.loading.lock().unwrap();
        paths.iter().filter(|p| !th.contains_key(*p) && !loading.contains(p)).cloned().collect()
    };
    if missing.is_empty() {
        return;
    }
    s.loading.lock().unwrap().extend(missing.iter().cloned());
    let (th, loading) = (s.thumbs.clone(), s.loading.clone());
    bg::spawn(move || {
        for p in missing {
            let png = shelf::thumbnail(&p, 160);
            th.lock().unwrap().insert(p.clone(), png);
            loading.lock().unwrap().retain(|x| *x != p);
        }
    });
}

/// The .NET ContextMenu look: Surface2, 8 radius, hairline border, 3 padding; 13 px items.
fn draw_menu(ui: &mut Ui, m: Option<Rect>) {
    let Some(m) = m else { return };
    let t = ui.t;
    ui.g.fill_round(m.d2d(), 8.0, ui.c(t.surface2));
    ui.g.stroke_round(m.inset(0.5, 0.5).d2d(), 7.5, ui.c(t.stroke), 1.0);
    let row = m.inset(4.0, 4.0);
    if row.contains(ui.mouse()) {
        ui.fill(row, 5.0, t.hover);
    }
    ui.text(tr("Clear unpinned"), row.pad(10.0, 0.0, 6.0, 0.0), 13.0, REGULAR, t.text, Align::Left);
    ui.animate = true;
}
