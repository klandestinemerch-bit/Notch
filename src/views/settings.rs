//! Settings: a nav column on the left (search box, one entry per page, version at the bottom) and the selected
//! page on the right, scrollable, as cards of rows: title and description on the left, the control on the right.
//! Searching lists every page's matching rows instead. The pages themselves are in `settings_pages.rs`.
//!
//! Every control reads `settings::shared()` and writes with `settings::update`, so the shell picks changes up
//! on its next frame (theme, size, style...). Sizes follow the .NET build's SettingsView (XAML DIPs).
//! Anything that touches the system (monitors, files, running apps) is read when the page opens, not per frame.

#[path = "settings_pages.rs"]
mod pages;

use std::sync::{Arc, Mutex};

use crate::gfx::{with_alpha, Align, BOLD, MEDIUM, REGULAR, SEMIBOLD};
use crate::i18n::tr;
use crate::services::settings::{self, Settings};
use crate::ui::{id, Key, Rect, Ui};

pub(super) struct Page {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    /// Caption shown above the entry in the nav ("" = just a gap).
    pub caption: Option<&'static str>,
    pub draw: fn(&mut Pg),
}

pub(super) const PAGES: &[Page] = &[
    Page { id: "general", name: "General", icon: "Adjust", caption: None, draw: pages::general },
    Page { id: "appearance", name: "Appearance", icon: "Palette", caption: None, draw: pages::appearance },
    Page { id: "display", name: "Display", icon: "Monitor", caption: Some("NOTCH"), draw: pages::display },
    Page { id: "tabs", name: "Tabs", icon: "Grid", caption: None, draw: pages::tabs },
    Page { id: "alerts", name: "Alerts", icon: "Bell", caption: None, draw: pages::alerts },
    Page { id: "keyboard", name: "Keyboard", icon: "Keyboard", caption: None, draw: pages::keyboard },
    Page { id: "music", name: "Music", icon: "MusicNote", caption: Some("TOOLS"), draw: pages::music },
    Page { id: "calendar", name: "Calendar", icon: "CalendarDays", caption: None, draw: pages::calendar },
    Page { id: "prayer", name: "Prayer", icon: "Moon", caption: None, draw: pages::prayer },
    Page { id: "weather", name: "Weather", icon: "Cloud", caption: None, draw: pages::weather },
    Page { id: "todos", name: "To-Dos", icon: "ListBullet", caption: None, draw: pages::todos },
    Page { id: "data", name: "Data", icon: "ClipboardList", caption: None, draw: pages::data },
];

/// Results that background jobs (geocoding, TickTick, calendar sync, app lists) leave for the page.
#[derive(Default)]
pub(super) struct Shared {
    pub prayer_status: String,
    pub prayer_busy: bool,
    /// Settings → Weather: places found for the typed name, a search running, a message.
    pub weather_results: Option<Vec<crate::services::weather::Place>>,
    pub weather_busy: bool,
    pub weather_status: String,
    pub feed_status: String,
    pub feed_busy: bool,
    /// Subscribed calendar links (decrypted), read off the UI thread.
    pub feeds: Option<Vec<(String, String)>>,
    pub tick_projects: Option<Vec<(String, String)>>,
    pub tick_loading: bool,
    pub tick_error: Option<String>,
    pub tick_syncing: bool,
    /// Apps with a window open right now: (process name, label).
    pub running: Option<Vec<(String, String)>>,
    pub log_exists: bool,
}

/// System facts read once per visit (cheap to draw from).
#[derive(Default, Clone)]
pub(super) struct Facts {
    pub monitors: Vec<(i32, i32, bool)>,
    pub first_run: bool,
    pub has_token: bool,
    pub windows: String,
}

pub struct State {
    page: usize,
    /// Content height of the page (or search results) last frame, for the scroll range.
    content_h: f32,
    nav_h: f32,
    /// Scroll offset to apply once the content height is known (FATHNOTCH_SETTINGS_SCROLL, screenshots).
    pending_scroll: Option<f32>,
    pending_search: Option<String>,
    /// Dropdown to open on its own (FATHNOTCH_SETTINGS_COMBO, screenshots).
    combo_dev: Option<String>,
    last_query: String,
    // Our dropdown (opens down, or up when there's no room, and scrolls past 240 DIP).
    combo_open: u64,
    combo_anchor: Rect,
    combo_opts: Vec<String>,
    combo_sel: usize,
    combo_seen: bool,
    combo_list: Option<Rect>,
    combo_pick: Option<(u64, usize)>,
    combo_scroll: f32,
    /// TickTick token being typed (masked).
    pub(super) token: String,
    pub(super) shared: Arc<Mutex<Shared>>,
    pub(super) facts: Facts,
    /// Calendar link help shown under the row.
    pub(super) feed_help: bool,
    /// Hotkey action waiting for keys (Keyboard page).
    pub(super) recording: Option<&'static str>,
    /// "Add an app": the picked running app.
    pub(super) app_pick: usize,
    /// Fields with a validation error (red border until edited).
    pub(super) field_error: Option<u64>,
}

