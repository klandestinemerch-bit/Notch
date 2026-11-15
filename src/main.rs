#![windows_subsystem = "windows"]
//! FathNotch: a light dynamic island for Windows.
//!
//! One layered, top-most window holds the notch. While nothing moves the thread sleeps in GetMessage
//! (0 % CPU); while something moves it draws one frame per display refresh (DwmFlush) and goes back to
//! sleep when everything has settled.

mod bg;
mod clipboard;
mod dragdrop;
mod gfx;
mod http;
mod model;
mod notch;
mod services;
mod shape;
mod shell;
mod spring;
mod theme;
mod tools;
mod ui;
mod views;

use std::cell::RefCell;
use std::time::{Duration, Instant};

use windows::core::{w, Result};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Input::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use notch::{Alert, AlertKind, Notch, CANVAS_H, CANVAS_W};
use services::settings;
use ui::Key;

const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_CLOCK: usize = 1;
const TIMER_LEAVE: usize = 2;
const TIMER_ALERT: usize = 3;
const TIMER_HOVER: usize = 4;
const TIMER_IDLE: usize = 5;
const TIMER_TICK: usize = 6;
const TIMER_EYES: usize = 7;
const TIMER_EDGE: usize = 8;
const TIMER_DODGE: usize = 9;

thread_local! {
    static HIDDEN: RefCell<bool> = const { RefCell::new(false) };
}

thread_local! {
    static HOTKEYS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// "Ctrl+Alt+N" → (modifiers, virtual key).
fn parse_hotkey(s: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    let mut mods = HOT_KEY_MODIFIERS(0);
    let mut vk = None;
    for part in s.split('+').map(|p| p.trim().to_ascii_lowercase()) {
        match part.as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "win" => mods |= MOD_WIN,
            "space" => vk = Some(0x20),
            "enter" => vk = Some(0x0D),
            p if p.len() == 1 => vk = Some(p.to_ascii_uppercase().as_bytes()[0] as u32),
            p if p.starts_with('f') && p[1..].parse::<u32>().is_ok() => vk = Some(0x6F + p[1..].parse::<u32>().unwrap()),
            _ => return None,
        }
    }
    Some((mods, vk?))
}

/// (Re)registers Settings → Keyboard hotkeys.
fn register_hotkeys(hwnd: HWND) {
    HOTKEYS.with(|h| {
        let mut h = h.borrow_mut();
        for i in 0..h.len() {
            unsafe {
                let _ = UnregisterHotKey(hwnd, i as i32 + 1);
            }
        }
        h.clear();
        for (action, keys) in settings::get().hotkeys {
            if let Some((mods, vk)) = parse_hotkey(&keys) {
                h.push(action);
                unsafe {
                    let _ = RegisterHotKey(hwnd, h.len() as i32, mods | MOD_NOREPEAT, vk);
                }
            }
        }
    });
}

fn on_hotkey(id: usize) {
    let Some(action) = HOTKEYS.with(|h| h.borrow().get(id.wrapping_sub(1)).cloned()) else { return };
    with(|n| {
        if action == "toggle" {
            if n.expanded() {
                n.pinned = false;
                n.collapse();
            } else {
                n.expand();
                n.pinned = true;
            }
        } else {
            n.expand();
            n.pinned = true;
            n.show_tool(tools::find(&action).id);
        }
    });
}

thread_local! {
    /// The pointer touched the top edge: a concealed notch shows until the pointer moves away.
    static REVEALED: RefCell<bool> = const { RefCell::new(false) };
    static WANT_CONCEAL: RefCell<bool> = const { RefCell::new(false) };
    static LAST_TRACK: RefCell<String> = const { RefCell::new(String::new()) };
    static MONITOR: RefCell<isize> = const { RefCell::new(0) };
}

/// The foreground app's exe name ("chrome.exe"), lower case.
fn foreground_exe() -> Option<String> {
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let fg = GetForegroundWindow();
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid == 0 || pid == std::process::id() {
            return None;
        }
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().map(|s| s.to_lowercase())
    }
}

/// Is the foreground app on Settings → General "Hide for these apps"?
fn listed_app_in_front() -> bool {
    let list = settings::get().hide_for_apps;
    if list.is_empty() {
        return false;
    }
    let Some(exe) = foreground_exe() else { return false };
    let stem = exe.trim_end_matches(".exe");
    list.iter().any(|a| {
        let a = a.trim().to_lowercase();
        let a = a.trim_end_matches(".exe");
        !a.is_empty() && a == stem
    })
}

/// Works out whether the closed notch should be concealed (Settings → "Show notch", hidden apps).
fn update_visibility(hwnd: HWND) {
    let s = settings::get();
    let listed = listed_app_in_front();
    let fade_for_app = listed && s.hide_for_apps_mode != "Hidden";
    let want = fade_for_app
        || match s.notch_visibility.as_str() {
            "WhenActive" => model::activity() == model::Activity::Idle,
            "HoverOnly" => true,
            _ => false,
        };
    WANT_CONCEAL.with(|w| *w.borrow_mut() = want);
    apply_conceal(hwnd);
    unsafe {
        if want {
            SetTimer(hwnd, TIMER_EDGE, 100, None);
        } else {
            let _ = KillTimer(hwnd, TIMER_EDGE);
        }
    }
    check_fullscreen(hwnd, listed && s.hide_for_apps_mode == "Hidden");
}

