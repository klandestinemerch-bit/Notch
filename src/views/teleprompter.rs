//! Teleprompter: the script scrolls smoothly upward from a reading line 40% down; click the text or press
//! Space to play/pause, the wheel nudges it, and the controls set speed, text size and mirroring.
//! Speed eases toward its target (play, pause and slider changes glide); nudges glide too.
//! The pencil opens a plain multi-line editor (the script lives in teleprompter.txt).
//!
//! Mirrored text is drawn as glyph outlines (DirectWrite) flipped with a transform, since the renderer
//! only draws text left-to-right.

use std::time::Instant;

use windows::core::w;
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::Direct2D::Common::D2D1_FILL_MODE_WINDING;
use windows::Win32::Graphics::Direct2D::{ID2D1Geometry, ID2D1PathGeometry};
use windows::Win32::Graphics::DirectWrite::*;

use crate::gfx::{with_alpha, Align, MEDIUM, REGULAR};
use crate::services::{settings, teleprompter};
use crate::ui::{id, Key, Rect, Ui};
use crate::i18n::tr;

const LINE_SPACING: f32 = 1.35;
const SIDE: f32 = 24.0;
const SPEED_EASE: f32 = 0.18;
const NUDGE_EASE: f32 = 0.09;
const EDIT_SIZE: f32 = 14.0;
const EDIT_LINE: f32 = 20.0;
const EDITOR_ID: &str = "prompter.editor";

/// Wrapped lines: (first char index, char count, text).
type Lines = Vec<(usize, usize, String)>;

struct Glyphs {
    face: IDWriteFontFace,
    units_per_em: f32,
}

#[derive(Default)]
pub struct State {
    loaded: bool,
    script: String,
    editing: bool,
    edit: String,
    caret: usize,
    all_selected: bool,
    edit_scroll: f32,
    running: bool,
    offset: f32,
    velocity: f32,
    nudge: f32,
    last_frame: Option<Instant>,
    /// Wrapped script for (size, width).
    layout: Option<(f32, f32, Lines)>,
    /// Mirrored outlines for (size, width).
    outline: Option<(f32, f32, ID2D1PathGeometry)>,
    glyphs: Option<Glyphs>,
    speed: f32,
    size: f32,
    mirror: bool,
    /// Background opacity 0.2..1 (PrompterOpacity).
    opacity: f32,
    dirty: bool,
    /// The editor text changed at: saved shortly after (in the background) so leaving the tab mid-edit loses nothing.
    edit_changed: Option<Instant>,
}

pub fn shown(s: &mut State) {
    if !s.loaded {
        s.script = teleprompter::load();
        let st = settings::get();
        s.speed = st.prompter_speed as f32;
        s.size = (st.prompter_font_size as f32).clamp(13.0, 34.0);
        s.mirror = st.prompter_mirror;
        s.opacity = (st.prompter_opacity as f32).clamp(0.2, 1.0);
        s.loaded = true;
    }
    // The .NET view finishes editing when it's hidden; with no hide hook, do it when the tab comes back.
    if s.editing {
        s.editing = false;
        s.script = s.edit.trim_end().to_string();
        let script = s.script.clone();
        crate::bg::spawn(move || teleprompter::save(&script));
        s.edit_changed = None;
        s.offset = 0.0;
        s.nudge = 0.0;
        invalidate(s);
    }
    s.running = false;
    s.last_frame = None;
}

fn has_script(s: &State) -> bool {
    !s.script.trim().is_empty()
}

fn invalidate(s: &mut State) {
    s.layout = None;
    s.outline = None;
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, _go: &mut Option<&'static str>) {
    let t = ui.t;
    if !s.loaded {
        shown(s);
    }
    let (controls, stage) = r.take_bottom(28.0, 6.0);

    if s.editing {
        editor(ui, s, stage);
    } else if !has_script(s) {
        let hov = ui.hovered(stage);
        if hov {
            ui.fill(stage, 10.0, t.surface2);
        }
        ui.text(tr("Paste or write your script"), stage, 17.0, MEDIUM, t.subtext, Align::Center);
        if ui.clicked(stage) {
            start_editing(ui, s);
        }
    } else {
        prompt(ui, s, stage);
    }
    controls_row(ui, s, controls);

    // Save slider changes once the drag ends (not every frame).
    if s.dirty && ui.st.active == 0 {
        s.dirty = false;
        let (sp, sz, op) = (s.speed as f64, s.size.round() as f64, s.opacity as f64);
        settings::update_quiet(|x| {
            x.prompter_speed = sp;
            x.prompter_font_size = sz;
            x.prompter_opacity = op;
        });
    }
}