impl Default for State {
    fn default() -> Self {
        // Dev/screenshot aids: start on a page, scrolled, or with a search typed.
        let page = std::env::var("FATHNOTCH_SETTINGS_PAGE")
            .ok()
            .and_then(|p| PAGES.iter().position(|x| x.id.eq_ignore_ascii_case(p.trim())))
            .unwrap_or(0);
        let pending_scroll = std::env::var("FATHNOTCH_SETTINGS_SCROLL").ok().and_then(|v| v.trim().parse().ok());
        let pending_search = std::env::var("FATHNOTCH_SETTINGS_SEARCH").ok().filter(|v| !v.is_empty());
        let mut s = State {
            page,
            content_h: 0.0,
            nav_h: 0.0,
            pending_scroll,
            pending_search,
            combo_dev: std::env::var("FATHNOTCH_SETTINGS_COMBO").ok().filter(|v| !v.is_empty()),
            last_query: String::new(),
            combo_open: 0,
            combo_anchor: Rect::default(),
            combo_opts: Vec::new(),
            combo_sel: 0,
            combo_seen: false,
            combo_list: None,
            combo_pick: None,
            combo_scroll: 0.0,
            token: String::new(),
            shared: Arc::default(),
            facts: Facts::default(),
            feed_help: false,
            recording: None,
            app_pick: 0,
            field_error: None,
        };
        refresh_facts(&mut s);
        s
    }
}

/// Reads what the pages show about the system: monitors, files, running apps (the slow parts in the background).
fn refresh_facts(s: &mut State) {
    s.facts = Facts {
        monitors: pages::monitors(),
        first_run: settings::first_run(),
        has_token: settings::has_ticktick_token(),
        windows: if s.facts.windows.is_empty() { pages::windows_name() } else { s.facts.windows.clone() },
    };
    let sh = s.shared.clone();
    crate::bg::spawn(move || {
        let feeds = pages::load_feeds();
        let running = pages::running_apps();
        let log = crate::services::store::path("error.log").exists();
        let mut g = sh.lock().unwrap();
        g.feeds = Some(feeds);
        g.running = Some(running);
        g.log_exists = log;
    });
}

pub fn shown(s: &mut State) {
    s.combo_open = 0;
    s.combo_list = None;
    s.token.clear();
    s.recording = None;
    s.feed_help = false;
    refresh_facts(s);
    // Fetch the TickTick lists again on the next visit to To-Dos.
    let mut sh = s.shared.lock().unwrap();
    if !sh.tick_loading {
        sh.tick_projects = None;
        sh.tick_error = None;
    }
}

const SEARCH_ID: u64 = id("settings.search");
const NAV_ID: u64 = id("settings.nav");
const PAGE_ID: u64 = id("settings.page");
const TOKEN_ID: u64 = id("settings.token");

// .NET SettingsView sizes.
const NAV_W: f32 = 176.0;
const PAGE_GAP: f32 = 12.0;
/// Page StackPanel margin (10,4,14,20) plus the ScrollViewer's right padding (4).
const PAGE_PAD_L: f32 = 10.0;
const PAGE_PAD_T: f32 = 4.0;
const PAGE_PAD_R: f32 = 18.0;
const PAGE_PAD_B: f32 = 20.0;
pub(super) const CTRL_H: f32 = 30.0;

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, _go: &mut Option<&'static str>) {
    combo_pre(ui, s);
    if let Some(q) = s.pending_search.take() {
        ui.set_field_text(SEARCH_ID, &q);
    }
    let (nav, rest) = r.take_left(NAV_W, PAGE_GAP);
    draw_nav(ui, s, nav);

    let query = ui.field_text(SEARCH_ID).trim().to_lowercase();
    if query != s.last_query {
        s.last_query = query.clone();
        ui.st.scroll.insert(PAGE_ID, 0.0);
        s.combo_open = 0;
    }
    if let Some(off) = s.pending_scroll {
        ui.animate = true;
        if s.content_h > rest.h {
            ui.st.scroll.insert(PAGE_ID, off);
            s.pending_scroll = None;
        }
    }
    let content_h = s.content_h;
    let mut measured = 0.0;
    s.combo_seen = false;
    ui.scroll(PAGE_ID, rest, content_h, |ui, top| {
        let mut p = Pg {
            ui,
            s,
            x: rest.x + PAGE_PAD_L,
            w: rest.w - PAGE_PAD_L - PAGE_PAD_R,
            y: top + PAGE_PAD_T,
            view: rest,
            search: (!query.is_empty()).then(|| query.clone()),
            page_name: "",
            last_header_page: "",
            first_header: true,
            any_match: false,
            st: settings::shared(),
        };
        if p.search.is_some() {
            for pg in PAGES {
                p.page_name = pg.name;
                (pg.draw)(&mut p);
            }
            if !p.any_match {
                let msg = format!("No settings match \"{}\".", p.search.clone().unwrap_or_default());
                let sub = p.ui.t.subtext;
                p.ui.text(&msg, Rect::new(p.x + 4.0, p.y + 8.0, p.w, 18.0), 12.0, SEMIBOLD, sub, Align::Left);
                p.y += 26.0;
            }
        } else {
            let pg = &PAGES[p.s.page];
            p.page_name = pg.name;
            (pg.draw)(&mut p);
        }
        measured = p.y - top + PAGE_PAD_B;
    });
    s.content_h = measured;
    combo_post(ui, s, r);
}

// ---------------------------------------------------------------- nav

