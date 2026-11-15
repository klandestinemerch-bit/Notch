//! Now Playing: art, source app, title, artist, seek bar (elapsed / -remaining, click to seek), the lyrics
//! toggle and previous / play-pause / next. With Lyrics on and synced lyrics found, the card splits in two:
//! a smaller player on the left, the lyrics on the right with the sung line lit and glided to the middle
//! (click a line to jump there).
//!
//! Music files on this PC (`services::player`): + opens songs (Play does too while nothing is playing and
//! the queue is empty), and songs or folders dropped on this tool join the queue. The queue button splits
//! the card the same way with the queue on the right: click a song to play it, × to take it out; shuffle,
//! repeat, add a folder and clear (click twice) sit above it.

use std::sync::{Arc, Mutex};

use crate::gfx::{with_alpha, Align, BOLD, REGULAR, SEMIBOLD};
use crate::model;
use crate::services::player::{self, Repeat};
use crate::services::{lyrics, media, settings};
use crate::ui::{ButtonStyle, Rect, Ui};
use crate::i18n::tr;

const LYRIC_SIZE: f32 = 14.0;
const LYRIC_LINE_H: f32 = 19.0;
/// Space above the first and below the last line (so they can reach the middle).
const LYRIC_PAD: f32 = 60.0;
/// A song in the queue list.
const ROW_H: f32 = 34.0;

#[derive(Default)]
pub struct State {
    /// Lyrics laid out for a width: the lines' identity, the width, each lyric's wrapped rows.
    laid_for: Option<(usize, f32)>,
    rows: Vec<Vec<String>>,
    tops: Vec<f32>,
    content_h: f32,
    /// Shown opacity per lyric (eases toward its target).
    alpha: Vec<f32>,
    current: Option<usize>,
    scroll: f32,
    target: f32,
    /// Seconds since the split opened (slide-in), and the last frame's clock.
    split_since: Option<f64>,
    last_t: f64,
    /// Card tint (C#: the art's average colour, a third of it over the surface, fading to the right),
    /// baked off the UI thread into a small picture: (key, BMP bytes).
    tint: Arc<Mutex<Option<(u64, Vec<u8>)>>>,
    tint_for: u64,
    /// The queue column is open (it takes the lyrics' place).
    queue_open: bool,
    /// What the split column showed last (true = queue): switching slides the new one in.
    split_queue: bool,
    /// The queue song last scrolled into view.
    queue_follow: Option<usize>,
    /// Clear was clicked once (when): a second click within 3 s empties the queue.
    clear_armed: Option<f64>,
}

pub fn shown(s: &mut State) {
    s.last_t = model::clock_secs();
    // Kick the lyrics lookup for the current song.
    let _ = model::lyric_line();
}