fn apply_conceal(hwnd: HWND) {
    let want = WANT_CONCEAL.with(|w| *w.borrow());
    let revealed = REVEALED.with(|r| *r.borrow());
    let changed = with(|n| {
        let c = want && !revealed;
        let changed = n.concealed != c;
        n.concealed = c;
        changed
    })
    .unwrap_or(false);
    if changed {
        redraw(hwnd);
    }
}

/// While concealed: touching the top edge above the notch reveals it; moving away hides it again.
fn poll_edge(hwnd: HWND) {
    let Some((near_edge, near_notch)) = with(|n| {
        let (x, y) = cursor_dip(n)?;
        let b = n.bounds();
        let over_x = x >= b.x - 40.0 && x <= b.right() + 40.0;
        Some((over_x && y <= 3.0, over_x && y <= b.bottom() + 24.0))
    })
    .flatten() else {
        return;
    };
    let was = REVEALED.with(|r| *r.borrow());
    let now = if was { near_notch } else { near_edge };
    if now != was {
        REVEALED.with(|r| *r.borrow_mut() = now);
        apply_conceal(hwnd);
    }
}

/// The monitor chosen in Settings → Display "Show notch on".
fn target_monitor() -> HMONITOR {
    unsafe {
        let idx = settings::get().monitor_index;
        if idx == -2 {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            return MonitorFromPoint(p, MONITOR_DEFAULTTOPRIMARY);
        }
        if idx >= 0 {
            unsafe extern "system" fn collect(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
                (*(data.0 as *mut Vec<HMONITOR>)).push(m);
                TRUE
            }
            let mut list: Vec<HMONITOR> = Vec::new();
            let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut list as *mut _ as isize));
            if let Some(m) = list.get(idx as usize) {
                return *m;
            }
        }
        MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY)
    }
}

/// Puts the window at the top centre of the chosen monitor (rebuilding the canvas for its DPI).
fn place(hwnd: HWND) {
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    unsafe {
        let mon = target_monitor();
        let prev = MONITOR.with(|m| std::mem::replace(&mut *m.borrow_mut(), mon.0 as isize));
        if prev == mon.0 as isize {
            return;
        }
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut info);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let scale = dx as f32 / 96.0;
        with(|n| {
            if (n.gfx.scale - scale).abs() > 0.01 {
                if let Ok(g) = gfx::Gfx::new(CANVAS_W, CANVAS_H, scale) {
                    n.gfx = g;
                }
            }
            let m = info.rcMonitor;
            n.win_x = m.left + ((m.right - m.left) - n.gfx.px_w) / 2;
            n.win_y = m.top;
        });
        redraw(hwnd);
    }
}

/// Settings → Music "Announce new songs": a short alert when the track changes.
fn track_peek() {
    let Some(np) = model::now_playing() else { return };
    let key = format!("{}\u{1}{}", np.artist, np.title);
    let changed = LAST_TRACK.with(|l| {
        let mut l = l.borrow_mut();
        let first = l.is_empty();
        let c = *l != key;
        *l = key;
        c && !first
    });
    if changed && np.playing && settings::get().track_peek && !with(|n| n.expanded()).unwrap_or(true) {
        model::push_alert(Alert {
            key: "track".into(),
            kind: AlertKind::Compact { icon: "MusicNote", title: np.title.clone(), sub: np.artist.clone() },
            duration: Duration::from_millis(2600),
        });
    }
}

/// Settings → General "Hide in full screen": games, videos and presentations hide the notch.
fn check_fullscreen(hwnd: HWND, app_hidden: bool) {
    let busy = (app_hidden
        || settings::get().hide_in_fullscreen
            && unsafe { SHQueryUserNotificationState() }
                .map(|s| s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE)
                .unwrap_or(false))
        && !with(|n| n.expanded()).unwrap_or(false);
    let was = HIDDEN.with(|h| std::mem::replace(&mut *h.borrow_mut(), busy));
    if busy != was {
        unsafe {
            let _ = ShowWindow(hwnd, if busy { SW_HIDE } else { SW_SHOWNOACTIVATE });
        }
    }
}
const WM_TRAY: u32 = WM_APP + 1;
const MENU_SETTINGS: usize = 10;
const MENU_EXIT: usize = 11;
const MENU_STYLE: usize = 12;

thread_local! {
    static APP: RefCell<Option<Notch>> = const { RefCell::new(None) };
    static HOVER: RefCell<bool> = const { RefCell::new(false) };
    static CAPS: RefCell<(bool, bool)> = const { RefCell::new((false, false)) }; // (toggled on, key held)
    /// The window that had the keyboard before the panel took it.
    static PREV_FOREGROUND: RefCell<isize> = const { RefCell::new(0) };
}

