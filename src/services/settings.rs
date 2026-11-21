//! App settings in settings.json, the same file (property names, types and defaults) the .NET build uses,
//! so either build can read what the other saved. Fields this build doesn't know are kept in `extra` and
//! written back unchanged.
//!
//! Also: the TickTick API token (DPAPI-encrypted in ticktick-token.bin, as the .NET build stores it) and
//! "open at login" (the per-user Run key).

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use windows::core::HSTRING;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

use super::store;

const FILE: &str = "settings.json";

/// `OpenMode` values (the .NET enum is stored as its number).
pub const OPEN_HOVER: i32 = 0;
pub const OPEN_CLICK: i32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Settings {
    /// 0 = open on hover, 1 = on click.
    pub open_mode: i32,
    pub hover_delay_ms: i32,
    pub tool_order: Vec<String>,
    pub hidden_tools: Vec<String>,
    /// -1 = primary display, -2 = the display the pointer is on.
    pub monitor_index: i32,
    /// Open at login (see `set_start_with_windows`).
    pub launch_at_startup: bool,
    pub startup_default_applied: bool,
    pub hide_in_fullscreen: bool,
    pub hide_for_apps: Vec<String>,
    /// "Fade" | "Hidden"
    pub hide_for_apps_mode: String,
    pub hide_for_apps_hold_alerts: bool,
    /// "Always" | "WhenActive" | "HoverOnly"
    pub notch_visibility: String,
    pub hide_from_capture: bool,

    // Alerts and HUDs
    pub power_alerts: bool,
    pub volume_hud: bool,
    pub bluetooth_alerts: bool,
    pub download_alerts: bool,
    pub track_peek: bool,
    pub low_memory_mode: bool,
    pub notch_border: bool,
    /// Caps Lock / Num Lock.
    pub lock_key_alerts: bool,
    pub privacy_indicator: bool,
    pub brightness_hud: bool,
    pub network_alerts: bool,
    pub keyboard_layout_alerts: bool,
    pub repel_cursor: bool,

    pub clipboard_enabled: bool,
    pub clipboard_skip_sensitive: bool,
    pub clipboard_max_items: i32,
    pub shelf_retention_days: i32,

    pub phone_enabled: bool,
    pub phone_receive_folder: String,
    pub phone_height: f64,
    pub phone_port: i32,

    pub weather_city: String,
    /// Where `weather_city` is, when it was picked from Settings → Weather's search (so a town that shares
    /// its name with others stays the one chosen). None: look the name up.
    pub weather_lat: Option<f64>,
    pub weather_lon: Option<f64>,
    pub use_fahrenheit: bool,

    pub pomodoro_work_min: i32,
    pub pomodoro_break_min: i32,
    pub timer_minutes: i32,
    pub pomodoro_sound: String,
    pub timer_sound: String,

    pub reminders_enabled: bool,
    pub reminder_lead_min: i32,
    pub reminder_linger_min: i32,
    pub reminder_sound: String,

    /// Windows notifications mirrored on the notch.
    pub notify_enabled: bool,
    pub notify_show_text: bool,
    pub notify_muted_apps: Vec<String>,
    pub notify_seen_apps: Vec<String>,
    pub notify_keep_history: bool,
    /// Control → Silent: notifications are collected in the Notifications tab but don't pop out, and
    /// notification sounds are off.
    pub silent_mode: bool,
    /// Control → Awake: keep the PC and screen from sleeping.
    pub keep_awake: bool,

    /// Idle closed notch: "clock", "date", "prayer", "weather", ...
    pub idle_items: Vec<String>,
    /// "Rotate" | "Split" | "Row"
    pub idle_layout: String,
    pub idle_rotate_sec: i32,

    pub last_tool: String,
    pub check_for_updates: bool,
    pub update_notified: Option<String>,

    pub theme_scheme: String,
    /// "auto" = the scheme's own accent.
    pub theme_accent: String,
    pub glass: bool,
    pub glass_strength: f64,

    /// Tiny, ExtraSmall, Small, Default, Large, ExtraLarge.
    pub notch_size: String,
    /// "Icons" | "Dots"
    pub tab_style: String,
    pub look_version: i32,
    pub header_clock: bool,
    /// How fast the panel animates: "Off" | "Fast" | "Normal" | "Slow".
    pub anim_speed: String,
    pub rest_glow: bool,
    pub idle_eyes: bool,
    pub idle_eyes_every_sec: i32,
    /// The peeking eyes also come out (for their few seconds) while the notch is hidden.
    pub eyes_when_hidden: bool,
    /// The idle eyes' look: "cat", "round", "robot" or "dots".
    pub eyes_style: String,
    /// Their colour: "theme" (the scheme's text colour), "accent", "yellow", "green", "blue" or "pink".
    pub eyes_color: String,
    /// "mix" takes turns through the animations; "look", "sleepy", "wink" or "curious" plays just that one.
    pub eyes_anim: String,
    /// The pupils follow the mouse pointer while the eyes are out.
    pub eyes_follow: bool,
    /// "Notch" | "Capsule"
    pub notch_style: String,
    /// Where the notch sits on its monitor: "center", "left" or "right" (along the top edge), or "free"
    /// (dragged there; `notch_free_x` is its centre, `notch_free_y` its top, as fractions of the monitor).
    pub notch_position: String,
    pub notch_free_x: f64,
    pub notch_free_y: f64,
    /// 0.8 compact … 1.0 full size.
    pub panel_scale: f64,
    pub lyrics_in_notch: bool,

    // Prayer times
    pub prayer_enabled: bool,
    /// Empty = the weather city.
    pub prayer_city: String,
    pub prayer_resolved_city: String,
    pub prayer_lat: Option<f64>,
    pub prayer_lon: Option<f64>,
    /// "kemenag", "mwl", "isna", "egypt", "makkah"
    pub prayer_method: String,
    pub prayer_asr_hanafi: bool,
    pub prayer_lead_min: i32,
    pub prayer_sound: String,
    /// The adzan at prayer times (services/adzan.rs): on/off, its audio file, Subuh's own file (empty = the
    /// same), which prayers (ids as in prayer::IDS) and the volume 0..1.
    pub adzan_enabled: bool,
    pub adzan_file: String,
    pub adzan_fajr_file: String,
    pub adzan_prayers: Vec<String>,
    pub adzan_volume: f64,

    pub display_name: String,
    /// Interface language: "" = as Windows, "en", "id" (see `i18n`).
    pub language: String,
    pub lyrics_enabled: bool,
    /// Action id → gesture such as "Ctrl+Alt+N"; empty = disabled.
    pub hotkeys: BTreeMap<String, String>,

    pub prompter_speed: f64,
    pub prompter_opacity: f64,
    pub prompter_font_size: f64,
    pub prompter_height: f64,
    pub prompter_mirror: bool,

    pub note_height: f64,
    pub notifications_height: f64,
    pub last_note_id: Option<String>,

    // TickTick (the token itself: `ticktick_token`)
    #[serde(rename = "TickTickEnabled")]
    pub ticktick_enabled: bool,
    /// List to sync with; empty = Inbox.
    #[serde(rename = "TickTickProject")]
    pub ticktick_project: String,
    #[serde(rename = "TickTickProjectName")]
    pub ticktick_project_name: String,
    /// The Inbox's real id ("inbox123…"), learned from TickTick.
    #[serde(rename = "TickTickInboxId")]
    pub ticktick_inbox_id: String,
    /// Due date new to-dos get: "today" or "none".
    pub todo_default_due: String,

    /// Anything else in settings.json (other builds' settings), written back as it was.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        let s = |v: &str| v.to_string();
        Settings {
            open_mode: OPEN_HOVER,
            hover_delay_ms: 150,
            tool_order: vec![],
            hidden_tools: vec![],
            monitor_index: -1,
            launch_at_startup: false,
            startup_default_applied: false,
            hide_in_fullscreen: true,
            hide_for_apps: vec![],
            hide_for_apps_mode: s("Fade"),
            hide_for_apps_hold_alerts: true,
            notch_visibility: s("Always"),
            hide_from_capture: false,
            power_alerts: true,
            volume_hud: false,
            bluetooth_alerts: true,
            download_alerts: true,
            track_peek: false,
            low_memory_mode: true,
            notch_border: true,
            lock_key_alerts: true,
            privacy_indicator: true,
            brightness_hud: false,
            network_alerts: true,
            keyboard_layout_alerts: true,
            repel_cursor: true,
            clipboard_enabled: true,
            clipboard_skip_sensitive: true,
            clipboard_max_items: 200,
            shelf_retention_days: 7,
            phone_enabled: false,
            phone_receive_folder: s(""),
            phone_height: 220.0,
            phone_port: 0,
            weather_city: s("London"),
            weather_lat: None,
            weather_lon: None,
            use_fahrenheit: false,
            pomodoro_work_min: 25,
            pomodoro_break_min: 5,
            timer_minutes: 10,
            pomodoro_sound: s("Soft Chime"),
            timer_sound: s("Soft Chime"),
            reminders_enabled: true,
            reminder_lead_min: 10,
            reminder_linger_min: 5,
            reminder_sound: s("Calendar"),
            notify_enabled: true,
            notify_show_text: true,
            notify_muted_apps: vec![],
            notify_seen_apps: vec![],
            notify_keep_history: true,
            silent_mode: false,
            keep_awake: false,
            idle_items: vec![s("clock")],
            idle_layout: s("Rotate"),
            idle_rotate_sec: 8,
            last_tool: s("home"),
            check_for_updates: true,
            update_notified: None,
            theme_scheme: s("midnight"),
            theme_accent: s("auto"),
            glass: false,
            glass_strength: 0.6,
            notch_size: s("ExtraSmall"),
            tab_style: s("Icons"),
            look_version: 0,
            header_clock: true,
            anim_speed: "Normal".into(),
            rest_glow: true,
            idle_eyes: true,
            idle_eyes_every_sec: 45,
            eyes_when_hidden: false,
            eyes_style: s("cat"),
            eyes_color: s("theme"),
            eyes_anim: s("mix"),
            eyes_follow: false,
            notch_style: s("Notch"),
            notch_position: s("center"),
            notch_free_x: 0.5,
            notch_free_y: 0.0,
            panel_scale: 0.8,
            lyrics_in_notch: true,
            prayer_enabled: true,
            prayer_city: s(""),
            prayer_resolved_city: s(""),
            prayer_lat: None,
            prayer_lon: None,
            prayer_method: s("kemenag"),
            prayer_asr_hanafi: false,
            prayer_lead_min: 10,
            prayer_sound: s("Chime"),
            adzan_enabled: false,
            adzan_file: s(""),
            adzan_fajr_file: s(""),
            adzan_prayers: ["fajr", "dhuhr", "asr", "maghrib", "isha"].iter().map(|x| x.to_string()).collect(),
            adzan_volume: 0.8,
            display_name: s(""),
            language: s(""),
            lyrics_enabled: true,
            hotkeys: [("toggle", "Ctrl+Alt+N"), ("clipboard", ""), ("teleprompter", "")]
                .into_iter()
                .map(|(k, v)| (s(k), s(v)))
                .collect(),
            prompter_speed: 0.35,
            prompter_opacity: 1.0,
            prompter_font_size: 20.0,
            prompter_height: 203.0,
            prompter_mirror: false,
            note_height: 220.0,
            notifications_height: 220.0,
            last_note_id: None,
            ticktick_enabled: false,
            ticktick_project: s(""),
            ticktick_project_name: s("Inbox"),
            ticktick_inbox_id: s(""),
            todo_default_due: s("today"),
            extra: Map::new(),
        }
    }
}

