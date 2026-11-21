//! The Settings pages. Each one lays out cards of rows (`Pg::card`) and then draws the row controls.
//! Same settings, wording and choices as the .NET build's Settings.

use crate::bg;
use crate::gfx::{color, mix, with_alpha, Align, MEDIUM, REGULAR, SEMIBOLD};
use crate::i18n::tr;
use crate::model;
use crate::services::{calendar, cliphistory, phone, prayer, settings, ticktick};
use crate::theme::{self, ACCENTS, SCHEMES};
use crate::tools;
use crate::ui::{id, Rect};

use super::{button, button_w, lh, small_icon, switch, Pg, R, CTRL_H};

const SOUNDS: [&str; 4] = ["Calendar", "Chime", "Bell", "No Sound"];

// ---------------------------------------------------------------- General

pub fn general(p: &mut Pg) {
    let s = p.st.clone();
    p.title(tr("General"), tr("How FathNotch behaves on your PC."));

    // Language names stay in their own language, so either can be found whichever is on.
    let rr = p.card(Some("LANGUAGE"), &[R::c("Language", "The language of FathNotch's menus, buttons and alerts.", 200.0)]);
    let langs = [crate::i18n::SYSTEM.to_string(), crate::i18n::ENGLISH.into(), crate::i18n::INDONESIAN.into()];
    p.pick(rr[0], "set.lang", &["Same as Windows", "English", "Bahasa Indonesia"], &langs, &s.language, 0, |x, v| x.language = v);

    let rr = p.card(Some(tr("PROFILE")), &[R::c(tr("Name"), tr("Shown in the greeting on Home."), 200.0)]);
    if let Some(r) = rr[0] {
        let user = std::env::var("USERNAME").unwrap_or_default();
        if let Some(v) = p.field("settings.name", r, &user, &s.display_name) {
            settings::update(|x| x.display_name = v);
        }
    }

    let rr = p.card(
        Some(tr("BEHAVIOR")),
        &[
            R::c(tr("Open notch"), tr("Open when the pointer rests on the notch, or only when you click it."), 140.0),
            R::c(tr("Hover delay"), tr("How long the pointer rests on the notch before it opens."), 140.0),
            R::c(tr("Show notch"), tr("Always, only while music, a timer or a reminder is on, or only when the pointer touches the top edge of the screen."), 140.0),
            R::t(tr("Hide in full screen"), tr("Stay out of the way of full-screen apps and videos; touch the top edge to bring it back. Games hide it completely.")),
            R::t(tr("Hide from screenshots"), tr("Leave the notch out of screenshots, recordings and screen sharing.")),
            R::t(tr("Dodge the pointer"), tr("The closed notch dodges the pointer from the sides. Catch it from below.")),
        ],
    );
    p.pick(rr[0], "set.open", &[tr("On Hover"), tr("On Click")], &[0, 1], &s.open_mode, 0, |x, v| x.open_mode = v);
    p.pick(rr[1], "set.delay", &[tr("Instant"), tr("Fast"), tr("Normal"), tr("Slow")], &[0, 80, 150, 400], &s.hover_delay_ms, 2, |x, v| x.hover_delay_ms = v);
    let vis = ["Always".to_string(), "WhenActive".into(), "HoverOnly".into()];
    p.pick(rr[2], "set.vis", &[tr("Always"), tr("When active"), tr("On top-edge hover")], &vis, &s.notch_visibility, 0, |x, v| x.notch_visibility = v);
    p.tog(rr[3], s.hide_in_fullscreen, |x, v| x.hide_in_fullscreen = v);
    p.tog(rr[4], s.hide_from_capture, |x, v| x.hide_from_capture = v);
    p.tog(rr[5], s.repel_cursor, |x, v| x.repel_cursor = v);

    // Hide for these apps: listed apps, then a pick of apps with a window open right now.
    let running: Vec<(String, String)> = p.s.shared.lock().unwrap().running.clone().unwrap_or_default();
    let label_of = |name: &str| running.iter().find(|(n, _)| n == name).map_or(name.to_string(), |(_, l)| l.clone());
    let pickable: Vec<&(String, String)> = running.iter().filter(|(n, _)| !s.hide_for_apps.contains(n)).collect();
    let rm_w = p.button_w(tr("Remove"));
    let add_w = p.button_w(tr("Add"));
    let mut rows: Vec<R> = s.hide_for_apps.iter().map(|a| R::c(label_of(a), format!("{a}.exe"), rm_w)).collect();
    rows.push(R::c(tr("Add an app"), tr("Hide the notch while it's in front, on any display."), 200.0 + 6.0 + add_w));
    if !s.hide_for_apps.is_empty() {
        rows.push(R::c(tr("When one is in front"), tr("Fade out (touch the top edge to bring it back), or hide completely."), 170.0));
        rows.push(R::c(tr("Alerts"), tr("Hold alerts until you switch away, or let them pop out anyway."), 200.0));
    }
    let rr = p.card(Some(tr("HIDE FOR THESE APPS")), &rows);
    let n = s.hide_for_apps.len();
    for (i, app) in s.hide_for_apps.iter().enumerate() {
        if p.button(rr[i], tr("Remove"), false) {
            let app = app.clone();
            settings::update(|x| x.hide_for_apps.retain(|a| *a != app));
        }
    }
    if let Some(r) = rr[n] {
        let (combo_r, rest) = r.take_left(200.0, 6.0);
        let labels: Vec<&str> = pickable.iter().map(|(_, l)| l.as_str()).collect();
        p.s.app_pick = p.s.app_pick.min(labels.len().saturating_sub(1));
        let sel = p.s.app_pick;
        if let Some(i) = p.combo_enabled("set.addapp", combo_r, &labels, sel, !labels.is_empty()) {
            p.s.app_pick = i;
        }
        if button(p.ui, rest, tr("Add"), false, !labels.is_empty()) {
            if let Some((name, _)) = pickable.get(p.s.app_pick) {
                let name = name.clone();
                settings::update(|x| {
                    if !x.hide_for_apps.contains(&name) {
                        x.hide_for_apps.push(name);
                    }
                });
                p.s.app_pick = 0;
            }
        }
    }
    if n > 0 {
        let modes = ["Fade".to_string(), "Hidden".into()];
        p.pick(rr[n + 1], "set.hidemode", &[tr("Fade out"), tr("Hide completely")], &modes, &s.hide_for_apps_mode, 0, |x, v| x.hide_for_apps_mode = v);
        p.pick(rr[n + 2], "set.hidehold", &[tr("Wait until I switch away"), tr("Show them anyway")], &[true, false], &s.hide_for_apps_hold_alerts, 0, |x, v| {
            x.hide_for_apps_hold_alerts = v
        });
    }

    let rr = p.card(
        Some(tr("SYSTEM")),
        &[
            R::t(tr("Open at login"), tr("Start FathNotch when you sign in to Windows.")),
            R::t(tr("Low memory mode"), tr("While the notch is closed, hand unused memory back to Windows. The panel may take a moment longer to open the first time after a while.")),
        ],
    );
    if let Some(r) = rr[0] {
        if switch(p.ui, r, s.launch_at_startup) {
            let on = !s.launch_at_startup;
            bg::spawn(move || {
                settings::set_start_with_windows(on);
                settings::update(|_| {});
            });
        }
    }
    p.tog(rr[1], s.low_memory_mode, |x, v| x.low_memory_mode = v);
}

// ---------------------------------------------------------------- Appearance

