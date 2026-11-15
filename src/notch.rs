//! The notch: its state, springs, closed-state activities, alerts and the open panel (header, tool, tab bar).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE;
use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::gfx::{color, with_alpha, Align, Gfx, BOLD, LIGHT, MEDIUM, REGULAR, SEMIBOLD};
use crate::model;
use crate::services::settings;
use crate::shape::Outline;
use crate::spring::Spring;
use crate::tools::{self, CONTENT_W};
use crate::ui::{Input, Rect, Theme, Ui, UiState};
use crate::views;
use crate::i18n::tr;

const EAR_CLOSED: f32 = 8.0;
const EAR_OPEN: f32 = 11.0;
const RADIUS_CLOSED: f32 = 14.0;
const RADIUS_OPEN: f32 = 32.0;
const RADIUS_PEEK: f32 = 28.0;
/// How long switching tools takes (fade + slide).
const TOOL_ANIM_SECS: f32 = 0.28;
/// How far a capsule floats below the top edge.
const GAP: f32 = 8.0;
const MINI_H: f32 = 40.0;
const PEEK_H: f32 = 76.0;
const PEEK_W: f32 = 420.0;
const LEVEL_W: f32 = 216.0;
const LEVEL_BAR: f32 = 150.0;
const HEADER_H: f32 = 56.0;
const TABBAR_H: f32 = 56.0;
const SIDE: f32 = 20.0;
/// Canvas: the biggest the notch gets, plus room for ears, the capsule drop and the outline.
pub const CANVAS_W: f32 = CONTENT_W + 2.0 * SIDE + 2.0 * EAR_OPEN + 8.0;
pub const CANVAS_H: f32 = HEADER_H + tools::SETTINGS_H + SETTINGS_BOTTOM + GAP + 10.0;
/// Settings has no tab bar, just a little room under its content.
const SETTINGS_BOTTOM: f32 = 16.0;

/// Space under the content: the tab bar, or Settings' bottom margin.
fn bottom_h(tool: &str) -> f32 {
    if tool == "settings" {
        SETTINGS_BOTTOM
    } else {
        TABBAR_H
    }
}

pub const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
pub const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