impl Settings {
    pub fn open_on_click(&self) -> bool {
        self.open_mode == OPEN_CLICK
    }

    /// The city prayer times are for: its own, else the weather city.
    pub fn prayer_city_or_weather(&self) -> &str {
        if self.prayer_city.trim().is_empty() { &self.weather_city } else { &self.prayer_city }
    }
}

// ---------------------------------------------------------------- current settings

fn cell() -> &'static Mutex<std::sync::Arc<Settings>> {
    static CELL: OnceLock<Mutex<std::sync::Arc<Settings>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(std::sync::Arc::new(load_file())))
}

fn listeners() -> &'static Mutex<Vec<fn()>> {
    static L: OnceLock<Mutex<Vec<fn()>>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(Vec::new()))
}

/// Reads settings.json (defaults when it's missing; a broken file also gives defaults, like the .NET build).
pub fn load_file() -> Settings {
    store::load(FILE)
}

/// No settings saved yet (first launch).
pub fn first_run() -> bool {
    !store::path(FILE).exists()
}

/// A copy of the current settings.
pub fn get() -> Settings {
    (**cell().lock().unwrap_or_else(|e| e.into_inner())).clone()
}

/// Settings → Display "Animations" as a length factor: Fast 0.55, Normal 1, Slow 1.8; Off 0 (no animation).
pub fn anim_scale() -> f32 {
    match shared().anim_speed.as_str() {
        "Off" => 0.0,
        "Fast" => 0.55,
        "Slow" => 1.8,
        _ => 1.0,
    }
}