fn draw_nav(ui: &mut Ui, s: &mut State, r: Rect) {
    let t = ui.t;
    // Search box (34 tall, magnifier, clear button), 8 below it.
    let (sr, rest) = r.take_top(34.0, 8.0);
    let focused = ui.st.focus == SEARCH_ID;
    field_chrome(ui, sr, focused, false);
    ui.icon("Magnifier", sr.x + 18.5, sr.cy(), 15.0, t.subtext);
    let has_text = !ui.field_text(SEARCH_ID).is_empty();
    let text_r = sr.pad(36.0, 0.0, if has_text { 32.0 } else { 10.0 }, 0.0);
    if let Some(v) = ui.text_field(SEARCH_ID, text_r, tr("Search settings"), 13.0) {
        ui.set_field_text(SEARCH_ID, &v); // Enter keeps the query
    }
    if has_text {
        let x = Rect::new(sr.right() - 28.0, sr.cy() - 11.0, 22.0, 22.0);
        if ui.icon_button(x, "XMark", 12.0, t.subtext) {
            ui.set_field_text(SEARCH_ID, "");
        }
    }
    let searching = !ui.field_text(SEARCH_ID).trim().is_empty();

    // Version, docked at the bottom.
    let (ver, list) = rest.take_bottom(15.0, 0.0);
    let version = format!("FathNotch {}", env!("CARGO_PKG_VERSION"));
    ui.text(&version, Rect::new(ver.x + 8.0, ver.y - 2.0, ver.w - 8.0, ver.h), 10.0, REGULAR, t.subtext, Align::Left);

    let nav_h = s.nav_h;
    let mut measured = 0.0;
    let mut clicked = None;
    // The nav scrolls without a visible bar (as in the .NET build).
    let max = (nav_h - list.h).max(0.0);
    let mut off = ui.st.scroll.get(&NAV_ID).copied().unwrap_or(0.0).clamp(0.0, max);
    if ui.input.wheel != 0.0 && list.contains(ui.mouse()) {
        off = (off - ui.input.wheel * 48.0).clamp(0.0, max);
        ui.input.wheel = 0.0;
    }
    ui.st.scroll.insert(NAV_ID, off);
    ui.push_clip(list);
    let top = list.y - off;
    let mut y = top;
    for (i, pg) in PAGES.iter().enumerate() {
        if let Some(c) = pg.caption {
            if c.is_empty() {
                y += 8.0 + 4.0;
            } else {
                y += 14.0;
                ui.text(tr(c), Rect::new(list.x + 10.0, y, list.w - 10.0, 14.0), 11.0, BOLD, t.subtext, Align::Left);
                y += 14.0 + 4.0;
            }
        }
        let row = Rect::new(list.x, y + 1.0, list.w, 30.0);
        let selected = i == s.page && !searching;
        let hov = ui.hovered(row);
        if selected {
            ui.fill(row, 8.0, t.accent_soft);
            ui.fill(Rect::new(row.x + 3.0, row.cy() - 7.0, 3.0, 14.0), 1.5, t.accent);
        } else if hov {
            ui.fill(row, 8.0, t.surface2);
        }
        let fg = if selected || hov { t.text } else { t.subtext };
        ui.icon(pg.icon, row.x + 12.0 + 7.5, row.cy(), 15.0, fg);
        ui.text(tr(pg.name), Rect::new(row.x + 37.0, row.y, row.w - 41.0, row.h), 13.0, SEMIBOLD, fg, Align::Left);
        if ui.clicked(row) {
            clicked = Some(i);
        }
        y += 32.0;
    }
    measured = (y - top).max(measured);
    ui.pop_clip();
    s.nav_h = measured;
    if let Some(i) = clicked {
        if i != s.page || searching {
            s.page = i;
            ui.set_field_text(SEARCH_ID, "");
            ui.st.focus = 0;
            ui.st.scroll.insert(PAGE_ID, 0.0);
            s.combo_open = 0;
            s.content_h = 0.0;
            s.recording = None;
        }
    }
}

// ---------------------------------------------------------------- shared widgets (the .NET look)