pub fn appearance(p: &mut Pg) {
    let s = p.st.clone();
    p.title(tr("Appearance"), tr("Material, color scheme and accent. Changes apply instantly."));

    if p.free() {
        p.caption(tr("MATERIAL"));
        let y = p.y;
        for (i, (name, sub, glass)) in [(tr("Solid"), tr("Opaque, easiest to read"), false), (tr("Translucent"), tr("Your wallpaper shows through"), true)].into_iter().enumerate() {
            let r = Rect::new(p.x + i as f32 * 167.0, y, 157.0, 108.0);
            if material_card(p, r, name, sub, glass, s.glass == glass, &s.theme_scheme) && s.glass != glass {
                settings::update(|x| x.glass = glass);
            }
        }
        p.y = y + 112.0;

        p.caption(tr("COLOR SCHEME"));
        let (cw, ch, gap) = (125.0, 85.0, 8.0);
        let per = (((p.w + gap) / (cw + gap)).floor() as usize).max(1);
        let auto = s.theme_accent.is_empty() || s.theme_accent == "auto";
        let current = theme::scheme(&s.theme_scheme).id;
        for (i, sc) in SCHEMES.iter().enumerate() {
            let r = Rect::new(p.x + (i % per) as f32 * (cw + gap), p.y + (i / per) as f32 * (ch + gap), cw, ch);
            if !p.on_screen(r.y, r.h) {
                continue;
            }
            let acc = theme::accent(if auto { sc.accent } else { &s.theme_accent });
            if scheme_card(p, r, sc, acc, sc.id == current) {
                let id = sc.id.to_string();
                settings::update(|x| x.theme_scheme = id);
            }
        }
        p.y += SCHEMES.len().div_ceil(per) as f32 * (ch + gap);

        p.caption(tr("ACCENT"));
        p.y -= 4.0;
        let sub = p.ui.t.subtext;
        let hint = tr("Every scheme comes with its own accent (Auto). Pick another to override it.");
        p.ui.text(hint, Rect::new(p.x + 4.0, p.y, p.w - 4.0, lh(11.0)), 11.0, REGULAR, sub, Align::Left);
        p.y += lh(11.0) + 8.0;
        let own = theme::accent(theme::scheme(&s.theme_scheme).accent);
        let mut x = p.x;
        let mut y = p.y;
        let mut hit = None;
        for i in 0..=ACCENTS.len() {
            if x + 28.0 > p.x + p.w {
                x = p.x;
                y += 32.0;
            }
            let r = Rect::new(x, y, 28.0, 28.0);
            let (rgb, selected) = if i == 0 { (own, auto) } else { (ACCENTS[i - 1].2, !auto && ACCENTS[i - 1].0 == s.theme_accent) };
            let c = color(rgb, 1.0);
            if selected {
                p.ui.g.stroke_circle(r.cx(), r.cy(), 13.0, p.ui.c(c), 2.0);
            }
            p.ui.g.fill_circle(r.cx(), r.cy(), 10.0, p.ui.c(c));
            if i == 0 {
                let light = 0.299 * c.r + 0.587 * c.g + 0.114 * c.b > 0.55;
                p.ui.icon("Sparkles", r.cx(), r.cy(), 11.0, if light { color(0x111113, 1.0) } else { color(0xFFFFFF, 1.0) });
            }
            if p.ui.clicked(r) {
                hit = Some(i);
            }
            x += 30.0;
        }
        p.y = y + 32.0;
        if let Some(i) = hit {
            let id = if i == 0 { "auto".to_string() } else { ACCENTS[i - 1].0.to_string() };
            settings::update(|x| x.theme_accent = id);
        }
    }

    let rr = p.card(
        Some(tr("TRANSLUCENCY")),
        &[
            R::t(
                tr("Translucent"),
                tr("See-through notch and panels tinted by the color scheme, with a glass sheen. (Windows can't blur behind this kind of window, so it's a clear tint rather than frosted.)"),
            ),
            R::new(tr("Tint"), tr("Lower shows more of the wallpaper; higher keeps text easier to read."), 170.0, 22.0),
        ],
    );
    p.tog(rr[0], s.glass, |x, v| x.glass = v);
    if let Some(r) = rr[1] {
        if s.glass {
            if let Some(v) = p.ui.slider(id("settings.glass"), r.inset(8.0, 0.0), s.glass_strength as f32) {
                // Live: the shell re-reads the theme every frame. Save quietly while dragging.
                settings::update_quiet(|x| x.glass_strength = (v as f64 * 100.0).round() / 100.0);
            }
        } else {
            let t = p.ui.t;
            let r = r.inset(8.0, 0.0);
            p.ui.g.line(r.x, r.cy(), r.right(), r.cy(), p.ui.c(with_alpha(t.track, 0.35)), 4.0);
            let kx = r.x + r.w * s.glass_strength as f32;
            p.ui.g.fill_circle(kx, r.cy(), 7.5, p.ui.c(with_alpha(t.text, 0.35)));
        }
    }
}

/// Solid / Translucent: the notch as a pill over a bit of "wallpaper", with the name below (157 wide).
fn material_card(p: &mut Pg, r: Rect, name: &str, sub: &str, glass: bool, selected: bool, scheme_id: &str) -> bool {
    let t = p.ui.t;
    let sc = theme::scheme(scheme_id);
    let frame = Rect::new(r.x, r.y, 157.0, 71.0);
    let border = if selected { t.accent } else { t.stroke };
    p.ui.g.stroke_round(frame.inset(1.0, 1.0).d2d(), 16.0, p.ui.c(border), 2.0);
    let wall = frame.inset(3.5, 3.5);
    wallpaper(p, wall, 14.0);
    let pill = Rect::new(wall.cx() - 52.0, wall.cy() - 15.0, 104.0, 30.0);
    p.ui.fill(pill, 15.0, color(sc.pill, if glass { 0x88 as f32 / 255.0 } else { 1.0 }));
    p.ui.g.stroke_round(pill.inset(0.5, 0.5).d2d(), 14.5, p.ui.c(color(0xFFFFFF, if glass { 0x60 as f32 / 255.0 } else { 0x20 as f32 / 255.0 })), 1.0);
    let n = crate::notch::now();
    p.ui.text(&format!("{:02}:{:02}", n.hour, n.minute), pill, 12.0, SEMIBOLD, color(sc.text, 1.0), Align::Center);
    p.ui.text(name, Rect::new(r.x + 4.0, frame.bottom() + 6.0, r.w, lh(12.0)), 12.0, SEMIBOLD, t.text, Align::Left);
    p.ui.text(sub, Rect::new(r.x + 4.0, frame.bottom() + 6.0 + lh(12.0), r.w, lh(11.0)), 11.0, REGULAR, t.subtext, Align::Left);
    p.ui.clicked(Rect::new(r.x, r.y, 157.0, 71.0 + 6.0 + lh(12.0) + lh(11.0)))
}

/// A blue → pink gradient (30°) in a rounded rectangle: strips, clipped by rounded end caps.
fn wallpaper(p: &mut Pg, r: Rect, radius: f32) {
    let a = color(0x3B82F6, 1.0);
    let b = color(0xEC4899, 1.0);
    p.ui.fill(Rect::new(r.x, r.y, radius * 2.0 + 2.0, r.h), radius, a);
    p.ui.fill(Rect::new(r.right() - radius * 2.0 - 2.0, r.y, radius * 2.0 + 2.0, r.h), radius, b);
    let x0 = r.x + radius;
    let x1 = r.right() - radius;
    let n = 24;
    let sw = (x1 - x0) / n as f32;
    for i in 0..n {
        let c = mix(a, b, (i as f32 + 0.5) / n as f32);
        p.ui.g.fill_rect(Rect::new(x0 + i as f32 * sw, r.y, sw + 0.6, r.h).d2d(), p.ui.c(c));
    }
}

/// A scheme as a tiny open panel (118×78 inside a 2 px ring): its notch as a capsule, two widget cards and the
/// name, in its own colours.
fn scheme_card(p: &mut Pg, r: Rect, sc: &theme::Scheme, acc: u32, selected: bool) -> bool {
    let t = p.ui.t;
    let accent = color(acc, 1.0);
    let hov = p.ui.hovered(r);
    let border = if selected { accent } else if hov { t.subtext } else { t.stroke };
    p.ui.g.stroke_round(r.inset(1.0, 1.0).d2d(), 18.0, p.ui.c(border), 2.0);
    let inner = r.inset(3.5, 3.5);
    p.ui.fill(inner, 16.0, color(sc.surface, 1.0));
    let text = color(sc.text, 1.0);
    let cap = Rect::new(inner.cx() - 20.0, inner.y + 6.0, 40.0, 9.0);
    p.ui.fill(cap, 4.5, color(sc.pill, 1.0));
    p.ui.g.stroke_round(cap.inset(0.4, 0.4).d2d(), 4.1, p.ui.c(with_alpha(text, 0x38 as f32 / 255.0)), 0.8);
    // Widgets: margin 9,21,9,24; columns 1.4* | 5 | 1*.
    let wy = inner.y + 21.0;
    let wh = inner.h - 21.0 - 24.0;
    let ww = inner.w - 18.0 - 5.0;
    let left = Rect::new(inner.x + 9.0, wy, ww * 1.4 / 2.4, wh);
    let right = Rect::new(left.right() + 5.0, wy, ww / 2.4, wh);
    p.ui.fill(left, 6.0, color(sc.surface2, 1.0));
    p.ui.fill(right, 6.0, color(sc.surface2, 1.0));
    p.ui.fill(Rect::new(left.x + 5.0, left.y + 5.0, 22.0, 3.0), 1.5, text);
    p.ui.fill(Rect::new(left.x + 5.0, left.y + 12.0, 30.0, 3.0), 1.5, accent);
    p.ui.g.fill_circle(right.cx(), right.cy(), 4.5, p.ui.c(accent));
    let name_c = if selected { text } else { color(sc.subtext, 1.0) };
    p.ui.text(sc.name, Rect::new(inner.x + 10.0, inner.bottom() - 7.0 - lh(11.0), inner.w - 30.0, lh(11.0)), 11.0, SEMIBOLD, name_c, Align::Left);
    if selected {
        let (cx, cy) = (inner.right() - 7.0 - 7.5, inner.bottom() - 6.0 - 7.5);
        p.ui.g.fill_circle(cx, cy, 7.5, p.ui.c(accent));
        let light = 0.299 * accent.r + 0.587 * accent.g + 0.114 * accent.b > 0.55;
        p.ui.icon("Check", cx, cy, 9.0, if light { color(0x111113, 1.0) } else { color(0xFFFFFF, 1.0) });
    }
    p.ui.clicked(r)
}

// ---------------------------------------------------------------- Display

const IDLE_ITEMS: [(&str, &str, &str); 6] = [
    ("clock", "Clock", "Current time."),
    ("date", "Date", "Weekday and date."),
    ("prayer", "Next prayer time", "The next prayer and its time (Settings → Prayer)."),
    ("weather", "Weather", "Temperature for your Weather city. Updates every 10 minutes."),
    ("todos", "Open to-dos", "How many to-dos are still open."),
    ("battery", "Battery", "Charge level; green while charging. Hidden on PCs without a battery."),
];