/// "m:ss", or "h:mm:ss" from an hour.
pub fn fmt(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Greedy word wrap of `text` into rows no wider than `width`.
fn wrap(ui: &Ui, text: &str, size: f32, weight: u16, width: f32) -> Vec<String> {
    let mut rows = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let candidate = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if cur.is_empty() || ui.measure(&candidate, size, weight) <= width {
            cur = candidate;
        } else {
            rows.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
    }
    if !cur.is_empty() || rows.is_empty() {
        rows.push(cur);
    }
    rows
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, _go: &mut Option<&'static str>) {
    let t = ui.t;
    let now_t = model::clock_secs();
    let dt = (now_t - s.last_t).clamp(0.0, 0.25) as f32;
    s.last_t = now_t;

    let np = model::now_playing();
    let local = player::view();
    card(ui, s, r, np.as_ref());
    let inner = r.pad(16.0, 12.0, 16.0, 12.0);

    let lyrics_on = settings::get().lyrics_enabled;
    // Keeps the lookup going for this song (the model fetches in the background).
    let _ = model::lyric_line();
    let (lines, loading) = if lyrics_on && np.is_some() && !s.queue_open { model::lyrics_all() } else { (None, false) };
    let split = s.queue_open || lines.is_some();
    if split {
        if s.split_queue != s.queue_open {
            s.split_queue = s.queue_open;
            s.split_since = None;
        }
        s.split_since.get_or_insert(now_t);
    } else {
        s.split_since = None;
        s.laid_for = None;
        s.current = None;
    }

    // Player column, and the lyrics column (1 : 1.1) when split.
    let player = if split { Rect::new(inner.x, inner.y, inner.w / 2.1, inner.h) } else { inner };

    // ---- art and text
    let (art_size, title_size) = if split { (64.0, 14.0) } else { (84.0, 17.0) };
    // The top area: what's left above the seek bar (10 + 14) and the controls (4 + 36).
    let (_, top) = player.take_bottom(36.0 + 4.0 + 14.0 + 10.0, 0.0);
    let art = Rect::new(player.x, top.cy() - art_size / 2.0, art_size, art_size);
    ui.fill(art, 16.0, t.surface2);
    match &np {
        Some(p) if ui.g.has_image(p.art_id) => ui.g.image(p.art_id, art.d2d(), 12.0, ui.alpha),
        _ => ui.icon("MusicNote", art.cx(), art.cy(), 24.0, t.subtext),
    }
    let tx = art.right() + 16.0;
    let tw = (player.right() - tx).max(0.0);
    let from_pc = np.as_ref().is_some_and(|p| p.app == player::APP);
    let status = if np.is_none() {
        if local.adding {
            tr("Adding songs…")
        } else if let Some(e) = local.error.as_deref() {
            e
        } else if local.queue.is_empty() {
            tr("Press + or drop songs here to play music from this PC")
        } else {
            tr("Press play to start your queue")
        }
    } else if let Some(e) = local.error.as_deref().filter(|_| from_pc) {
        e
    } else if !lyrics_on || s.queue_open {
        ""
    } else if loading {
        tr("Looking for lyrics…")
    } else if lines.is_none() {
        tr("No synced lyrics for this song")
    } else {
        ""
    };
    let (app, title, artist) = match &np {
        Some(p) => (p.app.as_str(), p.title.as_str(), p.artist.as_str()),
        None => ("", tr("Nothing playing"), ""),
    };
    // A centred stack: caps (14), title, artist (17), status (4 + 15).
    let title_h = title_size * 1.33;
    let mut heights = vec![14.0, 2.0 + title_h, 17.0];
    if !status.is_empty() {
        heights.push(19.0);
    }
    let total: f32 = heights.iter().sum();
    let mut y = top.cy() - total / 2.0;
    ui.text(tr(app), Rect::new(tx, y, tw, 14.0), 11.0, BOLD, t.subtext, Align::Left);
    y += 14.0 + 2.0;
    ui.text(title, Rect::new(tx, y, tw, title_h), title_size, SEMIBOLD, t.text, Align::Left);
    y += title_h;
    ui.text(artist, Rect::new(tx, y, tw, 17.0), 12.0, REGULAR, t.subtext, Align::Left);
    y += 17.0;
    if !status.is_empty() {
        ui.text(status, Rect::new(tx, y + 4.0, tw, 15.0), 11.0, REGULAR, with_alpha(t.subtext, 0.8), Align::Left);
    }

    // ---- seek bar: elapsed, bar, -remaining
    let pos = np.as_ref().map_or(0.0, |p| p.position_now());
    let dur = np.as_ref().map_or(0.0, |p| p.duration);
    let seek_row = Rect::new(player.x, player.bottom() - 36.0 - 4.0 - 14.0, player.w, 14.0);
    ui.text(&fmt(pos), Rect::new(seek_row.x, seek_row.y - 4.0, 40.0, 22.0), 11.0, REGULAR, t.subtext, Align::Left);
    if dur > 0.0 {
        ui.text(&format!("-{}", fmt(dur - pos)), Rect::new(seek_row.right() - 44.0, seek_row.y - 4.0, 44.0, 22.0), 11.0, REGULAR, t.subtext, Align::Right);
    }
    let bar = Rect::new(seek_row.x + 40.0, seek_row.y, seek_row.w - 84.0, 14.0);
    ui.fill(Rect::new(bar.x, bar.cy() - 2.0, bar.w, 4.0), 2.0, t.track);
    if dur > 0.0 {
        let frac = (pos / dur).clamp(0.0, 1.0) as f32;
        if frac > 0.0 {
            ui.fill(Rect::new(bar.x, bar.cy() - 2.0, (bar.w * frac).max(4.0), 4.0), 2.0, t.text);
        }
        if ui.clicked(bar) {
            let at = ((ui.mouse().0 - bar.x) / bar.w).clamp(0.0, 1.0) as f64 * dur;
            crate::bg::spawn(move || media::seek(at));
        }
    }

    // ---- controls: lyrics toggle on the left, transport in the middle
    let cy = player.bottom() - 18.0;
    let lyr = Rect::new(player.x, cy - 15.0, 30.0, 30.0);
    if ui.icon_button(lyr, "Quote", 16.0, if lyrics_on { t.accent } else { t.subtext }) {
        settings::update(|st| st.lyrics_enabled = !st.lyrics_enabled);
        s.laid_for = None;
    }
    let has = np.is_some();
    let fg = if has { t.text } else { with_alpha(t.text, 0.35) };
    let mid = player.cx();
    let prev = Rect::new(mid - 19.0 - 12.0 - 32.0, cy - 16.0, 32.0, 32.0);
    let play = Rect::new(mid - 19.0, cy - 18.0, 38.0, 36.0);
    let next = Rect::new(mid + 19.0 + 12.0, cy - 16.0, 32.0, 32.0);
    let playing = np.as_ref().is_some_and(|p| p.playing);
    if has {
        if ui.icon_button(prev, "Backward", 16.0, fg) {
            crate::bg::spawn(media::previous);
        }
        if ui.icon_button(play, if playing { "Pause" } else { "Play" }, 21.0, fg) {
            crate::bg::spawn(media::play_pause);
        }
        if ui.icon_button(next, "Forward", 16.0, fg) {
            crate::bg::spawn(media::next);
        }
    } else {
        ui.icon("Backward", prev.cx(), prev.cy(), 16.0, fg);
        ui.icon("Forward", next.cx(), next.cy(), 16.0, fg);
        // Nothing playing: Play starts the queue from this PC (or asks for songs when it's empty).
        if ui.icon_button(play, "Play", 21.0, t.text) {
            crate::bg::spawn(player::play_or_pick);
        }
    }
    if playing {
        ui.animate = true;
    }

    // ---- music from this PC: add songs, and the queue toggle
    let queue_btn = Rect::new(player.right() - 30.0, cy - 15.0, 30.0, 30.0);
    let add_btn = Rect::new(queue_btn.x - 32.0, cy - 15.0, 30.0, 30.0);
    if ui.icon_button(add_btn, "Plus", 16.0, t.subtext) {
        crate::bg::spawn(|| player::pick_files(true));
    }
    if ui.icon_button(queue_btn, "ListBullet", 16.0, if s.queue_open { t.accent } else { t.subtext }) {
        s.queue_open = !s.queue_open;
        s.queue_follow = None;
        s.clear_armed = None;
    }

    // ---- the split column: queue or lyrics
    if let Some(since) = s.split_since {
        let col = Rect::new(player.right() + 16.0, inner.y, inner.right() - player.right() - 16.0, inner.h);
        let age = (now_t - since) as f32;
        if s.queue_open {
            draw_queue(ui, s, col, &local, now_t, age);
        } else if let Some(lines) = lines {
            draw_lyrics(ui, s, col, &lines, pos, dt, age);
        }
    }
}

/// The queue column: header (count, shuffle, repeat, add folder, clear) over the scrolling song list.
fn draw_queue(ui: &mut Ui, s: &mut State, col: Rect, q: &player::View, now_t: f64, age: f32) {
    // Slide in from the right and fade in, as the lyrics do.
    let k = (age / 0.32).clamp(0.0, 1.0);
    let ease = 1.0 - (1.0 - k).powi(3);
    if k < 1.0 {
        ui.animate = true;
    }
    let shift = 24.0 * (1.0 - ease);
    let alpha = ui.alpha;
    ui.alpha *= ease;
    queue_body(ui, s, Rect::new(col.x + shift, col.y, col.w, col.h), q, now_t);
    ui.alpha = alpha;
}

fn queue_body(ui: &mut Ui, s: &mut State, col: Rect, q: &player::View, now_t: f64) {
    let t = ui.t;
    ui.g.fill_round(Rect::new(col.x - 8.0, col.y + 6.0, 1.0, col.h - 12.0).d2d(), 0.0, ui.c(t.stroke));
    let view = Rect::new(col.x + 8.0, col.y, col.w - 8.0, col.h);

    // ---- header
    let head = Rect::new(view.x, view.y, view.w, 24.0);
    let mut label = if q.queue.is_empty() { tr("QUEUE").to_string() } else { crate::i18n::tf!("QUEUE · {}", "ANTREAN · {}", q.queue.len()) };
    if q.adding {
        label.push_str(tr(" · adding…"));
    }
    ui.text(&label, Rect::new(head.x, head.y, head.w - 4.0 * 26.0, head.h), 11.0, BOLD, t.subtext, Align::Left);
    let button = |i: f32| Rect::new(head.right() - 24.0 - i * 26.0, head.cy() - 12.0, 24.0, 24.0);
    if s.clear_armed.is_some_and(|at| now_t - at > 3.0) {
        s.clear_armed = None;
    }
    if !q.queue.is_empty() {
        let armed = s.clear_armed.is_some();
        if ui.icon_button(button(0.0), "Trash", 14.0, if armed { t.danger } else { t.subtext }) {
            if armed {
                s.clear_armed = None;
                crate::bg::spawn(player::clear);
            } else {
                s.clear_armed = Some(now_t);
            }
        }
        if armed {
            ui.animate = true; // to disarm on time
        }
    }
    if ui.icon_button(button(1.0), "Folder", 14.0, t.subtext) {
        crate::bg::spawn(|| player::pick_folder(true));
    }
    let (repeat_icon, repeat_fg) = match q.repeat {
        Repeat::Off => ("Repeat", t.subtext),
        Repeat::All => ("Repeat", t.accent),
        Repeat::One => ("RepeatOnce", t.accent),
    };
    if ui.icon_button(button(2.0), repeat_icon, 14.0, repeat_fg) {
        crate::bg::spawn(player::cycle_repeat);
    }
    if ui.icon_button(button(3.0), "Shuffle", 14.0, if q.shuffle { t.accent } else { t.subtext }) {
        let on = !q.shuffle;
        crate::bg::spawn(move || player::set_shuffle(on));
    }

    // ---- the songs
    let list = Rect::new(view.x, head.bottom() + 4.0, view.w, view.bottom() - head.bottom() - 4.0);
    if q.queue.is_empty() {
        let msg = if q.adding { tr("Adding songs…") } else { tr("Drop songs or a folder here") };
        ui.text(msg, Rect::new(list.x, list.cy() - 26.0, list.w, 18.0), 12.0, REGULAR, t.subtext, Align::Center);
        let b = Rect::new(list.cx() - 55.0, list.cy() + 0.0, 110.0, 28.0);
        if !q.adding && ui.button(b, tr("Add songs"), ButtonStyle::Normal) {
            crate::bg::spawn(|| player::pick_files(true));
        }
        return;
    }
    let id = crate::ui::idx("np.queue", 0);
    let content_h = q.queue.len() as f32 * ROW_H;
    // Keep the current song in view when it changes.
    if s.queue_follow != Some(q.index) {
        s.queue_follow = Some(q.index);
        let top = q.index as f32 * ROW_H;
        let max = (content_h - list.h).max(0.0);
        let off = ui.st.scroll.get(&id).copied().unwrap_or(0.0);
        if top < off || top + ROW_H > off + list.h {
            ui.st.scroll.insert(id, (top - (list.h - ROW_H) / 2.0).clamp(0.0, max));
        }
    }
    // (song, take it out)
    let mut act: Option<(usize, bool)> = None;
    ui.scroll(id, list, content_h, |ui, top| {
        let first = ((list.y - top) / ROW_H).floor().max(0.0) as usize;
        let last = (((list.bottom() - top) / ROW_H).ceil().max(0.0) as usize).min(q.queue.len());
        for i in first..last {
            let song = &q.queue[i];
            let row = Rect::new(list.x, top + i as f32 * ROW_H, list.w - 6.0, ROW_H);
            let current = q.loaded && i == q.index;
            let hover = ui.hovered(row);
            if current || hover {
                let bg = if current { t.accent_soft } else { t.hover };
                ui.g.fill_round(row.inset(0.0, 1.0).d2d(), 8.0, ui.c(bg));
            }
            let mark = Rect::new(row.x, row.y, 26.0, row.h);
            if current {
                ui.icon(if q.playing { "MusicNote" } else { "Pause" }, mark.cx(), mark.cy(), 12.0, t.accent);
            } else {
                // Nothing loaded: the song Play starts with is marked.
                let up_next = !q.loaded && i == q.index;
                ui.text(&(i + 1).to_string(), mark, 11.0, if up_next { BOLD } else { REGULAR }, if up_next { t.accent } else { t.subtext }, Align::Center);
            }
            let side = Rect::new(row.right() - 40.0, row.y, 40.0, row.h);
            let tx = mark.right() + 2.0;
            let tw = (side.x - tx - 4.0).max(0.0);
            let fg = if current { t.accent } else { t.text };
            if song.artist.is_empty() {
                ui.text(&song.title, Rect::new(tx, row.y, tw, row.h), 12.5, SEMIBOLD, fg, Align::Left);
            } else {
                ui.text(&song.title, Rect::new(tx, row.y + 3.0, tw, 16.0), 12.5, SEMIBOLD, fg, Align::Left);
                ui.text(&song.artist, Rect::new(tx, row.y + 18.0, tw, 14.0), 11.0, REGULAR, t.subtext, Align::Left);
            }
            if hover {
                let x = Rect::new(side.right() - 28.0, row.cy() - 12.0, 24.0, 24.0);
                if ui.icon_button(x, "XMark", 12.0, t.subtext) {
                    act = Some((i, true));
                }
            } else if song.duration > 0.0 {
                ui.text(&fmt(song.duration), side.pad(0.0, 0.0, 6.0, 0.0), 11.0, REGULAR, t.subtext, Align::Right);
            }
            if act.is_none() && ui.clicked(row) {
                act = Some((i, false));
            }
        }
    });
    match act {
        Some((i, true)) => crate::bg::spawn(move || player::remove(i)),
        Some((i, false)) => crate::bg::spawn(move || player::play_at(i)),
        None => {}
    }
}

fn draw_lyrics(ui: &mut Ui, s: &mut State, col: Rect, lines: &Arc<Vec<lyrics::Line>>, pos: f64, dt: f32, age: f32) {
    let t = ui.t;
    // Slide in from the right and fade in (~300 ms, ease-out).
    let k = (age / 0.32).clamp(0.0, 1.0);
    let ease = 1.0 - (1.0 - k).powi(3);
    if k < 1.0 {
        ui.animate = true;
    }
    let shift = 24.0 * (1.0 - ease);
    let fade = ease;

    // Divider 8 px left of the column.
    ui.fill(Rect::new(col.x - 8.0 + shift, col.y + 6.0, 1.0, col.h - 12.0), 0.0, with_alpha(t.stroke, fade));
    let view = Rect::new(col.x + 8.0 + shift, col.y, col.w - 8.0, col.h);

    // Lay the lines out for this width (once per song / width).
    let key = (Arc::as_ptr(lines) as usize, view.w);
    if s.laid_for != Some(key) {
        s.rows = lines.iter().map(|l| wrap(ui, if l.text.trim().is_empty() { "♪" } else { l.text.trim() }, LYRIC_SIZE, SEMIBOLD, view.w - 4.0)).collect();
        s.tops.clear();
        let mut y = LYRIC_PAD;
        for rows in &s.rows {
            s.tops.push(y);
            y += rows.len() as f32 * LYRIC_LINE_H + 6.0;
        }
        s.content_h = y + LYRIC_PAD;
        s.alpha = vec![0.3; lines.len()];
        s.current = None;
        s.scroll = 0.0;
        s.target = 0.0;
        s.laid_for = Some(key);
    }

    // The sung line: centre it.
    let index = lyrics::line_at(lines, pos).map(|(i, _)| i);
    if index != s.current {
        s.current = index;
        if let Some(i) = index {
            let h = s.rows[i].len() as f32 * LYRIC_LINE_H + 6.0;
            let max = (s.content_h - view.h).max(0.0);
            s.target = (s.tops[i] - (view.h - h) / 2.0).clamp(0.0, max);
        }
    }
    // Wheel scrolls freely until the next line comes.
    if ui.input.wheel != 0.0 && view.contains(ui.mouse()) {
        let max = (s.content_h - view.h).max(0.0);
        s.target = (s.target - ui.input.wheel * 48.0).clamp(0.0, max);
        ui.input.wheel = 0.0;
    }
    let step = 1.0 - (-dt / 0.09).exp();
    if (s.target - s.scroll).abs() > 0.3 {
        s.scroll += (s.target - s.scroll) * step;
        ui.animate = true;
    } else {
        s.scroll = s.target;
    }

    ui.push_clip(view);
    let a_step = 1.0 - (-dt / 0.08).exp();
    for (i, rows) in s.rows.iter().enumerate() {
        let target = match s.current {
            Some(c) if c == i => 1.0,
            Some(c) if c.abs_diff(i) == 1 => 0.5,
            Some(_) => 0.28,
            None => 0.3,
        };
        let a = &mut s.alpha[i];
        if (*a - target).abs() > 0.005 {
            *a += (target - *a) * a_step;
            ui.animate = true;
        } else {
            *a = target;
        }
        let top = view.y + s.tops[i] + 3.0 - s.scroll;
        let h = rows.len() as f32 * LYRIC_LINE_H;
        if top + h < view.y || top > view.bottom() {
            continue;
        }
        let block = Rect::new(view.x, top - 3.0, view.w, h + 6.0);
        if ui.clicked(block) {
            let at = lines[i].at.max(0.0);
            crate::bg::spawn(move || media::seek(at));
        }
        for (j, row) in rows.iter().enumerate() {
            let ry = top + j as f32 * LYRIC_LINE_H;
            // Fade at the top and bottom 22 % (the C# opacity mask).
            let rel = ((ry + LYRIC_LINE_H / 2.0 - view.y) / view.h).clamp(0.0, 1.0);
            let mask = (rel / 0.22).min((1.0 - rel) / 0.22).clamp(0.0, 1.0);
            let alpha = *a * mask * fade;
            if alpha <= 0.01 {
                continue;
            }
            ui.text(row, Rect::new(view.x, ry, view.w, LYRIC_LINE_H), LYRIC_SIZE, SEMIBOLD, with_alpha(t.text, alpha), Align::Left);
        }
    }
    ui.pop_clip();
    ui.text(tr("LRCLIB"), Rect::new(view.right() - 60.0, view.bottom() - 12.0, 60.0, 12.0), 10.0, REGULAR, with_alpha(t.subtext, 0.5 * fade), Align::Right);
}

/// The card: surface, or (with art) a left-to-right gradient from the art's tint to the surface.
fn card(ui: &mut Ui, s: &mut State, r: Rect, np: Option<&media::NowPlaying>) {
    let sc = ui.t.surface;
    if let Some(p) = np.filter(|p| p.art_id != 0) {
        let key = p.art_id ^ ((sc.r * 255.0) as u64) << 8 ^ ((sc.g * 255.0) as u64) << 16 ^ ((sc.b * 255.0) as u64) << 24 ^ 0x7177_0000_0000_0000;
        if s.tint_for != key {
            s.tint_for = key;
            if let Some(art) = p.art.clone() {
                let slot = s.tint.clone();
                let surface = [sc.r, sc.g, sc.b];
                let (w, h) = (r.w, r.h);
                crate::bg::spawn(move || {
                    let Some((px, _)) = crate::views::home::art_pixels(&art, 16, 16) else { return };
                    let n = (px.len() / 4) as f32;
                    let mut avg = [0.0f32; 3];
                    for p in px.chunks_exact(4) {
                        avg[0] += p[2] as f32 / 255.0;
                        avg[1] += p[1] as f32 / 255.0;
                        avg[2] += p[0] as f32 / 255.0;
                    }
                    let mixed: Vec<f32> = (0..3).map(|i| (avg[i] / n + surface[i] * 2.0) / 3.0).collect();
                    let gw = 64u32;
                    let gh = ((gw as f32 * h / w).round() as u32).max(2);
                    let mut pixels = Vec::with_capacity((gw * gh) as usize);
                    for _ in 0..gh {
                        for x in 0..gw {
                            let k = (x as f32 + 0.5) / gw as f32;
                            pixels.push([0, 1, 2].map(|i| mixed[i] * (1.0 - k) + surface[i] * k));
                        }
                    }
                    *slot.lock().unwrap() = Some((key, crate::views::home::bmp(gw, gh, &pixels)));
                });
            }
        }
        let ready = s.tint.lock().unwrap().as_ref().filter(|(k, _)| *k == key).map(|(_, b)| ui.g.load_image(key, b)).unwrap_or(false);
        if ready {
            ui.g.image(key, r.d2d(), 16.0, ui.alpha);
            return;
        }
    }
    ui.card(r, 16.0);
}