#[derive(Clone)]
pub enum AlertKind {
    /// A glyph and one line (Caps Lock, keyboard layout, network, Bluetooth, power…).
    Compact { icon: &'static str, title: String, sub: String },
    /// A glyph and a bar (volume, brightness).
    Level { icon: &'static str, level: f32 },
    /// Icon tile, title, body (notifications, reminders, prayer).
    /// `app` (AppUserModelId, display name) marks another app's notification: its icon replaces `icon`.
    Full { icon: &'static str, title: String, sub: String, open: Option<&'static str>, app: Option<(String, String)> },
}

#[derive(Clone)]
pub struct Alert {
    pub key: String,
    pub kind: AlertKind,
    pub duration: Duration,
}

pub struct Notch {
    pub gfx: Gfx,
    pub win_x: i32,
    /// Pointer dodge: how far the closed notch has slid sideways (DIP).
    pub shove: Spring,
    /// The pointer came at the notch from the side: keep out of its way until it leaves the band.
    dodging: Option<f32>,
    pub win_y: i32,
    width: Spring,
    height: Spring,
    /// 0 → 1 while an alert is up: the notch melts into a floating capsule.
    alert_mix: Spring,
    /// Smooth level bar for volume/brightness alerts.
    level: Spring,
    /// Idle item change: 0 → 1 as the next item slides in.
    idle_swap: Spring,
    expanded: bool,
    pub pinned: bool,
    pub tool: &'static str,
    /// Where Settings' back button returns.
    back: Option<&'static str>,
    /// Switching tools: when, and the side the new one comes from (-1 left, 1 right, 0 straight up).
    tool_anim: Option<(Instant, f32)>,
    alert: Option<Alert>,
    queue: VecDeque<Alert>,
    pub alert_until: Option<Instant>,
    pub input: Input,
    pub ui: UiState,
    pub views: views::Views,
    last_frame: Option<Instant>,
    idle_index: usize,
    /// Idle cat eyes: when the current look started.
    eyes: Option<Instant>,
    /// Which eye animation is playing (the next peek plays the next one).
    eyes_kind: u8,
    /// Set by a frame: the window should take keyboard focus.
    pub wants_keyboard: bool,
    /// Screenshot mode: frames go to this PNG instead of the screen.
    pub shot: Option<String>,
    /// Settings → General "Show notch" / "Hide for these apps": the closed pill fades away (and lets
    /// clicks through) until the pointer touches the top edge or something happens.
    pub concealed: bool,
    conceal: Spring,
    /// Set by a frame: something is animating inside (draw again next refresh).
    pub busy: bool,
    /// The line the closed notch last sized itself for while music plays.
    music_line: String,
    tab_hits: Vec<(Rect, &'static str)>,
    pin_hit: Rect,
    panel: (f32, f32, f32), // origin x, origin y, scale
}

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

const EYES_SECS: f32 = 3.6;

/// Closed-notch text size for its height (as the C# app: 40 % of the height, 11–14, in half points).
fn idle_font(h: f32) -> f32 {
    ((h * 0.4).clamp(11.0, 14.0) * 2.0).round() / 2.0
}

/// How many different eye animations there are; each peek plays the next one.
pub const EYE_KINDS: u8 = 4;

/// One moment of the cat's face.
struct Face {
    /// Lid opening per eye (0 shut … 1 open).
    open: [f32; 2],
    /// Shut eyes drawn as arcs: 1 = happy "^", -1 = sleeping "‿", 0 = not shut.
    arc: [f32; 2],
    /// Where the pupils look (−1 … 1 each way).
    look: (f32, f32),
    /// Pupil width as a share of the eye size.
    pupil: f32,
}

fn ease01(x: f32) -> f32 {
    let x = clamp01(x);
    x * x * (3.0 - 2.0 * x)
}

/// A value that ramps 0 → 1 between `a` and `b`.
fn ramp(el: f32, a: f32, b: f32) -> f32 {
    ease01((el - a) / (b - a))
}

/// A blink: 1 open, dipping to 0 between `a` and `b` (shut for the middle `hold`).
fn blink(el: f32, a: f32, b: f32, hold: f32) -> f32 {
    let mid = (a + b) / 2.0;
    let half = ((b - a) - hold) / 2.0;
    if el < a || el > b {
        1.0
    } else if el < mid - hold / 2.0 {
        1.0 - ramp(el, a, a + half)
    } else if el < mid + hold / 2.0 {
        0.0
    } else {
        ramp(el, b - half, b)
    }
}

/// The cat's face at `el` seconds into animation `kind`.
fn face(kind: u8, el: f32) -> Face {
    let k = EYES_SECS;
    // Every animation opens over the first 0.4 s and closes over the last 0.4 s.
    let lids = ease01(el / 0.4).min(ease01((k - el) / 0.4));
    match kind % EYE_KINDS {
        // Sleepy: half-closed eyes, two slow blinks, then dozing off with closed "‿ ‿" eyes.
        1 => {
            let o = lids * 0.5 * blink(el, 1.0, 1.7, 0.2) * blink(el, 2.0, 2.6, 0.15);
            let doze = (2.75..k - 0.15).contains(&el);
            Face {
                open: [if doze { 0.0 } else { o }; 2],
                arc: [if doze { -1.0 } else { 0.0 }; 2],
                look: (0.0, 0.35),
                pupil: 0.5,
            }
        }
        // Wink: looks at you, winks the right eye, then a happy blink.
        2 => {
            let wink = (1.25..1.85).contains(&el);
            let happy = (2.5..2.95).contains(&el);
            Face {
                open: [if happy { 0.0 } else { lids }, if wink || happy { 0.0 } else { lids * blink(el, 1.1, 2.0, 0.6) }],
                arc: [if happy { 1.0 } else { 0.0 }, if wink || happy { 1.0 } else { 0.0 }],
                look: (0.0, 0.0),
                pupil: 0.5 - 0.1 * ramp(el, 0.4, 1.0),
            }
        }
        // Curious: wide eyes look up, dart left and right, then a quick double blink.
        3 => {
            let up = ramp(el, 0.5, 0.8) * (1.0 - ramp(el, 2.2, 2.5));
            let dart = if el < 1.1 {
                0.0
            } else if el < 1.3 {
                -ramp(el, 1.1, 1.3)
            } else if el < 1.6 {
                -1.0 + 2.0 * ramp(el, 1.4, 1.6)
            } else if el < 1.9 {
                1.0 - 2.0 * ramp(el, 1.75, 1.9)
            } else {
                -1.0 + ramp(el, 2.0, 2.2)
            };
            let o = lids * blink(el, 2.55, 2.75, 0.03) * blink(el, 2.85, 3.05, 0.03);
            Face { open: [o; 2], arc: [0.0; 2], look: (dart, -0.5 * up), pupil: 0.62 }
        }
        // Look around: glance left and right, then a slow "^ ^" cat blink (how cats blink at someone they like).
        _ => {
            let smiling = (2.12..2.68).contains(&el);
            let o = lids * blink(el, 2.0, 2.9, 0.4);
            let look = if el < 0.9 {
                0.0
            } else if el < 1.45 {
                -ramp(el, 0.9, 1.2)
            } else if el < 1.95 {
                -1.0 + 2.0 * ramp(el, 1.45, 1.75)
            } else {
                1.0 - ramp(el, 1.95, 2.2)
            };
            let pupil = if el < 2.6 { 0.58 - 0.2 * ramp(el, 0.4, 1.0) } else { 0.38 + 0.16 * ramp(el, 2.6, 3.0) };
            Face {
                open: [if smiling { 0.0 } else { o }; 2],
                arc: [if smiling { 1.0 } else { 0.0 }; 2],
                look: (look, 0.0),
                pupil,
            }
        }
    }
}

/// A cute white cat face: round-ish, gently slanted eyes with big dark pupils (no highlights).
/// `kind` picks one of the `EYE_KINDS` animations.
fn draw_eyes(g: &Gfx, r: &Rect, el: f32, a: f32, t: &Theme, kind: u8) {
    let k = EYES_SECS;
    let f = face(kind, el);
    let fade = clamp01(el / 0.2).min(clamp01((k - el) / 0.2));
    let cy = r.cy();
    let size = (r.h * 0.26).clamp(7.0, 11.0);
    // White on the black pill (the scheme's text colour, so light schemes stay readable).
    let white = with_alpha(t.text, a * fade);
    let dark = with_alpha(t.pill, a * fade);
    let p = crate::gfx::pt;
    let w = size * 1.1; // half width of an eye
    for (i, side) in [-1.0f32, 1.0].into_iter().enumerate() {
        let cx = r.cx() + side * size * 2.0;
        if f.arc[i] != 0.0 {
            // Shut: "^" (happy) or "‿" (asleep).
            let up = f.arc[i] > 0.0;
            let (ey, cy2) = if up { (cy + size * 0.3, cy - size * 0.7) } else { (cy - size * 0.1, cy + size * 0.8) };
            if let Some(geo) = g.path(|sink| unsafe {
                use windows::Win32::Graphics::Direct2D::Common::{D2D1_BEZIER_SEGMENT, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN};
                sink.BeginFigure(p(cx - w * 0.85, ey), D2D1_FIGURE_BEGIN_HOLLOW);
                sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: p(cx - w * 0.45, cy2), point2: p(cx + w * 0.45, cy2), point3: p(cx + w * 0.85, ey) });
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
            }) {
                g.stroke_geometry(&geo.into(), white, size * 0.32);
            }
            continue;
        }

        let open = f.open[i].max(0.06);
        let h = size * 0.95 * open; // half height: nearly round when open
        // A gentle slant: outer corner a little higher than the inner one.
        let inner = (cx - side * w, cy + size * 0.04);
        let outer = (cx + side * w, cy - size * 0.14 * open.max(0.3));
        if let Some(geo) = g.path(|sink| unsafe {
            use windows::Win32::Graphics::Direct2D::Common::{D2D1_BEZIER_SEGMENT, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED};
            sink.BeginFigure(p(inner.0, inner.1), D2D1_FIGURE_BEGIN_FILLED);
            sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: p(inner.0, cy - h * 1.3), point2: p(outer.0, outer.1 - h * 1.25), point3: p(outer.0, outer.1) });
            sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: p(outer.0, cy + h * 1.3), point2: p(inner.0, cy + h * 1.3), point3: p(inner.0, inner.1) });
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        }) {
            g.fill_geometry(&geo.into(), white);
        }
        // Pupil, kept inside the eye.
        if open > 0.3 {
            let ox = f.look.0 * w * 0.3;
            let room = h * (1.0 - (ox / w).powi(2)).max(0.0).sqrt();
            let ry = (h * 0.8).min(room * 0.9);
            let py = cy - size * 0.02 + f.look.1 * (h - ry).max(0.0);
            let rx = (size * f.pupil).min(ry);
            g.fill_ellipse(D2D1_ELLIPSE { point: p(cx + ox, py), radiusX: rx, radiusY: ry }, dark);
        }
    }
}