pub fn display(p: &mut Pg) {
    let s = p.st.clone();
    p.title(tr("Display"), tr("Where the notch appears, and how big it is."));

    let screens = p.s.facts.monitors.clone();
    let mut names = vec![tr("Primary display").to_string(), tr("Follow the pointer").to_string()];
    for (i, (w, h, primary)) in screens.iter().enumerate() {
        names.push(crate::i18n::tf!("Display {} · {w}×{h}{}", "Layar {} · {w}×{h}{}", i + 1, if *primary { tr(" (primary)") } else { "" }));
    }
    let labels: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut values = vec![-1, -2];
    values.extend(0..screens.len() as i32);
    let move_w = p.button_w("Move…");
    let rr = p.card(
        Some(tr("LOCATION")),
        &[
            R::c(tr("Show notch on"), tr("Which monitor hosts the notch. Follow the pointer moves it to whichever display you're using."), 210.0),
            R::c("Position", "Along the top edge (centre or a corner), or anywhere you drag it. The panel opens next to it.", 190.0),
            R::c("Move the notch", "Closes the panel: drag the notch where you want it and let go. Right-click to cancel.", move_w),
        ],
    );
    p.pick(rr[0], "set.monitor", &labels, &values, &s.monitor_index, 0, |x, v| x.monitor_index = v);
    let places = ["center".to_string(), "left".into(), "right".into(), "free".into()];
    p.pick(rr[1], "set.position", &["Top centre", "Top left", "Top right", "Free (drag it)"], &places, &s.notch_position, 0, |x, v| x.notch_position = v);
    if p.button(rr[2], "Move…", false) {
        crate::notch::request_move();
    }

    let rr = p.card(
        Some(tr("STYLE")),
        &[
            R::c(tr("Shape"), tr("A notch hanging from the top edge, or a floating capsule just below it."), 140.0),
            R::t(tr("Border"), tr("A hairline outline around the notch. Off: pure black edges that blend into the screen.")),
            R::c(tr("Tab bar"), tr("A compact row of tool icons along the bottom of the open panel (easiest to navigate), or page dots."), 140.0),
            R::t(tr("Clock in the panel"), tr("Today's date and the time in the middle of the open panel's header.")),
            R::t(tr("Lyric glow"), tr("The closed notch is plain black; while a synced lyric line shows, a soft glow in the song's colour rises from its bottom edge.")),
            R::c(tr("Animations"), tr("How fast tabs slide in and their cards appear one by one. Off shows everything at once."), 140.0),
        ],
    );
    let styles = ["Notch".to_string(), "Capsule".into()];
    p.pick(rr[0], "set.style", &[tr("Notch"), tr("Capsule")], &styles, &s.notch_style, 0, |x, v| x.notch_style = v);
    p.tog(rr[1], s.notch_border, |x, v| x.notch_border = v);
    let tabs = ["Icons".to_string(), "Dots".into()];
    p.pick(rr[2], "set.tabstyle", &[tr("Icons"), tr("Dots")], &tabs, &s.tab_style, 0, |x, v| x.tab_style = v);
    p.tog(rr[3], s.header_clock, |x, v| x.header_clock = v);
    p.tog(rr[4], s.rest_glow, |x, v| x.rest_glow = v);
    let speeds = ["Off".to_string(), "Fast".into(), "Normal".into(), "Slow".into()];
    p.pick(rr[5], "set.anim", &[tr("Off"), tr("Fast"), tr("Normal"), tr("Slow")], &speeds, &s.anim_speed, 2, |x, v| x.anim_speed = v);

    let rr = p.card(
        Some(tr("SIZE")),
        &[
            R::c(tr("Notch size"), tr("Width and height of the closed notch. Tiny and Extra Small keep it narrow; larger is easier to hit on big monitors."), 140.0),
            R::c(tr("Panel size"), tr("How big the open panel is drawn: header, tools and tab bar together."), 160.0),
        ],
    );
    let sizes: Vec<String> = ["Tiny", "ExtraSmall", "Small", "Default", "Large", "ExtraLarge"].iter().map(|x| x.to_string()).collect();
    p.pick(rr[0], "set.size", &[tr("Tiny"), tr("Extra Small"), tr("Small"), tr("Default"), tr("Large"), tr("Extra Large")], &sizes, &s.notch_size, 0, |x, v| x.notch_size = v);
    if let Some(r) = rr[1] {
        let scales = [0.8, 0.85, 0.92, 1.0];
        let sel = scales.iter().position(|v| (v - s.panel_scale).abs() < 0.01).unwrap_or(0);
        if let Some(i) = p.combo("set.scale", r, &[tr("Compact (80%)"), tr("Small (85%)"), tr("Medium (92%)"), tr("Full (100%)")], sel) {
            settings::update(|x| x.panel_scale = scales[i]);
        }
    }

    let rr = p.card(
        Some(tr("IDLE NOTCH")),
        &[
            R::c(tr("Layout"), tr("How several items share the closed notch when nothing is playing."), 180.0),
            R::c(tr("Rotate every"), tr("How long each item stays before the next one slides in."), 140.0),
            R::t(tr("Peeking eyes"), tr("Now and then a pair of cat eyes peeks out of the resting notch, blinks and looks around for a few seconds.")),
            R::c(tr("Eyes every"), tr("How often the eyes come out while nothing is happening."), 140.0),
            R::t(tr("Eyes while hidden"), tr("When the notch is hidden (Show notch, hidden apps), the eyes still peek out for a few seconds, then it hides again.")),
        ],
    );
    let layouts = ["Rotate".to_string(), "Split".into(), "Row".into()];
    p.pick(rr[0], "set.idlelayout", &[tr("Rotate one at a time"), tr("Left and right"), tr("All in a row")], &layouts, &s.idle_layout, 0, |x, v| x.idle_layout = v);
    p.pick(rr[1], "set.idlesec", &[tr("4 seconds"), tr("8 seconds"), tr("15 seconds"), tr("30 seconds")], &[4, 8, 15, 30], &s.idle_rotate_sec, 0, |x, v| x.idle_rotate_sec = v);
    p.tog(rr[2], s.idle_eyes, |x, v| x.idle_eyes = v);
    p.pick(rr[3], "set.eyes", &[tr("20 seconds"), tr("45 seconds"), tr("1.5 minutes"), tr("3 minutes")], &[20, 45, 90, 180], &s.idle_eyes_every_sec, 0, |x, v| {
        x.idle_eyes_every_sec = v
    });
    p.tog(rr[4], s.eyes_when_hidden, |x, v| x.eyes_when_hidden = v);

    eyes(p, &s);

    let rows: Vec<R> = IDLE_ITEMS.iter().map(|(_, name, desc)| R::t(*name, *desc)).collect();
    let rr = p.card(Some(tr("SHOW WHEN IDLE")), &rows);
    for (i, (id, _, _)) in IDLE_ITEMS.iter().enumerate() {
        let on = s.idle_items.iter().any(|x| x == id);
        if let Some(r) = rr[i] {
            if switch(p.ui, r, on) {
                // Keep the catalogue order (the notch shows them in this order); keep ids this build doesn't list.
                settings::update(|x| {
                    let mut set: Vec<String> = x.idle_items.clone();
                    if on {
                        set.retain(|v| v != id);
                    } else {
                        set.push(id.to_string());
                    }
                    let known: Vec<String> = IDLE_ITEMS.iter().map(|c| c.0.to_string()).filter(|c| set.contains(c)).collect();
                    let other: Vec<String> = set.into_iter().filter(|v| !IDLE_ITEMS.iter().any(|c| c.0 == v)).collect();
                    x.idle_items = known.into_iter().chain(other).collect();
                });
            }
        }
    }
}

/// (width, height, primary) of each display, in Windows' order. Called when Settings opens, not per frame.
pub(super) fn monitors() -> Vec<(i32, i32, bool)> {
    use windows::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
    unsafe extern "system" fn collect(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        (*(data.0 as *mut Vec<HMONITOR>)).push(m);
        TRUE
    }
    let mut list: Vec<HMONITOR> = Vec::new();
    let mut out = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(HDC::default(), None, Some(collect), LPARAM(&mut list as *mut _ as isize));
        for m in list {
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if GetMonitorInfoW(m, &mut info).as_bool() {
                let r = info.rcMonitor;
                out.push((r.right - r.left, r.bottom - r.top, info.dwFlags & 1 != 0));
            }
        }
    }
    out
}

/// Apps with a visible, titled window right now: (process name, its description), sorted by description.
/// Slow-ish (opens processes, reads version info): background thread only.
pub(super) fn running_apps() -> Vec<(String, String)> {
    use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, TRUE};
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindow, GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible, GW_OWNER};
    unsafe extern "system" fn collect(h: HWND, data: LPARAM) -> BOOL {
        if IsWindowVisible(h).as_bool() && GetWindowTextLengthW(h) > 0 && GetWindow(h, GW_OWNER).map_or(true, |o| o.0.is_null()) {
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            (*(data.0 as *mut Vec<u32>)).push(pid);
        }
        TRUE
    }
    let mut pids: Vec<u32> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut pids as *mut _ as isize));
    }
    pids.sort_unstable();
    pids.dedup();
    let me = std::process::id();
    let mut out: Vec<(String, String)> = Vec::new();
    for pid in pids {
        if pid == me {
            continue;
        }
        let path = unsafe {
            let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { continue };
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
            let _ = CloseHandle(h);
            if !ok {
                continue;
            }
            String::from_utf16_lossy(&buf[..len as usize])
        };
        let p = std::path::Path::new(&path);
        let name = p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        if name.is_empty() || name == "applicationframehost" || out.iter().any(|(n, _)| *n == name) {
            continue;
        }
        let label = file_description(&path).unwrap_or_else(|| name.clone());
        out.push((name, label));
    }
    out.sort_by_key(|(_, l)| l.to_lowercase());
    out
}