// ---------------------------------------------------------------- prompting

thread_local! {
    /// Widths of " word" segments: wrapping sums these instead of measuring whole lines (fast for long scripts).
    static WIDTHS: std::cell::RefCell<std::collections::HashMap<(String, u32, u16), f32>> = Default::default();
}

fn segment_width(ui: &Ui, seg: &[char], size: f32, weight: u16) -> f32 {
    if seg.iter().all(|c| *c == ' ') {
        return 0.0;
    }
    let s: String = seg.iter().collect();
    WIDTHS.with(|w| {
        let mut w = w.borrow_mut();
        if w.len() > 50_000 {
            w.clear();
        }
        *w.entry((s.clone(), (size * 10.0) as u32, weight)).or_insert_with(|| ui.measure(&s, size, weight))
    })
}

fn wrap(ui: &Ui, text: &str, size: f32, weight: u16, width: f32) -> Lines {
    let mut out = Vec::new();
    let mut base = 0usize;
    for para in text.split('\n') {
        let para_t = para.trim_end_matches('\r');
        let chars: Vec<char> = para_t.chars().collect();
        if chars.is_empty() {
            out.push((base, 0, String::new()));
        }
        let mut start = 0;
        while start < chars.len() {
            // Greedy: take words while they fit; break a too-long word by characters.
            let mut end = start;
            let mut last_fit = start;
            let mut line_w = 0.0;
            loop {
                let mut next = end;
                while next < chars.len() && chars[next] == ' ' {
                    next += 1;
                }
                while next < chars.len() && chars[next] != ' ' {
                    next += 1;
                }
                let w = line_w + segment_width(ui, &chars[end..next], size, weight);
                if w <= width {
                    line_w = w;
                    last_fit = next;
                    end = next;
                    if next >= chars.len() {
                        break;
                    }
                } else {
                    break;
                }
            }
            if last_fit == start {
                // One word wider than the line: as many characters as fit (at least one).
                let mut e = start + 1;
                while e < chars.len() && ui.measure(&chars[start..e + 1].iter().collect::<String>(), size, weight) <= width {
                    e += 1;
                }
                last_fit = e;
            }
            let line: String = chars[start..last_fit].iter().collect();
            out.push((base + start, last_fit - start, line));
            start = last_fit;
            // Spaces at a wrap point belong to the line before.
            while start < chars.len() && chars[start] == ' ' && start > 0 {
                start += 1;
                if let Some(l) = out.last_mut() {
                    l.1 += 1;
                }
            }
        }
        base += para.chars().count() + 1;
    }
    out
}