/// The panel is open on the Music tool (drops there go into its queue).
fn music_open() -> bool {
    with(|n| n.expanded() && n.tool == "nowplaying").unwrap_or(false)
}

/// Runs `f` on the notch if it exists and isn't already borrowed (re-entrant messages are skipped).
fn with<R>(f: impl FnOnce(&mut Notch) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

/// Draws a frame now and applies what it asked for (keyboard focus).
fn redraw(hwnd: HWND) {
    // Dev: FATHNOTCH_STATS=1 logs how many frames were drawn every 30 s (idle cost check).
    {
        use std::sync::atomic::{AtomicU64, Ordering};
        static FRAMES: AtomicU64 = AtomicU64::new(0);
        static SINCE: std::sync::OnceLock<std::sync::Mutex<Instant>> = std::sync::OnceLock::new();
        if std::env::var_os("FATHNOTCH_STATS").is_some() {
            let n = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
            let mut since = SINCE.get_or_init(|| std::sync::Mutex::new(Instant::now())).lock().unwrap();
            if since.elapsed() >= Duration::from_secs(30) {
                let playing = model::now_playing().map(|p| (p.title.clone(), p.playing));
                log(&format!("frames in last 30 s: {n} · activity {:?} · media {playing:?}", model::activity()));
                FRAMES.store(0, Ordering::Relaxed);
                *since = Instant::now();
            }
        }
    }
    let keyboard = with(|n| {
        prepare(n);
        if let Err(e) = n.render(hwnd) {
            log(&format!("render: {e:?}"));
        }
        n.wants_keyboard
    })
    .unwrap_or(false);
    if keyboard {
        take_keyboard(hwnd);
    }
}

/// Appends a line to fathnotch.log in the data folder (errors only, kept small).
pub fn log(line: &str) {
    use std::io::Write;
    let path = services::store::path("fathnotch-rs.log");
    if std::fs::metadata(&path).map(|m| m.len() > 256 * 1024).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let n = notch::now();
        let _ = writeln!(f, "{:02}:{:02}:{:02} {line}", n.hour, n.minute, n.second);
    }
}

/// Things to fold in before a frame: queued alerts, album art, activity changes.
fn prepare(n: &mut Notch) {
    for a in model::take_alerts() {
        n.push_alert(a);
    }
    // Safety net: "held" while neither mouse button is actually down (a release we never heard of).
    // Screenshot runs hold the button by script, so they're left alone.
    if n.input.down && std::env::var_os("FATHNOTCH_SHOT").is_none() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
        let held = |k: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| unsafe { GetAsyncKeyState(k.0 as i32) } as u16 & 0x8000 != 0;
        if !held(VK_LBUTTON) && !held(VK_RBUTTON) {
            n.input.down = false;
            n.input.released = true;
        }
    }
    // A ringing alarm's pop-up stays until Snooze or Stop (and comes back if the panel was open).
    let ringing = services::alarms::ringing();
    let showing_alarm = n.alert_key().is_some_and(|k| k.starts_with("alarm:"));
    match ringing {
        None if showing_alarm => n.end_alert(),
        Some(a) if !showing_alarm && !n.expanded() && !n.has_alert() => n.push_alert(model::alarm_alert(&a)),
        _ => {}
    }
    if let Some(np) = model::now_playing() {
        if let Some(art) = &np.art {
            n.gfx.load_image(np.art_id, art);
        }
    }
    if model::activity_changed() {
        n.activity_changed();
    } else {
        n.refit_idle();
    }
}

/// Lets the window take typing (it is normally never activated, so it doesn't steal focus).
fn take_keyboard(hwnd: HWND) {
    if std::env::var_os("FATHNOTCH_SHOT").is_some() {
        return; // off-screen runs get their typing from the script
    }
    unsafe {
        // Remember the window the user was in, to hand the keyboard back when the panel closes.
        let fg = GetForegroundWindow();
        if fg != hwnd && !fg.is_invalid() {
            PREV_FOREGROUND.with(|p| *p.borrow_mut() = fg.0 as isize);
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE);
        SetWindowLongW(hwnd, GWL_EXSTYLE, ex & !(WS_EX_NOACTIVATE.0 as i32));
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
    }
}

fn release_keyboard(hwnd: HWND) {
    unsafe {
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE);
        SetWindowLongW(hwnd, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as i32);
        // Still in front (closed by the pointer leaving or Esc): give the keyboard back.
        if GetForegroundWindow() == hwnd {
            let prev = PREV_FOREGROUND.with(|p| std::mem::take(&mut *p.borrow_mut()));
            if prev != 0 {
                let _ = SetForegroundWindow(HWND(prev as _));
            }
        }
    }
}

/// Opening the panel by clicking in it or with a hotkey lets it take keys (tab switching with ←/→ and 1…9).
fn panel_keyboard(hwnd: HWND) {
    if with(|n| n.expanded()).unwrap_or(false) {
        take_keyboard(hwnd);
    } else {
        release_keyboard(hwnd);
    }
}

/// Re-arms the clock so the minute changes exactly on time.
fn arm_clock(hwnd: HWND) {
    let t = notch::now();
    let ms = (60 - t.second as u32) * 1000 + 30;
    unsafe { SetTimer(hwnd, TIMER_CLOCK, ms, None) };
}