/// The exe's FileDescription (e.g. "Microsoft PowerPoint").
fn file_description(path: &str) -> Option<String> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
    unsafe {
        let p = HSTRING::from(path);
        let size = GetFileVersionInfoSizeW(&p, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(&p, 0, size, data.as_mut_ptr() as _).ok()?;
        // First translation's language/code page.
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        let q = HSTRING::from("\\VarFileInfo\\Translation");
        let (lang, cp) = if VerQueryValueW(data.as_ptr() as _, PCWSTR(q.as_ptr()), &mut ptr, &mut len).as_bool() && len >= 4 {
            let w = std::slice::from_raw_parts(ptr as *const u16, 2);
            (w[0], w[1])
        } else {
            (0x0409, 0x04B0)
        };
        let q = HSTRING::from(format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\FileDescription"));
        if VerQueryValueW(data.as_ptr() as _, PCWSTR(q.as_ptr()), &mut ptr, &mut len).as_bool() && len > 1 {
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr as *const u16, len as usize - 1));
            let s = s.trim().to_string();
            return (!s.is_empty()).then_some(s);
        }
        None
    }
}

// ---------------------------------------------------------------- Tabs

pub fn tabs(p: &mut Pg) {
    if !p.free() {
        return; // nothing here is a searchable row (as in the .NET build)
    }
    let s = p.st.clone();
    if p.s.facts.first_run {
        p.title(
            tr("Welcome! Pick your tools"),
            "A few tools start switched off: turn on any you want, and move the rest into order with the arrows. \
             Notifications shown on the notch are kept on this PC for 30 days (Settings → Alerts to turn that off).",
        );
    } else {
        p.title(tr("Tabs"), tr("The tools in the notch's bottom bar, and their order."));
    }
    let visible = tools::ordered(&s.tool_order, &s.hidden_tools);
    p.caption(tr("IN THE NOTCH"));
    tool_list(p, &visible, true);
    p.hint(tr("Use the arrows to move a tab. Settings always stays at the end of the bar."), false);

    let all = tools::ordered(&s.tool_order, &[]);
    let hidden: Vec<&tools::Tool> = all.into_iter().filter(|t| s.hidden_tools.iter().any(|h| h == t.id)).collect();
    if !hidden.is_empty() {
        p.caption(tr("MORE TOOLS"));
        tool_list(p, &hidden, false);
    }
    p.y += 10.0;
    let bw = p.button_w(tr("Restore defaults"));
    let r = Rect::new(p.x + p.w - bw, p.y, bw, CTRL_H);
    if button(p.ui, r, tr("Restore defaults"), false, true) {
        settings::update(|x| {
            x.tool_order.clear();
            x.hidden_tools.clear();
        });
    }
    p.y += CTRL_H;
}

fn tool_list(p: &mut Pg, list: &[&tools::Tool], shown: bool) {
    let t = p.ui.t;
    let row_h = 40.0;
    let card = Rect::new(p.x, p.y, p.w, row_h * list.len() as f32 + 4.0);
    p.ui.fill(card, 16.0, t.surface);
    let mut action: Option<(usize, i32)> = None; // (index, -1 up / +1 down / 0 toggle)
    for (i, tool) in list.iter().enumerate() {
        let r = Rect::new(card.x + 16.0, card.y + 2.0 + i as f32 * row_h, card.w - 32.0, row_h);
        if !p.on_screen(r.y, r.h) {
            continue;
        }
        let tile = Rect::new(r.x, r.cy() - 12.0, 24.0, 24.0);
        p.ui.fill(tile, 6.0, t.hover);
        p.ui.icon(tool.icon, tile.cx(), tile.cy(), 13.0, t.text);
        let name = if tool.title.is_empty() { tr("Home") } else { tr(tool.title) };
        p.ui.text(name, Rect::new(tile.right() + 11.0, r.y, 260.0, r.h), 13.0, MEDIUM, t.text, Align::Left);
        let tog = Rect::new(r.right() - 38.0, r.cy() - 11.0, 38.0, 22.0);
        if switch(p.ui, tog, shown) {
            action = Some((i, 0));
        }
        if shown {
            let down = Rect::new(tog.x - 8.0 - 22.0, r.cy() - 11.0, 22.0, 22.0);
            let up = Rect::new(down.x - 22.0, r.cy() - 11.0, 22.0, 22.0);
            if small_icon(p.ui, up, "ChevronUp", i > 0) {
                action = Some((i, -1));
            }
            if small_icon(p.ui, down, "ChevronDown", i + 1 < list.len()) {
                action = Some((i, 1));
            }
        }
    }
    p.y = card.bottom();
    let Some((i, what)) = action else { return };
    let id = list[i].id.to_string();
    if what == 0 {
        settings::update(|x| {
            if shown {
                if tools::ordered(&x.tool_order, &x.hidden_tools).len() > 1 && !x.hidden_tools.contains(&id) {
                    x.hidden_tools.push(id);
                }
            } else {
                x.hidden_tools.retain(|h| *h != id);
            }
        });
    } else {
        let other = list[(i as i32 + what) as usize].id.to_string();
        settings::update(|x| {
            // Swap within the full order (hidden tools included) so hidden ones keep their places.
            let mut ids: Vec<String> = tools::ordered(&x.tool_order, &[]).iter().map(|t| t.id.to_string()).collect();
            let a = ids.iter().position(|v| *v == id);
            let b = ids.iter().position(|v| *v == other);
            if let (Some(a), Some(b)) = (a, b) {
                ids.swap(a, b);
                x.tool_order = ids;
            }
        });
    }
}

// ---------------------------------------------------------------- Alerts

pub fn alerts(p: &mut Pg) {
    let s = p.st.clone();
    p.title(tr("Alerts"), tr("What pops out of the closed notch, and from which apps."));
    let bw = p.button_w(tr("Open settings"));
    let problem = crate::services::notifications::problem();
    let mut rows = vec![R::t(
        tr("Show on the notch"),
        tr("Windows notifications from other apps pop out of the closed notch. Uses Windows notification access."),
    )];
    if let Some(pr) = &problem {
        rows.push(R::c(tr("Not receiving notifications"), pr.clone(), bw));
    }
    rows.push(R::t(tr("Show message text"), tr("Off shows only the app name, for privacy when sharing your screen.")));
    rows.push(R::t(tr("Keep history"), tr("Save notifications on this PC to read later in the Notifications tool (last 30 days). Clear them there anytime.")));
    rows.push(R::c(tr("Windows banners"), tr("Windows still shows its own pop-up too. Turn banners off per app in Windows Settings if you only want the notch."), bw));
    let rr = p.card(Some(tr("WINDOWS NOTIFICATIONS")), &rows);
    let k = if problem.is_some() { 1 } else { 0 };
    p.tog(rr[0], s.notify_enabled, |x, v| x.notify_enabled = v);
    if k == 1 && p.button(rr[1], tr("Open settings"), false) {
        crate::shell::open_settings_page("ms-settings:privacy-notifications");
    }
    p.tog(rr[1 + k], s.notify_show_text, |x, v| x.notify_show_text = v);
    p.tog(rr[2 + k], s.notify_keep_history, |x, v| x.notify_keep_history = v);
    if p.button(rr[3 + k], tr("Open settings"), false) {
        crate::shell::open_settings_page("ms-settings:notifications");
    }

    let rr = p.card(
        Some(tr("SYSTEM ALERTS")),
        &[
            R::t(tr("Battery alerts"), tr("Show on the notch when the charger is plugged in or pulled, and when the battery drops to 20% and 10%.")),
            R::t(tr("Volume on the notch"), tr("Show the volume level on the notch when it changes. Windows still shows its own volume pop-up too.")),
            R::t(tr("Bluetooth devices"), tr("Show on the notch when paired headphones, a mouse or other Bluetooth device connects or disconnects.")),
            R::t(tr("Finished downloads"), tr("Show on the notch when a browser download finishes in your Downloads folder. Click to show the file.")),
            R::t(tr("Caps Lock & Num Lock"), tr("Show on the notch when Caps Lock or Num Lock is turned on or off.")),
            R::t(tr("Camera & microphone"), tr("Show which app starts using the camera or microphone, and keep a green (camera) or orange (mic) dot on the notch while it does.")),
            R::t(tr("Brightness on the notch"), tr("Show the screen brightness on the notch when it changes (laptop screens). Windows still shows its own pop-up too.")),
            R::t(tr("Network"), tr("Show on the notch when the PC connects to a network or goes offline.")),
            R::t(tr("Keyboard language"), tr("Show the new language on the notch when you switch keyboard layouts (Win+Space).")),
        ],
    );
    p.tog(rr[0], s.power_alerts, |x, v| x.power_alerts = v);
    p.tog(rr[1], s.volume_hud, |x, v| x.volume_hud = v);
    p.tog(rr[2], s.bluetooth_alerts, |x, v| x.bluetooth_alerts = v);
    p.tog(rr[3], s.download_alerts, |x, v| x.download_alerts = v);
    p.tog(rr[4], s.lock_key_alerts, |x, v| x.lock_key_alerts = v);
    p.tog(rr[5], s.privacy_indicator, |x, v| x.privacy_indicator = v);
    p.tog(rr[6], s.brightness_hud, |x, v| x.brightness_hud = v);
    p.tog(rr[7], s.network_alerts, |x, v| x.network_alerts = v);
    p.tog(rr[8], s.keyboard_layout_alerts, |x, v| x.keyboard_layout_alerts = v);

    if !s.notify_seen_apps.is_empty() {
        let apps: Vec<(String, String)> = s
            .notify_seen_apps
            .iter()
            .map(|e| {
                let mut it = e.splitn(2, '|');
                let id = it.next().unwrap_or("").to_string();
                let name = it.next().map(str::to_string).unwrap_or_else(|| id.clone());
                (id, name)
            })
            .collect();
        let rows: Vec<R> = apps.iter().map(|(_, n)| R::t(n.clone(), tr("Recently sent a notification."))).collect();
        let rr = p.card(Some(tr("SHOW NOTIFICATIONS FROM")), &rows);
        for (i, (id, _)) in apps.iter().enumerate() {
            let on = !s.notify_muted_apps.iter().any(|m| m.eq_ignore_ascii_case(id));
            let id = id.clone();
            p.tog(rr[i], on, move |x, v| {
                x.notify_muted_apps.retain(|m| !m.eq_ignore_ascii_case(&id));
                if !v {
                    x.notify_muted_apps.push(id);
                }
            });
        }
    }
}