fn prompt(ui: &mut Ui, s: &mut State, stage: Rect) {
    let t = ui.t;
    let width = (stage.w - 2.0 * SIDE).max(50.0);
    let size = s.size.round();
    if !s.layout.as_ref().is_some_and(|(sz, w, _)| *sz == size && *w == width) {
        // Re-wrapping changes the script height: keep the same place in it.
        let old_h = s.layout.as_ref().map(|(sz, _, l)| l.len() as f32 * sz * LINE_SPACING);
        let lines = wrap(ui, &s.script, size, MEDIUM, width);
        let new_h = lines.len() as f32 * size * LINE_SPACING;
        if let Some(h) = old_h.filter(|h| *h > 0.0) {
            s.offset *= new_h / h;
            s.nudge *= new_h / h;
        }
        s.layout = Some((size, width, lines));
        s.outline = None;
    }
    let lh = size * LINE_SPACING;
    let lines = s.layout.as_ref().map(|l| l.2.clone()).unwrap_or_default();
    let end = lines.len() as f32 * lh;

    // Motion on the frame clock.
    let now = Instant::now();
    let dt = s.last_frame.map_or(1.0 / 60.0, |f| (now - f).as_secs_f32()).clamp(0.0, 0.1);
    s.last_frame = Some(now);
    let target = if s.running { teleprompter::pixels_per_second(s.speed as f64) } else { 0.0 };
    s.velocity += (target - s.velocity) * (1.0 - (-dt / SPEED_EASE).exp());
    let step = s.nudge * (1.0 - (-dt / NUDGE_EASE).exp());
    s.nudge -= step;
    s.offset = (s.offset + s.velocity * dt + step).clamp(0.0, end);
    if s.running && s.offset >= end {
        s.running = false; // reached the end
    }
    let moving = s.running || s.velocity > 0.5 || s.nudge.abs() > 0.25;
    if moving {
        ui.animate = true;
    } else {
        s.velocity = 0.0;
        s.offset = (s.offset + s.nudge).round().clamp(0.0, end);
        s.nudge = 0.0;
        s.last_frame = None;
    }

    // Input: click toggles, wheel nudges, Space toggles.
    if ui.input.wheel != 0.0 && stage.contains(ui.mouse()) {
        s.nudge -= ui.input.wheel * 48.0;
        s.nudge = s.nudge.clamp(-s.offset, end - s.offset);
        ui.input.wheel = 0.0;
        ui.animate = true;
    }
    if ui.clicked(stage) {
        toggle(s, end);
    }
    if ui.st.focus == 0 && ui.input.chars.contains(' ') {
        ui.input.chars.retain(|c| c != ' ');
        toggle(s, end);
    }

    let base_y = stage.y + (stage.h * 0.4).round();
    ui.push_clip(stage);
    if s.mirror {
        draw_mirrored(ui, s, stage, &lines, size, lh, base_y);
    } else {
        for (i, (_, _, line)) in lines.iter().enumerate() {
            let y = base_y - s.offset + i as f32 * lh;
            if y + lh < stage.y || y > stage.bottom() {
                continue;
            }
            ui.text(line, Rect::new(stage.x + SIDE, y, width, lh), size, MEDIUM, t.text, Align::Center);
        }
    }
    // Edge fades in the notch colour.
    fade(ui, Rect::new(stage.x, stage.y, stage.w, 44.0), true, s.opacity);
    fade(ui, Rect::new(stage.x, stage.bottom() - 36.0, stage.w, 36.0), false, s.opacity);
    ui.pop_clip();
    if !s.running && s.offset > 0.0 {
        ui.text(tr("Paused · click or press Space"), Rect::new(stage.x, stage.bottom() - 2.0 - 16.0, stage.w, 16.0), 12.0, crate::gfx::SEMIBOLD, t.subtext, Align::Center);
    }
}

fn toggle(s: &mut State, end: f32) {
    if !has_script(s) {
        return;
    }
    s.running = !s.running;
    if s.running && s.offset >= end - 1.0 {
        s.offset = 0.0; // pressing play at the end starts over
    }
    s.last_frame = None;
}

/// Edge fade in the notch colour at the background opacity (as the .NET gradient).
fn fade(ui: &mut Ui, r: Rect, top: bool, opacity: f32) {
    let pill = ui.t.pill;
    let steps = 14;
    for k in 0..steps {
        let f = k as f32 / steps as f32;
        let a = if top { 1.0 - f } else { f + 1.0 / steps as f32 };
        let y = r.y + f * r.h;
        ui.g.fill_rect(Rect::new(r.x, y, r.w, r.h / steps as f32 + 0.5).d2d(), ui.c(with_alpha(pill, a.min(1.0) * opacity.clamp(0.15, 1.0))));
    }
}

fn glyphs() -> Option<Glyphs> {
    unsafe {
        let f: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
        let mut coll = None;
        f.GetSystemFontCollection(&mut coll, false).ok()?;
        let coll = coll?;
        for name in [w!("Segoe UI Variable Text"), w!("Segoe UI")] {
            let (mut index, mut exists) = (0u32, BOOL(0));
            if coll.FindFamilyName(name, &mut index, &mut exists).is_ok() && exists.as_bool() {
                let fam = coll.GetFontFamily(index).ok()?;
                let font = fam
                    .GetFirstMatchingFont(DWRITE_FONT_WEIGHT(MEDIUM as i32), DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL)
                    .ok()?;
                let face = font.CreateFontFace().ok()?;
                let mut m = DWRITE_FONT_METRICS::default();
                face.GetMetrics(&mut m);
                return Some(Glyphs { face, units_per_em: m.designUnitsPerEm as f32 });
            }
        }
        None
    }
}