fn cursor_dip(n: &Notch) -> Option<(f32, f32)> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok()? };
    let s = n.gfx.scale;
    Some(((p.x - n.win_x - n.shove_px()) as f32 / s, (p.y - n.win_y) as f32 / s))
}

fn cursor_in_notch() -> bool {
    with(|n| {
        let Some((x, y)) = cursor_dip(n) else { return false };
        let b = n.bounds();
        x >= b.x - 2.0 && x <= b.right() + 2.0 && y >= b.y && y <= b.bottom() + 4.0
    })
    .unwrap_or(false)
}

fn arm_alert(hwnd: HWND) {
    let ms = with(|n| n.alert_until.map(|u| u.saturating_duration_since(Instant::now()).as_millis() as u32)).flatten();
    unsafe {
        match ms {
            Some(ms) => {
                SetTimer(hwnd, TIMER_ALERT, ms.max(50), None);
            }
            None => {
                let _ = KillTimer(hwnd, TIMER_ALERT);
            }
        }
    }
}

fn caps_alert(on: bool) {
    if !settings::get().lock_key_alerts {
        return;
    }
    let (title, sub) = if on { ("Caps Lock on", "TYPING IN CAPITALS") } else { ("Caps Lock off", "typing in lowercase") };
    model::push_alert(Alert {
        key: "lock".into(),
        kind: AlertKind::Compact { icon: "ArrowUp", title: title.into(), sub: sub.into() },
        duration: Duration::from_millis(1600),
    });
}

unsafe fn on_raw_input(lparam: LPARAM) {
    let mut size = 0u32;
    let header = std::mem::size_of::<RAWINPUTHEADER>() as u32;
    GetRawInputData(HRAWINPUT(lparam.0 as _), RID_INPUT, None, &mut size, header);
    if size == 0 || size as usize > std::mem::size_of::<RAWINPUT>() {
        return;
    }
    let mut raw = RAWINPUT::default();
    if GetRawInputData(HRAWINPUT(lparam.0 as _), RID_INPUT, Some(&mut raw as *mut _ as *mut _), &mut size, header) == u32::MAX {
        return;
    }
    if raw.header.dwType != RIM_TYPEKEYBOARD.0 {
        return;
    }
    let kb = raw.data.keyboard;
    if kb.VKey != VK_CAPITAL.0 {
        return;
    }
    let up = (kb.Flags as u32 & RI_KEY_BREAK) != 0;
    let toggled = CAPS.with(|c| {
        let mut c = c.borrow_mut();
        if up {
            c.1 = false;
            None
        } else if !c.1 {
            // A fresh press (not auto-repeat) flips the state.
            c.1 = true;
            c.0 = !c.0;
            Some(c.0)
        } else {
            None
        }
    });
    if let Some(on) = toggled {
        caps_alert(on);
    }
}

fn tray_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let capsule = settings::get().notch_style == "Capsule";
        let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("Settings"));
        let _ = AppendMenuW(menu, MF_STRING | if capsule { MF_CHECKED } else { MF_UNCHECKED }, MENU_STYLE, w!("Capsule style"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Quit FathNotch"));
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, 0, hwnd, None);
        let _ = DestroyMenu(menu);
        match cmd.0 as usize {
            MENU_SETTINGS => {
                with(|n| {
                    n.expand();
                    n.pinned = true;
                    n.show_tool("settings");
                });
            }
            MENU_STYLE => {
                settings::update(|s| s.notch_style = if capsule { "Notch".into() } else { "Capsule".into() });
                redraw(hwnd);
            }
            MENU_EXIT => PostQuitMessage(0),
            _ => {}
        }
    }
}

/// Mouse position (client px) → canvas DIP → panel coordinates.
fn set_mouse(n: &mut Notch, lparam: LPARAM) {
    let s = n.gfx.scale;
    let x = ((lparam.0 & 0xFFFF) as i16 as i32 + n.gfx.region_left()) as f32 / s;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / s;
    n.input.mouse = n.to_panel(x, y);
}