// ---------------------------------------------------------------- Keyboard

const HOTKEYS: [(&str, &str, &str); 3] = [
    ("toggle", "Open or close the notch", "Works from anywhere."),
    ("clipboard", "Clipboard history", "Opens the notch on clipboard history."),
    ("teleprompter", "Teleprompter", "Opens the teleprompter and starts or pauses scrolling."),
];

pub fn keyboard(p: &mut Pg) {
    let s = p.st.clone();
    p.title("Keyboard", tr("Global shortcuts work from any app. Click a shortcut, then press the new keys."));

    // Recording: read the keys straight from the keyboard (the global hotkeys don't reach the window).
    if let Some(action) = p.s.recording {
        p.ui.animate = true;
        match poll_gesture() {
            Some(Some(g)) => {
                settings::update(|x| {
                    x.hotkeys.insert(action.to_string(), g);
                });
                p.s.recording = None;
            }
            Some(None) => p.s.recording = None,
            None => {}
        }
    }

    // Chip: at least 110 wide, as wide as the gesture needs; then the clear button (22).
    let chips: Vec<String> = HOTKEYS
        .iter()
        .map(|(id, _, _)| {
            if p.s.recording == Some(*id) {
                tr("Press keys…").to_string()
            } else {
                let g = s.hotkeys.get(*id).cloned().unwrap_or_default();
                if g.is_empty() {
                    tr("Not set").to_string()
                } else {
                    g.replace('+', " + ")
                }
            }
        })
        .collect();
    let widths: Vec<f32> = chips.iter().map(|c| button_w(p.ui, c, false).max(110.0) + 22.0).collect();
    let rows: Vec<R> = HOTKEYS.iter().zip(&widths).map(|((_, n, d), w)| R::c(*n, *d, *w)).collect();
    let rr = p.card(Some(tr("SHORTCUTS")), &rows);
    for (i, (id, _, _)) in HOTKEYS.iter().enumerate() {
        let Some(r) = rr[i] else { continue };
        let (x, chip) = r.take_right(22.0, 0.0);
        let recording = p.s.recording == Some(*id);
        if recording {
            let t = p.ui.t;
            p.ui.fill(chip, 8.0, t.accent_soft);
            p.ui.text(&chips[i], chip, 13.0, MEDIUM, t.text, Align::Center);
            if p.ui.clicked(chip) {
                p.s.recording = None;
            }
        } else if button(p.ui, chip, &chips[i], false, true) {
            p.s.recording = Some(*id);
        }
        let set = s.hotkeys.get(*id).is_some_and(|g| !g.is_empty());
        let xr = Rect::new(x.x, x.cy() - 11.0, 22.0, 22.0);
        if small_icon(p.ui, xr, "XMark", set) {
            settings::update(|x| {
                x.hotkeys.insert(id.to_string(), String::new());
            });
        }
    }
    let k1 = "1 – 9  ·  ← →";
    let bh = lh(11.0) + 6.0;
    let rr = p.card(
        Some(tr("INSIDE THE NOTCH")),
        &[
            R::new(tr("Switch tabs"), tr("1–9 jump to a tab; ← and → step through tabs."), p.badge_w(k1), bh),
            R::new(tr("Close"), tr("Esc closes the notch (or goes back from a sub-page)."), p.badge_w(tr("Esc")), bh),
            R::new("Teleprompter", tr("Space plays or pauses the script."), p.badge_w(tr("Space")), bh),
        ],
    );
    p.badge(rr[0], k1);
    p.badge(rr[1], tr("Esc"));
    p.badge(rr[2], tr("Space"));
}

/// While recording a shortcut: Some(Some("Ctrl+Alt+N")) once a key is pressed with a modifier (F-keys alone are
/// fine), Some(None) on Esc, None while waiting. Only keys the hotkey parser understands.
fn poll_gesture() -> Option<Option<String>> {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    let down = |vk: i32| unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 };
    if down(0x1B) {
        return Some(None);
    }
    let ctrl = down(0x11);
    let alt = down(0x12);
    let shift = down(0x10);
    let win = down(0x5B) || down(0x5C);
    let mut key = None;
    for vk in (0x30..=0x39).chain(0x41..=0x5A) {
        if down(vk) {
            key = Some(((vk as u8) as char).to_string());
        }
    }
    for n in 1..=24 {
        if down(0x6F + n) {
            key = Some(format!("F{n}"));
        }
    }
    if down(0x20) {
        key = Some("Space".into());
    }
    let key = key?;
    let fkey = key.starts_with('F') && key.len() > 1;
    if !(ctrl || alt || shift || win) && !fkey {
        return None;
    }
    let mut parts = Vec::new();
    if ctrl {
        parts.push("Ctrl".to_string());
    }
    if alt {
        parts.push("Alt".into());
    }
    if shift {
        parts.push("Shift".into());
    }
    if win {
        parts.push("Win".into());
    }
    parts.push(key);
    Some(Some(parts.join("+")))
}

// ---------------------------------------------------------------- Music

pub fn music(p: &mut Pg) {
    let s = p.st.clone();
    p.title(tr("Music"), tr("Lyrics and what the notch shows while a song plays."));
    let rr = p.card(
        Some(tr("NOW PLAYING")),
        &[
            R::t(tr("Lyrics"), tr("Show time-synced lyrics on Now Playing. Songs are looked up on LRCLIB, a free lyrics library.")),
            R::t(tr("Lyrics on the notch"), tr("While music plays, the closed notch shows the line being sung, in time with the song, instead of the title.")),
            R::t(tr("Announce new songs"), tr("Pop the new song's art and title out of the notch for a moment when the track changes.")),
        ],
    );
    p.tog(rr[0], s.lyrics_enabled, |x, v| x.lyrics_enabled = v);
    p.tog(rr[1], s.lyrics_in_notch, |x, v| x.lyrics_in_notch = v);
    p.tog(rr[2], s.track_peek, |x, v| x.track_peek = v);
}

// ---------------------------------------------------------------- Calendar

/// Subscribed links and their labels (reads and decrypts a file: background thread only).
pub(super) fn load_feeds() -> Vec<(String, String)> {
    calendar::feeds().into_iter().map(|f| (calendar::label(&f), f)).collect()
}

fn reload_feeds(p: &mut Pg, sync: bool) {
    let sh = p.shared();
    if sync {
        sh.lock().unwrap().feed_busy = true;
    }
    bg::spawn(move || {
        let feeds = load_feeds();
        sh.lock().unwrap().feeds = Some(feeds);
        if sync {
            let status = calendar::refresh_feeds();
            let mut g = sh.lock().unwrap();
            g.feed_status = status;
            g.feed_busy = false;
        }
    });
}

const ICS_HELP: &str = "Google: calendar.google.com → ⚙ Settings → under \"Settings for my calendars\" pick the calendar → \
Integrate calendar → copy \"Secret address in iCal format\". Each calendar has its own link.\n\
Outlook: Settings → Calendar → Shared calendars → Publish → ICS link.\n\
webcal:// links work too. Links are stored encrypted.";