/// Builds the outlines of all lines (centred in `width`, baseline per line) once per layout.
fn build_outline(ui: &Ui, g: &Glyphs, lines: &Lines, size: f32, lh: f32, width: f32) -> Option<ID2D1PathGeometry> {
    unsafe {
        let mut runs = Vec::new();
        for (i, (_, _, line)) in lines.iter().enumerate() {
            let cps: Vec<u32> = line.chars().map(|c| c as u32).collect();
            if cps.is_empty() {
                continue;
            }
            let mut idx = vec![0u16; cps.len()];
            g.face.GetGlyphIndices(cps.as_ptr(), cps.len() as u32, idx.as_mut_ptr()).ok()?;
            let mut gm = vec![DWRITE_GLYPH_METRICS::default(); idx.len()];
            g.face.GetDesignGlyphMetrics(idx.as_ptr(), idx.len() as u32, gm.as_mut_ptr(), false).ok()?;
            let adv: Vec<f32> = gm.iter().map(|m| m.advanceWidth as f32 * size / g.units_per_em).collect();
            let total: f32 = adv.iter().sum();
            let x = (width - total) / 2.0;
            // Baseline roughly where DirectWrite puts it in a line box of height lh.
            let y = i as f32 * lh + (lh - size * 1.33) / 2.0 + size * 1.06;
            let offs = vec![DWRITE_GLYPH_OFFSET { advanceOffset: x, ascenderOffset: -y }; idx.len()];
            runs.push((idx, adv, offs));
        }
        ui.g.path(|sink| {
            sink.SetFillMode(D2D1_FILL_MODE_WINDING);
            for (idx, adv, offs) in &runs {
                let _ = g.face.GetGlyphRunOutline(
                    size,
                    idx.as_ptr(),
                    Some(adv.as_ptr()),
                    Some(offs.as_ptr()),
                    idx.len() as u32,
                    false,
                    false,
                    sink,
                );
            }
        })
    }
}

fn draw_mirrored(ui: &mut Ui, s: &mut State, stage: Rect, lines: &Lines, size: f32, lh: f32, base_y: f32) {
    let width = (stage.w - 2.0 * SIDE).max(50.0);
    if s.glyphs.is_none() {
        s.glyphs = glyphs();
    }
    let Some(g) = s.glyphs.as_ref() else { return };
    if !s.outline.as_ref().is_some_and(|(sz, w, _)| *sz == size && *w == width) {
        s.outline = build_outline(ui, g, lines, size, lh, width).map(|geo| (size, width, geo));
    }
    let Some((_, _, geo)) = s.outline.as_ref() else { return };
    // x' = (stage.x + SIDE + width) - x  (flipped inside the text column), y' = y + base_y - offset.
    let m = Matrix3x2 { M11: -1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: stage.x + SIDE + width, M32: base_y - s.offset };
    unsafe {
        let src: ID2D1Geometry = geo.clone().into();
        if let Ok(tg) = ui.g.factory.CreateTransformedGeometry(&src, &m) {
            ui.g.fill_geometry(&tg.into(), ui.c(ui.t.text));
        }
    }
}

// ---------------------------------------------------------------- controls

