//! System: a "Performance | Apps" switch like the .NET view.
//! Performance: six mini cards (CPU with a 60 s graph, memory, system disk, network down/up, battery,
//! uptime), sampled every second on a background thread while on screen.
//! Apps: running programs grouped by name, sorted by CPU / memory / name, with End task (first click arms
//! it as "Sure?", the second ends the group), sampled every 2 s; the arrow opens Windows Task Manager.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::bg;
use crate::gfx::{color, mix, Align, BOLD, REGULAR, SEMIBOLD};
use crate::services::appicons;
use crate::services::system::{self, Proc, Stats};
use crate::ui::{id, ButtonStyle, Rect, Ui};
use crate::i18n::tr;

const HISTORY: usize = 60;
const MAX_ROWS: usize = 40;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    Performance,
    Apps,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Sort {
    #[default]
    Cpu,
    Memory,
    Name,
}

#[derive(Default)]
struct Data {
    stats: Option<Stats>,
    /// Samples taken so far (the first reports 0 for CPU / network).
    samples: u32,
    cpu_history: VecDeque<f32>,
    procs: Vec<Proc>,
    proc_samples: u32,
    busy: bool,
    message: Option<(String, Instant)>,
}

#[derive(Default)]
pub struct State {
    data: Arc<Mutex<Data>>,
    mode: Mode,
    sort: Sort,
    last: Option<Instant>,
    /// Program (name) whose End task was clicked once.
    confirming: Option<String>,
}

pub fn shown(s: &mut State) {
    // Dev aid for screenshots: FATHNOTCH_DEMO_SUB=apps opens on the Apps page.
    if std::env::var("FATHNOTCH_DEMO_SUB").is_ok_and(|v| v == "apps") {
        s.mode = Mode::Apps;
    }
    s.confirming = None;
    {
        let mut d = s.data.lock().unwrap();
        d.proc_samples = 0;
        d.message = None;
    }
    tick(s);
}

fn tick(s: &mut State) {
    s.last = Some(Instant::now());
    let data = s.data.clone();
    {
        let mut d = data.lock().unwrap();
        if d.busy {
            return;
        }
        d.busy = true;
    }
    let apps = s.mode == Mode::Apps;
    bg::spawn(move || {
        let stats = system::sample();
        let procs = if apps { Some(system::processes()) } else { None };
        let mut d = data.lock().unwrap();
        d.busy = false;
        if d.samples > 0 {
            d.cpu_history.push_back(stats.cpu);
            while d.cpu_history.len() > HISTORY {
                d.cpu_history.pop_front();
            }
        }
        d.samples += 1;
        d.stats = Some(stats);
        if let Some(p) = procs {
            d.procs = p;
            d.proc_samples += 1;
        }
    });
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, _go: &mut Option<&'static str>) {
    let every = if s.mode == Mode::Apps { 2000 } else { 1000 };
    if s.last.map_or(true, |t| t.elapsed().as_millis() >= every) {
        tick(s);
    }
    let (top, body) = r.take_top(32.0, 8.0);
    switch(ui, s, top);
    match s.mode {
        Mode::Performance => performance(ui, s, body),
        Mode::Apps => apps(ui, s, body),
    }
}

fn switch(ui: &mut Ui, s: &mut State, r: Rect) {
    let t = ui.t;
    let labels = [tr("Performance"), tr("Apps")];
    let w = 96.0;
    let bg = Rect::new(r.cx() - w - 3.0, r.y, w * 2.0 + 6.0, r.h);
    ui.fill(bg, 9.0, t.surface);
    for (i, label) in labels.iter().enumerate() {
        let c = Rect::new(bg.x + 3.0 + i as f32 * w, bg.y + 3.0, w, bg.h - 6.0);
        let mode = if i == 0 { Mode::Performance } else { Mode::Apps };
        let on = s.mode == mode;
        if on {
            ui.fill(c, 7.0, t.surface2);
        }
        let hov = ui.hovered(c);
        ui.text(label, c, 12.0, SEMIBOLD, if on || hov { t.text } else { t.subtext }, Align::Center);
        if ui.clicked(c) && !on {
            s.mode = mode;
            s.confirming = None;
            s.data.lock().unwrap().proc_samples = 0;
            tick(s);
        }
    }
}