pub fn calendar(p: &mut Pg) {
    let s = p.st.clone();
    p.title(tr("Calendar"), tr("Event reminders on the notch and calendars you subscribe to."));
    let rr = p.card(
        Some(tr("REMINDERS")),
        &[
            R::t(tr("Event reminders"), tr("Show upcoming calendar events on the closed notch.")),
            R::c(tr("Remind me"), tr("How long before an event the reminder appears."), 170.0),
            R::c(tr("Keep showing"), tr("How long the reminder stays once the event has started."), 140.0),
            R::c(tr("Sound"), tr("Plays when a reminder appears and when the event starts."), 140.0),
        ],
    );
    p.tog(rr[0], s.reminders_enabled, |x, v| x.reminders_enabled = v);
    p.pick(
        rr[1],
        "set.remlead",
        &[tr("5 minutes before"), tr("10 minutes before"), tr("15 minutes before"), tr("30 minutes before"), tr("1 hour before")],
        &[5, 10, 15, 30, 60],
        &s.reminder_lead_min,
        0,
        |x, v| x.reminder_lead_min = v,
    );
    p.pick(rr[2], "set.remlinger", &[tr("1 minute"), tr("5 minutes"), tr("10 minutes"), tr("15 minutes")], &[1, 5, 10, 15], &s.reminder_linger_min, 0, |x, v| {
        x.reminder_linger_min = v
    });
    let sounds: Vec<String> = SOUNDS.iter().map(|x| x.to_string()).collect();
    p.pick(rr[3], "set.remsound", &SOUNDS, &sounds, &s.reminder_sound, 0, |x, v| x.reminder_sound = v);

    let (feeds, status, busy) = {
        let sh = p.s.shared.lock().unwrap();
        (sh.feeds.clone().unwrap_or_default(), sh.feed_status.clone(), sh.feed_busy)
    };
    let rm = p.button_w(tr("Remove"));
    let mut rows: Vec<R> =
        feeds.iter().map(|(label, _)| R::c(label.clone(), tr("Read-only. Shows in Calendar, Home and reminders; refreshes every 30 minutes."), rm)).collect();
    let add_desc = if busy {
        tr("Syncing…").to_string()
    } else if !status.is_empty() {
        status
    } else {
        tr("Google, Outlook or iCloud link.").to_string()
    };
    let add_w = p.button_w(tr("Add"));
    let mut add = R::c(tr("Add a calendar"), add_desc, 200.0 + 6.0 + add_w + 4.0 + CTRL_H);
    if p.s.feed_help {
        add = add.foot_styled(ICS_HELP, 11.0, SEMIBOLD);
    }
    rows.push(add);
    let rr = p.card(Some(tr("SUBSCRIBED CALENDARS")), &rows);
    for (i, (_, link)) in feeds.iter().enumerate() {
        if p.button(rr[i], tr("Remove"), false) {
            calendar::remove_feed(link);
            reload_feeds(p, false);
        }
    }
    if let Some(r) = rr[feeds.len()] {
        let (field, rest) = r.take_left(200.0, 6.0);
        let (btn, help) = rest.take_left(add_w, 4.0);
        let fid = id("settings.ics");
        let mut submit = p.input("settings.ics", field, tr("Paste .ics link"));
        if button(p.ui, btn, tr("Add"), false, true) {
            submit = Some(p.ui.field_text(fid));
        }
        let t = p.ui.t;
        if p.ui.icon_button(help, "Info", 15.0, if p.s.feed_help { t.accent } else { t.subtext }) {
            p.s.feed_help = !p.s.feed_help;
        }
        if let Some(link) = submit {
            let link = link.trim().to_string();
            if calendar::add_feed(&link) {
                p.s.field_error = None;
                p.ui.set_field_text(fid, "");
                p.ui.st.focus = 0;
                reload_feeds(p, true);
            } else {
                p.s.field_error = Some(fid);
            }
        }
    }
}

// ---------------------------------------------------------------- Prayer

pub fn prayer(p: &mut Pg) {
    let s = p.st.clone();
    p.title(tr("Prayer"), tr("Prayer times for your city, worked out on this PC, with alerts on the notch."));
    let (status, busy) = {
        let sh = p.s.shared.lock().unwrap();
        (sh.prayer_status.clone(), sh.prayer_busy)
    };
    let table = if !s.prayer_enabled {
        tr("Off.").to_string()
    } else if busy {
        tr("Looking up the city…").to_string()
    } else if !status.is_empty() {
        status
    } else if let Some(t) = model::prayer_times() {
        let list = model::times_list(&t);
        let parts: Vec<String> = (0..6).filter(|&i| i != 1).map(|i| format!("{} {}", prayer::NAMES[i], prayer::hhmm(list[i]))).collect();
        format!("{} · {}", model::prayer_city(), parts.join("   "))
    } else {
        tr("Set a city to see today's times.").to_string()
    };
    let rr = p.card(
        Some(tr("PRAYER TIMES")),
        &[
            R::t(tr("Prayer times"), tr("Alert on the notch at each prayer time (Subuh, Dzuhur, Ashar, Maghrib, Isya), worked out for your city.")).foot(table),
            R::c(tr("City"), tr("Where the times are for. Empty uses your Weather city."), 200.0),
            R::c(tr("Calculation method"), tr("The twilight angles used for Subuh and Isya. Kemenag RI matches Indonesia's official timetable."), 200.0),
            R::c(tr("Ashar"), tr("Standard (Syafi'i, Maliki, Hanbali) or Hanafi, which is later."), 170.0),
            R::c(tr("Heads-up"), tr("A short alert before each prayer, as well as at the time itself."), 170.0),
            R::c(tr("Sound"), tr("Plays when the prayer time comes."), 140.0),
        ],
    );
    p.tog(rr[0], s.prayer_enabled, |x, v| x.prayer_enabled = v);
    if let Some(r) = rr[1] {
        if let Some(v) = p.field("settings.prayercity", r, &s.weather_city, &s.prayer_city) {
            settings::update(|x| x.prayer_city = v.clone());
            let city = if v.trim().is_empty() { s.weather_city.clone() } else { v };
            let sh = p.shared();
            {
                let mut g = sh.lock().unwrap();
                g.prayer_busy = true;
                g.prayer_status.clear();
            }
            bg::spawn(move || {
                let found = prayer::geocode(&city);
                let mut g = sh.lock().unwrap();
                g.prayer_busy = false;
                match found {
                    Some((lat, lon, name)) => settings::update(|x| {
                        x.prayer_lat = Some(lat);
                        x.prayer_lon = Some(lon);
                        x.prayer_resolved_city = name;
                    }),
                    None => g.prayer_status = crate::i18n::tf!("Couldn't find \"{city}\". Check the spelling or your connection.", "Kota \"{city}\" tidak ditemukan. Periksa ejaan atau koneksimu."),
                }
            });
        }
    }
    let names: Vec<&str> = prayer::METHODS.iter().map(|m| m.name).collect();
    let ids: Vec<String> = prayer::METHODS.iter().map(|m| m.id.to_string()).collect();
    p.pick(rr[2], "set.method", &names, &ids, &s.prayer_method, 0, |x, v| x.prayer_method = v);
    p.pick(rr[3], "set.asr", &[tr("Standard (Syafi'i)"), tr("Hanafi")], &[false, true], &s.prayer_asr_hanafi, 0, |x, v| x.prayer_asr_hanafi = v);
    p.pick(
        rr[4],
        "set.prayerlead",
        &[tr("Only at the time"), tr("5 minutes before"), tr("10 minutes before"), tr("15 minutes before"), tr("30 minutes before")],
        &[0, 5, 10, 15, 30],
        &s.prayer_lead_min,
        0,
        |x, v| x.prayer_lead_min = v,
    );
    let sounds: Vec<String> = SOUNDS.iter().map(|x| x.to_string()).collect();
    p.pick(rr[5], "set.prayersound", &SOUNDS, &sounds, &s.prayer_sound, 0, |x, v| x.prayer_sound = v);

    adzan(p, &s);
}