fn controls_row(ui: &mut Ui, s: &mut State, r: Rect) {
    let t = ui.t;
    // .NET layout: Edit 28 · 8 · Play 28 · 8 · Restart 28 · 14 Signal 6 [speed 90] 6 Rocket · 14 Language 6 [size 60]
    // · 14 Sun 6 [opacity 70] · 10 Mirror 28 = 480, centred.
    let total = 480.0;
    let mut x = r.cx() - total / 2.0;
    let mut next = |w: f32, gap: f32| {
        let rr = Rect::new(x, r.y, w, r.h);
        x += w + gap;
        rr
    };
    let edit_r = next(28.0, 8.0);
    let play_r = next(28.0, 8.0);
    let restart_r = next(28.0, 14.0);
    let speed_lo = next(14.0, 6.0);
    let speed_r = next(90.0, 6.0);
    let speed_hi = next(14.0, 14.0);
    let size_icon = next(14.0, 6.0);
    let size_r = next(60.0, 14.0);
    let sun = next(14.0, 6.0);
    let opacity_r = next(70.0, 10.0);
    let mirror_r = next(28.0, 0.0);

    if ui.icon_button(edit_r, if s.editing { "Check" } else { "Pencil" }, 14.0, t.subtext) {
        if s.editing {
            finish_editing(ui, s);
        } else {
            start_editing(ui, s);
        }
    }
    let can_play = !s.editing && has_script(s);
    let hov = ui.hovered(play_r) && can_play;
    let a = if !can_play { 0.35 } else if hov { 0.85 } else { 1.0 };
    ui.g.fill_circle(play_r.cx(), play_r.cy(), 14.0, ui.c(with_alpha(t.accent, a)));
    ui.icon(if s.running { "Pause" } else { "Play" }, play_r.cx(), play_r.cy(), 14.0, with_alpha(t.on_accent, a));
    if ui.clicked(play_r) && can_play {
        let end = s.layout.as_ref().map_or(f32::MAX, |(sz, _, l)| l.len() as f32 * sz * LINE_SPACING);
        toggle(s, end);
    }
    if ui.icon_button(restart_r, "Refresh", 14.0, t.subtext) {
        s.offset = 0.0;
        s.nudge = 0.0;
    }
    ui.icon("Signal", speed_lo.cx(), speed_lo.cy(), 14.0, t.subtext);
    if let Some(v) = ui.slider(id("prompter.speed"), speed_r.inset(6.0, 0.0), s.speed) {
        s.speed = v;
        s.dirty = true;
    }
    ui.icon("Rocket", speed_hi.cx(), speed_hi.cy(), 14.0, t.subtext);
    ui.icon("Language", size_icon.cx(), size_icon.cy(), 14.0, t.subtext);
    let frac = (s.size - 13.0) / (34.0 - 13.0);
    if let Some(v) = ui.slider(id("prompter.size"), size_r.inset(6.0, 0.0), frac) {
        s.size = (13.0 + v * 21.0).round();
        s.dirty = true;
    }
    ui.icon("Sun", sun.cx(), sun.cy(), 14.0, t.subtext);
    let frac = (s.opacity - 0.2) / 0.8;
    if let Some(v) = ui.slider(id("prompter.opacity"), opacity_r.inset(6.0, 0.0), frac) {
        s.opacity = 0.2 + v * 0.8;
        s.dirty = true;
    }
    if ui.icon_button(mirror_r, "ArrowsLeftRight", 14.0, t.subtext) {
        s.mirror = !s.mirror;
        let m = s.mirror;
        settings::update_quiet(|x| x.prompter_mirror = m);
    }
}

// ---------------------------------------------------------------- editing

fn start_editing(ui: &mut Ui, s: &mut State) {
    s.running = false;
    s.editing = true;
    s.edit = s.script.clone();
    s.caret = s.edit.chars().count();
    s.all_selected = false;
    ui.st.focus = id(EDITOR_ID);
    ui.wants_keyboard = true;
}

fn finish_editing(ui: &mut Ui, s: &mut State) {
    s.editing = false;
    s.edit_changed = None;
    s.script = s.edit.trim_end().to_string();
    teleprompter::save(&s.script);
    s.offset = 0.0;
    s.nudge = 0.0;
    invalidate(s);
    if ui.st.focus == id(EDITOR_ID) {
        ui.st.focus = 0;
    }
}

fn byte_at(s: &str, ch: usize) -> usize {
    s.char_indices().nth(ch).map_or(s.len(), |(b, _)| b)
}

