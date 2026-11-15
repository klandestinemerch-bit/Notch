//! Phone: send files both ways between the phone and this PC over Wi-Fi. Off: a hero with what it does and
//! "Turn on". On: a link chip in the header, the QR card, what the PC offers the phone ("To phone") and transfer
//! activity, all tinted with the accent (the phone page uses the same accent, live).
//! The server itself is `services::phone`; it starts at app launch when Phone was left on.

use crate::bg;
use crate::gfx::{color, mix, with_alpha, Align, Color, BOLD, MEDIUM, REGULAR, SEMIBOLD};
use crate::services::phone::{self, OutKind, Snapshot};
use crate::services::{qr, settings, shortcuts};
use crate::ui::{id, Rect, Ui};
use crate::i18n::tr;

pub struct State {
    qr_for: Option<String>,
    qr: Option<qr::Qr>,
    note: Option<(String, std::time::Instant)>,
    /// "Downloads\FathNotch": where received files go, for the empty Activity text (read when shown).
    folder_short: String,
}

impl Default for State {
    fn default() -> Self {
        // Created once at startup: bring the server back if Phone was left on.
        if settings::get().phone_enabled {
            std::thread::spawn(phone::sync);
        }
        Self { qr_for: None, qr: None, note: None, folder_short: short_folder() }
    }
}

fn short_folder() -> String {
    let folder = phone::receive_folder();
    folder
        .parent()
        .and_then(|p| p.file_name())
        .map(|p| format!("{}\\{}", p.to_string_lossy(), folder.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()))
        .unwrap_or_else(|| folder.to_string_lossy().into_owned())
}

pub fn shown(s: &mut State) {
    s.folder_short = short_folder();
    let snap = phone::snapshot();
    if settings::get().phone_enabled && !snap.running && !snap.starting {
        bg::spawn(phone::sync);
    }
}

fn lh(size: f32) -> f32 {
    (size * 1.33).round()
}

/// Seconds since the view first drew, for the gentle pulses.
fn clock() -> f32 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f32()
}

/// A surface with a little of the accent in it.
fn tint(ui: &Ui, amount: f32) -> Color {
    let t = ui.t;
    let mut c = mix(t.surface, t.accent, amount);
    c.a = t.surface.a.max(0.9);
    c
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, _go: &mut Option<&'static str>) {
    let snap = phone::snapshot();
    if snap.running {
        on_state(ui, s, &snap, r);
    } else {
        off_state(ui, &snap, r);
    }
    if snap.transfers.iter().any(|t| t.active) || snap.starting || snap.running {
        ui.animate = true;
    }
}

/// Button: accent (solid) or plain (surface2); hover dims to 85 %. An optional leading icon.
fn button(ui: &mut Ui, r: Rect, label: &str, icon: Option<&'static str>, size: f32, accent: bool, enabled: bool) -> bool {
    let t = ui.t;
    let hov = enabled && ui.hovered(r);
    let a = if !enabled { 0.35 } else if hov { 0.85 } else { 1.0 };
    let (bg, fg) = if accent { (t.accent, t.on_accent) } else { (t.surface2, t.text) };
    ui.fill(r, (r.h / 2.0).min(11.0), with_alpha(bg, a));
    let fg = with_alpha(fg, if enabled { 1.0 } else { 0.35 });
    let weight = if accent { SEMIBOLD } else { MEDIUM };
    match icon {
        Some(name) => {
            let tw = ui.measure(label, size, weight).ceil();
            let x = r.cx() - (tw + size + 6.0) / 2.0;
            ui.icon(name, x + size / 2.0, r.cy(), size, fg);
            ui.text(label, Rect::new(x + size + 6.0, r.y, tw + 2.0, r.h), size, weight, fg, Align::Left);
        }
        None => ui.text(label, r, size, weight, fg, Align::Center),
    }
    enabled && ui.clicked(r)
}

fn button_w(ui: &Ui, label: &str, icon: bool, size: f32, weight: u16) -> f32 {
    ui.measure(label, size, weight).ceil() + 28.0 + if icon { size + 6.0 } else { 0.0 }
}