/// Prayer → Adzan: the call to prayer as an audio file the user picks, per prayer, with volume and a test.
fn adzan(p: &mut Pg, s: &settings::Settings) {
    use crate::services::adzan;
    let file_label = |f: &str| -> String {
        let f = f.trim();
        if f.is_empty() {
            tr("No file chosen").to_string()
        } else {
            std::path::Path::new(f).file_name().map_or_else(|| f.to_string(), |n| n.to_string_lossy().into_owned())
        }
    };
    let choose_w = p.button_w("Choose…");
    let playing = adzan::playing().is_some();
    let test_w = p.button_w("Stop").max(p.button_w("Play"));
    // The five prayers as chips, as wide as they need.
    let chip_ws: Vec<f32> = adzan::PRAYERS.iter().map(|&(_, i)| p.ui.chip_w(prayer::NAMES[i], false)).collect();
    let chips_w = chip_ws.iter().sum::<f32>() + 4.0 * (chip_ws.len() - 1) as f32;
    let rr = p.card(
        Some("ADZAN"),
        &[
            R::t("Play the adzan", "At each prayer time, the call to prayer plays instead of the sound above. Music pauses while it plays; Silent (Control) keeps it quiet."),
            R::c("Adzan sound", format!("{} · {}", tr("An MP3, M4A or WAV of the adzan."), file_label(&s.adzan_file)), choose_w),
            R::c(
                "Subuh adzan",
                format!("{} · {}", tr("Optional: Subuh's own adzan. Empty uses the one above."), file_label(&s.adzan_fajr_file)),
                choose_w + 6.0 + 24.0,
            ),
            R::c("Play for", "Which prayer times get the adzan.", chips_w),
            R::new("Volume", "How loud the adzan plays (on top of the Windows volume).", 170.0, 22.0),
            R::c("Try it", "Play the adzan now to check the file and the volume.", test_w),
        ],
    );
    p.tog(rr[0], s.adzan_enabled, |x, v| x.adzan_enabled = v);
    if p.button(rr[1], "Choose…", false) {
        bg::spawn(|| {
            if let Some(f) = crate::services::player::pick_file(tr("Choose the adzan")) {
                settings::update(|x| x.adzan_file = f);
            }
        });
    }
    if let Some(r) = rr[2] {
        let (b, rest) = r.take_left(choose_w, 6.0);
        if button(p.ui, b, "Choose…", false, true) {
            bg::spawn(|| {
                if let Some(f) = crate::services::player::pick_file(tr("Choose the Subuh adzan")) {
                    settings::update(|x| x.adzan_fajr_file = f);
                }
            });
        }
        if small_icon(p.ui, Rect::new(rest.x, rest.cy() - 11.0, 22.0, 22.0), "XMark", !s.adzan_fajr_file.trim().is_empty()) {
            settings::update(|x| x.adzan_fajr_file.clear());
        }
    }
    if let Some(r) = rr[3] {
        let mut x = r.x;
        for (&(pid, i), w) in adzan::PRAYERS.iter().zip(&chip_ws) {
            let on = s.adzan_prayers.iter().any(|q| q == pid);
            if p.ui.chip(Rect::new(x, r.y, *w, r.h), prayer::NAMES[i], on, None) {
                settings::update(|st| {
                    if on {
                        st.adzan_prayers.retain(|q| q != pid);
                    } else {
                        st.adzan_prayers.push(pid.to_string());
                    }
                });
            }
            x += w + 4.0;
        }
    }
    if let Some(r) = rr[4] {
        if let Some(v) = p.ui.slider(id("settings.adzanvol"), r.inset(8.0, 0.0), s.adzan_volume as f32) {
            let v = (v as f64 * 100.0).round() / 100.0;
            settings::update_quiet(|x| x.adzan_volume = v);
            adzan::set_volume(v);
        }
    }
    if let Some(r) = rr[5] {
        let file = adzan::file_for("dhuhr");
        if button(p.ui, r, if playing { "Stop" } else { "Play" }, !playing, playing || file.is_some()) {
            if playing {
                bg::spawn(adzan::stop);
            } else if let Some(f) = file {
                bg::spawn(move || {
                    adzan::play_file(&f, "");
                });
            }
        }
    }
    if s.adzan_enabled && s.adzan_file.trim().is_empty() {
        p.hint("Choose an adzan file above; until then the prayer sound plays.", true);
    }
}

/// Display → Eyes: the peeking eyes' shape, colour and animation, following the pointer, and a preview.
fn eyes(p: &mut Pg, s: &settings::Settings) {
    let rr = p.card(
        Some("EYES"),
        &[
            R::new("Preview", "How the eyes look in the notch.", 150.0, 34.0),
            R::c("Eye shape", "Cat eyes, round eyes with a shine, robot panels, or two simple dots.", 150.0),
            R::c("Eye colour", "Theme follows the colour scheme (white on the dark notch).", 150.0),
            R::c("Animation", "Take turns through all of them, or always the same one.", 150.0),
            R::t("Look at the pointer", "The pupils follow the mouse pointer while the eyes are out."),
        ],
    );
    if let Some(r) = rr[0] {
        // The notch in small, with the eyes looping through their animation(s).
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let t = p.ui.t;
        let pill = Rect::new(r.x, r.cy() - 17.0, r.w, 34.0);
        p.ui.g.fill_round(pill.d2d(), 17.0, p.ui.c(t.pill));
        p.ui.g.stroke_round(pill.inset(0.5, 0.5).d2d(), 16.5, p.ui.c(t.stroke), 1.0);
        let loop_s = crate::notch::EYES_LOOP_SECS;
        let el = START.get_or_init(std::time::Instant::now).elapsed().as_secs_f32();
        let kind = match s.eyes_anim.as_str() {
            "look" => 0,
            "sleepy" => 1,
            "wink" => 2,
            "curious" => 3,
            _ => (el / loop_s) as u8 % 4,
        };
        let look = if s.eyes_follow {
            let (mx, my) = p.ui.input.mouse;
            (!mx.is_nan()).then(|| (((mx - pill.cx()) / 160.0).clamp(-1.0, 1.0), ((my - pill.cy()) / 90.0).clamp(-1.0, 1.0)))
        } else {
            None
        };
        crate::notch::draw_eyes(p.ui.g, &pill, el % loop_s, p.ui.alpha, &t, kind, look);
        p.ui.animate = true;
    }
    let shapes = ["cat".to_string(), "round".into(), "robot".into(), "dots".into()];
    p.pick(rr[1], "set.eyeshape", &["Cat", "Round", "Robot", "Dots"], &shapes, &s.eyes_style, 0, |x, v| x.eyes_style = v);
    let colours = ["theme".to_string(), "accent".into(), "yellow".into(), "green".into(), "blue".into(), "pink".into()];
    p.pick(rr[2], "set.eyecolor", &["Theme", "Accent", "Yellow", "Green", "Blue", "Pink"], &colours, &s.eyes_color, 0, |x, v| x.eyes_color = v);
    let anims = ["mix".to_string(), "look".into(), "sleepy".into(), "wink".into(), "curious".into()];
    p.pick(rr[3], "set.eyeanim", &["Take turns", "Look around", "Sleepy", "Wink", "Curious"], &anims, &s.eyes_anim, 0, |x, v| x.eyes_anim = v);
    p.tog(rr[4], s.eyes_follow, |x, v| x.eyes_follow = v);
}

// ---------------------------------------------------------------- Weather

/// The place the weather is for (search, then pick the right one: towns share names) and its units.
pub fn weather(p: &mut Pg) {
    use crate::services::weather;
    let s = p.st.clone();
    p.title("Weather", "Which place the weather is for, and its units.");
    let (results, busy, status) = {
        let sh = p.s.shared.lock().unwrap();
        (sh.weather_results.clone(), sh.weather_busy, sh.weather_status.clone())
    };
    let now = if s.weather_city.trim().is_empty() {
        tr("Not set yet.").to_string()
    } else {
        crate::i18n::tf!("Now: {}", "Sekarang: {}", s.weather_city.trim())
    };
    let foot = if busy {
        tr("Searching…").to_string()
    } else if !status.is_empty() {
        status
    } else {
        now
    };
    let rr = p.card(
        Some("LOCATION"),
        &[R::c("City or area", "Type a city or district and press Enter, then pick the right one from the list.", 220.0).foot(foot)],
    );
    if let Some(r) = rr[0] {
        if let Some(q) = p.input("settings.weathercity", r, "e.g. Bekasi") {
            let sh = p.shared();
            {
                let mut g = sh.lock().unwrap();
                g.weather_busy = true;
                g.weather_status.clear();
                g.weather_results = None;
            }
            bg::spawn(move || {
                let found = weather::search(&q);
                let mut g = sh.lock().unwrap();
                g.weather_busy = false;
                match found {
                    Some(list) if !list.is_empty() => g.weather_results = Some(list),
                    Some(_) => g.weather_status = tr("No place found. Check the spelling or your connection.").to_string(),
                    None => g.weather_status = tr("Couldn't search. Check your connection.").to_string(),
                }
            });
        }
    }

    // The places found: pick one.
    if let Some(list) = results.filter(|l| !l.is_empty()) {
        let choose_w = p.button_w("Choose");
        let rows: Vec<R> = list.iter().map(|pl| R::c(pl.name.clone(), pl.area.clone(), choose_w)).collect();
        let rr = p.card(Some("PICK THE PLACE"), &rows);
        for (pl, at) in list.iter().zip(rr) {
            if p.button(at, "Choose", true) {
                let (label, lat, lon) = (pl.label(), pl.lat, pl.lon);
                settings::update(|x| {
                    x.weather_city = label;
                    x.weather_lat = Some(lat);
                    x.weather_lon = Some(lon);
                });
                weather::invalidate();
                model::weather_changed();
                p.s.shared.lock().unwrap().weather_results = None;
                p.ui.set_field_text(id("settings.weathercity"), "");
            }
        }
    }

    let rr = p.card(Some("UNITS"), &[R::c("Units", "Temperature and wind speed.", 170.0)]);
    if p.pick(rr[0], "set.units", &["Celsius · km/h", "Fahrenheit · mph"], &[false, true], &s.use_fahrenheit, 0, |x, v| x.use_fahrenheit = v) {
        weather::invalidate();
        model::weather_changed();
    }
}

// ---------------------------------------------------------------- To-dos