/// Seconds since an animation started, on the animation clock: run faster or slower by the Animations
/// setting (Off: as if long finished).
pub fn anim_secs(real: f32) -> f32 {
    let k = anim_scale();
    if k <= 0.0 {
        1e6
    } else {
        real / k
    }
}

/// The current settings without copying them (cheap: for code that runs every frame).
pub fn shared() -> std::sync::Arc<Settings> {
    cell().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Changes settings, saves them and tells the `on_change` listeners.
pub fn update(f: impl FnOnce(&mut Settings)) {
    update_quiet(f);
    notify();
}

/// Changes and saves settings without telling listeners (for values nothing needs to react to).
pub fn update_quiet(f: impl FnOnce(&mut Settings)) {
    let mut s = cell().lock().unwrap_or_else(|e| e.into_inner());
    let m = std::sync::Arc::make_mut(&mut s);
    f(m);
    store::save(FILE, &**s);
}

/// Re-reads settings.json (e.g. after the other build changed it) and tells listeners.
pub fn reload() {
    *cell().lock().unwrap_or_else(|e| e.into_inner()) = std::sync::Arc::new(load_file());
    notify();
}

/// Called (on the thread that made the change) after every `update`.
pub fn on_change(cb: fn()) {
    listeners().lock().unwrap_or_else(|e| e.into_inner()).push(cb);
}

fn notify() {
    let list = listeners().lock().unwrap_or_else(|e| e.into_inner()).clone();
    for cb in list {
        cb();
    }
}

// ---------------------------------------------------------------- TickTick token

const TOKEN_FILE: &str = "ticktick-token.bin";
/// The pre-rename name, kept so tokens saved by WinNotch / the .NET build still decrypt.
const TOKEN_ENTROPY: &[u8] = b"WinNotch.TickTick";

pub fn has_ticktick_token() -> bool {
    store::path(TOKEN_FILE).exists()
}

/// The TickTick API token, decrypted (DPAPI, current Windows user).
pub fn ticktick_token() -> Option<String> {
    let cipher = std::fs::read(store::path(TOKEN_FILE)).ok()?;
    let plain = store::unprotect(&cipher, TOKEN_ENTROPY)?;
    let token = String::from_utf8(plain).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// Stores the token encrypted for this Windows account, or forgets it with `None` / an empty string.
pub fn set_ticktick_token(token: Option<&str>) {
    let path = store::path(TOKEN_FILE);
    match token.map(str::trim).filter(|t| !t.is_empty()) {
        None => {
            let _ = std::fs::remove_file(path);
        }
        Some(t) => {
            if let Some(cipher) = store::protect(t.as_bytes(), TOKEN_ENTROPY) {
                let tmp = path.with_extension("tmp");
                if std::fs::write(&tmp, cipher).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            }
        }
    }
    notify();
}

// ---------------------------------------------------------------- open at login

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "FathNotch";

fn run_command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\"", exe.display()))
}