/// Text field box: Surface2, 1 px border (Track on hover, Accent with a soft glow while focused, red on error).
pub(super) fn field_chrome(ui: &mut Ui, r: Rect, focused: bool, error: bool) {
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

/// Toggle switch, 38×22: Hover track with a white knob when off, accent track and OnAccent knob when on.
pub(super) fn switch(ui: &mut Ui, r: Rect, on: bool) -> bool {
    let t = ui.t;
    let r = Rect::new(r.x, r.cy() - 11.0, 38.0, 22.0);
    ui.fill(r, 11.0, if on { t.accent } else { t.hover });
    let kx = if on { r.x + 2.0 + 16.0 } else { r.x + 2.0 };
    let knob = if on { t.on_accent } else { crate::gfx::color(0xFFFFFF, 1.0) };
    ui.g.fill_circle(kx + 9.0, r.cy(), 9.0, ui.c(knob));
    ui.clicked(r)
}

/// Width of a default button for `label` (13 Medium, 14 padding each side).
pub(super) fn button_w(ui: &Ui, label: &str, accent: bool) -> f32 {
    ui.measure(tr(label), 13.0, if accent { SEMIBOLD } else { MEDIUM }).ceil() + 28.0
}

/// Default (Surface2) or accent button, 8 radius; hover dims it a little; disabled at 35 %.
pub(super) fn button(ui: &mut Ui, r: Rect, label: &str, accent: bool, enabled: bool) -> bool {
    let t = ui.t;
    let hov = enabled && ui.hovered(r);
    let pressed = hov && ui.input.down;
    let a = if !enabled { 0.35 } else if pressed { 0.65 } else if hov { 0.85 } else { 1.0 };
    let (bg, fg) = if accent { (t.accent, t.on_accent) } else { (t.surface2, t.text) };
    ui.fill(r, 8.0, with_alpha(bg, a));
    ui.text(tr(label), r, 13.0, if accent { SEMIBOLD } else { MEDIUM }, with_alpha(fg, if enabled { 1.0 } else { 0.35 }), Align::Center);
    enabled && ui.clicked(r)
}

/// Small icon button (22×22, 11 px glyph), dimmed when disabled.
pub(super) fn small_icon(ui: &mut Ui, r: Rect, icon: &'static str, enabled: bool) -> bool {
    let t = ui.t;
    if !enabled {
        ui.icon(icon, r.cx(), r.cy(), 11.0, with_alpha(t.subtext, 0.35));
        return false;
    }
    ui.icon_button(r, icon, 11.0, t.subtext)
}

// ---------------------------------------------------------------- page builder

/// One settings row: what `Pg::card` needs to lay it out.
pub(super) struct R {
    pub title: String,
    pub desc: String,
    /// Control size.
    pub cw: f32,
    pub ch: f32,
    /// Full-width line under the row: (text, size, weight).
    pub foot: Option<(String, f32, u16)>,
}

impl R {
    pub fn new(title: impl Into<String>, desc: impl Into<String>, cw: f32, ch: f32) -> R {
        R { title: title.into(), desc: desc.into(), cw, ch, foot: None }
    }
    /// Row with a toggle.
    pub fn t(title: impl Into<String>, desc: impl Into<String>) -> R {
        R::new(title, desc, 38.0, 22.0)
    }
    /// Row with a dropdown / button / field `w` wide.
    pub fn c(title: impl Into<String>, desc: impl Into<String>, w: f32) -> R {
        R::new(title, desc, w, CTRL_H)
    }
    /// Footer line (prayer timetable style: 12 regular).
    pub fn foot(mut self, f: impl Into<String>) -> R {
        let f = f.into();
        self.foot = (!f.is_empty()).then_some((f, 12.0, REGULAR));
        self
    }
    pub fn foot_styled(mut self, f: impl Into<String>, size: f32, weight: u16) -> R {
        let f = f.into();
        self.foot = (!f.is_empty()).then_some((f, size, weight));
        self
    }
}

/// Line heights WPF gives these sizes (Segoe UI: 1.33 em).
pub(super) fn lh(size: f32) -> f32 {
    (size * 1.33).round()
}

/// Lays a page out top to bottom (`y` is the cursor) and draws its widgets.
pub(super) struct Pg<'a, 'g> {
    pub ui: &'a mut Ui<'g>,
    pub s: &'a mut State,
    pub x: f32,
    pub w: f32,
    pub y: f32,
    /// Visible part of the page.
    pub view: Rect,
    /// Lower-case search text: only matching rows are drawn, under their page's name.
    pub search: Option<String>,
    page_name: &'static str,
    last_header_page: &'static str,
    first_header: bool,
    any_match: bool,
    /// The settings this frame (shared, not copied).
    pub st: Arc<Settings>,
}

impl Pg<'_, '_> {
    /// Free-form content (titles, swatches, lists) shows only when not searching.
    pub fn free(&self) -> bool {
        self.search.is_none()
    }

    /// Is `r` (in page coordinates) inside the visible part? Off-screen content is laid out but not drawn.
    pub fn on_screen(&self, y: f32, h: f32) -> bool {
        y + h >= self.view.y - 4.0 && y <= self.view.bottom() + 4.0
    }

    /// Page header: 40 px icon tile, name (20 bold) and a summary (12, wraps).
    pub fn title(&mut self, title: &str, subtitle: &str) {
        if !self.free() {
            return;
        }
        let (title, subtitle) = (tr(title), tr(subtitle));
        let t = self.ui.t;
        let icon = PAGES.iter().find(|p| p.name == self.page_name).map_or("Gear", |p| p.icon);
        let tx = self.x + 52.0;
        let tw = self.w - 52.0;
        let lines = wrap_lines(self.ui, subtitle, 12.0, REGULAR, tw);
        let text_h = lh(17.0) + 1.0 + lines.len() as f32 * lh(12.0);
        let y0 = self.y + 2.0;
        let h = text_h.max(40.0);
        let badge = Rect::new(self.x, y0 + (h - 40.0) / 2.0, 40.0, 40.0);
        self.ui.fill(badge, 13.0, t.accent_soft);
        self.ui.icon(icon, badge.cx(), badge.cy(), 19.0, t.accent);
        let mut y = y0 + (h - text_h) / 2.0;
        self.ui.text(title, Rect::new(tx, y, tw, lh(17.0)), 17.0, BOLD, t.text, Align::Left);
        y += lh(17.0) + 1.0;
        for l in &lines {
            self.ui.text(l, Rect::new(tx, y, tw, lh(12.0)), 12.0, REGULAR, t.subtext, Align::Left);
            y += lh(12.0);
        }
        self.y = y0 + h;
    }

    /// Section caption (Caps, 10.5 bold; 22 above, 8 below).
    pub fn caption(&mut self, text: &str) {
        let text = tr(text);
        let sub = self.ui.t.subtext;
        self.y += 22.0;
        self.ui.text(text, Rect::new(self.x + 6.0, self.y, self.w, 14.0), 11.0, BOLD, sub, Align::Left);
        self.y += 14.0 + 8.0;
    }

    /// A short wrapped note (free-form, 10 px SubText).
    pub fn hint(&mut self, text: &str, bad: bool) {
        if !self.free() {
            return;
        }
        let c = if bad { self.ui.t.danger } else { self.ui.t.subtext };
        let lines = wrap_lines(self.ui, tr(text), 10.0, SEMIBOLD, self.w - 2.0);
        self.y += 6.0;
        for l in &lines {
            self.ui.text(l, Rect::new(self.x + 2.0, self.y, self.w - 2.0, lh(10.0)), 10.0, SEMIBOLD, c, Align::Left);
            self.y += lh(10.0);
        }
    }

    fn matches(&self, r: &R) -> bool {
        match &self.search {
            None => true,
            Some(q) => r.title.to_lowercase().contains(q) || r.desc.to_lowercase().contains(q),
        }
    }

    /// While searching: the page's name above its first matching card.
    fn search_header(&mut self) {
        if self.search.is_none() {
            return;
        }
        self.any_match = true;
        if self.last_header_page != self.page_name {
            if !self.first_header {
                self.y += 18.0;
            }
            self.first_header = false;
            self.last_header_page = self.page_name;
            let t = self.ui.t;
            self.ui.text(tr(self.page_name), Rect::new(self.x, self.y, self.w, lh(17.0)), 17.0, BOLD, t.text, Align::Left);
            self.y += lh(17.0);
        }
    }

    /// A card of rows under an optional caption. Returns each row's control rectangle (None when the row is
    /// filtered out by the search, or scrolled out of sight).
    pub fn card(&mut self, caption: Option<&str>, rows: &[R]) -> Vec<Option<Rect>> {
        // In the interface language (searching matches what's shown).
        let rows: Vec<R> = rows
            .iter()
            .map(|r| R {
                title: tr(&r.title).to_string(),
                desc: tr(&r.desc).to_string(),
                cw: r.cw,
                ch: r.ch,
                foot: r.foot.as_ref().map(|(f, size, weight)| (tr(f).to_string(), *size, *weight)),
            })
            .collect();
        let rows = &rows[..];
        let shown: Vec<bool> = rows.iter().map(|r| self.matches(r)).collect();
        if !shown.iter().any(|&b| b) {
            return vec![None; rows.len()];
        }
        let t = self.ui.t;
        self.search_header();
        if let Some(c) = caption {
            self.caption(c);
        }
        // Lay out first so the card can be drawn behind the rows.
        struct L {
            desc: Vec<String>,
            foot: Vec<String>,
            text_h: f32,
            inner: f32,
            h: f32,
        }
        let pad = 16.0;
        let mut lay: Vec<Option<L>> = Vec::new();
        let mut total = 4.0;
        let mut n = 0;
        for (r, &vis) in rows.iter().zip(&shown) {
            if !vis {
                lay.push(None);
                continue;
            }
            let tw = self.w - 2.0 * pad - if r.cw > 0.0 { r.cw + 12.0 } else { 0.0 };
            let desc = if r.desc.is_empty() { vec![] } else { wrap_lines(self.ui, &r.desc, 11.0, SEMIBOLD, tw) };
            let foot = r.foot.as_ref().map_or(vec![], |(f, size, weight)| wrap_lines(self.ui, f, *size, *weight, self.w - 2.0 * pad));
            let text_h = lh(13.0) + if desc.is_empty() { 0.0 } else { 1.0 + desc.len() as f32 * lh(11.0) };
            let inner = text_h.max(r.ch).max(30.0);
            let foot_h = r.foot.as_ref().map_or(0.0, |(_, size, _)| 8.0 + foot.len() as f32 * lh(*size));
            let h = 11.0 + inner + foot_h + 11.0;
            if n > 0 {
                total += 1.0;
            }
            n += 1;
            total += h;
            lay.push(Some(L { desc, foot, text_h, inner, h }));
        }
        let card = Rect::new(self.x, self.y, self.w, total);
        let visible = self.on_screen(card.y, card.h);
        if visible {
            self.ui.fill(card, 16.0, t.surface);
        }
        let mut y = self.y + 2.0;
        let mut out = Vec::new();
        let mut first = true;
        for (r, l) in rows.iter().zip(lay) {
            let Some(l) = l else {
                out.push(None);
                continue;
            };
            if !first {
                if visible {
                    self.ui.fill(Rect::new(self.x + pad, y, self.w - 2.0 * pad, 1.0), 0.0, t.stroke);
                }
                y += 1.0;
            }
            first = false;
            if !self.on_screen(y, l.h) {
                out.push(None);
                y += l.h;
                continue;
            }
            let tx = self.x + pad;
            let tw = self.w - 2.0 * pad - if r.cw > 0.0 { r.cw + 12.0 } else { 0.0 };
            let mut ty = y + 11.0 + (l.inner - l.text_h) / 2.0;
            self.ui.text(&r.title, Rect::new(tx, ty, tw, lh(13.0)), 13.0, SEMIBOLD, t.text, Align::Left);
            ty += lh(13.0) + 1.0;
            for d in &l.desc {
                self.ui.text(d, Rect::new(tx, ty, tw, lh(11.0)), 11.0, SEMIBOLD, t.subtext, Align::Left);
                ty += lh(11.0);
            }
            if let Some((_, size, weight)) = &r.foot {
                let mut fy = y + 11.0 + l.inner + 8.0;
                for f in &l.foot {
                    self.ui.text(f, Rect::new(tx, fy, self.w - 2.0 * pad, lh(*size)), *size, *weight, t.subtext, Align::Left);
                    fy += lh(*size);
                }
            }
            out.push(Some(Rect::new(self.x + self.w - pad - r.cw, y + 11.0 + (l.inner - r.ch) / 2.0, r.cw, r.ch)));
            y += l.h;
        }
        self.y = card.bottom();
        out
    }

    // ------------------------------------------------------------ widgets

    /// Toggle bound to a setting.
    pub fn tog(&mut self, at: Option<Rect>, on: bool, set: impl FnOnce(&mut Settings, bool)) -> bool {
        let Some(r) = at else { return false };
        if switch(self.ui, r, on) {
            settings::update(|s| set(s, !on));
            return true;
        }
        false
    }

    /// Dropdown bound to a setting: `values[i]` is stored for `labels[i]`; an unknown value shows `fallback`.
    #[allow(clippy::too_many_arguments)]
    pub fn pick<T: PartialEq + Clone>(
        &mut self,
        at: Option<Rect>,
        key: &str,
        labels: &[&str],
        values: &[T],
        current: &T,
        fallback: usize,
        set: impl FnOnce(&mut Settings, T),
    ) -> bool {
        let Some(r) = at else { return false };
        let sel = values.iter().position(|v| v == current).unwrap_or(fallback);
        if let Some(i) = self.combo(key, r, labels, sel) {
            if i != sel || values.get(sel) != Some(current) {
                let v = values[i].clone();
                settings::update(|s| set(s, v));
                return true;
            }
        }
        false
    }

    /// A dropdown (30 tall, 7 radius, chevron); returns the picked index.
    pub fn combo(&mut self, key: &str, r: Rect, labels: &[&str], sel: usize) -> Option<usize> {
        self.combo_enabled(key, r, labels, sel, true)
    }

    pub fn combo_enabled(&mut self, key: &str, r: Rect, labels: &[&str], sel: usize, enabled: bool) -> Option<usize> {
        let cid = id(key);
        let labels: Vec<&str> = labels.iter().map(|l| tr(l)).collect();
        let labels = &labels[..];
        let t = self.ui.t;
        let open = self.s.combo_open == cid;
        let hov = enabled && self.ui.hovered(r);
        let a = if enabled { 1.0 } else { 0.35 };
        let bg = if hov || open { t.hover } else { t.surface2 };
        self.ui.fill(r, 7.0, with_alpha(bg, a));
        let label = labels.get(sel).copied().unwrap_or("");
        self.ui.text(label, r.pad(9.0, 0.0, 26.0, 0.0), 13.0, REGULAR, with_alpha(t.text, a), Align::Left);
        // Chevron: 8×4 path, 10 from the right.
        let (cx, cy) = (r.right() - 14.0, r.cy());
        let c = self.ui.c(with_alpha(t.subtext, a));
        self.ui.g.line(cx - 4.0, cy - 2.0, cx, cy + 2.0, c, 1.4);
        self.ui.g.line(cx, cy + 2.0, cx + 4.0, cy - 2.0, c, 1.4);
        if let Some((pid, i)) = self.s.combo_pick {
            if pid == cid {
                self.s.combo_pick = None;
                return Some(i);
            }
        }
        if !enabled {
            return None;
        }
        if open {
            self.s.combo_anchor = r;
            self.s.combo_opts = labels.iter().map(|s| s.to_string()).collect();
            self.s.combo_sel = sel;
            self.s.combo_seen = true;
        } else if (self.s.combo_dev.as_deref() == Some(key) && self.s.combo_dev.take().is_some())
            || (self.ui.clicked(r) && self.view.contains((r.cx(), r.cy())))
        {
            self.s.combo_open = cid;
            self.s.combo_anchor = r;
            self.s.combo_opts = labels.iter().map(|s| s.to_string()).collect();
            self.s.combo_sel = sel;
            self.s.combo_seen = true;
            // Open scrolled so the selected item shows.
            let visible = ((COMBO_MAX_H - 8.0) / COMBO_ROW).floor();
            self.s.combo_scroll = ((sel as f32 - visible + 1.0).max(0.0) * COMBO_ROW).max(0.0);
            self.ui.animate = true;
        }
        None
    }

    pub fn button(&mut self, at: Option<Rect>, label: &str, accent: bool) -> bool {
        let Some(r) = at else { return false };
        button(self.ui, r, label, accent, true)
    }

    /// Width a default button needs for `label`.
    pub fn button_w(&self, label: &str) -> f32 {
        button_w(self.ui, label, false)
    }

    /// Text field (30 tall, 13.5 px) showing `current` until edited; returns the new text on Enter or when it
    /// loses focus.
    pub fn field(&mut self, key: &str, r: Rect, placeholder: &str, current: &str) -> Option<String> {
        let fid = id(key);
        let was = self.ui.st.focus == fid;
        if !was && self.ui.field_text(fid) != current {
            self.ui.set_field_text(fid, current);
        }
        let error = self.s.field_error == Some(fid);
        field_chrome(self.ui, r, was, error);
        let inner = r.pad(12.0, 0.0, 10.0, 0.0);
        let clip = intersect(inner, self.view);
        self.ui.push_clip(clip);
        let before = self.ui.field_text(fid);
        let sub = self.ui.text_field(fid, inner, tr(placeholder), 13.0);
        self.ui.pop_clip();
        if error && self.ui.field_text(fid) != before {
            self.s.field_error = None;
        }
        if let Some(v) = sub {
            self.ui.set_field_text(fid, &v);
            return (v != current).then_some(v);
        }
        if was && self.ui.st.focus != fid {
            let v = self.ui.field_text(fid).trim().to_string();
            return (v != current).then_some(v);
        }
        None
    }

    /// A free text input (keeps what's typed, e.g. a link to add); returns the text on Enter. Red border while
    /// `State::field_error` names it, until the text changes.
    pub fn input(&mut self, key: &str, r: Rect, placeholder: &str) -> Option<String> {
        let fid = id(key);
        let focused = self.ui.st.focus == fid;
        let error = self.s.field_error == Some(fid);
        field_chrome(self.ui, r, focused, error);
        let inner = r.pad(12.0, 0.0, 10.0, 0.0);
        let clip = intersect(inner, self.view);
        self.ui.push_clip(clip);
        let before = self.ui.field_text(fid);
        let sub = self.ui.text_field(fid, inner, tr(placeholder), 13.0);
        self.ui.pop_clip();
        if let Some(v) = &sub {
            self.ui.set_field_text(fid, v); // the kit clears on Enter; keep it until it's accepted
        }
        if error && self.ui.field_text(fid) != before && sub.is_none() {
            self.s.field_error = None;
        }
        sub
    }

    /// A field for secrets: shows dots, returns the text on Enter. The text lives in `State::token`.
    pub fn secret(&mut self, r: Rect, placeholder: &str) -> Option<String> {
        let t = self.ui.t;
        if self.ui.clicked(r) {
            self.ui.st.focus = TOKEN_ID;
            self.ui.wants_keyboard = true;
        } else if self.ui.input.pressed && !r.contains(self.ui.input.mouse) && self.ui.st.focus == TOKEN_ID {
            self.ui.st.focus = 0;
        }
        let focused = self.ui.st.focus == TOKEN_ID;
        let mut submitted = None;
        if focused {
            for ch in self.ui.input.chars.chars() {
                if !ch.is_control() {
                    self.s.token.push(ch);
                }
            }
            for k in self.ui.input.keys.clone() {
                match k {
                    Key::Backspace => {
                        self.s.token.pop();
                    }
                    Key::Paste => {
                        if let Some(p) = crate::clipboard::get_text() {
                            self.s.token.push_str(p.trim());
                        }
                    }
                    Key::SelectAll | Key::Cut | Key::Delete => self.s.token.clear(),
                    Key::Escape => self.ui.st.focus = 0,
                    Key::Enter if !self.s.token.trim().is_empty() => submitted = Some(std::mem::take(&mut self.s.token).trim().to_string()),
                    _ => {}
                }
            }
        }
        field_chrome(self.ui, r, focused, false);
        let inner = r.pad(12.0, 0.0, 10.0, 0.0);
        let n = self.s.token.chars().count().min(40);
        if n == 0 {
            self.ui.text(tr(placeholder), inner, 13.0, REGULAR, t.subtext, Align::Left);
        } else {
            let dots: String = "•".repeat(n);
            self.ui.text(&dots, inner, 13.0, REGULAR, t.text, Align::Left);
        }
        if focused {
            self.ui.animate = true;
            if self.ui.st.caret_on {
                let dots: String = "•".repeat(n);
                let x = inner.x + self.ui.measure(&dots, 13.0, REGULAR) + 1.0;
                self.ui.g.line(x, r.cy() - 8.0, x, r.cy() + 8.0, self.ui.c(t.accent), 2.0);
            }
        }
        submitted
    }

    /// A small label in a soft box (keyboard keys): 10.5 Medium, 8×3 padding, 6 radius.
    pub fn badge(&mut self, at: Option<Rect>, text: &str) {
        let Some(r) = at else { return };
        let t = self.ui.t;
        self.ui.fill(r, 6.0, t.hover);
        self.ui.text(text, r, 11.0, MEDIUM, t.text, Align::Center);
    }

    pub fn badge_w(&self, text: &str) -> f32 {
        self.ui.measure(text, 11.0, MEDIUM).ceil() + 16.0
    }

    /// Small helper to run something off the UI thread with the shared results.
    pub fn shared(&self) -> Arc<Mutex<Shared>> {
        self.s.shared.clone()
    }
}

fn intersect(a: Rect, b: Rect) -> Rect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let r = a.right().min(b.right());
    let btm = a.bottom().min(b.bottom());
    Rect::new(x, y, (r - x).max(0.0), (btm - y).max(0.0))
}