pub fn todos(p: &mut Pg) {
    let s = p.st.clone();
    p.title(tr("To-Dos"), tr("Keep your to-dos in step with TickTick: add, tick off, star or delete on either side."));
    let rr = p.card(
        Some(tr("NEW TO-DOS")),
        &[R::c(tr("Due date"), tr("What a new to-do starts with. Change it per to-do with the date chip next to the box."), 140.0)],
    );
    let dues = ["today".to_string(), "none".into()];
    p.pick(rr[0], "set.due", &[tr("Today"), tr("No date")], &dues, &s.todo_default_due, 0, |x, v| x.todo_default_due = v);

    let has = p.s.facts.has_token;
    let save_w = button_w(p.ui, tr("Save"), true);
    let token_desc = if has {
        tr("Saved and encrypted for your Windows account. Paste a new token to replace it.")
    } else {
        tr("In TickTick on the web: your avatar → Settings → Account → API Token → create and copy it. It's stored encrypted on this PC.")
    };
    let mut rows = vec![R::c(tr("API token"), token_desc, 200.0 + 6.0 + save_w)];
    let sh = p.shared();
    let (projects, loading, error, syncing) = {
        let g = sh.lock().unwrap();
        (g.tick_projects.clone(), g.tick_loading, g.tick_error.clone(), g.tick_syncing)
    };
    if has {
        let status = ticktick::status();
        rows.push(R::t(tr("Sync with TickTick"), if status.is_empty() { tr("Every few minutes, and whenever you change something.").to_string() } else { status }));
        rows.push(R::c(tr("List"), tr("Which TickTick list the To-Dos tab shows. New to-dos go there too."), 200.0));
        let st = ticktick::status();
        let last = st.strip_prefix("Synced ").map(|t| crate::i18n::tf!("Last synced at {t}.", "Terakhir disinkronkan {t}."));
        let sync_desc = if syncing { tr("Syncing…").to_string() } else { last.unwrap_or_else(|| tr("Not synced yet.").into()) };
        rows.push(R::c(tr("Sync now"), sync_desc, p.button_w(tr("Sync now"))));
        rows.push(R::c(tr("Forget token"), tr("Stop syncing and delete the saved token from this PC. Your to-dos stay in both places."), p.button_w(tr("Remove"))));
    }
    let rr = p.card(Some(tr("TICKTICK")), &rows);
    if let Some(r) = rr[0] {
        let (field, btn) = r.take_left(200.0, 6.0);
        let mut submit = p.secret(field, if has { tr("••••••••  saved") } else { tr("Paste your TickTick API token") });
        if button(p.ui, btn, tr("Save"), true, true) && !p.s.token.trim().is_empty() {
            submit = Some(std::mem::take(&mut p.s.token).trim().to_string());
        }
        if let Some(tok) = submit {
            settings::set_ticktick_token(Some(&tok));
            p.s.facts.has_token = true;
            if !s.ticktick_enabled {
                settings::update(|x| x.ticktick_enabled = true); // pasting a token means "sync with TickTick"
            }
            p.ui.st.focus = 0;
            let mut g = sh.lock().unwrap();
            g.tick_projects = None;
            g.tick_error = None;
            drop(g);
            ticktick::sync_soon();
        }
    }
    if !has {
        return;
    }
    p.tog(rr[1], s.ticktick_enabled, |x, v| x.ticktick_enabled = v);
    if let Some(r) = rr[2] {
        match &projects {
            Some(list) if !list.is_empty() => {
                let names: Vec<&str> = list.iter().map(|(_, n)| n.as_str()).collect();
                let sel = list.iter().position(|(id, _)| *id == s.ticktick_project).unwrap_or(0);
                if let Some(i) = p.combo("set.ticklist", r, &names, sel) {
                    let (id, name) = list[i].clone();
                    if id != s.ticktick_project {
                        settings::update(|x| {
                            x.ticktick_project = id;
                            x.ticktick_project_name = name;
                        });
                        ticktick::sync_soon();
                    }
                }
            }
            _ => {
                // Not loaded yet: the saved name, disabled, while the lists load in the background.
                let name = s.ticktick_project_name.clone();
                let _ = p.combo_enabled("set.ticklist", r, &[name.as_str()], 0, false);
                if !loading && projects.is_none() && error.is_none() {
                    sh.lock().unwrap().tick_loading = true;
                    let sh = sh.clone();
                    bg::spawn(move || {
                        let res = ticktick::projects();
                        let mut g = sh.lock().unwrap();
                        g.tick_loading = false;
                        match res {
                            Ok(list) => g.tick_projects = Some(list),
                            Err(e) => {
                                g.tick_error = Some(e);
                                g.tick_projects = Some(vec![]);
                            }
                        }
                    });
                }
            }
        }
    }
    if let Some(e) = error {
        let _ = e;
        p.hint(tr("TickTick refused the token. Paste a new one above."), true);
    }
    if let Some(r) = rr[3] {
        if button(p.ui, r, tr("Sync now"), false, !syncing) {
            sh.lock().unwrap().tick_syncing = true;
            let sh = sh.clone();
            bg::spawn(move || {
                let _ = ticktick::sync();
                sh.lock().unwrap().tick_syncing = false;
            });
        }
    }
    if p.button(rr[4], tr("Remove"), false) {
        settings::set_ticktick_token(None);
        p.s.facts.has_token = false;
        settings::update(|x| x.ticktick_enabled = false);
        let mut g = sh.lock().unwrap();
        g.tick_projects = None;
        g.tick_error = None;
    }
}

// ---------------------------------------------------------------- Data

pub fn data(p: &mut Pg) {
    let s = p.st.clone();
    p.title(tr("Clipboard & Shelf"), tr("History and dropped files are stored only on this PC."));
    let rr = p.card(
        Some(tr("CLIPBOARD")),
        &[
            R::t(tr("Record history"), tr("Keep a list of what you copy.")),
            R::t(tr("Skip private clips"), tr("Ignore copies that password managers mark as sensitive.")),
            R::c(tr("Keep up to"), tr("Older unpinned clips are removed."), 140.0),
            R::c(tr("Clear history"), tr("Pinned clips are kept."), p.button_w(tr("Clear history"))),
        ],
    );
    p.tog(rr[0], s.clipboard_enabled, |x, v| x.clipboard_enabled = v);
    p.tog(rr[1], s.clipboard_skip_sensitive, |x, v| x.clipboard_skip_sensitive = v);
    p.pick(rr[2], "set.clipmax", &[tr("50 items"), tr("100 items"), tr("200 items"), tr("500 items")], &[50, 100, 200, 500], &s.clipboard_max_items, 0, |x, v| {
        x.clipboard_max_items = v
    });
    if p.button(rr[3], tr("Clear history"), false) {
        bg::spawn(cliphistory::clear);
    }

    let rr = p.card(Some(tr("SHELF")), &[R::c(tr("Remove items after"), tr("Files you drop on the notch are forgotten after this long."), 140.0)]);
    p.pick(rr[0], "set.shelf", &[tr("1 day"), tr("3 days"), tr("7 days"), tr("30 days"), tr("Never")], &[1, 3, 7, 30, 0], &s.shelf_retention_days, 0, |x, v| {
        x.shelf_retention_days = v
    });

    let folder = phone::receive_folder().to_string_lossy().into_owned();
    let rr = p.card(
        Some(tr("PHONE TRANSFER")),
        &[
            R::t(tr("Phone transfer"), tr("Send files with phones on the same Wi-Fi: scan a QR code, no app needed.")),
            R::c(tr("Save received files in"), folder.clone(), p.button_w(tr("Change…"))),
        ],
    );
    if p.tog(rr[0], s.phone_enabled, |x, v| x.phone_enabled = v) {
        bg::spawn(phone::sync);
    }
    if p.button(rr[1], tr("Change…"), false) {
        bg::spawn(move || {
            if let Some(dir) = pick_folder(tr("Save received files in"), &folder) {
                settings::update(|x| x.phone_receive_folder = dir);
            }
        });
    }

    let rr = p.card(Some(tr("TIMERS")), &[R::c(tr("Break length"), tr("Pomodoro break after each focus session."), 140.0)]);
    p.pick(rr[0], "set.break", &[tr("3 minutes"), tr("5 minutes"), tr("10 minutes"), tr("15 minutes")], &[3, 5, 10, 15], &s.pomodoro_break_min, 0, |x, v| {
        x.pomodoro_break_min = v
    });
}

/// The folder picker (own STA thread). Blocking: call from a background thread.
fn pick_folder(title: &str, start: &str) -> Option<String> {
    let title = title.to_string();
    let start = start.to_string();
    std::thread::spawn(move || unsafe {
        use windows::core::HSTRING;
        use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH};
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let mut out = None;
        if let Ok(dlg) = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) {
            let _ = dlg.SetTitle(&HSTRING::from(title));
            if let Ok(opts) = dlg.GetOptions() {
                let _ = dlg.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
            }
            if std::path::Path::new(&start).is_dir() {
                if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start), None) {
                    let _ = dlg.SetFolder(&item);
                }
            }
            if dlg.Show(None).is_ok() {
                if let Ok(item) = dlg.GetResult() {
                    if let Ok(p) = item.GetDisplayName(SIGDN_FILESYSPATH) {
                        out = p.to_string().ok();
                        CoTaskMemFree(Some(p.0 as _));
                    }
                }
            }
        }
        CoUninitialize();
        out
    })
    .join()
    .ok()
    .flatten()
}

/// "Windows 11 · build 26200" (build 22000 and later is Windows 11). Read once.
pub(super) fn windows_name() -> String {
    use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
    // RtlGetVersion tells the truth regardless of manifest.
    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(info: *mut OSVERSIONINFOW) -> i32;
    }
    let mut info = OSVERSIONINFOW { dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32, ..Default::default() };
    unsafe {
        RtlGetVersion(&mut info);
    }
    let name = if info.dwMajorVersion == 10 && info.dwBuildNumber >= 22000 { tr("Windows 11").to_string() } else { format!("Windows {}", info.dwMajorVersion) };
    format!("{name} · build {}", info.dwBuildNumber)
}