/// What the Run key says now, if anything.
fn run_value() -> Option<String> {
    unsafe {
        let mut buf = vec![0u16; 2048];
        let mut size = (buf.len() * 2) as u32;
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_KEY),
            &HSTRING::from(RUN_VALUE),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as _),
            Some(&mut size),
        );
        if r != ERROR_SUCCESS {
            return None;
        }
        let len = (size as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

/// True when the Run key starts this exe at login.
pub fn starts_with_windows() -> bool {
    matches!((run_value(), run_command()), (Some(v), Some(c)) if v.eq_ignore_ascii_case(&c))
}

/// Turns "open at login" on (Run value pointing at this exe) or off, and records it in LaunchAtStartup.
pub fn set_start_with_windows(on: bool) {
    unsafe {
        if on {
            if let Some(cmd) = run_command() {
                if run_value().as_deref() != Some(cmd.as_str()) {
                    let wide: Vec<u16> = cmd.encode_utf16().chain(Some(0)).collect();
                    let _ = RegSetKeyValueW(
                        HKEY_CURRENT_USER,
                        &HSTRING::from(RUN_KEY),
                        &HSTRING::from(RUN_VALUE),
                        REG_SZ.0,
                        Some(wide.as_ptr() as _),
                        (wide.len() * 2) as u32,
                    );
                }
            }
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(RUN_KEY), &HSTRING::from(RUN_VALUE));
        }
    }
    if get().launch_at_startup != on {
        update_quiet(|s| s.launch_at_startup = on);
    }
}