thread_local! {
    /// Wrapped lines by (text, size, weight, width): the pages lay the same text out every frame.
    static WRAPS: std::cell::RefCell<std::collections::HashMap<(String, u32, u16, u32), Vec<String>>> = Default::default();
}

/// Greedy word wrap with the real font metrics (cached).
pub(super) fn wrap_lines(ui: &Ui, s: &str, size: f32, weight: u16, w: f32) -> Vec<String> {
    let key = (s.to_string(), (size * 10.0) as u32, weight, w.round() as u32);
    if let Some(v) = WRAPS.with(|c| c.borrow().get(&key).cloned()) {
        return v;
    }
    let mut out = Vec::new();
    for para in s.split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if !line.is_empty() && ui.measure(&candidate, size, weight) > w {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
            } else {
                line = candidate;
            }
        }
        out.push(line);
    }
    WRAPS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 2000 {
            c.clear();
        }
        c.insert(key, out.clone());
    });
    out
}

// ---------------------------------------------------------------- dropdown list

/// Item pitch: 13 px text, 8×5 padding, 1 px margins.
const COMBO_ROW: f32 = 29.0;
const COMBO_MAX_H: f32 = 240.0;

fn combo_geometry(ui: &Ui, s: &State, bounds: Rect) -> Rect {
    let a = s.combo_anchor;
    let full = COMBO_ROW * s.combo_opts.len() as f32 + 8.0;
    let h = full.min(COMBO_MAX_H);
    let w = s.combo_opts.iter().map(|o| ui.measure(o, 13.0, REGULAR).ceil() + 24.0).fold(a.w, f32::max).min(bounds.w);
    let below = a.bottom() + 4.0;
    let y = if below + h <= bounds.bottom() + 6.0 {
        below
    } else if a.y - 4.0 - h >= bounds.y - 6.0 {
        a.y - 4.0 - h
    } else {
        (bounds.bottom() - h).max(bounds.y - 6.0)
    };
    let x = a.x.min(bounds.right() - w).max(bounds.x);
    Rect::new(x, y, w, h)
}