// ------------------------------------------------------------------ performance

fn gb(b: u64) -> String {
    let v = b as f64 / (1u64 << 30) as f64;
    let s = format!("{v:.1}");
    format!("{} GB", s.trim_end_matches(".0"))
}

fn mini_card(ui: &mut Ui, r: Rect, icon: &'static str, label: &str) -> Rect {
    let t = ui.t;
    ui.card(r, 16.0);
    let inner = r.pad(10.0, 8.0, 10.0, 8.0);
    ui.icon(icon, inner.x + 6.0, inner.y + 8.0, 12.0, t.subtext);
    caps(ui, label, Rect::new(inner.x + 17.0, inner.y, inner.w - 17.0, 16.0), 11.5, Align::Left);
    inner
}

fn big(ui: &mut Ui, inner: Rect, value: &str) {
    ui.text(value, Rect::new(inner.x, inner.y + 20.0, inner.w, 27.0), 17.0, SEMIBOLD, ui.t.text, Align::Left);
}

fn sub(ui: &mut Ui, inner: Rect, value: &str) {
    ui.text(value, Rect::new(inner.x, inner.y + 48.0, inner.w, 15.0), 11.0, REGULAR, ui.t.subtext, Align::Left);
}

fn bar(ui: &mut Ui, inner: Rect, frac: f32, c: crate::gfx::Color) {
    // Like the WPF ProgressBar: a 3 px track with radius-2 ends.
    let track = Rect::new(inner.x, inner.y + 71.0, inner.w, 3.0);
    ui.fill(track, 1.5, ui.t.track);
    let w = track.w * frac.clamp(0.0, 1.0);
    if w > 0.0 {
        ui.fill(Rect::new(track.x, track.y, w, track.h), 1.5, c);
    }
}