/// At launch: on for new installs (once, as the .NET build does), then make the Run key match the setting.
pub fn apply_startup() {
    let mut s = get();
    if !s.startup_default_applied {
        update_quiet(|x| {
            x.launch_at_startup = true;
            x.startup_default_applied = true;
        });
        s = get();
    }
    set_start_with_windows(s.launch_at_startup);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Copies the real data folder to a scratch folder and points FATHNOTCH_DATA_DIR at it (once per process).
    pub(crate) fn scratch() -> std::path::PathBuf {
        static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
        DIR.get_or_init(|| {
            let real = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap()).join("FathNotch");
            let dir = std::env::temp_dir().join(format!("fathnotch-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            for e in std::fs::read_dir(&real).unwrap().flatten() {
                if e.path().is_file() {
                    std::fs::copy(e.path(), dir.join(e.file_name())).unwrap();
                }
            }
            std::env::set_var("FATHNOTCH_DATA_DIR", &dir);
            println!("scratch data: {}", dir.display());
            dir
        })
        .clone()
    }

    pub(crate) fn read_json(p: &std::path::Path) -> Value {
        let b = std::fs::read(p).unwrap();
        let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(&b);
        serde_json::from_slice(b).unwrap()
    }

    /// JSON equality where 1 and 1.0 are the same number.
    pub(crate) fn same(a: &Value, b: &Value, at: &str, diffs: &mut Vec<String>) {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => {
                if x.as_f64() != y.as_f64() {
                    diffs.push(format!("{at}: {x} != {y}"));
                }
            }
            (Value::Object(x), Value::Object(y)) => {
                for (k, v) in x {
                    match y.get(k) {
                        Some(w) => same(v, w, &format!("{at}.{k}"), diffs),
                        None => diffs.push(format!("{at}.{k}: missing after save")),
                    }
                }
                for k in y.keys().filter(|k| !x.contains_key(*k)) {
                    diffs.push(format!("{at}.{k}: added by save"));
                }
            }
            (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
                for (i, (v, w)) in x.iter().zip(y).enumerate() {
                    same(v, w, &format!("{at}[{i}]"), diffs);
                }
            }
            _ if a == b => {}
            _ => diffs.push(format!("{at}: {a} != {b}")),
        }
    }

    #[test]
    #[ignore]
    fn settings_round_trip() {
        let dir = scratch();
        let before = read_json(&dir.join(FILE));
        let s = get();
        println!("NotchStyle={} NotchSize={} PanelScale={} Weather={} °F={} Prayer={:?},{:?} Idle={:?} TickTick={} extra={:?}",
            s.notch_style, s.notch_size, s.panel_scale, s.weather_city, s.use_fahrenheit, s.prayer_lat, s.prayer_lon,
            s.idle_items, s.ticktick_enabled, s.extra.keys().collect::<Vec<_>>());
        update(|_| {});
        let after = read_json(&dir.join(FILE));
        let mut diffs = vec![];
        same(&before, &after, "settings", &mut diffs);
        for d in &diffs {
            println!("DIFF {d}");
        }
        assert!(diffs.is_empty());

        // Unknown fields survive.
        update(|s| {
            s.extra.insert("FutureThing".into(), Value::from(42));
        });
        assert_eq!(read_json(&dir.join(FILE))["FutureThing"], 42);
        reload();
        assert_eq!(get().extra["FutureThing"], 42);
        println!("token decrypts: {}", ticktick_token().is_some());
        println!("starts with Windows (Run key → this test exe): {}", starts_with_windows());
    }
}