fn editor(ui: &mut Ui, s: &mut State, r: Rect) {
    let t = ui.t;
    if let Some(at) = s.edit_changed {
        if at.elapsed().as_millis() > 800 {
            s.edit_changed = None;
            let text = s.edit.trim_end().to_string();
            crate::bg::spawn(move || teleprompter::save(&text));
        } else {
            ui.animate = true;
        }
    }
    // The .NET TextBox look: Surface fill (set on the editor), 9 radius, hairline border; accent + glow while focused.
    let focused_now = ui.st.focus == id(EDITOR_ID);
    if focused_now {
        ui.g.stroke_round(r.inset(-1.5, -1.5).d2d(), 10.5, ui.c(t.accent_soft), 3.0);
    }
    ui.fill(r, 9.0, t.surface);
    let border = if focused_now { t.accent } else if ui.hovered(r) { t.track } else { t.stroke };
    ui.g.stroke_round(r.inset(0.5, 0.5).d2d(), 8.5, ui.c(border), 1.0);
    let inner = r.pad(12.0, 10.0, 12.0, 10.0);
    let fid = id(EDITOR_ID);
    if ui.clicked(r) {
        ui.st.focus = fid;
        ui.wants_keyboard = true;
    }
    let focused = ui.st.focus == fid;

    let lines = wrap(ui, &s.edit, EDIT_SIZE, REGULAR, inner.w - 6.0);
    if focused {
        handle_keys(ui, s, &lines);
    }
    let lines = wrap(ui, &s.edit, EDIT_SIZE, REGULAR, inner.w - 6.0);
    let caret_line = lines.iter().rposition(|(st, _, _)| *st <= s.caret).unwrap_or(0);

    // Keep the caret in view; the wheel scrolls.
    let content_h = lines.len() as f32 * EDIT_LINE;
    let max = (content_h - inner.h).max(0.0);
    if ui.input.wheel != 0.0 && r.contains(ui.mouse()) {
        s.edit_scroll -= ui.input.wheel * 48.0;
        ui.input.wheel = 0.0;
    }
    let cy = caret_line as f32 * EDIT_LINE;
    if focused && (!ui.input.keys.is_empty() || !ui.input.chars.is_empty()) {
        if cy < s.edit_scroll {
            s.edit_scroll = cy;
        } else if cy + EDIT_LINE > s.edit_scroll + inner.h {
            s.edit_scroll = cy + EDIT_LINE - inner.h;
        }
    }
    s.edit_scroll = s.edit_scroll.clamp(0.0, max);

    // Click places the caret.
    if ui.input.pressed && inner.contains(ui.mouse()) {
        let (mx, my) = ui.mouse();
        let li = (((my - inner.y + s.edit_scroll) / EDIT_LINE).floor().max(0.0) as usize).min(lines.len().saturating_sub(1));
        if let Some((st, n, text)) = lines.get(li) {
            let chars: Vec<char> = text.chars().collect();
            let mut col = 0;
            while col < chars.len() && col < *n {
                let w = ui.measure(&chars[..col + 1].iter().collect::<String>(), EDIT_SIZE, REGULAR);
                let wprev = ui.measure(&chars[..col].iter().collect::<String>(), EDIT_SIZE, REGULAR);
                if inner.x + (w + wprev) / 2.0 > mx {
                    break;
                }
                col += 1;
            }
            s.caret = st + col;
            s.all_selected = false;
        }
    }

    ui.push_clip(r.inset(2.0, 2.0));
    if s.edit.is_empty() {
        ui.text(tr("Paste your script here."), Rect::new(inner.x, inner.y, inner.w, EDIT_LINE), EDIT_SIZE, REGULAR, t.subtext, Align::Left);
    }
    for (i, (_, _, text)) in lines.iter().enumerate() {
        let y = inner.y + i as f32 * EDIT_LINE - s.edit_scroll;
        if y + EDIT_LINE < r.y || y > r.bottom() {
            continue;
        }
        let row = Rect::new(inner.x, y, inner.w, EDIT_LINE);
        if s.all_selected && !text.is_empty() {
            let w = ui.measure(text, EDIT_SIZE, REGULAR);
            ui.fill(Rect::new(row.x, row.y + 1.0, w, row.h - 2.0), 3.0, t.accent_soft);
        }
        ui.text(text, row, EDIT_SIZE, REGULAR, t.text, Align::Left);
    }
    if focused {
        ui.animate = true;
        if ui.st.caret_on {
            if let Some((st, _, text)) = lines.get(caret_line) {
                let before: String = text.chars().take(s.caret - st).collect();
                let x = inner.x + ui.measure(&before, EDIT_SIZE, REGULAR);
                let y = inner.y + cy - s.edit_scroll;
                ui.fill(Rect::new(x, y + 2.0, 2.0, EDIT_LINE - 4.0), 1.0, t.accent);
            }
        }
    }
    ui.pop_clip();
    if max > 0.0 {
        let bar_h = (r.h * r.h / (content_h + 20.0)).max(18.0);
        let y = r.y + (r.h - bar_h) * (s.edit_scroll / max);
        ui.fill(Rect::new(r.right() - 5.0, y, 3.0, bar_h), 1.5, with_alpha(t.text, 0.18));
    }
}