/// Before anything else this frame: a click on the open list picks; any other click closes it (and is eaten).
/// The wheel over the list scrolls it.
fn combo_pre(ui: &mut Ui, s: &mut State) {
    if s.combo_open == 0 {
        return;
    }
    let m = ui.input.mouse;
    if let Some(list) = s.combo_list {
        if ui.input.wheel != 0.0 && list.contains(m) {
            let max = (COMBO_ROW * s.combo_opts.len() as f32 + 8.0 - list.h).max(0.0);
            s.combo_scroll = (s.combo_scroll - ui.input.wheel * 48.0).clamp(0.0, max);
            ui.input.wheel = 0.0;
        }
    }
    if !(ui.input.pressed || ui.input.right_pressed) || ui.input.consumed {
        return;
    }
    if let Some(list) = s.combo_list {
        if list.contains(m) && ui.input.pressed {
            let i = ((m.1 - list.y - 4.0 + s.combo_scroll) / COMBO_ROW).floor();
            if i >= 0.0 && (i as usize) < s.combo_opts.len() {
                s.combo_pick = Some((s.combo_open, i as usize));
            }
        }
    }
    s.combo_open = 0;
    s.combo_list = None;
    ui.input.consumed = true;
}

/// After the page: draws the open list on top, below its anchor or above when there's no room.
fn combo_post(ui: &mut Ui, s: &mut State, bounds: Rect) {
    if s.combo_open == 0 {
        return;
    }
    if !s.combo_seen {
        s.combo_open = 0;
        s.combo_list = None;
        return;
    }
    let t = ui.t;
    let list = combo_geometry(ui, s, bounds);
    let full = COMBO_ROW * s.combo_opts.len() as f32 + 8.0;
    let max = (full - list.h).max(0.0);
    s.combo_scroll = s.combo_scroll.clamp(0.0, max);
    ui.g.fill_round(list.d2d(), 8.0, ui.c(t.surface2));
    ui.g.stroke_round(list.inset(0.5, 0.5).d2d(), 7.5, ui.c(t.stroke), 1.0);
    ui.g.push_clip(list.inset(1.0, 3.0).d2d());
    for (i, o) in s.combo_opts.iter().enumerate() {
        let y = list.y + 4.0 + i as f32 * COMBO_ROW - s.combo_scroll;
        if y + COMBO_ROW < list.y || y > list.bottom() {
            continue;
        }
        let row = Rect::new(list.x + 3.0, y, list.w - 6.0, COMBO_ROW - 2.0);
        if row.contains(ui.input.mouse) && list.contains(ui.input.mouse) {
            ui.g.fill_round(row.d2d(), 5.0, ui.c(t.hover));
        }
        let sel = i == s.combo_sel;
        ui.text(o, row.pad(8.0, 0.0, 8.0, 0.0), 13.0, REGULAR, if sel { t.accent } else { t.text }, Align::Left);
    }
    ui.g.pop_clip();
    if max > 0.0 {
        let bar_h = (list.h * list.h / full).max(18.0);
        let y = list.y + 3.0 + (list.h - 6.0 - bar_h) * (s.combo_scroll / max);
        ui.fill(Rect::new(list.right() - 5.0, y, 3.0, bar_h), 1.5, with_alpha(t.text, 0.18));
    }
    s.combo_list = Some(list);
    ui.animate = true;
}