fn performance(ui: &mut Ui, s: &mut State, r: Rect) {
    let t = ui.t;
    let d = s.data.lock().unwrap();
    let st = d.stats.clone();
    let ready = d.samples > 1;
    let history: Vec<f32> = d.cpu_history.iter().copied().collect();
    drop(d);
    let rows = r.rows(2, 6.0);
    let top = rows[0].columns(3, 6.0);
    let bottom = rows[1].columns(3, 6.0);
    let dash = "–".to_string();

    // CPU, with the last minute as a line along the bottom.
    let inner = mini_card(ui, top[0], "Cpu", tr("CPU"));
    big(ui, inner, &st.as_ref().filter(|_| ready).map_or(dash.clone(), |x| format!("{:.0}%", x.cpu)));
    static THREADS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let threads = *THREADS.get_or_init(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0));
    sub(ui, inner, &crate::i18n::tf!("{threads} threads", "{threads} thread"));
    if history.len() > 1 {
        let g = Rect::new(inner.x, inner.bottom() - 26.0, inner.w, 26.0);
        let pts: Vec<(f32, f32)> = history
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let k = HISTORY - history.len() + i;
                (g.x + g.w * k as f32 / (HISTORY - 1) as f32, g.bottom() - g.h * (v / 100.0).clamp(0.0, 1.0))
            })
            .collect();
        let c = ui.c(mix(t.accent, t.surface, 0.1));
        for w in pts.windows(2) {
            ui.g.line(w[0].0, w[0].1, w[1].0, w[1].1, c, 1.4);
        }
    }

    // Memory.
    let inner = mini_card(ui, top[1], "Memory", tr("Memory"));
    match &st {
        Some(x) if x.ram_total > 0 => {
            let f = x.ram_used as f32 / x.ram_total as f32;
            big(ui, inner, &format!("{:.0}%", f * 100.0));
            sub(ui, inner, &format!("{} / {}", gb(x.ram_used), gb(x.ram_total)));
            bar(ui, inner, f, t.accent);
        }
        _ => big(ui, inner, &dash),
    }

    // System disk.
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let inner = mini_card(ui, top[2], "Drive", &crate::i18n::tf!("Disk {drive}", "Disk {drive}"));
    match st.as_ref().and_then(|x| x.disk) {
        Some((used, total)) if total > 0 => {
            let f = used as f32 / total as f32;
            big(ui, inner, &format!("{:.0}%", f * 100.0));
            sub(ui, inner, &crate::i18n::tf!("{} free", "{} kosong", gb(total - used)));
            bar(ui, inner, f, t.accent);
        }
        _ => big(ui, inner, &dash),
    }

    // Network.
    let inner = mini_card(ui, bottom[0], "Wifi", tr("Network"));
    let (down, up) = match &st {
        Some(x) if ready => (system::rate(x.net_down), system::rate(x.net_up)),
        _ => (dash.clone(), dash.clone()),
    };
    ui.icon("ArrowDown", inner.x + 6.0, inner.y + 32.0, 13.0, color(0x3DD68C, 1.0));
    ui.text(&down, Rect::new(inner.x + 18.0, inner.y + 22.0, inner.w - 18.0, 20.0), 14.0, SEMIBOLD, t.text, Align::Left);
    ui.icon("ArrowUp", inner.x + 6.0, inner.y + 53.0, 13.0, t.accent);
    ui.text(&up, Rect::new(inner.x + 18.0, inner.y + 43.0, inner.w - 18.0, 20.0), 14.0, SEMIBOLD, t.text, Align::Left);

    // Battery.
    let battery = st.as_ref().and_then(|x| x.battery);
    let charging = battery.is_some_and(|(_, plugged)| plugged);
    let inner = mini_card(ui, bottom[1], if battery.is_none() || charging { "Bolt" } else { "BatteryFull" }, tr("Battery"));
    match battery {
        None if st.is_some() => {
            big(ui, inner, tr("AC"));
            sub(ui, inner, tr("No battery"));
            bar(ui, inner, 1.0, t.accent);
        }
        None => big(ui, inner, &dash),
        Some((pct, plugged)) => {
            big(ui, inner, &format!("{pct}%"));
            let text = if plugged {
                if pct >= 99 { tr("Fully charged").to_string() } else { tr("Charging").to_string() }
            } else {
                match st.as_ref().and_then(|x| x.battery_secs) {
                    Some(secs) if secs > 0 => crate::i18n::tf!("{}:{:02} remaining", "{}:{:02} tersisa", secs / 3600, secs / 60 % 60),
                    _ => tr("On battery").to_string(),
                }
            };
            sub(ui, inner, &text);
            bar(ui, inner, pct as f32 / 100.0, if pct < 20 && !plugged { t.danger } else { t.accent });
        }
    }

    // Uptime.
    let inner = mini_card(ui, bottom[2], "Clock", tr("Uptime"));
    let ms = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
    let (days, hours, mins) = (ms / 86_400_000, ms / 3_600_000 % 24, ms / 60_000 % 60);
    big(ui, inner, &if days >= 1 { crate::i18n::tf!("{days}d {hours}h", "{days}h {hours}j") } else { crate::i18n::tf!("{hours}h {mins}m", "{hours}j {mins}m") });
    sub(ui, inner, &std::env::var("COMPUTERNAME").unwrap_or_default());
}

// ------------------------------------------------------------------ apps

fn mem_text(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.1} GB", b as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.0} MB", b as f64 / (1u64 << 20) as f64)
    }
}