fn handle_keys(ui: &mut Ui, s: &mut State, lines: &Lines) {
    if !ui.input.chars.is_empty() || !ui.input.keys.is_empty() {
        s.edit_changed = Some(Instant::now());
    }
    let insert = |s: &mut State, text: &str| {
        if s.all_selected {
            s.edit.clear();
            s.caret = 0;
            s.all_selected = false;
        }
        let b = byte_at(&s.edit, s.caret);
        s.edit.insert_str(b, text);
        s.caret += text.chars().count();
    };
    let chars: String = ui.input.chars.chars().filter(|c| !c.is_control()).collect();
    if !chars.is_empty() {
        insert(s, &chars);
    }
    let keys = ui.input.keys.clone();
    let line_of = |caret: usize| lines.iter().rposition(|(st, _, _)| *st <= caret).unwrap_or(0);
    for k in keys {
        let len = s.edit.chars().count();
        match k {
            Key::Enter => insert(s, "\n"),
            Key::Tab => insert(s, "    "),
            Key::Paste => {
                if let Some(p) = crate::clipboard::get_text() {
                    insert(s, &p.replace("\r\n", "\n").replace('\r', "\n"));
                }
            }
            Key::SelectAll => s.all_selected = true,
            Key::Copy => {
                if s.all_selected {
                    crate::clipboard::set_text(&s.edit);
                }
            }
            Key::Cut => {
                if s.all_selected {
                    crate::clipboard::set_text(&s.edit);
                    s.edit.clear();
                    s.caret = 0;
                    s.all_selected = false;
                }
            }
            Key::Backspace | Key::Delete if s.all_selected => {
                s.edit.clear();
                s.caret = 0;
                s.all_selected = false;
            }
            Key::Backspace if s.caret > 0 => {
                let b = byte_at(&s.edit, s.caret - 1);
                s.edit.remove(b);
                s.caret -= 1;
            }
            Key::Delete if s.caret < len => {
                let b = byte_at(&s.edit, s.caret);
                s.edit.remove(b);
            }
            Key::Left => {
                s.caret = s.caret.saturating_sub(1);
                s.all_selected = false;
            }
            Key::Right => {
                s.caret = (s.caret + 1).min(len);
                s.all_selected = false;
            }
            Key::Up | Key::Down => {
                s.all_selected = false;
                let li = line_of(s.caret);
                let col = s.caret - lines.get(li).map_or(0, |l| l.0);
                let target = if k == Key::Up { li.checked_sub(1) } else { (li + 1 < lines.len()).then_some(li + 1) };
                match target.and_then(|t| lines.get(t)) {
                    Some((st, n, _)) => s.caret = st + col.min(*n),
                    None => s.caret = if k == Key::Up { 0 } else { len },
                }
            }
            Key::Home => {
                s.caret = lines.get(line_of(s.caret)).map_or(0, |l| l.0);
                s.all_selected = false;
            }
            Key::End => {
                s.caret = lines.get(line_of(s.caret)).map_or(len, |l| {
                    let text_len = l.2.chars().count();
                    l.0 + text_len
                });
                s.all_selected = false;
            }
            Key::Escape => {
                finish_editing(ui, s);
                return;
            }
            _ => {}
        }
    }
    ui.input.keys.clear();
    ui.input.chars.clear();
}
