//! What the notch shows, gathered from the services on background threads and read by the UI each frame.
//! Watchers (volume, brightness, notifications, prayer times, timers) queue alerts here; the window picks
//! them up on its next refresh.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::bg;
use crate::notch::{Alert, AlertKind};
use crate::services::{audio, brightness, lyrics, media, notifications, prayer, settings, ticktick, timers, todos, weather};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Activity {
    Idle,
    Music,
    Timer,
}

#[derive(Clone)]
pub struct IdleItem {
    pub id: &'static str,
    pub icon: &'static str,
    pub value: String,
    pub accent: bool,
}

#[derive(Default)]
struct Model {
    weather: Option<weather::Current>,
    weather_at: Option<Instant>,
    lyrics_key: String,
    lyrics: Option<Arc<Vec<lyrics::Line>>>,
    lyrics_loading: bool,
    alerts: Vec<Alert>,
    prayer_alerted: String,
    /// The prayer (name + time) the heads-up before it was shown for.
    prayer_lead_alerted: String,
    activity: Option<Activity>,
}

static MODEL: OnceLock<Mutex<Model>> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();

fn model() -> std::sync::MutexGuard<'static, Model> {
    MODEL.get_or_init(|| Mutex::new(Model::default())).lock().unwrap()
}

/// Seconds since start (for animations).
pub fn clock_secs() -> f64 {
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// Queues an alert for the notch. Notification-style alerts (`Full`) also land in the Notifications tab
/// (Windows' own are recorded by the watcher), and while Silent is on they're only recorded, not shown.
#[track_caller]
pub fn push_alert(a: Alert) {
    if let AlertKind::Full { icon, title, sub, open, .. } = &a.kind {
        if !a.key.starts_with("app:") {
            let source = match open {
                Some("phone") => "Phone",
                Some("timer") => "Timers",
                Some("prayer") => "Prayer",
                Some("calendar") => "Calendar",
                Some("alarm") => "Alarm",
                _ if a.key.starts_with("download:") => "Downloads",
                _ => "FathNotch",
            };
            notifications::add_local(crate::i18n::tr(source), icon, title, sub);
        }
        // Silent keeps notifications quiet, but an alarm still rings.
        if settings::shared().silent_mode && !a.key.starts_with("alarm:") {
            bg::refresh();
            return;
        }
    }
    model().alerts.push(a);
    bg::refresh();
}

pub fn take_alerts() -> Vec<Alert> {
    std::mem::take(&mut model().alerts)
}

fn on_volume(level: f32, muted: bool) {
    crate::stat("on_volume");
    if !settings::shared().volume_hud {
        return;
    }
    let silent = muted || level <= 0.001;
    push_alert(Alert {
        key: "volume".into(),
        kind: AlertKind::Level { icon: if silent { "SpeakerMuted" } else { "Speaker" }, level: if muted { 0.0 } else { level } },
        duration: Duration::from_millis(1600),
    });
}

fn on_brightness(level: u32) {
    if !settings::shared().brightness_hud {
        return;
    }
    push_alert(Alert { key: "brightness".into(), kind: AlertKind::Level { icon: "Sun", level: level as f32 / 100.0 }, duration: Duration::from_millis(1600) });
}

fn on_notification(n: notifications::Notification) {
    let s = settings::shared();
    if !s.notify_enabled || s.notify_muted_apps.iter().any(|a| a == &n.app) {
        return;
    }
    let body = if s.notify_show_text { n.body.clone() } else { String::new() };
    let sub = if body.trim().is_empty() { n.app.clone() } else { format!("{} · {}", n.app, body.replace(['\r', '\n'], " ")) };
    let secs = (4 + n.body.chars().count() as u64 / 60).min(8);
    push_alert(Alert {
        key: format!("app:{}:{}", n.app, n.id),
        kind: AlertKind::Full { icon: "Bell", title: n.title.clone(), sub, open: Some("notifications"), app: Some((n.app_id.clone(), n.app.clone())) },
        duration: Duration::from_secs(secs),
    });
}

/// Starts the watchers and background refreshes. Call once after the window exists.
pub fn start() {
    let _ = clock_secs();
    todos::on_change(bg::refresh);
    settings::on_change(bg::refresh);
    settings::on_change(crate::i18n::refresh);
    // WinRT start-ups wait on async operations: never on the window (STA) thread.
    bg::spawn(media::start);
    bg::spawn(crate::services::watchers::start);
    bg::spawn(|| notifications::start(on_notification));
    bg::spawn(|| audio::watch(on_volume));
    bg::spawn(|| brightness::watch_internal(on_brightness));
    bg::spawn(|| {
        // Screenshot/demo runs never talk to the user's TickTick account.
        let demo = std::env::var_os("FATHNOTCH_DEMO").is_some() || std::env::var_os("FATHNOTCH_SHOT").is_some();
        if ticktick::enabled() && !demo {
            ticktick::start_auto(300);
        }
    });
    // Alarms: checked every 2 s so they ring on the minute.
    // A ringing alarm keeps its sound going (restarted now and then, in case another sound cut it off)
    // until Snooze or Stop; the pop-up stays up just as long.
    std::thread::spawn(|| {
        let mut sound_at: Option<(u64, Instant, bool)> = None;
        loop {
            if let Some(a) = crate::services::alarms::due() {
                ring_alarm(&a);
                sound_at = Some((a.id, Instant::now(), crate::services::sound::alarm()));
            }
            match (crate::services::alarms::ringing(), sound_at) {
                (Some(a), Some((id, at, looping))) if a.id == id => {
                    if !looping || at.elapsed() > Duration::from_secs(60) {
                        sound_at = Some((id, Instant::now(), crate::services::sound::alarm()));
                    }
                }
                (None, Some(_)) => {
                    crate::services::sound::stop();
                    sound_at = None;
                }
                _ => {}
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
    // Once a minute: weather (every 10 min), prayer alerts, finished timers.
    std::thread::spawn(|| loop {
        minute_tick();
        std::thread::sleep(Duration::from_secs(20));
    });
}

fn minute_tick() {
    let stale = model().weather_at.map_or(true, |t| t.elapsed() > Duration::from_secs(600));
    if stale {
        model().weather_at = Some(Instant::now());
        bg::spawn(|| {
            let s = settings::shared();
            if !s.weather_city.trim().is_empty() {
                if let Some(w) = weather::current(&s.weather_city, s.use_fahrenheit) {
                    model().weather = Some(w);
                }
            }
        });
    }
    // Prayer time alert.
    if let Some((name, at)) = prayer_now() {
        let key = format!("{name}{at}");
        let due = {
            let m = model();
            m.prayer_alerted != key
        };
        if due {
            model().prayer_alerted = key;
            // The adzan when it's on for this prayer (Settings → Prayer → Adzan), else the prayer sound.
            let id = prayer::NAMES.iter().position(|n| *n == name).map_or("", |i| prayer::IDS[i]);
            let adzan = crate::services::adzan::wanted(id);
            if adzan {
                let (id, name) = (id.to_string(), name.to_string());
                bg::spawn(move || {
                    if !crate::services::adzan::play(&id, &name) {
                        crate::services::sound::chime(&settings::shared().prayer_sound);
                    }
                });
            } else {
                crate::services::sound::chime(&settings::shared().prayer_sound);
            }
            let sub = if adzan {
                crate::i18n::tf!("{at} · {} · adzan playing", "{at} · {} · adzan berkumandang", prayer_city())
            } else {
                format!("{at} · {}", prayer_city())
            };
            push_alert(Alert {
                key: "prayer".into(),
                kind: AlertKind::Full { icon: "Moon", title: format!("Waktu {name}"), sub, open: Some("prayer"), app: None },
                duration: Duration::from_secs(if adzan { 20 } else { 10 }),
            });
        }
    }
    // Heads-up a few minutes before (Settings → Prayer → Heads-up), once per prayer, without the sound.
    if let Some((name, at, mins)) = prayer_soon() {
        let key = format!("{name}{at}");
        let due = model().prayer_lead_alerted != key;
        if due {
            model().prayer_lead_alerted = key;
            push_alert(Alert {
                key: "prayer".into(),
                kind: AlertKind::Full { icon: "Moon", title: crate::i18n::tf!("{name} in {mins} min", "{name} {mins} menit lagi"), sub: format!("{at} · {}", prayer_city()), open: Some("prayer"), app: None },
                duration: Duration::from_secs(6),
            });
        }
    }
}

/// An alarm going off: a pop-up (also kept in Notifications) that stays while it rings; clicking it opens
/// Alarm with Snooze and Stop. The sound is the caller's.
fn ring_alarm(a: &crate::services::alarms::Alarm) {
    push_alert(alarm_alert(a));
}

/// The pop-up of a ringing alarm (× snoozes it, a click opens Snooze / Stop).
pub fn alarm_alert(a: &crate::services::alarms::Alarm) -> Alert {
    let title = if a.label.is_empty() { crate::i18n::tr("Alarm").to_string() } else { a.label.clone() };
    Alert {
        key: format!("alarm:{}", a.id),
        kind: AlertKind::Full { icon: "Alarm", title, sub: crate::i18n::tf!("{} · tap for Snooze or Stop", "{} · ketuk untuk Tunda atau Berhenti", a.time()), open: Some("alarm"), app: None },
        duration: Duration::from_secs(6 * 3600),
    }
}

/// Timers are polled on every refresh (cheap).
pub fn poll_timers() {
    if let Some(msg) = timers::poll() {
        let s = settings::shared();
        crate::services::sound::play(if msg == "Timer finished" { &s.timer_sound } else { &s.pomodoro_sound });
        push_alert(Alert {
            key: "timer".into(),
            kind: AlertKind::Full { icon: "Clock", title: crate::i18n::tr(&msg).to_string(), sub: crate::i18n::tr("Timers").into(), open: Some("timer"), app: None },
            duration: Duration::from_secs(6),
        });
    }
}

// ---------------------------------------------------------------------- prayer helpers

pub fn prayer_city() -> String {
    let s = settings::shared();
    if s.prayer_resolved_city.is_empty() {
        s.prayer_city.clone()
    } else {
        s.prayer_resolved_city.clone()
    }
}

/// Today's times for the configured place.
pub fn prayer_times() -> Option<prayer::Times> {
    // Same day, same place: the times don't change; recompute once a minute at most.
    static CACHE: Mutex<Option<(Instant, Option<prayer::Times>)>> = Mutex::new(None);
    let mut c = CACHE.lock().unwrap();
    if let Some((at, t)) = c.as_ref() {
        if at.elapsed() < Duration::from_secs(60) {
            return t.clone();
        }
    }
    let t = compute_prayer_times();
    *c = Some((Instant::now(), t.clone()));
    t
}

fn compute_prayer_times() -> Option<prayer::Times> {
    let s = settings::shared();
    if !s.prayer_enabled {
        return None;
    }
    let (lat, lon) = (s.prayer_lat?, s.prayer_lon?);
    let n = crate::notch::now();
    let m = prayer::method(&s.prayer_method);
    let tz = prayer::tz_hours_on(n.year as i32, n.month as u32, n.day as u32);
    Some(prayer::compute_with(n.year as i32, n.month as u32, n.day as u32, lat, lon, tz, m, if s.prayer_asr_hanafi { 2.0 } else { 1.0 }))
}

pub fn now_hours() -> f64 {
    let n = crate::notch::now();
    n.hour as f64 + n.minute as f64 / 60.0 + n.second as f64 / 3600.0
}

pub fn times_list(t: &prayer::Times) -> [f64; 6] {
    [t.fajr, t.sunrise, t.dhuhr, t.asr, t.maghrib, t.isha]
}

/// The prayer whose time is this minute (skipping sunrise).
fn prayer_now() -> Option<(&'static str, String)> {
    let t = prayer_times()?;
    let now = now_hours();
    for (i, at) in times_list(&t).iter().enumerate() {
        if i == 1 {
            continue;
        }
        if now >= *at && now - at < 1.0 / 60.0 {
            return Some((prayer::NAMES[i], prayer::hhmm(*at)));
        }
    }
    None
}

/// The prayer coming within the heads-up lead (name, time, whole minutes left), if the heads-up is on.
fn prayer_soon() -> Option<(&'static str, String, i64)> {
    let lead = settings::shared().prayer_lead_min;
    if lead <= 0 {
        return None;
    }
    let t = prayer_times()?;
    let now = now_hours();
    for (i, at) in times_list(&t).iter().enumerate() {
        let left = at - now;
        if i != 1 && left > 1.0 / 60.0 && left <= lead as f64 / 60.0 {
            return Some((prayer::NAMES[i], prayer::hhmm(*at), (left * 60.0).ceil() as i64));
        }
    }
    None
}

// ---------------------------------------------------------------------- music

pub fn now_playing() -> Option<media::NowPlaying> {
    // Dev aid for screenshots: FATHNOTCH_NO_MEDIA hides whatever is playing on this PC.
    if std::env::var_os("FATHNOTCH_NO_MEDIA").is_some() {
        return None;
    }
    media::current().filter(|n| !n.title.is_empty())
}

/// The current lyric line and how far through it the song is (fetches lyrics in the background).
pub fn lyric_line() -> Option<(String, f32)> {
    let s = settings::shared();
    if !s.lyrics_enabled {
        return None;
    }
    let np = now_playing()?;
    let key = format!("{}\u{1}{}", np.artist, np.title);
    let lines = {
        let mut m = model();
        if m.lyrics_key != key {
            m.lyrics_key = key.clone();
            m.lyrics = None;
            m.lyrics_loading = true;
            let np2 = np.clone();
            bg::spawn(move || {
                let l = lyrics::fetch(&np2.artist, &np2.title, &np2.album, np2.duration);
                let mut m = model();
                if m.lyrics_key == key {
                    m.lyrics = l;
                    m.lyrics_loading = false;
                }
            });
        }
        m.lyrics.clone()?
    };
    let (i, prog) = lyrics::line_at(&lines, np.position_now())?;
    let text = lines[i].text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    Some((text, prog))
}

/// All lyric lines for the current song (None while loading or when there are none).
pub fn lyrics_all() -> (Option<Arc<Vec<lyrics::Line>>>, bool) {
    let m = model();
    (m.lyrics.clone(), m.lyrics_loading)
}

// ---------------------------------------------------------------------- closed notch

pub fn activity() -> Activity {
    if timers::current().is_some_and(|t| t.running()) {
        return Activity::Timer;
    }
    if now_playing().is_some_and(|n| n.playing) {
        return Activity::Music;
    }
    Activity::Idle
}

/// Remembers the last activity; true when it changed (the closed notch resizes).
pub fn activity_changed() -> bool {
    let a = activity();
    let mut m = model();
    let changed = m.activity != Some(a);
    m.activity = Some(a);
    changed
}

pub fn timer_text() -> Option<(String, String)> {
    let t = timers::current()?;
    Some((crate::i18n::tr(t.label()).to_string(), t.text()))
}

/// The weather place or units changed: fetch again on the next tick.
pub fn weather_changed() {
    let mut m = model();
    m.weather_at = None;
    m.weather = None;
}

pub fn weather_now() -> Option<weather::Current> {
    model().weather.clone()
}

pub fn weather_icon(code: u32, is_day: bool) -> &'static str {
    match weather::icon_kind(code, is_day) {
        "sun" => "Sun",
        "moon" => "Moon",
        "rain" | "storm" => "Drop",
        "snow" => "Sparkles",
        _ => "Cloud",
    }
}

/// The idle items in rotation (Settings → Display "Idle items").
/// The idle items, rebuilt at most once a second (frames during animations reuse them).
pub fn idle_items() -> Vec<IdleItem> {
    static CACHE: Mutex<Option<(Instant, Vec<IdleItem>)>> = Mutex::new(None);
    let mut c = CACHE.lock().unwrap();
    if let Some((at, items)) = c.as_ref() {
        if at.elapsed() < Duration::from_millis(1000) {
            return items.clone();
        }
    }
    let items = build_idle_items();
    *c = Some((Instant::now(), items.clone()));
    items
}

fn build_idle_items() -> Vec<IdleItem> {
    let s = settings::shared();
    let n = crate::notch::now();
    let mut out = Vec::new();
    let wanted: Vec<String> = if s.idle_items.is_empty() { vec!["clock".into()] } else { s.idle_items.clone() };
    for id in wanted {
        match id.as_str() {
            "clock" => out.push(IdleItem { id: "clock", icon: "Clock", value: format!("{:02}:{:02}", n.hour, n.minute), accent: false }),
            "date" => out.push(IdleItem {
                id: "date",
                icon: "CalendarDays",
                value: format!("{} {}", crate::i18n::day3(n.dow), n.day),
                accent: false,
            }),
            "weather" => {
                if let Some(w) = weather_now() {
                    // Always the cloud: the idle pill marks the temperature, not the sky.
                    out.push(IdleItem { id: "weather", icon: "Cloud", value: weather::degrees(w.temp), accent: false });
                }
            }
            "prayer" => {
                if let Some(t) = prayer_times() {
                    let (i, _) = prayer::next(&t, now_hours());
                    out.push(IdleItem {
                        id: "prayer",
                        icon: "Moon",
                        value: format!("{} {}", prayer::NAMES[i], prayer::hhmm(times_list(&t)[i])),
                        accent: false,
                    });
                }
            }
            "todos" => {
                let open = todos::all().iter().filter(|t| !t.done).count();
                if open > 0 {
                    out.push(IdleItem { id: "todos", icon: "ListBullet", value: crate::i18n::tf!("{open} open", "{open} tugas"), accent: false });
                }
            }
            "battery" => {
                if let Some((pct, charging)) = crate::services::system::sample().battery {
                    out.push(IdleItem { id: "battery", icon: if charging { "Bolt" } else { "BatteryFull" }, value: format!("{pct}%"), accent: false });
                }
            }
            _ => {}
        }
    }
    if out.is_empty() {
        out.push(IdleItem { id: "clock", icon: "Clock", value: format!("{:02}:{:02}", n.hour, n.minute), accent: false });
    }
    out
}