/// Switching tabs from the keyboard. Ctrl+←/→ and Ctrl+1…9 always work, even while typing; plain ←/→ and
/// 1…9 work while no text field has focus (in a note or a search box they keep typing). The keys used here
/// are taken out of the frame's input so the tool never sees them.
fn keyboard_tab(ui: &mut Ui, tool: &'static str, s: &settings::Settings) -> Option<&'static str> {
    use crate::ui::Key;
    if tool == "settings" || ui.st.dropdown != 0 {
        return None;
    }
    let order = tools::ordered(&s.tool_order, &s.hidden_tools);
    if order.is_empty() {
        return None;
    }
    let typing = ui.st.focus != 0;
    let cur = order.iter().position(|t| t.id == tool).unwrap_or(0) as i32;
    let n = order.len() as i32;
    // A number jumps to that tab (past the last one: the last); arrows step and wrap around.
    let mut jump: Option<i32> = None;
    let mut step = 0;
    let mut keep = Vec::new();
    for k in std::mem::take(&mut ui.input.keys) {
        match k {
            Key::TabPrev => step -= 1,
            Key::TabNext => step += 1,
            Key::TabAt(i) => jump = Some(i as i32 - 1),
            Key::Left if !typing => step -= 1,
            Key::Right if !typing => step += 1,
            other => keep.push(other),
        }
    }
    ui.input.keys = keep;
    if !typing && ui.input.chars.chars().any(|c| ('1'..='9').contains(&c)) {
        let mut rest = String::new();
        for c in std::mem::take(&mut ui.input.chars).chars() {
            match c.to_digit(10) {
                Some(d @ 1..=9) => jump = Some(d as i32 - 1),
                _ => rest.push(c),
            }
        }
        ui.input.chars = rest;
    }
    let i = match jump {
        Some(j) => j.min(n - 1),
        None if step != 0 => (cur + step).rem_euclid(n),
        None => return None,
    };
    let id = order[i as usize].id;
    (id != tool).then_some(id)
}

pub struct Now {
    pub year: u16,
    pub month: usize,
    pub day: u16,
    pub dow: usize,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
}

pub fn now() -> Now {
    let t = unsafe { GetLocalTime() };
    Now {
        year: t.wYear,
        month: t.wMonth as usize,
        day: t.wDay,
        dow: t.wDayOfWeek as usize,
        hour: t.wHour,
        minute: t.wMinute,
        second: t.wSecond,
    }
}

/// Music in the closed notch: art (12 + 22 + 10) on the left, equalizer (14 gap + bars + 16) on the right.
const MUSIC_LEFT: f32 = 44.0;
const MUSIC_RIGHT: f32 = 48.0;
/// Widest the closed notch grows for a long lyric line.
const MUSIC_MAX_W: f32 = 640.0;

/// The line the closed notch shows while music plays: the current lyric line, else the title.
fn music_text() -> String {
    if settings::shared().lyrics_in_notch {
        if let Some((line, _)) = model::lyric_line() {
            return line;
        }
    }
    model::now_playing().map(|np| np.title.clone()).unwrap_or_default()
}

/// Closed size from Settings → Display "Notch size".
fn idle_size() -> (f32, f32) {
    match settings::shared().notch_size.as_str() {
        "ExtraSmall" => (128.0, 29.0),
        "Small" => (156.0, 32.0),
        "Large" => (236.0, 42.0),
        "ExtraLarge" => (266.0, 46.0),
        _ => (196.0, 36.0),
    }
}

pub fn theme() -> Theme {
    crate::theme::current()
}

impl Notch {
    pub fn new(gfx: Gfx, win_x: i32, win_y: i32) -> Result<Self> {
        let (w, h) = idle_size();
        let s = settings::shared();
        Ok(Self {
            gfx,
            win_x,
            shove: Spring::new(0.0, 900.0, 40.0),
            dodging: None,
            win_y,
            width: Spring::new(w, 260.0, 29.0),
            height: Spring::new(h, 260.0, 29.0),
            alert_mix: Spring::new(0.0, 150.0, 23.0),
            level: Spring::new(0.0, 320.0, 36.0),
            idle_swap: Spring::new(1.0, 120.0, 22.0),
            expanded: false,
            pinned: false,
            tool: tools::find(if s.last_tool.is_empty() { "home" } else { &s.last_tool }).id,
            back: None,
            tool_anim: None,
            alert: None,
            queue: VecDeque::new(),
            alert_until: None,
            input: Input { mouse: (f32::NAN, f32::NAN), ..Default::default() },
            ui: UiState::default(),
            views: views::Views::default(),
            last_frame: None,
            idle_index: 0,
            eyes: None,
            eyes_kind: 0,
            wants_keyboard: false,
            shot: None,
            concealed: false,
            conceal: Spring::new(1.0, 200.0, 28.0),
            busy: false,
            music_line: String::new(),
            tab_hits: Vec::new(),
            pin_hit: Rect::default(),
            panel: (0.0, 0.0, 1.0),
        })
    }

    fn capsule_style(&self) -> bool {
        settings::shared().notch_style == "Capsule"
    }

    pub fn animating(&self) -> bool {
        self.width.moving() || self.height.moving() || self.alert_mix.moving() || self.level.moving() || self.idle_swap.moving() || self.conceal.moving() || self.shove.moving() || self.busy
    }

    pub fn expanded(&self) -> bool {
        self.expanded
    }

    fn panel_scale() -> f32 {
        settings::shared().panel_scale.clamp(0.7, 1.0) as f32
    }

    fn open_size(&self) -> (f32, f32) {
        let s = Self::panel_scale();
        ((CONTENT_W + 2.0 * SIDE) * s, (HEADER_H + tools::height(self.tool) + bottom_h(self.tool)) * s)
    }

    pub fn expand(&mut self) {
        if self.expanded {
            return;
        }
        self.expanded = true;
        self.alert = None;
        self.alert_until = None;
        self.alert_mix.target = 0.0;
        let (w, h) = self.open_size();
        self.width.target = w;
        self.height.target = h;
        views::shown(self.tool, &mut self.views);
        // Opening the panel: the tool's cards come in one by one too.
        self.tool_anim = Some((Instant::now(), 0.0));
        self.busy = true;
    }

    pub fn collapse(&mut self) {
        if !self.expanded || self.pinned {
            return;
        }
        self.expanded = false;
        self.ui.focus = 0;
        self.ui.dropdown = 0;
        self.ui.overlay = None;
        self.resize_closed();
        if !self.queue.is_empty() {
            self.next_alert();
        }
    }

    /// Whether the open panel should stay open although the pointer left (typing, a list open).
    pub fn holding(&self) -> bool {
        self.pinned || self.ui.dropdown != 0 || (self.ui.focus != 0 && self.ui.edits.get(&self.ui.focus).is_some_and(|e| !e.text.is_empty()))
    }

    pub fn show_tool(&mut self, id: &'static str) {
        // Alarms live in the Timers tab (its Alarm segment).
        let alarm = id == "alarm";
        let id = if alarm { "timer" } else { id };
        if id == "settings" && self.tool != "settings" {
            self.back = Some(self.tool);
        }
        // Slide in from the side of the tab bar the new tool sits on (straight up when the same / Settings).
        let s = settings::shared();
        let order = tools::ordered(&s.tool_order, &s.hidden_tools);
        let at = |t: &str| order.iter().position(|x| x.id == t);
        let dir = match (at(self.tool), at(id)) {
            (Some(a), Some(b)) if a != b => if b > a { 1.0 } else { -1.0 },
            _ => 0.0,
        };
        self.tool_anim = Some((Instant::now(), dir));
        // Keep drawing frames from now on, or the transition would be over before the next one.
        self.busy = true;
        self.tool = id;
        self.ui.focus = 0;
        self.ui.dropdown = 0;
        self.ui.overlay = None;
        if id != "settings" && std::env::var_os("FATHNOTCH_DEMO").is_none() {
            settings::update(|s| s.last_tool = id.to_string());
        }
        views::shown(id, &mut self.views);
        if alarm {
            self.views.timer.show_alarm = true;
        }
        if self.expanded {
            let (w, h) = self.open_size();
            self.width.target = w;
            self.height.target = h;
        }
    }

    /// Width/height of the closed notch for what's going on (music, timer, idle).
    fn resize_closed(&mut self) {
        let (w, h) = idle_size();
        let wide = match model::activity() {
            model::Activity::Music => {
                let text = music_text();
                self.music_width(&text, w, idle_font(h))
            }
            model::Activity::Timer => w + if w < 170.0 { 34.0 } else { 44.0 },
            model::Activity::Idle => match settings::shared().idle_layout.as_str() {
                "Row" => {
                    let fs = idle_font(h);
                    let items = model::idle_items();
                    let total: f32 = items.iter().map(|i| 20.0 + self.gfx.measure(&i.value, fs, SEMIBOLD)).sum::<f32>()
                        + 18.0 * (items.len() as f32 - 1.0).max(0.0);
                    (total + 40.0).clamp(w, 640.0)
                }
                "Split" => w + if w < 170.0 { 120.0 } else { 150.0 },
                // One width for the whole rotation, sized for the widest item, so the notch doesn't grow and
                // shrink as items change: glyph + gap + value + side margins (as the C# app).
                _ => {
                    let fs = idle_font(h);
                    let widest = model::idle_items()
                        .iter()
                        .map(|i| {
                            let template = match i.id {
                                "clock" => "00:00",
                                "weather" => "-00°",
                                "prayer" => tr("Maghrib 00:00"),
                                "todos" => tr("00 open"),
                                "battery" => "100%",
                                _ => "",
                            };
                            self.gfx.measure(&i.value, fs, SEMIBOLD).max(self.gfx.measure(template, fs, SEMIBOLD))
                        })
                        .fold(0.0f32, f32::max);
                    (fs + 2.0 + 16.0 + widest + 25.0).ceil().clamp(w, 640.0)
                }
            },
        };
        self.width.target = wide;
        self.height.target = h;
    }

    /// Closed width while music plays: art + the whole line (lyric or title) + equalizer, never less than
    /// the old fixed music width, so the line is shown in full and the notch follows its length.
    fn music_width(&self, text: &str, idle_w: f32, fs: f32) -> f32 {
        let min = idle_w + if idle_w < 170.0 { 120.0 } else { 150.0 };
        let tw = self.gfx.measure(text, fs, SEMIBOLD);
        (MUSIC_LEFT + tw + MUSIC_RIGHT).ceil().clamp(min, MUSIC_MAX_W)
    }

    /// Idle items changed length (the weather arrived, the next prayer moved on): fit the closed notch
    /// again so nothing is cut off.
    pub fn refit_idle(&mut self) {
        if self.expanded || self.alert.is_some() || model::activity() != model::Activity::Idle {
            return;
        }
        let before = self.width.target;
        self.resize_closed();
        if (self.width.target - before).abs() > 0.5 {
            self.busy = true;
        }
    }

    /// The closed notch's content changed (music started, a timer ran out…).
    pub fn activity_changed(&mut self) {
        if !self.expanded && self.alert.is_none() {
            self.resize_closed();
        }
    }

    // ------------------------------------------------------------------ alerts

    /// Queues an alert. The same key as the one showing refreshes it in place (volume steps).
    pub fn push_alert(&mut self, alert: Alert) {
        if self.expanded {
            return;
        }
        if let Some(cur) = &self.alert {
            if cur.key == alert.key {
                if let AlertKind::Level { level, .. } = alert.kind {
                    self.level.target = level;
                }
                self.alert = Some(alert.clone());
                self.alert_until = Some(Instant::now() + alert.duration);
                self.fit_alert();
                return;
            }
        }
        self.queue.retain(|a| a.key != alert.key);
        self.queue.push_back(alert);
        while self.queue.len() > 8 {
            self.queue.pop_front();
        }
        if self.alert.is_none() {
            self.next_alert();
        }
    }

    fn next_alert(&mut self) {
        let Some(a) = self.queue.pop_front() else { return };
        if let AlertKind::Level { level, .. } = a.kind {
            self.level.value = 0.0;
            self.level.target = level;
        }
        self.alert_until = Some(Instant::now() + a.duration);
        self.alert = Some(a);
        self.alert_mix.target = 1.0;
        self.fit_alert();
    }

    fn fit_alert(&mut self) {
        let Some(a) = &self.alert else { return };
        let (w, h) = match &a.kind {
            AlertKind::Level { .. } => (LEVEL_W, MINI_H),
            AlertKind::Compact { title, sub, .. } => {
                let tw = self.gfx.measure(title, 13.0, SEMIBOLD);
                let sw = if sub.is_empty() { 0.0 } else { 8.0 + self.gfx.measure(sub, 12.0, REGULAR) };
                ((tw + sw + 66.0).ceil().clamp(idle_size().0.max(180.0), 380.0), MINI_H)
            }
            AlertKind::Full { .. } => (PEEK_W, PEEK_H),
        };
        self.width.target = w;
        self.height.target = h;
    }

    /// Ends the alert on screen (timeout or dismiss); the next queued one takes its place.
    pub fn end_alert(&mut self) {
        self.alert = None;
        self.alert_until = None;
        if !self.expanded && !self.queue.is_empty() {
            self.next_alert();
            return;
        }
        self.alert_mix.target = 0.0;
        if !self.expanded {
            self.resize_closed();
        }
    }

    /// The app behind the alert on screen (another app's notification): (AppUserModelId, name).
    pub fn alert_app(&self) -> Option<(String, String)> {
        match &self.alert {
            Some(Alert { kind: AlertKind::Full { app, .. }, .. }) => app.clone(),
            _ => None,
        }
    }

    pub fn alert_open_target(&self) -> Option<&'static str> {
        match &self.alert {
            Some(Alert { kind: AlertKind::Full { open, .. }, .. }) => *open,
            _ => None,
        }
    }

    // ------------------------------------------------------------------ idle rotation

    pub fn shove_px(&self) -> i32 {
        (self.shove.value * self.gfx.scale).round() as i32
    }

    /// Settings → General "Dodge the pointer": a pointer coming at the closed notch from the left or right
    /// pushes it along (keeping a gap); it can still be caught from below, and drifts home afterwards.
    /// `(x, y)` is the pointer in canvas DIP measured from the notch's home position.
    pub fn dodge(&mut self, x: f32, y: f32) {
        const GAP: f32 = 28.0;
        const BAND: f32 = 24.0;
        if self.expanded || self.alert.is_some() || !settings::shared().repel_cursor {
            self.dodging = None;
            self.shove.target = 0.0;
            return;
        }
        let b = self.bounds();
        let half = b.w / 2.0;
        let home = b.cx();
        let centre = home + self.shove.value;
        let in_band = y <= b.bottom() + BAND && y >= 0.0;
        if !in_band {
            self.dodging = None;
            self.shove.target = 0.0;
            return;
        }
        let rel = x - centre;
        match self.dodging {
            None => {
                // Start only when the pointer arrives beside the notch (not from below, inside it).
                if rel.abs() <= half + GAP && rel.abs() >= half - 4.0 {
                    self.dodging = Some(rel.signum());
                }
            }
            Some(side) => {
                // Keep the notch's near edge GAP away from the pointer, on the side it came from.
                let want = x - side * (half + GAP) - home;
                let max = 600.0;
                if (side > 0.0 && rel < half + GAP) || (side < 0.0 && rel > -(half + GAP)) {
                    self.shove.target = want.clamp(-max, max);
                } else if (x - (home + self.shove.target)).abs() > half + GAP * 3.0 {
                    // Pointer backed off: drift home.
                    self.shove.target = 0.0;
                }
            }
        }
    }

    /// A moment of the cat looking around (Settings → Display "Idle eyes").
    pub fn show_eyes(&mut self) {
        let s = settings::shared();
        if self.expanded || self.alert.is_some() || model::activity() != model::Activity::Idle || !s.idle_eyes {
            return;
        }
        // Hidden notch: the eyes peek out only if Settings → "Eyes while hidden" is on.
        if self.concealed && !s.eyes_when_hidden {
            return;
        }
        // Take turns through the animations (dev: FATHNOTCH_EYES_KIND picks one).
        self.eyes_kind = match std::env::var("FATHNOTCH_EYES_KIND").ok().and_then(|v| v.parse().ok()) {
            Some(k) => k,
            None => (self.eyes_kind + 1) % EYE_KINDS,
        };
        self.eyes = Some(Instant::now());
        self.busy = true;
    }

    pub fn rotate_idle(&mut self) {
        if self.expanded || self.alert.is_some() || model::activity() != model::Activity::Idle {
            return;
        }
        if model::idle_items().len() > 1 {
            self.idle_index += 1;
            self.idle_swap.value = 0.0;
            self.idle_swap.target = 1.0;
        }
    }

    // ------------------------------------------------------------------ frames

    /// Advances the springs to now; returns true while anything is still moving.
    pub fn step(&mut self) -> bool {
        let now = Instant::now();
        let dt = self.last_frame.map_or(1.0 / 60.0, |t| (now - t).as_secs_f32()).min(0.05);
        self.last_frame = Some(now);
        let mut moving = false;
        moving |= self.width.step(dt);
        moving |= self.height.step(dt);
        moving |= self.alert_mix.step(dt);
        moving |= self.level.step(dt);
        moving |= self.idle_swap.step(dt);
        moving |= self.conceal.step(dt);
        moving |= self.shove.step(dt);
        if !moving && !self.busy {
            self.last_frame = None;
        }
        moving
    }

    /// The notch's bounds in canvas DIP (for hover checks).
    pub fn bounds(&self) -> Rect {
        let (o, _) = self.outline();
        Rect::new(o.x - o.ear, 0.0, o.w + 2.0 * o.ear, o.y + o.h)
    }

    fn progress(&self) -> (f32, f32) {
        let peek = clamp01(self.alert_mix.value);
        let (_, idle_h) = idle_size();
        let (_, open_h) = self.open_size();
        let p = if self.alert.is_some() && !self.expanded {
            0.0
        } else {
            clamp01((self.height.value - idle_h) / (open_h - idle_h).max(1.0))
        };
        (p, peek)
    }

    fn outline(&self) -> (Outline, (f32, f32)) {
        let (p, peek) = self.progress();
        let cap = self.capsule_style();
        let (_, idle_h) = idle_size();
        let peek_r = match &self.alert {
            Some(Alert { kind: AlertKind::Full { .. }, .. }) => RADIUS_PEEK,
            _ => MINI_H / 2.0,
        };
        let ear_fade = if cap { 0.0 } else { clamp01(1.0 - 2.0 * peek) };
        let ear = (EAR_CLOSED + (EAR_OPEN - EAR_CLOSED) * p) * ear_fade;
        let closed_r = if cap { idle_h / 2.0 } else { RADIUS_CLOSED };
        let radius = closed_r + (RADIUS_OPEN - closed_r) * p + (peek_r - closed_r) * peek;
        let top_radius = if cap { radius } else { radius * clamp01(2.0 * peek - 1.0) };
        let drop = if cap { GAP } else { GAP * peek };
        let w = self.width.value.max(60.0);
        let h = self.height.value.max(20.0);
        let x = CANVAS_W / 2.0 - w / 2.0;
        (Outline { x, y: drop, w, h, ear, radius, top_radius }, (p, peek))
    }

    /// Pointer position (canvas DIP) → panel coordinates.
    pub fn to_panel(&self, x: f32, y: f32) -> (f32, f32) {
        let (ox, oy, s) = self.panel;
        (ox + (x - ox) / s, oy + (y - oy) / s)
    }

    /// Handles a click outside widgets: tabs and pin. Returns true if handled.
    fn shell_click(&mut self) -> bool {
        if !self.input.pressed || self.input.consumed {
            return false;
        }
        let m = self.input.mouse;
        if self.pin_hit.contains(m) {
            self.pinned = !self.pinned;
            self.input.consumed = true;
            return true;
        }
        if let Some((_, id)) = self.tab_hits.iter().find(|(r, _)| r.contains(m)).copied() {
            self.input.consumed = true;
            self.show_tool(id);
            return true;
        }
        false
    }

    pub fn render(&mut self, hwnd: HWND) -> Result<()> {
        let mut theme = theme();
        let (o, (p, peek)) = self.outline();
        self.busy = false;
        self.wants_keyboard = false;
        // Concealed: fade the closed pill out entirely (transparent pixels let clicks through).
        // The peeking eyes fade a hidden notch in for their few seconds, then it fades away again.
        let eyes_out = self.eyes.is_some_and(|s| s.elapsed().as_secs_f32() < EYES_SECS);
        self.conceal.target = if self.concealed && !self.expanded && self.alert.is_none() && !eyes_out { 0.0 } else { 1.0 };
        // Teleprompter: Settings → its opacity slider makes the panel see-through while it's open (as C#).
        if self.expanded && self.tool == "teleprompter" {
            theme.pill.a *= settings::shared().prompter_opacity.clamp(0.2, 1.0) as f32;
        }
        let vis = clamp01(self.conceal.value);
        if vis < 0.01 {
            if !eyes_out {
                self.eyes = None;
            }
            self.gfx.begin_partial(CANVAS_W / 2.0 - 2.0, 4.0, 4.0);
            self.input.end_frame();
            return match &self.shot {
                Some(path) => self.gfx.save_png(path),
                None => self.gfx.present(hwnd, self.win_x + self.shove_px(), self.win_y),
            };
        }
        if vis < 0.999 {
            for c in [&mut theme.pill, &mut theme.stroke, &mut theme.text, &mut theme.subtext, &mut theme.accent, &mut theme.surface2, &mut theme.track] {
                c.a *= vis;
            }
        }
        {
            let g = &self.gfx;
            // Closed (no panel showing): only the strip around the pill is drawn and put on screen.
            if self.expanded || p > 0.001 || self.shot.is_some() {
                g.begin();
            } else {
                // Box around the pill (ears and outline included).
                g.begin_partial(o.x - o.ear - 4.0, o.w + 2.0 * o.ear + 8.0, o.y + o.h + 4.0);
            }
            let geo = o.build(&g.factory)?;
            g.fill_geometry(&geo.clone().into(), theme.pill);
            if settings::shared().notch_border {
                g.stroke_geometry(&geo.into(), theme.stroke, 1.0);
            }
            g.push_clip(Rect::new(o.x, o.y, o.w, o.h).d2d());
        }

        let idle = clamp01(1.0 - p * 4.0 - peek * 3.0);
        if idle > 0.0 {
            self.draw_closed(&theme, &o, idle);
        }
        let shown = clamp01((peek - 0.45) / 0.55);
        if shown > 0.0 && !self.expanded {
            self.draw_alert(&theme, &o, shown);
        }

        self.tab_hits.clear();
        let reveal = ease_out(clamp01((p - 0.3) / 0.7));
        if reveal > 0.0 && self.expanded {
            self.draw_panel(&theme, &o, reveal);
        } else if reveal > 0.0 {
            // Closing: let the content fade with the shape.
            self.draw_panel(&theme, &o, reveal * 0.9);
        }
        self.gfx.pop_clip();
        self.input.end_frame();
        match &self.shot {
            Some(path) => self.gfx.save_png(path),
            None => self.gfx.present(hwnd, self.win_x + self.shove_px(), self.win_y),
        }
    }

    fn draw_closed(&mut self, t: &Theme, o: &Outline, a: f32) {
        let g = &self.gfx;
        let (_, idle_h) = idle_size();
        let r = Rect::new(o.x, o.y, o.w, idle_h.min(o.h));
        let fs = idle_font(idle_h);
        match model::activity() {
            model::Activity::Music => {
                if let Some(np) = model::now_playing() {
                    // Art on the left, title or the current lyric line, equalizer on the right.
                    let art = Rect::new(r.x + 12.0, r.cy() - 11.0, 22.0, 22.0);
                    if g.has_image(np.art_id) {
                        g.image(np.art_id, art.d2d(), 6.0, a);
                    } else {
                        g.fill_round(art.d2d(), 6.0, with_alpha(t.surface2, a));
                        g.icon("MusicNote", art.x + 4.0, art.y + 4.0, 14.0, with_alpha(t.subtext, a));
                    }
                    let text_r = Rect::new(art.right() + 10.0, r.y, r.w - MUSIC_LEFT - MUSIC_RIGHT, r.h);
                    // The line changed (next lyric, next song): the notch grows or shrinks to fit it.
                    let line_now = music_text();
                    if line_now != self.music_line {
                        let (iw, _) = idle_size();
                        let target = self.music_width(&line_now, iw, fs);
                        self.music_line = line_now;
                        if !self.expanded && self.alert.is_none() {
                            self.width.target = target;
                        }
                    }
                    let g = &self.gfx;
                    // Laid out at its full width and only clipped by the notch, so it never turns into "…"
                    // while the notch is still growing; only a line longer than the widest notch is trimmed.
                    let lay = |s: &str| {
                        let full = g.measure(s, fs, SEMIBOLD);
                        let w = if full <= MUSIC_MAX_W - MUSIC_LEFT - MUSIC_RIGHT { full + 4.0 } else { text_r.w };
                        (Rect::new(text_r.x, text_r.y, w, text_r.h), full)
                    };
                    g.push_clip(text_r.d2d());
                    match model::lyric_line() {
                        Some((line, prog)) if settings::shared().lyrics_in_notch => {
                            // Karaoke: the sung part bright, the rest dim.
                            let (lr, full) = lay(&line);
                            g.text(&line, fs, SEMIBOLD, lr.d2d(), Align::Left, with_alpha(t.subtext, a));
                            let w = full.min(lr.w);
                            g.push_clip(Rect::new(text_r.x, text_r.y, w * prog, text_r.h).d2d());
                            g.text(&line, fs, SEMIBOLD, lr.d2d(), Align::Left, with_alpha(t.text, a));
                            g.pop_clip();
                            self.busy = true;
                        }
                        _ => {
                            let (lr, _) = lay(&np.title);
                            g.text(&np.title, fs, SEMIBOLD, lr.d2d(), Align::Left, with_alpha(t.text, a));
                        }
                    }
                    g.pop_clip();
                    // Equalizer: four bars bouncing while playing.
                    let ex = r.right() - 30.0;
                    let ms = Instant::now().elapsed().as_millis(); // placeholder replaced below
                    let _ = ms;
                    let phase = model::clock_secs() as f32;
                    for i in 0..4 {
                        let hgt = if np.playing { 4.0 + 9.0 * (0.5 + 0.5 * (phase * (5.0 + i as f32 * 1.7) + i as f32).sin()) } else { 3.0 };
                        let x = ex + i as f32 * 4.5;
                        g.line(x, r.cy() + hgt / 2.0, x, r.cy() - hgt / 2.0, with_alpha(t.accent, a), 2.4);
                    }
                    if np.playing {
                        self.busy = true;
                    }
                }
            }
            model::Activity::Timer => {
                if let Some((label, remaining)) = model::timer_text() {
                    g.icon("Clock", r.x + 14.0, r.cy() - 8.0, 16.0, with_alpha(t.accent, a));
                    g.text(&label, fs - 1.0, MEDIUM, Rect::new(r.x + 38.0, r.y, 100.0, r.h).d2d(), Align::Left, with_alpha(t.subtext, a));
                    g.text(&remaining, fs, SEMIBOLD, Rect::new(r.x, r.y, r.w - 16.0, r.h).d2d(), Align::Right, with_alpha(t.text, a));
                }
            }
            model::Activity::Idle => {
                if let Some(start) = self.eyes {
                    let el = start.elapsed().as_secs_f32();
                    if el < EYES_SECS {
                        draw_eyes(g, &r, el, a, t, self.eyes_kind);
                        self.busy = true;
                        return;
                    }
                    // Hidden notch fading out after the eyes: show nothing, not the idle items
                    // (render clears the eyes once it is gone).
                    if self.concealed {
                        return;
                    }
                    self.eyes = None;
                }
                let items = model::idle_items();
                if items.is_empty() {
                    return;
                }
                let layout = settings::shared().idle_layout.clone();
                let k = ease_out(clamp01(self.idle_swap.value));
                let dy = (1.0 - k) * 8.0;
                match layout.as_str() {
                    // Every item side by side.
                    "Row" => {
                        let widths: Vec<f32> = items.iter().map(|i| 20.0 + g.measure(&i.value, fs, SEMIBOLD)).collect();
                        let total: f32 = widths.iter().sum::<f32>() + 18.0 * (items.len() as f32 - 1.0);
                        let mut x = r.cx() - total / 2.0;
                        for (i, (it, w)) in items.iter().zip(&widths).enumerate() {
                            if i > 0 {
                                g.fill_circle(x - 9.0, r.cy(), 1.5, with_alpha(t.track, a));
                            }
                            g.icon(it.icon, x, r.cy() - 7.0, 14.0, with_alpha(if it.accent { t.accent } else { t.subtext }, a));
                            g.text(&it.value, fs, SEMIBOLD, Rect::new(x + 18.0, r.y, *w, r.h).d2d(), Align::Left, with_alpha(t.text, a));
                            x += w + 18.0;
                        }
                    }
                    // First item on the left, the others rotate on the right.
                    "Split" if items.len() > 1 => {
                        let first = &items[0];
                        g.icon(first.icon, r.x + 14.0, r.cy() - 8.0, 16.0, with_alpha(t.subtext, a));
                        g.text(&first.value, fs, SEMIBOLD, Rect::new(r.x + 36.0, r.y, r.w / 2.0 - 36.0, r.h).d2d(), Align::Left, with_alpha(t.text, a));
                        let rest = &items[1..];
                        let it = &rest[self.idle_index % rest.len()];
                        let aa = a * k;
                        let vw = g.measure(&it.value, fs, SEMIBOLD);
                        g.icon(it.icon, r.right() - 16.0 - vw - 22.0, r.cy() - 8.0 + dy, 16.0, with_alpha(t.subtext, aa));
                        g.text(&it.value, fs, SEMIBOLD, Rect::new(r.x, r.y + dy, r.w - 16.0, r.h).d2d(), Align::Right, with_alpha(t.text, aa));
                    }
                    // One at a time: the new item rises in.
                    _ => {
                        let it = &items[self.idle_index % items.len()];
                        let aa = a * k;
                        // Glyph in the left ear, value in the right (C#: 12 / 13 DIP side margins).
                        let gs = fs + 1.5;
                        g.icon(it.icon, r.x + 12.0, r.cy() - gs / 2.0 + dy, gs, with_alpha(if it.accent { t.accent } else { t.subtext }, aa));
                        g.text(&it.value, fs, SEMIBOLD, Rect::new(r.x + 12.0 + gs, r.y + dy, r.w - 25.0 - gs, r.h).d2d(), Align::Right, with_alpha(t.text, aa));
                    }
                }
                // Settings → Display "Lyric glow / rest glow": a soft accent glow along the bottom while resting.
                if settings::shared().rest_glow {
                    // Fades upward and toward the ends (layers of thin lines, each fainter and shorter).
                    for i in 0..6 {
                        let k = i as f32;
                        let half = r.w * (0.30 - k * 0.035);
                        let alpha = 0.045 * (1.0 - k / 6.0);
                        g.line(r.cx() - half, r.bottom() - 1.0 - k, r.cx() + half, r.bottom() - 1.0 - k, with_alpha(t.accent, a * alpha), 1.0);
                    }
                }
            }
        }
    }

    fn draw_alert(&mut self, t: &Theme, o: &Outline, a: f32) {
        let Some(alert) = self.alert.clone() else { return };
        let g = &self.gfx;
        let cy = o.y + o.h / 2.0;
        match alert.kind {
            AlertKind::Level { icon, .. } => {
                g.icon(icon, o.x + 16.0, cy - 8.0, 16.0, with_alpha(t.text, a));
                let x = o.x + 16.0 + 16.0 + 12.0;
                g.line(x, cy, x + LEVEL_BAR, cy, with_alpha(t.track, a), 6.0);
                let v = clamp01(self.level.value);
                if v > 0.005 {
                    g.line(x, cy, x + LEVEL_BAR * v, cy, with_alpha(t.text, a), 6.0);
                }
            }
            AlertKind::Compact { icon, title, sub } => {
                g.icon(icon, o.x + 16.0, cy - 8.0, 16.0, with_alpha(t.text, a));
                let x = o.x + 42.0;
                let tw = g.measure(&title, 13.0, SEMIBOLD);
                g.text(&title, 13.0, SEMIBOLD, Rect::new(x, o.y, tw + 4.0, o.h).d2d(), Align::Left, with_alpha(t.text, a));
                if !sub.is_empty() {
                    let sx = x + tw + 8.0;
                    g.text(&sub, 12.0, REGULAR, Rect::new(sx, o.y, (o.x + o.w - 16.0 - sx).max(0.0), o.h).d2d(), Align::Left, with_alpha(t.subtext, a));
                }
            }
            AlertKind::Full { icon, title, sub, app, .. } => {
                let tile = Rect::new(o.x + 18.0, cy - 20.0, 40.0, 40.0);
                g.fill_round(tile.d2d(), 11.0, with_alpha(t.surface2, a));
                // Another app's notification shows that app's icon (WhatsApp, Edge…) so it's clear who it's from;
                // until the icon has loaded (or when none is found) the bell stands in.
                let px = (28.0 * g.scale).round() as u32;
                let drawn = app.as_ref().and_then(|(id, name)| crate::services::appicons::app(id, name, px)).is_some_and(|(k, png)| {
                    g.load_image(k, &png) && {
                        g.image(k, Rect::new(tile.x + 6.0, tile.y + 6.0, 28.0, 28.0).d2d(), 0.0, a);
                        true
                    }
                });
                if !drawn {
                    g.icon(icon, tile.x + 10.0, tile.y + 10.0, 20.0, with_alpha(t.text, a));
                }
                let x = tile.right() + 13.0;
                let w = o.x + o.w - x - 50.0;
                g.text(&title, 14.0, SEMIBOLD, Rect::new(x, cy - 20.0, w, 20.0).d2d(), Align::Left, with_alpha(t.text, a));
                g.text(&sub, 12.0, REGULAR, Rect::new(x, cy + 1.0, w, 18.0).d2d(), Align::Left, with_alpha(t.subtext, a));
                let d = Rect::new(o.x + o.w - 42.0, cy - 14.0, 28.0, 28.0);
                g.fill_round(d.d2d(), 8.0, with_alpha(t.surface2, a));
                g.icon("XMark", d.x + 8.5, d.y + 8.5, 11.0, with_alpha(t.subtext, a));
                if !self.queue.is_empty() {
                    let more = format!("+{}", self.queue.len());
                    g.text(&more, 12.0, SEMIBOLD, Rect::new(d.x - 40.0, d.y, 34.0, 28.0).d2d(), Align::Right, with_alpha(t.subtext, a));
                }
            }
        }
    }

    /// Where a click on the alert landed: true for the dismiss button.
    pub fn alert_dismiss_hit(&self, x: f32, y: f32) -> bool {
        let (o, _) = self.outline();
        let cy = o.y + o.h / 2.0;
        Rect::new(o.x + o.w - 46.0, cy - 18.0, 36.0, 36.0).contains((x, y))
    }

    pub fn has_alert(&self) -> bool {
        self.alert.is_some()
    }

    /// Key of the alert on screen.
    pub fn alert_key(&self) -> Option<&str> {
        self.alert.as_ref().map(|a| a.key.as_str())
    }

    fn draw_panel(&mut self, t: &Theme, o: &Outline, reveal: f32) {
        let s = Self::panel_scale();
        // Lay out at full size, then scale into the notch (Settings → Display "Panel size").
        let ox = o.x;
        let oy = o.y;
        self.panel = (ox, oy, s);
        self.gfx.set_base(s, ox, oy);
        let tool = tools::find(self.tool);
        let tool_h = tools::height(self.tool);
        let full = Rect::new(ox, oy - (1.0 - reveal) * 6.0, CONTENT_W + 2.0 * SIDE, HEADER_H + tool_h + bottom_h(self.tool));
        let s_ = settings::shared();

        let now = now();
        let mut ui = Ui::new(&self.gfx, &mut self.input, &mut self.ui, *t, reveal);
        ui.bounds = Rect::new(full.x + 4.0, full.y + 6.0, full.w - 8.0, full.h - 12.0);
        ui.st.caret_on = (model::clock_secs() * 2.0) as u64 % 2 == 0;

        // Header: title, date and time in the middle (Settings → Display "Header clock"), pin on the right.
        let head = Rect::new(full.x + SIDE + 6.0, full.y + 6.0, CONTENT_W - 12.0, HEADER_H - 8.0);
        let mut back_clicked = false;
        if self.tool == "settings" {
            let back = Rect::new(head.x - 6.0, head.cy() - 15.0, 30.0, 30.0);
            back_clicked = ui.icon_button(back, "ChevronLeft", 14.0, t.text);
            ui.text(tr(tool.title), Rect::new(head.x + 28.0, head.y, 260.0, head.h), 17.0, SEMIBOLD, t.text, Align::Left);
        } else {
            ui.text(tr(tool.title), Rect::new(head.x, head.y, 260.0, head.h), 17.0, SEMIBOLD, t.text, Align::Left);
        }
        // Home shows its own date and clock, so the header one stays off there.
        if s_.header_clock && self.tool != "home" {
            let date = format!("{} {} {} {}", crate::i18n::day3(now.dow), now.day, crate::i18n::month3(now.month), now.year);
            let time = format!("{:02}:{:02}", now.hour, now.minute);
            let dw = ui.measure(&date, 14.0, REGULAR);
            let tw = ui.measure(&time, 14.0, BOLD);
            let x0 = head.cx() - (dw + 12.0 + tw) / 2.0;
            ui.text(&date, Rect::new(x0, head.y, dw + 2.0, head.h), 14.0, REGULAR, t.subtext, Align::Left);
            ui.text(&time, Rect::new(x0 + dw + 12.0, head.y, tw + 2.0, head.h), 14.0, BOLD, t.text, Align::Left);
        }
        let pin = Rect::new(head.right() - 30.0, head.cy() - 15.0, 30.0, 30.0);
        if ui.hovered(pin) {
            ui.fill(pin, 8.0, t.hover);
        }
        ui.icon(if self.pinned { "Pin" } else { "Pin" }, pin.cx(), pin.cy(), 15.0, if self.pinned { t.accent } else { t.subtext });
        self.pin_hit = pin;

        // Tool content.
        let content = Rect::new(full.x + SIDE, full.y + HEADER_H, CONTENT_W, tool_h);
        let mut next_tool = None;
        let key_tool = keyboard_tab(&mut ui, self.tool, &s_);
        // Every tool comes in the same smooth way: it fades in while sliding the last few pixels from the
        // side of its tab (or rising, when opened straight on it).
        // On top, its cards and rows come in one after another, each fading in and rising into place.
        let mut staggering = false;
        let shift = match self.tool_anim {
            Some((at, dir)) => {
                let secs = settings::anim_secs(at.elapsed().as_secs_f32());
                let k = (secs / TOOL_ANIM_SECS).clamp(0.0, 1.0);
                ui.animate = true;
                if self.tool != "prayer" {
                    // Prayer has its own entrance.
                    ui.start_stagger(secs);
                    staggering = true;
                }
                if k >= 1.0 && secs > TOOL_ANIM_SECS + 0.9 {
                    self.tool_anim = None;
                }
                let e = 1.0 - (1.0 - k).powi(3);
                dir * 28.0 * (1.0 - e)
            }
            None => 0.0,
        };
        let content = Rect::new(content.x + shift, content.y, content.w, content.h);
        if shift != 0.0 {
            ui.push_clip(Rect::new(full.x + SIDE - 4.0, full.y + HEADER_H - 4.0, CONTENT_W + 8.0, tool_h + 8.0));
        }
        views::draw(self.tool, &mut ui, &mut self.views, content, &mut next_tool);
        if shift != 0.0 {
            ui.pop_clip();
        }
        if staggering && !ui.end_stagger() && self.tool_anim.is_some_and(|(at, _)| settings::anim_secs(at.elapsed().as_secs_f32()) > TOOL_ANIM_SECS) {
            self.tool_anim = None;
        }
        if key_tool.is_some() {
            next_tool = key_tool;
        }

        // Resizable tools: a grab handle under the content; drag to change the height.
        let mut resized = None;
        if tools::resizable(self.tool) {
            const RESIZE: u64 = crate::ui::id("shell.resize");
            let grip = Rect::new(content.cx() - 30.0, content.bottom() + 1.0, 60.0, 9.0);
            let hot = ui.hovered(grip) || ui.st.active == RESIZE;
            ui.fill(Rect::new(grip.cx() - 18.0, grip.cy() - 1.5, 36.0, 3.0), 1.5, if hot { t.subtext } else { t.track });
            if ui.clicked(grip) {
                ui.st.active = RESIZE;
                ui.st.values.insert(RESIZE, ui.mouse().1 - tool_h);
            }
            if ui.st.active == RESIZE {
                let offset = ui.st.values.get(&RESIZE).copied().unwrap_or(0.0);
                let h = ui.mouse().1 - offset;
                if ui.input.down {
                    resized = Some((h, false));
                } else {
                    ui.st.active = 0;
                    resized = Some((h, true));
                }
            }
        }

        // Tab bar: one icon (or dot) per tool, Settings after a divider.
        let bar = Rect::new(full.x + SIDE, full.y + HEADER_H + tool_h + 12.0, CONTENT_W, 36.0);
        // Settings has no tab bar (as the C# app): its back button returns to the tool you came from.
        let show_tabs = self.tool != "settings";
        let list = if show_tabs { tools::ordered(&s_.tool_order, &s_.hidden_tools) } else { Vec::new() };
        let dots = s_.tab_style == "Dots";
        let cell = if dots { 16.0 } else { 36.0 };
        let gap = if dots { 4.0 } else { 3.0 };
        let total = list.len() as f32 * (cell + gap) + 18.0 + cell;
        let mut x = bar.cx() - total / 2.0;
        let mut hits = Vec::new();
        for tl in list.iter().copied().chain(show_tabs.then_some(&tools::SETTINGS)) {
            if tl.id == "settings" {
                ui.fill(Rect::new(x + 6.0, bar.cy() - 9.0, 1.0, 18.0), 0.5, t.track);
                x += 15.0;
            }
            let r = Rect::new(x, bar.y, cell, 30.0);
            let sel = tl.id == self.tool;
            if dots {
                let w = if sel { 14.0 } else { 6.0 };
                let c = if sel { t.text } else if ui.hovered(r) { t.subtext } else { t.track };
                ui.fill(Rect::new(r.cx() - w / 2.0, r.cy() - 3.0, w, 6.0), 3.0, c);
            } else {
                if sel {
                    ui.fill(r, 9.0, t.surface2);
                } else if ui.hovered(r) {
                    ui.fill(r, 9.0, t.hover);
                }
                ui.icon(tl.icon, r.cx(), r.cy(), 15.0, if sel { t.text } else { t.subtext });
            }
            hits.push((r, tl.id));
            x += cell + gap;
        }
        ui.finish();
        let animate = ui.animate;
        let keyboard = ui.wants_keyboard;
        drop(ui);
        self.tab_hits = hits;
        self.busy |= animate;
        self.wants_keyboard |= keyboard;
        self.shell_click();
        if back_clicked {
            next_tool = Some(self.back.unwrap_or("home"));
        }
        if let Some(id) = next_tool {
            self.show_tool(id);
        }
        if let Some((h, save)) = resized {
            tools::set_height(self.tool, h, save);
            // Follow the pointer directly while dragging (no spring lag).
            let (_, oh) = self.open_size();
            self.height.target = oh;
            self.height.value = oh;
            self.busy = true;
        }
        self.gfx.set_base(1.0, 0.0, 0.0);
        let _ = (LIGHT, color(0, 0.0));
    }
}
