//! Control: Wi-Fi, Bluetooth, Hotspot, Silent and Awake tiles (the round badge toggles, the card opens its choices
//! or its Settings page), brightness per display, output/input devices with their levels (the icon mutes),
//! and volume per app. Sizes follow the C# ControlView.xaml.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::bg;
use crate::gfx::{color, mix, with_alpha, Align, Color, BOLD, MEDIUM, REGULAR, SEMIBOLD};
use crate::model;
use crate::services::{appicons, audio, brightness, radios};
use crate::ui::{id, idx, Rect, Ui};
use crate::i18n::tr;

/// C# "BadBrush" (mute on).
const BAD: u32 = 0xFF453A;

#[derive(Clone)]
enum Join {
    Connecting,
    Checking,
    Password { retry: bool },
    Failed(String),
}

#[derive(Default, Clone)]
struct Data {
    loaded: bool,
    wifi: Option<bool>,
    wifi_name: Option<String>,
    bt: Option<bool>,
    hotspot: Option<bool>,
    silent: bool,
    outputs: Vec<audio::Device>,
    inputs: Vec<audio::Device>,
    out_level: Option<(f32, bool)>,
    in_level: Option<(f32, bool)>,
    sessions: Vec<audio::AppSession>,
    displays: Vec<brightness::Display>,
    networks: Vec<radios::Network>,
    networks_loading: bool,
    paired: Vec<radios::BtDevice>,
    paired_loading: bool,
    /// A network being joined and how it's going.
    join: Option<(String, Join)>,
    /// Tiles switching ("Turning on…"): wifi, bluetooth, hotspot.
    busy: [Option<bool>; 3],
    /// A Bluetooth device being connected (true) or disconnected (false), by name.
    bt_busy: Option<(String, bool)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Detail {
    Wifi,
    Bluetooth,
}

#[derive(Default)]
pub struct State {
    data: Arc<Mutex<Data>>,
    detail: Option<Detail>,
    detail_at: f64,
    shown_at: f64,
    last_refresh: Option<Instant>,
}

/// Re-reads the system. Displays only when `displays` (C# loads them once per opening: DDC/CI is slow).
fn refresh(data: &Arc<Mutex<Data>>, displays: bool) {
    // One at a time: reading the radios can stall for seconds (right after a Bluetooth device comes or
    // goes), and a new read every 2.5 s would pile up threads and keep the CPU busy.
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let d = data.clone();
    bg::spawn(move || {
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                RUNNING.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        let _done = Done;
        let found = if displays { Some(brightness::displays()) } else { None };
        let n = Data {
            loaded: true,
            wifi: radios::wifi(),
            wifi_name: radios::connected_name(),
            bt: radios::bluetooth(),
            hotspot: radios::hotspot(),
            silent: crate::services::settings::shared().silent_mode,
            outputs: audio::devices(false),
            inputs: audio::devices(true),
            out_level: audio::master(false),
            in_level: audio::master(true),
            sessions: audio::sessions(),
            ..Default::default()
        };
        let mut cur = d.lock().unwrap();
        // Keep what the details loaded, and tiles that are mid-switch.
        let keep = std::mem::take(&mut *cur);
        *cur = Data {
            networks: keep.networks,
            networks_loading: keep.networks_loading,
            paired: keep.paired,
            paired_loading: keep.paired_loading,
            join: keep.join,
            busy: keep.busy,
            bt_busy: keep.bt_busy,
            displays: found.unwrap_or(keep.displays),
            loaded: displays || keep.loaded,
            wifi: keep.busy[0].or(n.wifi),
            bt: keep.busy[1].or(n.bt),
            hotspot: keep.busy[2].or(n.hotspot),
            ..n
        };
    });
}

pub fn shown(s: &mut State) {
    s.detail = None;
    s.shown_at = model::clock_secs();
    s.last_refresh = Some(Instant::now());
    s.data.lock().unwrap().join = None;
    refresh(&s.data, true);
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, _go: &mut Option<&'static str>) {
    // Keep it live while open (C#: every 2.5 s), but never while a slider is held.
    if s.last_refresh.map_or(true, |t| t.elapsed().as_secs_f32() >= 2.5) && ui.st.active == 0 {
        s.last_refresh = Some(Instant::now());
        refresh(&s.data, false);
    }
    let d = s.data.lock().unwrap().clone();
    let area = r.pad(18.0, 0.0, 18.0, 6.0);
    let (left, right) = area.take_left(376.0, 14.0);
    match s.detail {
        Some(det) => {
            // Slides in from the right (24 px, 260 ms) and fades in.
            let k = ((model::clock_secs() - s.detail_at) / 0.26).clamp(0.0, 1.0) as f32;
            if k < 1.0 {
                ui.animate = true;
            }
            let e = 1.0 - (1.0 - k).powi(3);
            let base = ui.alpha;
            ui.alpha = base * e;
            detail(ui, s, &d, Rect::new(left.x + 24.0 * (1.0 - e), left.y, left.w, left.h), det);
            ui.alpha = base;
        }
        None => {
            let (tiles, below) = left.take_top(78.0, 10.0);
            draw_tiles(ui, s, &d, tiles);
            brightness_card(ui, s, &d, below);
        }
    }
    sound_card(ui, s, &d, right);
}

fn set_local(s: &State, f: impl FnOnce(&mut Data)) {
    f(&mut s.data.lock().unwrap());
}

// ---------------------------------------------------------------------- tiles

/// One tile: round badge (toggles), name, detail, a quiet ›. Returns (toggled, opened).
fn tile(ui: &mut Ui, r: Rect, icon: &'static str, name: &str, status: &str, on: bool, available: bool) -> (bool, bool) {
    let t = ui.t;
    let base = ui.alpha;
    if !available {
        ui.alpha = base * 0.45;
    }
    let hov = available && ui.hovered(r);
    ui.fill(r, 16.0, if hov { t.surface2 } else { t.surface });
    let badge = Rect::new(r.x + 9.0, r.y + 9.0, 30.0, 30.0);
    let bhov = available && ui.hovered(badge);
    let bo = if bhov { 0.85 } else { 1.0 };
    let bg = if on { t.accent } else { t.hover };
    ui.g.fill_circle(badge.cx(), badge.cy(), 15.0, ui.c(with_alpha(bg, bg.a * bo)));
    ui.icon(icon, badge.cx(), badge.cy(), 15.0, with_alpha(if on { t.on_accent } else { t.subtext }, bo));
    ui.icon("ChevronRight", r.right() - 8.0 - 5.5, r.y + 13.0 + 5.5, 11.0, with_alpha(t.subtext, 0.6));
    ui.text(name, Rect::new(r.x + 9.0, r.y + 46.0, r.w - 13.0, 14.0), 11.0, SEMIBOLD, t.text, Align::Left);
    ui.text(status, Rect::new(r.x + 9.0, r.y + 60.0, r.w - 13.0, 13.0), 11.0, REGULAR, t.subtext, Align::Left);
    ui.alpha = base;
    if !available {
        let _ = ui.clicked(r); // swallow
        return (false, false);
    }
    let toggled = ui.clicked(badge);
    let opened = !toggled && ui.clicked(r);
    (toggled, opened)
}

fn draw_tiles(ui: &mut Ui, s: &mut State, d: &Data, r: Rect) {
    let w = (r.w + 6.0) / 5.0;
    let base = ui.alpha;
    let mut cols = Vec::new();
    for i in 0..5 {
        // Rise in, 40 ms apart (C# RiseIn).
        let k = ((model::clock_secs() - s.shown_at - 0.04 * i as f64) / 0.32).clamp(0.0, 1.0) as f32;
        if k < 1.0 {
            ui.animate = true;
        }
        let e = 1.0 - (1.0 - k).powi(3);
        cols.push((Rect::new(r.x + i as f32 * w, r.y + 8.0 * (1.0 - e), w - 6.0, r.h), e));
    }
    let turning = |b: Option<bool>| b.map(|on| if on { tr("Turning on…") } else { tr("Turning off…") });

    ui.alpha = base * cols[0].1;
    let wifi_on = d.wifi == Some(true);
    let wifi_status = turning(d.busy[0]).map(str::to_string).unwrap_or_else(|| match d.wifi {
        None if d.loaded => tr("Not available").into(),
        None => "…".into(),
        Some(false) => tr("Off").into(),
        Some(true) => d.wifi_name.clone().unwrap_or_else(|| tr("Not connected").into()),
    });
    let (tog, open) = tile(ui, cols[0].0, "Wifi", tr("Wi-Fi"), &wifi_status, wifi_on, d.wifi.is_some() || !d.loaded);
    if tog {
        switch_radio(s, 0, !wifi_on);
    }
    if open {
        open_detail(s, Detail::Wifi);
    }

    ui.alpha = base * cols[1].1;
    let bt_on = d.bt == Some(true);
    let bt_status = turning(d.busy[1]).unwrap_or(match d.bt {
        None if d.loaded => tr("Not available"),
        None => "…",
        Some(true) => tr("On"),
        Some(false) => tr("Off"),
    });
    let (tog, open) = tile(ui, cols[1].0, "Bluetooth", "Bluetooth", bt_status, bt_on, d.bt.is_some() || !d.loaded);
    if tog {
        switch_radio(s, 1, !bt_on);
    }
    if open {
        open_detail(s, Detail::Bluetooth);
    }

    ui.alpha = base * cols[2].1;
    let hs_on = d.hotspot == Some(true);
    let hs_status = turning(d.busy[2]).unwrap_or(match d.hotspot {
        None if d.loaded => tr("Not available"),
        None => "…",
        Some(true) => tr("On"),
        Some(false) => tr("Off"),
    });
    let (tog, open) = tile(ui, cols[2].0, "Signal", tr("Hotspot"), hs_status, hs_on, d.hotspot.is_some() || !d.loaded);
    if tog {
        switch_radio(s, 2, !hs_on);
    }
    if open {
        crate::shell::open_settings_page("ms-settings:network-mobilehotspot");
    }

    ui.alpha = base * cols[3].1;
    let (tog, open) = tile(
        ui,
        cols[3].0,
        if d.silent { "SpeakerMuted" } else { "Bell" },
        tr("Silent"),
        if d.silent { tr("No pop-ups") } else { tr("Off") },
        d.silent,
        true,
    );
    if tog {
        // Silent hides notification pop-ups (they still collect in Notifications) and mutes their sounds.
        let on = !d.silent;
        set_local(s, |x| x.silent = on);
        crate::services::settings::update(|x| x.silent_mode = on);
        bg::spawn(move || radios::set_silent(on));
    }
    if open {
        crate::shell::open_settings_page("ms-settings:notifications");
    }

    // Keep awake: the PC and screen don't sleep while it's on (runs on this, the window thread).
    ui.alpha = base * cols[4].1;
    let awake = crate::services::awake::on();
    let (tog, open) = tile(ui, cols[4].0, "Coffee", tr("Awake"), if awake { tr("On") } else { tr("Off") }, awake, true);
    if tog || open {
        crate::services::awake::set(!awake);
    }
    ui.alpha = base;
}

/// Switches Wi-Fi (0), Bluetooth (1) or the hotspot (2); when Windows says no, opens its Settings page.
fn switch_radio(s: &State, which: usize, on: bool) {
    let busy = s.data.lock().unwrap().busy.iter().any(|b| b.is_some());
    if busy {
        return;
    }
    set_local(s, |x| {
        x.busy[which] = Some(on);
        match which {
            0 => x.wifi = Some(on),
            1 => x.bt = Some(on),
            _ => x.hotspot = Some(on),
        }
    });
    let data = s.data.clone();
    bg::spawn(move || {
        let ok = match which {
            0 => radios::set_wifi(on),
            1 => radios::set_bluetooth(on),
            _ => radios::set_hotspot(on).is_ok(),
        };
        if !ok {
            crate::shell::open_settings_page(match which {
                0 => "ms-settings:network-wifi",
                1 => "ms-settings:bluetooth",
                _ => "ms-settings:network-mobilehotspot",
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(if which == 0 { 600 } else { 800 }));
        let state = match which {
            0 => radios::wifi(),
            1 => radios::bluetooth(),
            _ => radios::hotspot(),
        };
        let name = radios::connected_name();
        let mut x = data.lock().unwrap();
        x.busy[which] = None;
        match which {
            0 => {
                x.wifi = state;
                x.wifi_name = name;
            }
            1 => x.bt = state,
            _ => x.hotspot = state,
        }
    });
    // The open list follows the switch.
    if which == 0 {
        load_networks(s, 900);
    } else if which == 1 {
        load_paired(s, 900);
    }
}

fn open_detail(s: &mut State, det: Detail) {
    s.detail = Some(det);
    s.detail_at = model::clock_secs();
    set_local(s, |x| x.join = None);
    match det {
        Detail::Wifi => load_networks(s, 0),
        Detail::Bluetooth => load_paired(s, 0),
    }
}

fn load_networks(s: &State, delay_ms: u64) {
    set_local(s, |x| x.networks_loading = true);
    let data = s.data.clone();
    bg::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        let n = radios::networks();
        let mut x = data.lock().unwrap();
        x.networks = n;
        x.networks_loading = false;
    });
}

fn load_paired(s: &State, delay_ms: u64) {
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    set_local(s, |x| x.paired_loading = true);
    let data = s.data.clone();
    bg::spawn(move || {
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                RUNNING.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        let _done = Done;
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        let p = radios::paired();
        let mut x = data.lock().unwrap();
        x.paired = p;
        x.paired_loading = false;
    });
}

// ---------------------------------------------------------------------- shared bits

/// The C# slim slider: 3 px track, accent fill, 12 px thumb with a dark ring. Drag on `track`; the wheel
/// over `wheel_area` nudges it 4 % a notch. Returns the new value while it changes.
fn slider(ui: &mut Ui, sid: u64, track: Rect, wheel_area: Rect, value: f32) -> Option<f32> {
    let t = ui.t;
    let v = value.clamp(0.0, 1.0);
    let span = (track.w - 12.0).max(1.0);
    let mut out = None;
    if ui.clicked(Rect::new(track.x, track.cy() - 8.0, track.w, 16.0)) {
        ui.st.active = sid;
    }
    if ui.st.active == sid {
        if ui.input.down {
            // Only a real change counts (holding still sets nothing).
            let nv = ((ui.mouse().0 - track.x - 6.0) / span).clamp(0.0, 1.0);
            if (nv - v).abs() >= 0.005 {
                out = Some(nv);
            }
            ui.animate = true;
        } else {
            ui.st.active = 0;
        }
    } else if ui.input.wheel != 0.0 && wheel_area.contains(ui.mouse()) && ui.hovered(wheel_area) {
        out = Some((v + ui.input.wheel * 0.04).clamp(0.0, 1.0));
        ui.input.wheel = 0.0;
    }
    let shown = out.unwrap_or(v);
    let cy = track.cy();
    let kx = track.x + 6.0 + span * shown;
    ui.fill(Rect::new(track.x, cy - 1.5, track.w, 3.0), 1.5, t.track);
    ui.fill(Rect::new(track.x, cy - 1.5, kx - track.x, 3.0), 1.5, t.accent);
    ui.g.fill_circle(kx, cy, 6.0, ui.c(t.pill));
    ui.g.fill_circle(kx, cy, 4.0, ui.c(t.text));
    out
}

/// The C# combo box face over the kit dropdown (Surface2, radius 7, 11.5 text, a thin chevron).
fn combo(ui: &mut Ui, cid: u64, r: Rect, options: &[String], selected: usize, enabled: bool) -> Option<usize> {
    let t = ui.t;
    let picked = if enabled { ui.dropdown(cid, r, options, selected) } else { None };
    let hov = enabled && (ui.hovered(r) || ui.st.dropdown == cid);
    // Hover swaps Surface2 for the translucent hover brush over the card.
    if hov {
        ui.fill(r, 7.0, t.surface);
        ui.fill(r, 7.0, t.hover);
    } else {
        ui.fill(r, 7.0, t.surface2);
    }
    let label = options.get(selected).map(String::as_str).unwrap_or("");
    ui.text(label, r.pad(9.0, 0.0, 26.0, 0.0), 11.0, REGULAR, t.text, Align::Left);
    let (cx, cy) = (r.right() - 10.0 - 4.0, r.cy());
    let c = ui.c(t.subtext);
    ui.g.line(cx - 4.0, cy - 2.0, cx, cy + 2.0, c, 1.4);
    ui.g.line(cx, cy + 2.0, cx + 4.0, cy - 2.0, c, 1.4);
    picked
}

// ---------------------------------------------------------------------- brightness

fn brightness_card(ui: &mut Ui, s: &mut State, d: &Data, r: Rect) {
    let t = ui.t;
    ui.card(r, 16.0);
    let inner = r.pad(14.0, 10.0, 14.0, 8.0);
    ui.icon("Sun", inner.x + 6.5, inner.y + 7.0, 13.0, t.subtext);
    ui.text(tr("BRIGHTNESS"), Rect::new(inner.x + 19.0, inner.y, 200.0, 14.0), 11.0, BOLD, t.subtext, Align::Left);
    let list = Rect::new(inner.x, inner.y + 20.0, inner.w, inner.h - 20.0);
    if d.displays.is_empty() {
        let msg = if d.loaded {
            tr("No display here lets apps change its brightness. On external monitors, turn on DDC/CI in the monitor's menu.")
        } else {
            tr("Looking for displays…")
        };
        ui.wrap(msg, Rect::new(list.x, list.cy() - 15.0, list.w, 30.0), 11.0, REGULAR, t.subtext);
        return;
    }
    // Each display: name (14) and a 22 high slider line; 6 between.
    let row_h = 36.0;
    let content_h = row_h * d.displays.len() as f32 + 6.0 * (d.displays.len() as f32 - 1.0);
    let displays = d.displays.clone();
    let data = s.data.clone();
    ui.scroll(id("ctl.bright"), list, content_h, |ui, top| {
        for (i, disp) in displays.iter().enumerate() {
            let y = top + i as f32 * (row_h + 6.0);
            let name = if disp.internal && !disp.name.to_lowercase().contains("built-in") { crate::i18n::tf!("Built-in · {}", "Bawaan · {}", tr(&disp.name)) } else { tr(&disp.name).to_string() };
            ui.text(&name, Rect::new(list.x, y, list.w, 14.0), 11.0, MEDIUM, ui.t.text, Align::Left);
            let line = Rect::new(list.x, y + 14.0, list.w, 22.0);
            let track = Rect::new(line.x, line.y, line.w - 30.0 - 6.0, line.h);
            if let Some(nv) = slider(ui, idx("ctl.disp", i), track, Rect::new(list.x, y, list.w, row_h), disp.level as f32 / 100.0) {
                let level = (nv * 100.0).round() as u32;
                if level != disp.level {
                    let did = disp.id.clone();
                    data.lock().unwrap().displays[i].level = level;
                    bg::latest(id("ctl.bright") ^ i as u64, move || brightness::set(&did, level));
                }
            }
            let shown = data.lock().unwrap().displays.get(i).map_or(disp.level, |x| x.level);
            ui.text(&shown.to_string(), Rect::new(line.right() - 30.0, line.y, 30.0, line.h), 11.0, SEMIBOLD, ui.t.subtext, Align::Right);
        }
    });
}

// ---------------------------------------------------------------------- sound

fn sound_card(ui: &mut Ui, s: &mut State, d: &Data, r: Rect) {
    let t = ui.t;
    ui.card(r, 16.0);
    let inner = r.pad(12.0, 8.0, 8.0, 6.0);
    let bad = color(BAD, 1.0);
    let mut y = inner.y;
    for input in [false, true] {
        let devs = if input { &d.inputs } else { &d.outputs };
        let level = if input { d.in_level } else { d.out_level };
        // Picker row (28): mute icon, combo box.
        let ic = Rect::new(inner.x, y, 28.0, 28.0);
        let muted = level.is_some_and(|l| l.1);
        let icon = match (input, muted) {
            (false, false) => "Speaker",
            (false, true) => "SpeakerMuted",
            (true, _) => "Microphone",
        };
        if let Some((lv, m)) = level {
            if ui.icon_button(ic, icon, 15.0, if m { bad } else { t.subtext }) {
                let m = !m;
                set_local(s, |x| if input { x.in_level = Some((lv, m)) } else { x.out_level = Some((lv, m)) });
                bg::spawn(move || audio::set_mute(input, m));
            }
        } else {
            ui.icon(icon, ic.cx(), ic.cy(), 15.0, with_alpha(t.subtext, 0.35));
        }
        let mut names: Vec<String> = devs.iter().map(|x| x.name.clone()).collect();
        if names.is_empty() {
            names.push(if input { tr("No microphone") } else { tr("No speakers") }.into());
        }
        let sel = devs.iter().position(|x| x.is_default).unwrap_or(0);
        let cb = Rect::new(ic.right() + 4.0, y + 1.0, inner.right() - ic.right() - 8.0, 26.0);
        if let Some(i) = combo(ui, id(if input { "ctl.in" } else { "ctl.out" }), cb, &names, sel, devs.len() > 1) {
            if let Some(dev) = devs.get(i).filter(|x| !x.is_default) {
                let did = dev.id.clone();
                let data = s.data.clone();
                bg::spawn(move || {
                    audio::set_default(&did);
                    let (o, i, ol, il, ss) = (audio::devices(false), audio::devices(true), audio::master(false), audio::master(true), audio::sessions());
                    let mut x = data.lock().unwrap();
                    x.outputs = o;
                    x.inputs = i;
                    x.out_level = ol;
                    x.in_level = il;
                    x.sessions = ss;
                });
            }
        }
        y += 28.0 + 2.0;
        // Level row (30): slider, percentage (the mute column stays empty).
        if let Some((lv, m)) = level {
            let row = Rect::new(inner.x, y, inner.w, 30.0);
            let track = Rect::new(row.x + 2.0, row.y, row.w - 2.0 - 6.0 - 30.0 - 28.0, row.h);
            if let Some(nv) = slider(ui, id(if input { "ctl.inlv" } else { "ctl.outlv" }), track, row, lv) {
                set_local(s, |x| if input { x.in_level = Some((nv, m)) } else { x.out_level = Some((nv, m)) });
                bg::latest(id("ctl.level") ^ input as u64, move || audio::set_master(input, nv));
            }
            let shown = {
                let x = s.data.lock().unwrap();
                (if input { x.in_level } else { x.out_level }).map_or(lv, |l| l.0)
            };
            ui.text(&((shown * 100.0).round() as i32).to_string(), Rect::new(track.right() + 6.0, row.y, 30.0, row.h), 11.0, SEMIBOLD, t.subtext, Align::Right);
            y += 30.0;
        }
        y += 2.0;
    }
    // Hairline, then the apps (4 below it).
    ui.fill(Rect::new(inner.x, y, inner.w, 1.0), 0.0, t.stroke);
    y += 5.0;
    let list = Rect::new(inner.x, y, inner.w, inner.bottom() - y);
    let sessions = d.sessions.clone();
    if sessions.is_empty() {
        if d.loaded {
            ui.text(tr("No apps are using sound right now"), Rect::new(list.x + 2.0, list.y + 10.0, list.w, 16.0), 11.0, REGULAR, t.subtext, Align::Left);
        }
        return;
    }
    let row_h = 28.0;
    let data = s.data.clone();
    ui.scroll(id("ctl.apps"), list, row_h * sessions.len() as f32, |ui, top| {
        for (i, ses) in sessions.iter().enumerate() {
            let row = Rect::new(list.x, top + i as f32 * row_h, list.w - 4.0, row_h);
            if row.bottom() < list.y || row.y > list.bottom() {
                continue;
            }
            let t = ui.t;
            // Icon (16; a bell for System sounds), name (104), slider, %, mute.
            let ic = Rect::new(row.x, row.cy() - 8.0, 16.0, 16.0);
            let px = (16.0 * ui.g.scale).round() as u32;
            let drawn = ses.pid != 0
                && appicons::pid(ses.pid, px).is_some_and(|(k, png)| {
                    ui.g.load_image(k, &png) && {
                        ui.g.image(k, ic.d2d(), 0.0, ui.alpha);
                        true
                    }
                });
            if !drawn {
                ui.icon(if ses.pid == 0 { "Bell" } else { "Speaker" }, ic.x + 7.0, ic.cy(), 14.0, t.subtext);
            }
            // Apps not playing right now are dimmer (as in the C# app).
            let name_c = if ses.active || ses.pid == 0 { t.text } else { crate::gfx::with_alpha(t.text, 0.6) };
            ui.text(&ses.name, Rect::new(row.x + 24.0, row.y, 100.0, row.h), 12.0, MEDIUM, name_c, Align::Left);
            let rest = row.x + 24.0 + 104.0;
            let track = Rect::new(rest + 6.0, row.y, row.right() - 28.0 - 30.0 - 6.0 - rest - 6.0, row.h);
            let wheel = Rect::new(rest, row.y, row.right() - rest, row.h);
            if let Some(nv) = slider(ui, idx("ctl.app", ses.pid as usize), track, wheel, ses.volume) {
                let pid = ses.pid;
                if let Some(x) = data.lock().unwrap().sessions.get_mut(i) {
                    x.volume = nv;
                    if nv > 0.0 {
                        x.muted = false;
                    }
                }
                bg::latest(id("ctl.session") ^ pid as u64, move || audio::set_session_volume(pid, nv));
            }
            let (vol, muted) = data.lock().unwrap().sessions.get(i).map_or((ses.volume, ses.muted), |x| (x.volume, x.muted));
            ui.text(&((vol * 100.0).round() as i32).to_string(), Rect::new(row.right() - 28.0 - 30.0, row.y, 30.0, row.h), 11.0, SEMIBOLD, t.subtext, Align::Right);
            let mute = Rect::new(row.right() - 26.0, row.cy() - 13.0, 26.0, 26.0);
            if ui.icon_button(mute, if muted { "SpeakerMuted" } else { "Speaker" }, 13.0, if muted { color(BAD, 1.0) } else { t.subtext }) {
                let (pid, m) = (ses.pid, !muted);
                if let Some(x) = data.lock().unwrap().sessions.get_mut(i) {
                    x.muted = m;
                }
                bg::spawn(move || audio::set_session_mute(pid, m));
            }
        }
    });
}

// ---------------------------------------------------------------------- tile details

/// The iOS-style switch (38×22), knob gliding 180 ms. Returns true when clicked.
fn switch(ui: &mut Ui, sid: u64, r: Rect, on: bool) -> bool {
    let t = ui.t;
    let target = if on { 1.0 } else { 0.0 };
    let cur = ui.st.values.get(&sid).copied().unwrap_or(target);
    let next = if (cur - target).abs() < 0.02 { target } else { cur + (target - cur) * 0.35 };
    if next != target {
        ui.animate = true;
    }
    ui.st.values.insert(sid, next);
    let track = mix(t.hover, t.accent, next);
    ui.fill(r, 11.0, if next > 0.5 { track } else { t.hover });
    let knob = if next > 0.5 { t.on_accent } else { color(0xFFFFFF, 1.0) };
    ui.g.fill_circle(r.x + 2.0 + 9.0 + 16.0 * next, r.cy(), 9.0, ui.c(knob));
    ui.clicked(r)
}

/// Note text (11.5, wrapped, margin 6,8,6,4). Returns its height with margins.
fn note(ui: &mut Ui, text: &str, x: f32, y: f32, w: f32) -> f32 {
    let lines = (ui.measure(text, 11.0, REGULAR) / (w - 12.0)).ceil().max(1.0);
    let h = lines * 15.0;
    ui.wrap(text, Rect::new(x + 6.0, y + 8.0, w - 12.0, h + 2.0), 11.0, REGULAR, ui.t.subtext);
    h + 12.0
}

/// A choice row (min 34, radius 8): icon, name, a detail or trailing bit. Returns true when clicked.
#[allow(clippy::too_many_arguments)]
fn choice(ui: &mut Ui, row: Rect, icon: &'static str, icon_alpha: f32, title: &str, detail: &str, selected: bool, clickable: bool, trailing_w: f32) -> bool {
    let t = ui.t;
    if selected {
        ui.fill(row, 8.0, t.accent_soft);
    } else if clickable && ui.hovered(row) {
        ui.fill(row, 8.0, t.surface2);
    }
    let ic: Color = if selected { t.accent } else { t.subtext };
    ui.icon(icon, row.x + 8.0 + 7.0, row.cy(), 14.0, with_alpha(ic, icon_alpha));
    let dw = if detail.is_empty() { trailing_w } else { ui.measure(detail, 11.0, REGULAR) + 10.0 };
    ui.text(title, Rect::new(row.x + 34.0, row.y, row.w - 34.0 - 6.0 - dw, row.h), 12.5, if selected { SEMIBOLD } else { REGULAR }, t.text, Align::Left);
    if !detail.is_empty() {
        ui.text(detail, Rect::new(row.right() - 6.0 - dw, row.y, dw - 2.0, row.h), 11.0, REGULAR, if selected { t.accent } else { t.subtext }, Align::Right);
    }
    clickable && ui.clicked(row)
}

fn detail(ui: &mut Ui, s: &mut State, d: &Data, r: Rect, det: Detail) {
    let t = ui.t;
    ui.card(r, 16.0);
    let inner = r.pad(10.0, 8.0, 10.0, 8.0);
    // Header (32): ‹, title, switch.
    let back = Rect::new(inner.x, inner.y + 2.0, 28.0, 28.0);
    if ui.icon_button(back, "ChevronLeft", 15.0, t.subtext) {
        s.detail = None;
        set_local(s, |x| x.join = None);
        return;
    }
    let (title, on, more, uri) = match det {
        Detail::Wifi => (tr("Wi-Fi"), d.wifi == Some(true), tr("More Wi-Fi settings"), "ms-settings:network-wifi"),
        Detail::Bluetooth => ("Bluetooth", d.bt == Some(true), tr("Pair or connect in Bluetooth settings"), "ms-settings:bluetooth"),
    };
    ui.text(title, Rect::new(back.right() + 6.0, inner.y, 200.0, 32.0), 14.0, SEMIBOLD, t.text, Align::Left);
    let available = match det {
        Detail::Wifi => d.wifi.is_some(),
        Detail::Bluetooth => d.bt.is_some(),
    };
    if available {
        let sw = Rect::new(inner.right() - 38.0, inner.y + 5.0, 38.0, 22.0);
        if switch(ui, id(if det == Detail::Wifi { "ctl.sw.wifi" } else { "ctl.sw.bt" }), sw, on) {
            switch_radio(s, if det == Detail::Wifi { 0 } else { 1 }, !on);
        }
    }

    // "More …" link at the bottom (RowButton: 8,5 padding, radius 10).
    let more_r = Rect::new(inner.x, inner.bottom() - 24.0, ui.measure(more, 11.0, REGULAR) + 16.0, 24.0);
    if ui.hit(more_r, 10.0) {
        crate::shell::open_settings_page(uri);
    }
    ui.text(more, more_r.pad(8.0, 0.0, 0.0, 0.0), 11.0, REGULAR, t.subtext, Align::Left);

    let list = Rect::new(inner.x, inner.y + 36.0, inner.w, more_r.y - 4.0 - inner.y - 36.0);
    match det {
        Detail::Wifi => wifi_list(ui, s, d, list),
        Detail::Bluetooth => {
            let msg = if d.bt.is_none() {
                Some(tr("This PC has no Bluetooth."))
            } else if !on {
                Some(tr("Bluetooth is off. Switch it on above to see your devices."))
            } else if d.paired_loading && d.paired.is_empty() {
                Some(tr("Looking for paired devices…"))
            } else if d.paired.is_empty() {
                Some(tr("No paired devices yet."))
            } else {
                None
            };
            if let Some(m) = msg {
                note(ui, m, list.x, list.y, list.w);
                return;
            }
            let devs = d.paired.clone();
            let busy = d.bt_busy.clone();
            let mut clicked = None;
            ui.scroll(id("ctl.bt"), list, 36.0 * devs.len() as f32, |ui, top| {
                for (i, b) in devs.iter().enumerate() {
                    let row = Rect::new(list.x, top + i as f32 * 36.0, list.w - 6.0, 34.0);
                    let status = match &busy {
                        Some((n, true)) if *n == b.name => tr("Connecting…"),
                        Some((n, false)) if *n == b.name => tr("Disconnecting…"),
                        _ if b.connected => tr("Connected · click to disconnect"),
                        _ => tr("Not connected · click to connect"),
                    };
                    if choice(ui, row, "Bluetooth", 1.0, &b.name, status, b.connected, true, 0.0) && busy.is_none() {
                        clicked = Some((b.name.clone(), !b.connected));
                    }
                }
            });
            if let Some((name, connect)) = clicked {
                bt_toggle(s, name, connect);
            }
        }
    }
}

/// Connects or disconnects a paired Bluetooth device in place. Audio devices (headphones, earbuds,
/// speakers) switch right away; anything else (a mouse, a phone) can't be connected by an app, so its
/// Settings page opens for that one click.
fn bt_toggle(s: &State, name: String, connect: bool) {
    set_local(s, |x| x.bt_busy = Some((name.clone(), connect)));
    let data = s.data.clone();
    bg::spawn(move || {
        let ok = audio::bt_connect(&name, connect);
        if !ok {
            crate::shell::open_settings_page("ms-settings:bluetooth");
        }
        // Give the link a few seconds to come up (or go down), then read the list again.
        for _ in 0..8 {
            std::thread::sleep(std::time::Duration::from_millis(750));
            let p = radios::paired();
            let settled = p.iter().find(|b| b.name == name).map_or(true, |b| b.connected == connect);
            data.lock().unwrap().paired = p;
            crate::bg::refresh();
            if settled || !ok {
                break;
            }
        }
        data.lock().unwrap().bt_busy = None;
        crate::bg::refresh();
    });
}

fn wifi_list(ui: &mut Ui, s: &mut State, d: &Data, list: Rect) {
    let t = ui.t;
    let msg = if d.wifi.is_none() {
        Some(tr("This PC has no Wi-Fi."))
    } else if d.wifi != Some(true) {
        Some(tr("Wi-Fi is off. Switch it on above to see networks."))
    } else if d.networks_loading && d.networks.is_empty() {
        Some(tr("Looking for networks…"))
    } else if d.networks.is_empty() {
        Some(tr("No networks in range."))
    } else {
        None
    };
    if let Some(m) = msg {
        note(ui, m, list.x, list.y, list.w);
        return;
    }
    let nets = d.networks.clone();
    let join = d.join.clone();
    if !matches!(join, Some((_, Join::Password { .. }))) {
        ui.st.values.remove(&id("ctl.wifipw"));
    }
    // Row heights: 36, plus the join line under the network being joined.
    let extra = |j: &Join| match j {
        Join::Password { retry } => 36.0 + if *retry { 21.0 } else { 0.0 },
        Join::Connecting | Join::Checking | Join::Failed(_) => 21.0,
    };
    let content_h: f32 = nets.iter().map(|n| 36.0 + join.as_ref().filter(|(s, _)| *s == n.ssid).map_or(0.0, |(_, j)| extra(j))).sum();
    let data = s.data.clone();
    let mut start: Option<(String, Option<String>)> = None;
    let mut disconnect = false;
    ui.scroll(id("ctl.nets"), list, content_h, |ui, top| {
        let mut y = top;
        for n in &nets {
            let row = Rect::new(list.x, y, list.w - 6.0, 34.0);
            let bars = 0.35 + 0.65 * (n.signal as f32 / 4.0).clamp(0.0, 1.0);
            let trailing = if n.connected { ui.measure(tr("Disconnect"), 11.0, MEDIUM) + 20.0 + 8.0 } else if n.secured { 24.0 } else { 0.0 };
            let clicked = choice(ui, row, "Wifi", bars, &n.ssid, "", n.connected, !n.connected, trailing);
            if n.connected {
                let b = Rect::new(row.right() - 6.0 - (trailing - 8.0), row.cy() - 12.0, trailing - 8.0, 24.0);
                let hov = ui.hovered(b);
                ui.fill(b, 8.0, if hov { mix(t.surface2, t.text, 0.08) } else { t.surface2 });
                ui.text(tr("Disconnect"), b, 11.0, MEDIUM, t.text, Align::Center);
                if ui.clicked(b) {
                    disconnect = true;
                }
            } else if n.secured {
                ui.icon("Lock", row.right() - 6.0 - 4.0 - 6.0, row.cy(), 12.0, with_alpha(t.subtext, 0.7));
            }
            if clicked {
                start = Some((n.ssid.clone(), None));
            }
            y += 36.0;
            if let Some((_, j)) = join.as_ref().filter(|(s, _)| *s == n.ssid) {
                match j {
                    Join::Connecting => y += note(ui, tr("Connecting…"), list.x, y - 6.0, list.w) - 6.0,
                    Join::Checking => y += note(ui, tr("Checking the password…"), list.x, y - 6.0, list.w) - 6.0,
                    Join::Failed(e) => y += note(ui, e, list.x, y - 6.0, list.w) - 6.0,
                    Join::Password { retry } => {
                        // Password line (margin 34,2,6,6): masked box and Connect.
                        let line = Rect::new(list.x + 34.0, y + 2.0 - 2.0, list.w - 34.0 - 6.0 - 6.0, 28.0);
                        let cw = ui.measure(tr("Connect"), 13.0, SEMIBOLD) + 24.0;
                        let (go_r, field) = line.take_right(cw, 6.0);
                        let fid = id("ctl.wifipw");
                        let pw = password_box(ui, fid, field);
                        let hov = ui.hovered(go_r);
                        ui.fill(go_r, 8.0, if hov { mix(t.accent, t.pill, 0.12) } else { t.accent });
                        ui.text(tr("Connect"), go_r, 13.0, SEMIBOLD, t.on_accent, Align::Center);
                        let go = ui.clicked(go_r);
                        let typed = ui.field_text(fid);
                        if let Some(p) = pw.or_else(|| (go && !typed.is_empty()).then_some(typed)) {
                            ui.set_field_text(fid, "");
                            start = Some((n.ssid.clone(), Some(p)));
                        }
                        y += 36.0;
                        if *retry {
                            y += note(ui, tr("That password didn't work. Try again."), list.x, y - 6.0, list.w) - 6.0;
                        }
                    }
                }
            }
        }
    });
    if disconnect {
        let data = data.clone();
        bg::spawn(move || {
            radios::disconnect();
            std::thread::sleep(std::time::Duration::from_millis(600));
            let (n, name) = (radios::networks(), radios::connected_name());
            let mut x = data.lock().unwrap();
            x.networks = n;
            x.wifi_name = name;
        });
    }
    if let Some((ssid, password)) = start {
        let retrying = password.is_some();
        set_local(s, |x| x.join = Some((ssid.clone(), if retrying { Join::Checking } else { Join::Connecting })));
        bg::spawn(move || {
            let res = radios::connect(&ssid, password.as_deref());
            let mut x = data.lock().unwrap();
            match res {
                Ok(()) => {
                    x.join = None;
                    drop(x);
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    let (n, name) = (radios::networks(), radios::connected_name());
                    let mut x = data.lock().unwrap();
                    x.networks = n;
                    x.wifi_name = name;
                }
                Err(e) if e == "password" => x.join = Some((ssid, Join::Password { retry: retrying })),
                Err(e) => x.join = Some((ssid, Join::Failed(e))),
            }
        });
    }
}

/// A password box (Surface2, radius 7): the kit field underneath, dots on top. Focuses itself when it
/// appears. Returns the password on Enter.
fn password_box(ui: &mut Ui, fid: u64, r: Rect) -> Option<String> {
    let t = ui.t;
    if ui.st.values.insert(fid, 1.0).is_none() {
        ui.st.focus = fid;
        ui.wants_keyboard = true;
    }
    let focused = ui.st.focus == fid;
    ui.fill(r, 7.0, t.surface2);
    if focused {
        ui.g.stroke_round(r.inset(0.5, 0.5).d2d(), 6.5, ui.c(with_alpha(t.accent, 0.6)), 1.0);
    }
    let inner = r.pad(9.0, 0.0, 9.0, 0.0);
    let got = ui.text_field(fid, inner, tr("Network password"), 13.0);
    // Cover what the kit drew and show dots instead.
    let text = ui.field_text(fid);
    if !text.is_empty() || got.is_some() {
        ui.fill(r.inset(1.0, 1.0), 6.0, t.surface2);
        let dots: String = "•".repeat(text.chars().count());
        ui.text(&dots, inner, 12.0, REGULAR, t.text, Align::Left);
        if focused && ui.st.caret_on {
            let caret = ui.st.edits.get(&fid).map_or(0, |e| e.caret);
            let x = inner.x + ui.measure(&"•".repeat(caret), 12.0, REGULAR);
            ui.g.line(x + 0.5, r.cy() - 7.5, x + 0.5, r.cy() + 7.5, ui.c(t.text), 1.2);
        }
    }
    got
}