fn off_state(ui: &mut Ui, snap: &Snapshot, r: Rect) {
    let t = ui.t;
    let w = 460.0f32.min(r.w);
    let x = r.cx() - w / 2.0;
    let waiting = snap.outbox.len();
    let body = tr("Send files and text both ways between your phone and this PC. Scan a QR code, no app needed.");
    let lines = wrap(ui, body, 12.0, REGULAR, w - 40.0);
    let status_lines = if snap.status.is_empty() { vec![] } else { wrap(ui, &snap.status, 11.0, REGULAR, w) };
    let h = 72.0 + 14.0 + lh(17.0) + 4.0 + lines.len() as f32 * lh(12.0) + 14.0 + 26.0 + 16.0 + 34.0
        + if waiting > 0 { 10.0 + lh(11.0) } else { 0.0 }
        + if status_lines.is_empty() { 0.0 } else { 8.0 + status_lines.len() as f32 * lh(11.0) };
    let mut y = r.cy() - h / 2.0;

    // Hero: a breathing halo, the accent tile, the phone glyph.
    let (cx, cy) = (r.cx(), y + 36.0);
    let breathe = 0.5 + 0.5 * (clock() * 1.6).sin();
    ui.g.fill_circle(cx, cy, 44.0 + 3.0 * breathe, ui.c(with_alpha(t.accent, 0.06)));
    ui.g.fill_circle(cx, cy, 36.0, ui.c(with_alpha(t.accent, 0.10)));
    ui.fill(Rect::new(cx - 26.0, cy - 26.0, 52.0, 52.0), 16.0, t.accent);
    ui.icon("DevicePhone", cx, cy, 24.0, t.on_accent);
    ui.animate = true;
    y += 72.0 + 14.0;

    ui.text(tr("Connect your phone"), Rect::new(x, y, w, lh(17.0)), 17.0, SEMIBOLD, t.text, Align::Center);
    y += lh(17.0) + 4.0;
    for l in &lines {
        ui.text(l, Rect::new(x, y, w, lh(12.0)), 12.0, REGULAR, t.subtext, Align::Center);
        y += lh(12.0);
    }
    y += 14.0;

    // Three chips: what it is.
    let chips: [(&'static str, &str); 3] = [("Wifi", tr("Same Wi-Fi")), ("ArrowsUpDown", tr("Both ways")), ("Lock", tr("Stays on your network"))];
    let widths: Vec<f32> = chips.iter().map(|(_, l)| ui.measure(l, 11.0, MEDIUM).ceil() + 28.0 + 10.0).collect();
    let total = widths.iter().sum::<f32>() + 8.0 * (chips.len() - 1) as f32;
    let mut cx0 = r.cx() - total / 2.0;
    for ((icon, label), cw) in chips.iter().zip(&widths) {
        let c = Rect::new(cx0, y, *cw, 26.0);
        ui.fill(c, 13.0, t.accent_soft);
        ui.icon(icon, c.x + 16.0, c.cy(), 12.0, t.accent);
        ui.text(label, Rect::new(c.x + 28.0, c.y, cw - 28.0, c.h), 11.0, MEDIUM, t.text, Align::Left);
        cx0 += cw + 8.0;
    }
    y += 26.0 + 16.0;

    let label = if snap.starting { tr("Starting…") } else { tr("Turn on") };
    let bw = button_w(ui, label, true, 13.0, SEMIBOLD).max(140.0);
    if button(ui, Rect::new(r.cx() - bw / 2.0, y, bw, 34.0), label, Some("Bolt"), 13.0, true, !snap.starting) {
        bg::spawn(phone::turn_on);
    }
    y += 34.0;
    if waiting > 0 {
        y += 10.0;
        let msg = if waiting == 1 {
            tr("1 item waiting · turn on to send it").to_string()
        } else {
            crate::i18n::tf!("{waiting} items waiting · turn on to send them", "{waiting} item menunggu · nyalakan untuk mengirimnya")
        };
        ui.text(&msg, Rect::new(x, y, w, lh(11.0)), 11.0, MEDIUM, t.accent, Align::Center);
        y += lh(11.0);
    }
    if !status_lines.is_empty() {
        y += 8.0;
        for l in &status_lines {
            ui.text(l, Rect::new(x, y, w, lh(11.0)), 11.0, REGULAR, t.danger, Align::Center);
            y += lh(11.0);
        }
    }
}

fn on_state(ui: &mut Ui, s: &mut State, snap: &Snapshot, r: Rect) {
    let t = ui.t;
    let (head, body) = r.take_top(26.0, 10.0);

    // Header left: the link chip (accent with a pulse while the phone page is open).
    let link = if snap.connected { tr("Phone connected") } else { tr("Waiting for phone") };
    let lw = ui.measure(link, 11.0, SEMIBOLD).ceil() + 40.0;
    let chip = Rect::new(head.x, head.y, lw, head.h);
    let (dot_x, dot_y) = (chip.x + 16.0, chip.cy());
    if snap.connected {
        ui.fill(chip, 13.0, t.accent_soft);
        let p = (clock() * 0.7).fract();
        ui.g.fill_circle(dot_x, dot_y, 4.0 + 6.0 * p, ui.c(with_alpha(t.accent, 0.45 * (1.0 - p))));
        ui.g.fill_circle(dot_x, dot_y, 4.0, ui.c(t.accent));
        ui.text(link, Rect::new(dot_x + 10.0, chip.y, lw, chip.h), 11.0, SEMIBOLD, t.accent, Align::Left);
    } else {
        ui.fill(chip, 13.0, t.surface2);
        let blink = 0.45 + 0.55 * (0.5 + 0.5 * (clock() * 3.0).sin());
        ui.g.fill_circle(dot_x, dot_y, 4.0, ui.c(with_alpha(t.subtext, blink)));
        ui.text(link, Rect::new(dot_x + 10.0, chip.y, lw, chip.h), 11.0, SEMIBOLD, t.subtext, Align::Left);
    }

    // Header right: Received, Turn off; a status or note to their left.
    let off_w = button_w(ui, tr("Turn off"), false, 11.0, MEDIUM);
    let off_r = Rect::new(head.right() - off_w, head.y, off_w, head.h);
    if button(ui, off_r, tr("Turn off"), None, 11.5, false, true) {
        bg::spawn(phone::turn_off);
    }
    let rec_w = button_w(ui, tr("Received"), true, 11.0, MEDIUM);
    let rec_r = Rect::new(off_r.x - 6.0 - rec_w, head.y, rec_w, head.h);
    if button(ui, rec_r, tr("Received"), Some("Folder"), 11.5, false, true) {
        let dir = phone::receive_folder();
        bg::spawn(move || {
            let _ = std::fs::create_dir_all(&dir);
            crate::shell::open(&dir.to_string_lossy());
        });
    }
    let note = s.note.as_ref().filter(|(_, at)| at.elapsed().as_secs() < 3).map(|(n, _)| n.clone());
    let status = note.unwrap_or_else(|| tr(&snap.status).to_string());
    let free = rec_r.x - 10.0 - chip.right() - 10.0;
    if !status.is_empty() {
        let w = (ui.measure(&status, 11.0, REGULAR).ceil() + 2.0).min(free.max(0.0));
        ui.text(&status, Rect::new(rec_r.x - 10.0 - w, head.y, w, head.h), 11.0, REGULAR, t.danger, Align::Right);
    } else if !snap.connected && free > 240.0 {
        ui.text(tr("Scan the code with your phone's camera"), Rect::new(chip.right() + 10.0, head.y, free, head.h), 11.0, REGULAR, t.subtext, Align::Left);
    }

    // Columns: QR card (150) | 12 | To phone card | 12 | Activity card.
    let (qr_card, rest) = body.take_left(150.0, 12.0);
    let half = (rest.w - 12.0) / 2.0;
    let to_phone = Rect::new(rest.x, rest.y, half, rest.h);
    let activity = Rect::new(to_phone.right() + 12.0, rest.y, rest.right() - to_phone.right() - 12.0, rest.h);
    qr_panel(ui, s, snap, qr_card);
    ui.card(to_phone, 16.0);
    outbox_panel(ui, s, snap, to_phone.inset(10.0, 10.0));
    ui.card(activity, 16.0);
    activity_panel(ui, s, snap, activity.inset(10.0, 10.0));
}

fn qr_panel(ui: &mut Ui, s: &mut State, snap: &Snapshot, r: Rect) {
    let t = ui.t;
    ui.fill(r, 16.0, tint(ui, 0.12));
    ui.g.stroke_round(r.inset(0.5, 0.5).d2d(), 15.5, ui.c(with_alpha(t.accent, 0.28)), 1.0);
    let inner = r.inset(10.0, 10.0);
    let Some(url) = snap.web_url.clone() else {
        ui.icon("Wifi", inner.cx(), inner.cy() - 22.0, 22.0, t.accent);
        let lines = wrap(ui, tr("Join the same Wi-Fi as your phone"), 11.0, REGULAR, inner.w);
        let mut y = inner.cy();
        for l in &lines {
            ui.text(l, Rect::new(inner.x, y, inner.w, lh(11.0)), 11.0, REGULAR, t.subtext, Align::Center);
            y += lh(11.0);
        }
        return;
    };
    if s.qr_for.as_deref() != Some(url.as_str()) {
        s.qr = qr::encode(url.as_bytes());
        s.qr_for = Some(url.clone());
    }
    // The white code in an accent ring (up to 124, smaller in a short panel), "Scan with your phone", the
    // address chip when it fits; the steps when tall.
    let steps = r.h >= 340.0;
    let steps_h = if steps { 16.0 + 1.0 + 12.0 + lh(10.0) + 8.0 + 3.0 * 26.0 } else { 0.0 };
    let caption = 10.0 + lh(11.0);
    let addr = inner.h >= 110.0 + caption + 26.0;
    let ring_side = (inner.h - caption - if addr { 26.0 } else { 0.0 }).clamp(84.0, 124.0).min(inner.w).floor();
    let h = ring_side + caption + if addr { 26.0 } else { 0.0 } + steps_h;
    let mut y = inner.y + ((inner.h - h) / 2.0).max(0.0);
    let ring = Rect::new((inner.cx() - ring_side / 2.0).round(), y, ring_side, ring_side);
    ui.fill(ring, 14.0, t.accent);
    let sq = ring.inset(4.0, 4.0);
    ui.fill(sq, 11.0, color(0xFFFFFF, 1.0));
    if let Some(q) = &s.qr {
        // Modules snapped to whole device pixels inside the image (2-module quiet zone).
        let side = ring_side - 20.0;
        let n = q.size as f32 + 4.0;
        let scale = ui.g.scale.max(1.0);
        let m = ((side * scale / n).floor() / scale).max(1.0 / scale);
        let total = m * n;
        let ox = sq.x + 6.0 + ((side - total) / 2.0 * scale).round() / scale;
        let oy = sq.y + 6.0 + ((side - total) / 2.0 * scale).round() / scale;
        let dark = ui.c(color(0x000000, 1.0));
        for row in 0..q.size {
            let mut x = 0;
            while x < q.size {
                if !q.get(x, row) {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < q.size && q.get(x, row) {
                    x += 1;
                }
                let rx = ox + (start as f32 + 2.0) * m;
                let ry = oy + (row as f32 + 2.0) * m;
                ui.g.fill_rect(Rect::new(rx, ry, (x - start) as f32 * m, m).d2d(), dark);
            }
        }
    }
    y += ring_side + 10.0;
    ui.text(tr("Scan with your phone"), Rect::new(inner.x, y, inner.w, lh(11.0)), 11.0, SEMIBOLD, t.text, Align::Center);
    y += lh(11.0);
    if addr {
        y += 6.0;
        let authority = url.split("://").nth(1).and_then(|x| x.split('/').next()).unwrap_or("").to_string();
        let aw = (ui.measure(&authority, 10.0, MEDIUM).ceil() + 16.0).min(inner.w);
        let ar = Rect::new(inner.cx() - aw / 2.0, y, aw, 20.0);
        ui.fill(ar, 10.0, t.accent_soft);
        ui.text(&authority, ar, 10.0, MEDIUM, t.accent, Align::Center);
        y += 20.0;
    }
    if steps {
        let x0 = inner.x + 2.0;
        let w = inner.w - 4.0;
        y += 16.0;
        ui.fill(Rect::new(x0, y, w, 1.0), 0.0, with_alpha(t.accent, 0.18));
        y += 12.0;
        ui.text(tr("HOW IT WORKS"), Rect::new(x0, y, w, lh(10.0)), 10.0, BOLD, t.accent, Align::Left);
        y += lh(10.0) + 8.0;
        for (i, step) in [tr("Phone on the same Wi-Fi as this PC"), tr("Scan the code with its camera"), tr("Drop files here, or send from the page")].iter().enumerate() {
            let c = Rect::new(x0, y, 18.0, 18.0);
            ui.fill(c, 9.0, t.accent);
            ui.text(&(i + 1).to_string(), c, 10.0, BOLD, t.on_accent, Align::Center);
            let lines = wrap(ui, step, 11.0, REGULAR, w - 25.0);
            let mut ly = y + 1.0;
            for l in &lines {
                ui.text(l, Rect::new(x0 + 25.0, ly, w - 25.0, lh(11.0)), 11.0, REGULAR, t.subtext, Align::Left);
                ly += lh(11.0);
            }
            y = ly.max(y + 18.0) + 8.0;
        }
    }
}

/// Empty state for a column: a 38 px accent circle with an icon, a title and a wrapped line, centred.
fn empty(ui: &mut Ui, r: Rect, icon: &'static str, title: &str, sub: &str) {
    let t = ui.t;
    let inner = r.inset(10.0, 10.0);
    let lines = wrap(ui, sub, 11.0, REGULAR, inner.w);
    let h = 38.0 + 8.0 + lh(12.0) + 2.0 + lines.len() as f32 * lh(11.0);
    let mut y = r.cy() - h / 2.0;
    ui.g.fill_circle(r.cx(), y + 19.0, 19.0, ui.c(t.accent_soft));
    ui.icon(icon, r.cx(), y + 19.0, 17.0, t.accent);
    y += 38.0 + 8.0;
    ui.text(title, Rect::new(inner.x, y, inner.w, lh(12.0)), 12.0, SEMIBOLD, t.text, Align::Center);
    y += lh(12.0) + 2.0;
    for l in &lines {
        ui.text(l, Rect::new(inner.x, y, inner.w, lh(11.0)), 11.0, REGULAR, t.subtext, Align::Center);
        y += lh(11.0);
    }
}

/// Section header: an accent tick, the caps caption and a count badge.
fn caps(ui: &mut Ui, text: &str, count: usize, r: Rect) {
    let t = ui.t;
    ui.fill(Rect::new(r.x, r.cy() - 6.0, 3.0, 12.0), 1.5, t.accent);
    let tw = ui.measure(text, 11.0, BOLD).ceil();
    ui.text(text, Rect::new(r.x + 9.0, r.y, tw + 2.0, r.h), 11.0, BOLD, t.text, Align::Left);
    if count > 0 {
        let label = count.to_string();
        let bw = (ui.measure(&label, 10.0, BOLD).ceil() + 10.0).max(18.0);
        let b = Rect::new(r.x + 9.0 + tw + 6.0, r.cy() - 8.0, bw, 16.0);
        ui.fill(b, 8.0, t.accent_soft);
        ui.text(&label, b, 10.0, BOLD, t.accent, Align::Center);
    }
}

/// A 26 px rounded icon tile: accent when `on`, neutral otherwise, or in `c` (failed).
fn icon_tile(ui: &mut Ui, cx: f32, cy: f32, icon: &'static str, on: bool, c: Option<Color>) {
    let t = ui.t;
    let (bg, fg) = match c {
        Some(c) => (with_alpha(c, 0.16), c),
        None if on => (t.accent_soft, t.accent),
        None => (t.hover, t.subtext),
    };
    ui.fill(Rect::new(cx - 13.0, cy - 13.0, 26.0, 26.0), 8.0, bg);
    ui.icon(icon, cx, cy, 13.0, fg);
}

fn outbox_panel(ui: &mut Ui, s: &mut State, snap: &Snapshot, r: Rect) {
    let t = ui.t;
    let (head, body) = r.take_top(22.0, 8.0);
    caps(ui, tr("TO PHONE"), snap.outbox.len(), head);
    // Right to left: Trash (only with items), Clipboard, Plus (accent) — so they read Plus, Clipboard, Trash.
    let mut bx = head.right() - 22.0;
    if !snap.outbox.is_empty() {
        if ui.icon_button(Rect::new(bx, head.y, 22.0, 22.0), "Trash", 12.0, t.subtext) {
            phone::clear_outbox();
        }
        bx -= 24.0;
    }
    if ui.icon_button(Rect::new(bx, head.y, 22.0, 22.0), "ClipboardList", 12.0, t.subtext) && !phone::send_clipboard() {
        s.note = Some((tr("Nothing on the clipboard to send.").into(), std::time::Instant::now()));
    }
    bx -= 24.0;
    let plus = Rect::new(bx, head.y, 22.0, 22.0);
    let hov = ui.hovered(plus);
    ui.fill(plus, 7.0, if hov { mix(t.accent, t.pill, 0.15) } else { t.accent });
    ui.icon("Plus", plus.cx(), plus.cy(), 12.0, t.on_accent);
    if ui.clicked(plus) {
        bg::spawn(|| {
            let files = shortcuts::pick_files(tr("Send to phone"), true);
            if !files.is_empty() {
                phone::send(&files);
            }
        });
    }

    if snap.outbox.is_empty() {
        ui.fill(body, 12.0, with_alpha(t.accent, 0.05));
        dashed_round(ui, body.inset(0.75, 0.75), 12.0, with_alpha(t.accent, 0.5), 1.5, 6.0, 4.5);
        empty(ui, body, "ArrowUp", tr("Drop files or folders here"), tr("They show up on the phone page right away."));
        return;
    }
    let row_h = 8.0 + lh(11.0) + lh(11.0) + 8.0;
    let items = snap.outbox.clone();
    let mut remove = None;
    ui.scroll(id("phone.outbox"), body, items.len() as f32 * (row_h + 5.0), |ui, top| {
        for (i, o) in items.iter().enumerate() {
            let row = Rect::new(body.x, top + i as f32 * (row_h + 5.0), body.w, row_h);
            ui.fill(row, 10.0, t.surface2);
            let done = o.state == "Downloaded" || o.state == "Shown on phone";
            let icon = if done {
                "Check"
            } else {
                match o.kind {
                    OutKind::File => "File",
                    OutKind::Folder => "Folder",
                    OutKind::Text => "DocText",
                }
            };
            icon_tile(ui, row.x + 8.0 + 13.0, row.cy(), icon, true, None);
            let tx = Rect::new(row.x + 42.0, row.y + 8.0, row.w - 42.0 - 8.0 - 24.0, lh(11.0));
            ui.text(&o.name, tx, 11.0, SEMIBOLD, t.text, Align::Left);
            let sub = Rect::new(tx.x, tx.bottom(), tx.w, lh(11.0));
            let detail = format!("{} · ", tr(&o.detail()));
            let dw = ui.measure(&detail, 11.0, REGULAR).ceil();
            ui.text(&detail, sub, 11.0, REGULAR, t.subtext, Align::Left);
            if dw < sub.w {
                let (sc, sw) = if done { (t.accent, MEDIUM) } else { (t.subtext, REGULAR) };
                ui.text(tr(&o.state), Rect::new(sub.x + dw, sub.y, sub.w - dw, sub.h), 10.5, sw, sc, Align::Left);
            }
            if ui.icon_button(Rect::new(row.right() - 8.0 - 20.0, row.cy() - 10.0, 20.0, 20.0), "XMark", 10.0, t.subtext) {
                remove = Some(o.id.clone());
            }
        }
    });
    if let Some(id) = remove {
        phone::remove_outbox(&id);
    }
}

fn activity_panel(ui: &mut Ui, s: &State, snap: &Snapshot, r: Rect) {
    let t = ui.t;
    let (head, body) = r.take_top(22.0, 8.0);
    caps(ui, tr("ACTIVITY"), snap.transfers.len(), head);
    if snap.transfers.iter().any(|t| !t.active)
        && ui.icon_button(Rect::new(head.right() - 22.0, head.y, 22.0, 22.0), "Trash", 12.0, t.subtext)
    {
        phone::clear_transfers();
    }
    if snap.transfers.is_empty() {
        empty(ui, body, "ArrowsUpDown", tr("Nothing yet"), &crate::i18n::tf!("Files from the phone land on the Shelf and in {}.", "File dari ponsel masuk ke Rak dan ke {}.", s.folder_short));
        return;
    }
    let base_h = 8.0 + lh(11.0) + lh(11.0) + 8.0;
    let items = snap.transfers.clone();
    let heights: Vec<f32> = items.iter().map(|tr| base_h + if tr.active { 8.0 } else { 0.0 }).collect();
    let content: f32 = heights.iter().map(|h| h + 5.0).sum();
    let mut cancel = None;
    let mut delete = None;
    let mut open = None;
    ui.scroll(id("phone.activity"), body, content, |ui, top| {
        let mut y = top;
        for (tr, h) in items.iter().zip(&heights) {
            let row = Rect::new(body.x, y, body.w, *h);
            y += h + 5.0;
            let clickable = !tr.active && (tr.text.is_some() || tr.saved_path.is_some());
            let hov = clickable && ui.hovered(row);
            ui.fill(row, 10.0, if hov { mix(t.surface2, t.accent, 0.10) } else { t.surface2 });
            let dir = if tr.incoming { "ArrowDown" } else { "ArrowUp" };
            let c = if tr.failed { Some(t.danger) } else { None };
            icon_tile(ui, row.x + 8.0 + 13.0, row.y + 8.0 + 13.0, dir, tr.incoming || tr.active, c);
            let right_pad = 8.0 + 24.0;
            let tx = Rect::new(row.x + 42.0, row.y + 8.0, row.w - 42.0 - right_pad, lh(11.0));
            ui.text(&tr.title, tx, 11.0, SEMIBOLD, t.text, Align::Left);
            let dir = if tr.incoming { crate::i18n::tf!("From {}", "Dari {}", tr.peer) } else { crate::i18n::tf!("To {}", "Ke {}", tr.peer) };
            let sub = if tr.state.is_empty() { dir } else { format!("{dir} · {}", crate::i18n::tr(&tr.state)) };
            ui.text(&sub, Rect::new(tx.x, tx.bottom(), tx.w, lh(11.0)), 11.0, REGULAR, if tr.failed { t.danger } else { t.subtext }, Align::Left);
            if tr.active {
                let track = Rect::new(tx.x, tx.bottom() + lh(11.0) + 4.0, tx.w, 4.0);
                ui.fill(track, 2.0, t.accent_soft);
                let f = tr.progress().clamp(0.0, 1.0);
                if f > 0.0 {
                    ui.fill(Rect::new(track.x, track.y, (track.w * f).max(4.0), 4.0), 2.0, t.accent);
                }
                if ui.icon_button(Rect::new(row.right() - 8.0 - 20.0, row.y + 8.0, 20.0, 20.0), "XMark", 10.0, t.subtext) {
                    cancel = Some(tr.id);
                }
            } else if ui.icon_button(Rect::new(row.right() - 8.0 - 20.0, row.cy() - 10.0, 20.0, 20.0), "XMark", 10.0, t.subtext) {
                delete = Some(tr.id);
            } else if clickable && ui.clicked(row) {
                open = Some(tr.clone());
            }
        }
    });
    if let Some(id) = cancel {
        phone::cancel(id);
    }
    if let Some(id) = delete {
        phone::remove_transfer(id);
    }
    if let Some(tr) = open {
        if let Some(text) = &tr.text {
            crate::services::cliphistory::copy_quiet(text);
            phone::set_transfer_state(tr.id, "Copied");
        } else if let Some(p) = tr.saved_path.clone() {
            bg::spawn(move || {
                if std::path::Path::new(&p).exists() {
                    crate::shell::reveal(&p);
                } else if let Some(dir) = std::path::Path::new(&p).parent().filter(|d| d.exists()) {
                    crate::shell::open(&dir.to_string_lossy());
                }
            });
        }
    }
}

thread_local! {
    static WRAPS: std::cell::RefCell<std::collections::HashMap<(String, u32, u32), Vec<String>>> = Default::default();
}

/// Greedy word wrap with the real font metrics (cached: the same few strings every frame).
fn wrap(ui: &Ui, s: &str, size: f32, weight: u16, w: f32) -> Vec<String> {
    let key = (s.to_string(), (size * 10.0) as u32 ^ ((weight as u32) << 16), w.round() as u32);
    if let Some(v) = WRAPS.with(|c| c.borrow().get(&key).cloned()) {
        return v;
    }
    let mut out = Vec::new();
    let mut line = String::new();
    for word in s.split(' ') {
        let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if !line.is_empty() && ui.measure(&candidate, size, weight) > w {
            out.push(std::mem::take(&mut line));
            line = word.to_string();
        } else {
            line = candidate;
        }
    }
    out.push(line);
    WRAPS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 200 {
            c.clear();
        }
        c.insert(key, out.clone());
    });
    out
}

/// A dashed rounded-rectangle outline (WPF StrokeDashArray, in DIPs).
fn dashed_round(ui: &mut Ui, r: Rect, radius: f32, c: crate::gfx::Color, width: f32, dash: f32, gap: f32) {
    let mut pts: Vec<(f32, f32)> = Vec::new();
    let corners = [
        (r.right() - radius, r.y + radius, -90.0f32),
        (r.right() - radius, r.bottom() - radius, 0.0),
        (r.x + radius, r.bottom() - radius, 90.0),
        (r.x + radius, r.y + radius, 180.0),
    ];
    for (cx, cy, a0) in corners {
        for k in 0..=8 {
            let a = (a0 + k as f32 * 90.0 / 8.0).to_radians();
            pts.push((cx + radius * a.cos(), cy + radius * a.sin()));
        }
    }
    pts.push(pts[0]);
    let geo = ui.g.path(|sink| unsafe {
        use windows::Win32::Graphics::Direct2D::Common::{D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN};
        let mut on = true;
        let mut left = dash;
        let mut cur = pts[0];
        sink.BeginFigure(crate::gfx::pt(cur.0, cur.1), D2D1_FIGURE_BEGIN_HOLLOW);
        for &next in &pts[1..] {
            let mut seg = ((next.0 - cur.0).powi(2) + (next.1 - cur.1).powi(2)).sqrt();
            while seg > 0.0 {
                let step = left.min(seg);
                let f = step / seg;
                let p = (cur.0 + (next.0 - cur.0) * f, cur.1 + (next.1 - cur.1) * f);
                if on {
                    sink.AddLine(crate::gfx::pt(p.0, p.1));
                }
                cur = p;
                seg -= step;
                left -= step;
                if left <= 0.0 {
                    if on {
                        sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    } else {
                        sink.BeginFigure(crate::gfx::pt(cur.0, cur.1), D2D1_FIGURE_BEGIN_HOLLOW);
                    }
                    on = !on;
                    left = if on { dash } else { gap };
                }
            }
        }
        if on {
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
        }
    });
    if let Some(geo) = geo {
        ui.g.stroke_geometry(&geo.into(), ui.c(c), width);
    }
}