fn key_from(vk: u32, ctrl: bool) -> Option<Key> {
    Some(match vk {
        0x25 if ctrl => Key::TabPrev,
        0x27 if ctrl => Key::TabNext,
        0x31..=0x39 if ctrl => Key::TabAt((vk - 0x30) as u8),
        0x0D => Key::Enter,
        0x1B => Key::Escape,
        0x08 => Key::Backspace,
        0x2E => Key::Delete,
        0x25 => Key::Left,
        0x27 => Key::Right,
        0x26 => Key::Up,
        0x28 => Key::Down,
        0x24 => Key::Home,
        0x23 => Key::End,
        0x09 => Key::Tab,
        0x41 if ctrl => Key::SelectAll,
        0x43 if ctrl => Key::Copy,
        0x56 if ctrl => Key::Paste,
        0x58 if ctrl => Key::Cut,
        _ => return None,
    })
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_MOUSEMOVE => {
                let first = HOVER.with(|h| !std::mem::replace(&mut *h.borrow_mut(), true));
                if first {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                    // A short rest before opening, so passing over the notch doesn't open it.
                    let s = settings::get();
                    if s.open_mode == settings::OPEN_HOVER {
                        SetTimer(hwnd, TIMER_HOVER, s.hover_delay_ms.clamp(60, 1000) as u32, None);
                    }
                }
                let _ = KillTimer(hwnd, TIMER_LEAVE);
                let expanded = with(|n| {
                    set_mouse(n, lparam);
                    n.expanded()
                })
                .unwrap_or(false);
                if expanded {
                    redraw(hwnd); // hover states
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                HOVER.with(|h| *h.borrow_mut() = false);
                let _ = KillTimer(hwnd, TIMER_HOVER);
                with(|n| n.input.mouse = (f32::NAN, f32::NAN));
                SetTimer(hwnd, TIMER_LEAVE, 280, None);
                redraw(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                let expanded = with(|n| n.expanded()).unwrap_or(false);
                if !expanded {
                    // Closed: a click on an alert opens its tool (or dismisses it); else opens the panel.
                    let (dismiss, open) = with(|n| {
                        let s = n.gfx.scale;
                        let x = ((lparam.0 & 0xFFFF) as i16 as i32 + n.gfx.region_left()) as f32 / s;
                        let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / s;
                        (n.has_alert() && n.alert_dismiss_hit(x, y), n.alert_open_target())
                    })
                    .unwrap_or((false, None));
                    // Another app's notification (WhatsApp…): a click opens that app.
                    let app = if dismiss { None } else { with(|n| n.alert_app()).flatten() };
                    with(|n| {
                        if dismiss {
                            // × on a ringing alarm snoozes it (it never just goes quiet).
                            if n.alert_key().is_some_and(|k| k.starts_with("alarm:")) {
                                services::alarms::snooze();
                            }
                            n.end_alert();
                        } else if let Some((id, name)) = app {
                            n.end_alert();
                            bg::spawn(move || shell::open_app(&id, &name));
                        } else {
                            if n.has_alert() {
                                n.end_alert();
                            }
                            n.expand();
                            if let Some(t) = open {
                                n.show_tool(t);
                            }
                        }
                    });
                    arm_alert(hwnd);
                    return LRESULT(0);
                }
                SetCapture(hwnd);
                take_keyboard(hwnd);
                with(|n| {
                    set_mouse(n, lparam);
                    if msg == WM_LBUTTONDOWN {
                        n.input.pressed = true;
                        n.input.down = true;
                    } else {
                        n.input.right_pressed = true;
                    }
                });
                redraw(hwnd);
                LRESULT(0)
            }
            // Lost the mouse (another window took it): the button counts as released, or a held slider
            // would keep going after the button is up.
            WM_CAPTURECHANGED => {
                with(|n| {
                    if n.input.down {
                        n.input.down = false;
                        n.input.released = true;
                    }
                });
                redraw(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let _ = ReleaseCapture();
                with(|n| {
                    set_mouse(n, lparam);
                    n.input.down = false;
                    n.input.released = true;
                });
                redraw(hwnd);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
                // Wheel coordinates are on the screen.
                let mut p = POINT { x: (lparam.0 & 0xFFFF) as i16 as i32, y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 };
                let _ = ScreenToClient(hwnd, &mut p);
                with(|n| {
                    let s = n.gfx.scale;
                    n.input.mouse = n.to_panel((p.x + n.gfx.region_left()) as f32 / s, p.y as f32 / s);
                    n.input.wheel += delta;
                });
                redraw(hwnd);
                LRESULT(0)
            }
            WM_CHAR => {
                let c = char::from_u32(wparam.0 as u32).unwrap_or('\0');
                if !c.is_control() {
                    with(|n| n.input.chars.push(c));
                    redraw(hwnd);
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                if let Some(k) = key_from(wparam.0 as u32, ctrl) {
                    let esc_closes = k == Key::Escape && with(|n| n.ui.focus == 0 && n.ui.dropdown == 0).unwrap_or(false);
                    if esc_closes {
                        with(|n| {
                            n.pinned = false;
                            n.collapse();
                        });
                        release_keyboard(hwnd);
                    } else {
                        with(|n| n.input.keys.push(k));
                        redraw(hwnd);
                    }
                }
                LRESULT(0)
            }
            WM_ACTIVATE => {
                if (wparam.0 & 0xFFFF) == 0 {
                    // Lost focus: stop typing and close unless pinned.
                    release_keyboard(hwnd);
                    with(|n| n.ui.focus = 0);
                    if !cursor_in_notch() {
                        with(|n| n.collapse());
                    }
                }
                LRESULT(0)
            }
            WM_TIMER => {
                match wparam.0 {
                    TIMER_CLOCK => {
                        redraw(hwnd);
                        arm_clock(hwnd);
                    }
                    TIMER_HOVER => {
                        let _ = KillTimer(hwnd, TIMER_HOVER);
                        if cursor_in_notch() {
                            with(|n| {
                                if !n.has_alert() {
                                    n.expand();
                                }
                            });
                        }
                    }
                    TIMER_LEAVE => {
                        let _ = KillTimer(hwnd, TIMER_LEAVE);
                        if !cursor_in_notch() && !with(|n| n.holding()).unwrap_or(false) {
                            with(|n| n.collapse());
                            release_keyboard(hwnd);
                        }
                    }
                    TIMER_ALERT => {
                        let _ = KillTimer(hwnd, TIMER_ALERT);
                        if !cursor_in_notch() {
                            with(|n| n.end_alert());
                        } else {
                            // Resting the pointer on an alert keeps it up a little longer.
                            with(|n| n.alert_until = Some(Instant::now() + Duration::from_secs(2)));
                        }
                        arm_alert(hwnd);
                    }
                    TIMER_IDLE => {
                        with(|n| n.rotate_idle());
                    }
                    TIMER_EDGE => poll_edge(hwnd),
                    TIMER_DODGE => {
                        // Pointer near the top of the screen: run the dodge (cheap; a few calls).
                        let near = with(|n| {
                            let mut p = POINT::default();
                            if GetCursorPos(&mut p).is_err() {
                                return false;
                            }
                            let s = n.gfx.scale;
                            let (x, y) = ((p.x - n.win_x) as f32 / s, (p.y - n.win_y) as f32 / s);
                            n.dodge(x, y);
                            y < n.bounds().bottom() + 80.0 || n.shove.value.abs() > 0.5
                        })
                        .unwrap_or(false);
                        // Fast only while the pointer is up near the notch; otherwise a slow glance.
                        if !settings::get().repel_cursor {
                            let _ = KillTimer(hwnd, TIMER_DODGE);
                        } else {
                            SetTimer(hwnd, TIMER_DODGE, if near { 33 } else { 300 }, None);
                        }
                    }
                    TIMER_EYES => {
                        let open = with(|n| n.expanded()).unwrap_or(true);
                        if !open && !cursor_in_notch() {
                            with(|n| n.show_eyes());
                        }
                    }
                    TIMER_TICK => {
                        // Once a second: timers, live values in the open panel, full-screen apps.
                        model::poll_timers();
                        update_visibility(hwnd);
                        track_peek();
                        if settings::get().monitor_index == -2 && !with(|n| n.expanded()).unwrap_or(true) {
                            place(hwnd);
                        }
                        let open = with(|n| n.expanded()).unwrap_or(false);
                        if open || model::activity() != model::Activity::Idle {
                            redraw(hwnd);
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_HOTKEY => {
                on_hotkey(wparam.0);
                panel_keyboard(hwnd);
                LRESULT(0)
            }
            WM_INPUT => {
                on_raw_input(lparam);
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            views::shelf::WM_SHELF_DRAG => {
                // Shelf tiles dragged out: Windows' drag and drop runs here, on the window thread that owns
                // the mouse (a modal loop until the drop), never inside a frame.
                let _ = ReleaseCapture();
                views::shelf::run_pending_drag();
                with(|n| {
                    n.input.down = false;
                    n.input.released = true;
                });
                redraw(hwnd);
                LRESULT(0)
            }
            bg::WM_SETTINGS => {
                register_hotkeys(hwnd);
                SetTimer(hwnd, TIMER_IDLE, (settings::get().idle_rotate_sec.max(2) as u32) * 1000, None);
                SetTimer(hwnd, TIMER_EYES, (settings::get().idle_eyes_every_sec.max(20) as u32) * 1000, None);
                MONITOR.with(|m| *m.borrow_mut() = 0);
                with(|n| n.activity_changed());
                if settings::get().repel_cursor {
                    SetTimer(hwnd, TIMER_DODGE, 300, None);
                }
                place(hwnd);
                update_visibility(hwnd);
                redraw(hwnd);
                LRESULT(0)
            }
            bg::WM_REFRESH => {
                if std::env::var_os("FATHNOTCH_STATS").is_some() {
                    use std::sync::atomic::{AtomicU64, Ordering};
                    static N: AtomicU64 = AtomicU64::new(0);
                    if N.fetch_add(1, Ordering::Relaxed) % 100 == 0 {
                        log(&format!("refresh messages: {}", N.load(Ordering::Relaxed)));
                    }
                }
                redraw(hwnd);
                arm_alert(hwnd);
                LRESULT(0)
            }
            WM_TRAY => {
                let event = (lparam.0 & 0xFFFF) as u32;
                if event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
                    tray_menu(hwnd);
                } else if event == WM_LBUTTONUP {
                    with(|n| {
                        if n.expanded() {
                            n.pinned = false;
                            n.collapse();
                        } else {
                            n.expand();
                            n.pinned = true;
                        }
                    });
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn add_tray(hwnd: HWND) -> Result<NOTIFYICONDATAW> {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: gfx::cat_icon(32)?,
        ..Default::default()
    };
    for (i, c) in "FathNotch".encode_utf16().enumerate() {
        nid.szTip[i] = c;
    }
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }
    Ok(nid)
}

fn main() -> Result<()> {
    unsafe {
        // Screenshot mode (dev): FATHNOTCH_SHOT=<png> renders FATHNOTCH_DEMO's state off screen and exits.
        let shot = std::env::var("FATHNOTCH_SHOT").ok().filter(|s| !s.is_empty());
        // One copy at a time (screenshot runs don't count).
        let _mutex = CreateMutexW(None, true, w!("FathNotch.Rust.SingleInstance"))?;
        if shot.is_none() && GetLastError() == ERROR_ALREADY_EXISTS {
            return Ok(());
        }
        // OLE (an STA) for drag and drop, WIC and the shell.
        let _ = windows::Win32::System::Ole::OleInitialize(None);
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // Only a real install manages "open at login": never dev builds (target\) or screenshot/demo runs.
        let dev = std::env::current_exe().map(|p| p.to_string_lossy().to_lowercase().contains("\\target\\")).unwrap_or(true)
            || std::env::var_os("FATHNOTCH_DEMO").is_some()
            || shot.is_some();
        if !dev {
            settings::apply_startup();
        }

        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("FathNotchRs"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("FathNotchRs"),
            w!("FathNotch"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            instance,
            None,
        )?;
        bg::init(hwnd);

        // Top centre of the primary monitor.
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
        let gfx = gfx::Gfx::new(CANVAS_W, CANVAS_H, scale)?;
        let m = info.rcMonitor;
        let x = m.left + ((m.right - m.left) - gfx.px_w) / 2;
        let y = m.top;
        let notch = Notch::new(gfx, x, y)?;
        APP.with(|a| *a.borrow_mut() = Some(notch));
        redraw(hwnd);
        if shot.is_none() {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }

        // Caps Lock: raw keyboard input arrives even while another app has focus.
        CAPS.with(|c| c.borrow_mut().0 = (GetKeyState(VK_CAPITAL.0 as i32) & 1) != 0);
        let device = RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x06, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd };
        let _ = RegisterRawInputDevices(&[device], std::mem::size_of::<RAWINPUTDEVICE>() as u32);

        let tray = add_tray(hwnd)?;
        arm_clock(hwnd);
        if shot.is_none() {
            register_hotkeys(hwnd);
        }
        SetTimer(hwnd, TIMER_IDLE, (settings::get().idle_rotate_sec.max(2) as u32) * 1000, None);
        SetTimer(hwnd, TIMER_TICK, 1000, None);
        if settings::get().repel_cursor {
            SetTimer(hwnd, TIMER_DODGE, 300, None);
        }
        SetTimer(hwnd, TIMER_EYES, (settings::get().idle_eyes_every_sec.max(20) as u32) * 1000, None);
        model::start();
        // Control → Awake survives restarts (on the window thread, which lives as long as the app).
        if shot.is_none() && settings::get().keep_awake {
            services::awake::apply(true);
        }
        settings::on_change(|| bg::post(bg::WM_SETTINGS));
        place(hwnd);
        update_visibility(hwnd);
        if shot.is_none() {
            dragdrop::register(
                hwnd,
                dragdrop::Hooks {
                    enter: || {
                        // Our own Shelf drag passing over the notch: nothing to do.
                        if views::shelf::dragging() {
                            return;
                        }
                        // Over the open Music tool, songs join its queue: stay there.
                        if music_open() {
                            return;
                        }
                        with(|n| {
                            n.expand();
                            n.show_tool("shelf");
                        });
                    },
                    leave: || {},
                    drop: |files, text| {
                        if views::shelf::dragging() {
                            return;
                        }
                        if music_open() && files.iter().any(|f| services::player::droppable(f)) {
                            bg::spawn(move || services::player::add(files, true));
                        } else if !files.is_empty() {
                            services::shelf::add(&files);
                        } else if let Some(t) = text {
                            let _ = services::shelf::add_text(&t);
                        }
                        bg::refresh();
                    },
                },
            );
        }

        // Dev aid: FATHNOTCH_DEMO=<tool id> starts open on that tool; "alert" shows a Caps Lock alert.
        if let Ok(demo) = std::env::var("FATHNOTCH_DEMO") {
            if demo == "alert" {
                caps_alert(true);
            } else if demo == "prayeralert" {
                // a sample prayer heads-up (screenshots)
                model::push_alert(notch::Alert {
                    key: "prayer".into(),
                    kind: notch::AlertKind::Full { icon: "Moon", title: "Ashar in 10 min".into(), sub: format!("15:02 · {}", model::prayer_city()), open: Some("prayer"), app: None },
                    duration: Duration::from_secs(60),
                });
            } else if demo == "closed" {
                // stays closed (screenshots of the pill)
            } else if demo == "eyes" {
                // the peeking cat eyes (FATHNOTCH_SHOT_WAIT picks the moment)
                with(|n| n.show_eyes());
            } else if !demo.is_empty() {
                let id = match demo.as_str() { "settings" => "settings", "alarm" => "alarm", d => tools::find(d).id };
                let r = with(|n| {
                    n.expand();
                    n.pinned = true;
                    n.show_tool(id);
                    n.expanded()
                });
                let _ = r;
            }
        }

        if let Some(path) = shot {
            // Let springs settle and background data arrive, then write one frame and quit.
            let wait = std::env::var("FATHNOTCH_SHOT_WAIT").ok().and_then(|v| v.parse().ok()).unwrap_or(3500u64);
            // FATHNOTCH_SHOT_INPUT: scripted input, run after the wait, separated by ';':
            //   click:x,y   rclick:x,y   move:x,y   wheel:x,y,delta   type:text   key:Enter|Escape|Backspace|Tab|Left|Right|Up|Down
            //   wait:ms   (coordinates are panel DIP, the same space views draw in)
            let script: Vec<String> = std::env::var("FATHNOTCH_SHOT_INPUT")
                .ok()
                .map(|s| s.split(';').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect())
                .unwrap_or_default();
            let mut msg = MSG::default();
            let mut pump = |ms: u64| {
                let until = Instant::now() + Duration::from_millis(ms);
                while Instant::now() < until {
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    with(|n| n.step());
                    redraw(hwnd);
                    std::thread::sleep(Duration::from_millis(16));
                }
            };
            pump(wait);
            for step in &script {
                let (kind, arg) = step.split_once(':').unwrap_or((step.as_str(), ""));
                let nums: Vec<f32> = arg.split(',').filter_map(|v| v.trim().parse().ok()).collect();
                match kind {
                    "click" | "rclick" | "move" | "wheel" | "down" | "up" if nums.len() >= 2 => {
                        with(|n| {
                            n.input.mouse = (nums[0], nums[1]);
                            match kind {
                                // "down"/"up": a press held across moves (drags, box selection).
                                "down" => {
                                    n.input.pressed = true;
                                    n.input.down = true;
                                }
                                "up" => {
                                    n.input.down = false;
                                    n.input.released = true;
                                }
                                "click" => {
                                    n.input.pressed = true;
                                    n.input.down = true;
                                }
                                "rclick" => n.input.right_pressed = true,
                                "wheel" => n.input.wheel += nums.get(2).copied().unwrap_or(-1.0),
                                _ => {}
                            }
                        });
                        redraw(hwnd);
                        if kind == "click" {
                            with(|n| {
                                n.input.down = false;
                                n.input.released = true;
                            });
                            redraw(hwnd);
                        }
                    }
                    "type" => {
                        with(|n| n.input.chars.push_str(arg));
                        redraw(hwnd);
                    }
                    "key" => {
                        let k = match arg {
                            "Enter" => Some(Key::Enter),
                            "Escape" => Some(Key::Escape),
                            "Backspace" => Some(Key::Backspace),
                            "Tab" => Some(Key::Tab),
                            "Left" => Some(Key::Left),
                            "Right" => Some(Key::Right),
                            "Up" => Some(Key::Up),
                            "Down" => Some(Key::Down),
                            "Delete" => Some(Key::Delete),
                            "SelectAll" => Some(Key::SelectAll),
                            _ => None,
                        };
                        if let Some(k) = k {
                            with(|n| n.input.keys.push(k));
                            redraw(hwnd);
                        }
                    }
                    "wait" => pump(nums.first().copied().unwrap_or(300.0) as u64),
                    _ => {}
                }
                pump(120);
            }
            let until = Instant::now();
            while Instant::now() < until {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                with(|n| n.step());
                redraw(hwnd);
                std::thread::sleep(Duration::from_millis(16));
            }
            with(|n| n.shot = Some(path));
            redraw(hwnd);
            let _ = Shell_NotifyIconW(NIM_DELETE, &tray);
            return Ok(());
        }

        let mut msg = MSG::default();
        'outer: loop {
            if with(|n| n.animating()).unwrap_or(false) {
                // Animating: drain messages, draw a frame, wait for the next display refresh.
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        break 'outer;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                with(|n| n.step());
                redraw(hwnd);
                let _ = DwmFlush();
            } else {
                // At rest: sleep until something happens.
                if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let _ = Shell_NotifyIconW(NIM_DELETE, &tray);
        Ok(())
    }
}

/// Dev aid (FATHNOTCH_STATS): counts calls by name and logs every 100th, to find what floods redraws.
pub fn stat(name: &'static str) {
    if std::env::var_os("FATHNOTCH_STATS").is_none() {
        return;
    }
    static COUNTS: std::sync::Mutex<Option<std::collections::HashMap<&'static str, u64>>> = std::sync::Mutex::new(None);
    let mut g = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    let n = g.get_or_insert_with(Default::default).entry(name).or_insert(0);
    *n += 1;
    if *n % 100 == 1 {
        let c = *n;
        drop(g);
        log(&format!("stat {name}: {c}"));
    }
}

/// `stat` for a source location (who asked for a redraw).
pub fn stat_at(file: &'static str, line: u32) {
    static COUNTS: std::sync::Mutex<Option<std::collections::HashMap<(&'static str, u32), u64>>> = std::sync::Mutex::new(None);
    let mut g = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    let n = g.get_or_insert_with(Default::default).entry((file, line)).or_insert(0);
    *n += 1;
    if *n % 200 == 1 {
        let c = *n;
        drop(g);
        log(&format!("refresh from {file}:{line}: {c}"));
    }
}