fn apps(ui: &mut Ui, s: &mut State, r: Rect) {
    let t = ui.t;
    let (procs, first, mem_pct, message) = {
        let d = s.data.lock().unwrap();
        let mem = d.stats.as_ref().filter(|x| x.ram_total > 0).map(|x| x.ram_used as f64 * 100.0 / x.ram_total as f64);
        (d.procs.clone(), d.proc_samples < 2, mem, d.message.as_ref().filter(|(_, at)| at.elapsed().as_secs() < 5).map(|(m, _)| m.clone()))
    };
    // Inside the C# view's 18 px side margins.
    let r = r.pad(18.0, 0.0, 18.0, 4.0);
    let (head, rest) = r.take_top(30.0, 0.0);

    // Totals: "CPU 3%  ·  Memory 51%   58 apps".
    let cpu_total: f32 = procs.iter().map(|p| p.cpu).sum::<f32>().min(100.0);
    let cpu = if first { tr("CPU …").to_string() } else { format!("CPU {cpu_total:.0}%") };
    let mem = mem_pct.map_or(tr("Memory").to_string(), |m| crate::i18n::tf!("Memory {m:.0}%", "Memori {m:.0}%"));
    let mut x = head.x;
    let w = ui.measure(&cpu, 12.0, SEMIBOLD);
    ui.text(&cpu, Rect::new(x, head.y, w + 4.0, head.h), 12.0, SEMIBOLD, t.text, Align::Left);
    x += w;
    let sep = "  ·  ";
    let w = ui.measure(sep, 12.0, SEMIBOLD);
    ui.text(sep, Rect::new(x, head.y, w + 2.0, head.h), 12.0, SEMIBOLD, t.subtext, Align::Left);
    x += w;
    let w = ui.measure(&mem, 12.0, SEMIBOLD);
    ui.text(&mem, Rect::new(x, head.y, w + 4.0, head.h), 12.0, SEMIBOLD, t.text, Align::Left);
    x += w + 10.0;

    // Sort chips and the Task Manager arrow.
    let arrow = Rect::new(head.right() - 26.0, head.cy() - 13.0, 26.0, 26.0);
    if ui.icon_button(arrow, "ArrowRight", 13.0, t.subtext) {
        crate::shell::open("taskmgr.exe");
    }
    let mut cx = arrow.x - 4.0;
    for (sort, label) in [(Sort::Name, tr("Name")), (Sort::Memory, "Memory"), (Sort::Cpu, tr("CPU"))] {
        let cw = ui.measure(label, 11.0, SEMIBOLD) + 20.0;
        cx -= cw + 4.0;
        let c = Rect::new(cx, head.cy() - 13.0, cw, 26.0);
        let on = s.sort == sort;
        let hov = ui.hovered(c);
        if on {
            ui.fill(c, 8.0, t.accent_soft);
        } else if hov {
            ui.fill(c, 8.0, t.surface2);
        }
        ui.text(label, c, 11.0, SEMIBOLD, if on { t.text } else { t.subtext }, Align::Center);
        if ui.clicked(c) {
            s.sort = sort;
        }
    }
    let count = message.unwrap_or_else(|| crate::i18n::tf!("{} apps", "{} aplikasi", procs.len()));
    ui.text(&count, Rect::new(x, head.y, (cx - 8.0 - x).max(0.0), head.h), 11.0, REGULAR, t.subtext, Align::Left);

    // Column captions (margin 10,6,14,4).
    let (caps_r, list) = rest.take_top(6.0 + 14.0 + 4.0, 0.0);
    let caps_r = Rect::new(caps_r.x + 10.0, caps_r.y + 6.0, caps_r.w - 24.0, 14.0);
    let cols = Columns::new(caps_r);
    caps(ui, tr("App"), Rect::new(caps_r.x + 30.0, caps_r.y, 100.0, caps_r.h), 10.0, Align::Left);
    caps(ui, tr("CPU"), cols.cpu, 10.0, Align::Right);
    caps(ui, "Memory", cols.mem, 10.0, Align::Right);

    let mut sorted = procs;
    match s.sort {
        Sort::Cpu => sorted.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.mem.cmp(&a.mem))),
        Sort::Memory => sorted.sort_by(|a, b| b.mem.cmp(&a.mem)),
        Sort::Name => sorted.sort_by_key(|p| p.title.to_lowercase()),
    }
    sorted.truncate(MAX_ROWS);
    if sorted.is_empty() {
        ui.text(tr("Reading running apps…"), list, 12.0, REGULAR, t.subtext, Align::Center);
        return;
    }

    let row_h = 32.0;
    let mut end: Option<Proc> = None;
    let mut arm: Option<Option<String>> = None;
    let confirming = s.confirming.clone();
    ui.scroll(id("sys.apps"), list, row_h * sorted.len() as f32, |ui, top| {
        let t = ui.t;
        for (i, p) in sorted.iter().enumerate() {
            let row = Rect::new(list.x, top + i as f32 * row_h, list.w - 8.0, row_h);
            if row.bottom() < list.y || row.y > list.bottom() {
                continue;
            }
            let hov = ui.hovered(row);
            if hov {
                ui.fill(row, 8.0, t.surface);
            }
            let cols = Columns::new(row.pad(10.0, 0.0, 4.0, 0.0));
            // The exe's icon; a CPU glyph until it loads (or when there's none).
            let ic = Rect::new(cols.icon.x, row.cy() - 8.0, 16.0, 16.0);
            let px = (16.0 * ui.g.scale).round() as u32;
            let icon = if p.path.is_empty() { appicons::pid(p.pid, px) } else { appicons::exe(&p.path, px) };
            let drawn = icon.is_some_and(|(k, png)| {
                ui.g.load_image(k, &png) && {
                    ui.g.image(k, ic.d2d(), 0.0, ui.alpha);
                    true
                }
            });
            if !drawn {
                ui.icon("Cpu", ic.cx(), ic.cy(), 14.0, t.subtext);
            }
            let name = if p.pids.len() > 1 { format!("{}  ({})", p.title, p.pids.len()) } else { p.title.clone() };
            ui.text(&name, cols.name, 12.0, REGULAR, t.text, Align::Left);
            let cpu_c = if p.cpu >= 25.0 { t.danger } else if p.cpu >= 5.0 { t.text } else { t.subtext };
            ui.text(&if first { "…".to_string() } else { format!("{:.1}%", p.cpu) }, cols.cpu, 12.0, SEMIBOLD, cpu_c, Align::Right);
            ui.text(&mem_text(p.mem), cols.mem, 12.0, SEMIBOLD, if p.mem >= 1 << 30 { t.text } else { t.subtext }, Align::Right);
            let armed = confirming.as_deref() == Some(p.name.as_str());
            if hov || armed {
                let b = Rect::new(cols.end.right() - 70.0, row.cy() - 12.0, 70.0, 24.0);
                let clicked = if armed {
                    let hb = ui.hovered(b);
                    ui.fill(b, 8.0, if hb { mix(t.danger, t.text, 0.12) } else { t.danger });
                    ui.text(tr("Sure?"), b, 11.0, SEMIBOLD, color(0xFFFFFF, 1.0), Align::Center);
                    ui.clicked(b)
                } else {
                    ui.button(b, tr("End task"), ButtonStyle::Normal)
                };
                if clicked {
                    if armed {
                        end = Some(p.clone());
                    } else {
                        arm = Some(Some(p.name.clone()));
                    }
                }
            }
        }
    });
    if let Some(a) = arm {
        s.confirming = a;
    }
    if let Some(p) = end {
        s.confirming = None;
        let data = s.data.clone();
        bg::spawn(move || {
            let failed = system::end_all(&p.pids);
            let mut d = data.lock().unwrap();
            d.message = (failed > 0).then(|| (crate::i18n::tf!("Couldn't end {} (it may need administrator rights)", "Tidak bisa menutup {} (mungkin perlu hak administrator)", p.title), Instant::now()));
            d.procs.retain(|x| x.name != p.name || failed > 0);
        });
    }
}

/// Column layout shared by the caption row and the list rows (like the .NET grid: 26 | * | 64 | 84 | 86).
struct Columns {
    icon: Rect,
    name: Rect,
    cpu: Rect,
    mem: Rect,
    end: Rect,
}

impl Columns {
    fn new(r: Rect) -> Self {
        let (end, rest) = r.take_right(86.0, 0.0);
        let (mem, rest) = rest.take_right(84.0, 0.0);
        let (cpu, rest) = rest.take_right(64.0, 0.0);
        let (icon, name) = rest.take_left(26.0, 0.0);
        Self { icon, name: name.pad(0.0, 0.0, 8.0, 0.0), cpu, mem, end }
    }
}

/// Small uppercase caption in the C# "Caps" style (bold, subtext).
fn caps(ui: &mut Ui, s: &str, r: Rect, size: f32, align: Align) {
    ui.text(&s.to_uppercase(), r, size, BOLD, ui.t.subtext, align);
}
