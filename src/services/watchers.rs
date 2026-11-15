//! System watchers that raise alerts (Bluetooth, power, network, keyboard layout, downloads, reminders…).
//! Started once from `model::start`.
//!
//! Event-driven where Windows makes it cheap (Bluetooth device watcher, network status event, directory
//! changes on Downloads); the rest share one half-second poll loop of microsecond-cheap calls.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use windows::core::{Interface, HSTRING, PCWSTR, PWSTR};
use windows::Devices::Bluetooth::{BluetoothDevice, BluetoothLEDevice};
use windows::Devices::Enumeration::{
    DeviceInformation, DeviceInformationKind, DeviceInformationUpdate, DeviceWatcher,
};
use windows::Foundation::Collections::{IIterable, IMapView};
use windows::Foundation::{IReference, TypedEventHandler};
use windows::Networking::Connectivity::{NetworkInformation, NetworkStatusChangedEventHandler};
use windows::Win32::Foundation::{CloseHandle, BOOL, ERROR_SUCCESS, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_QWORD,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, GetKeyboardLayout, VK_NUMLOCK};
use windows::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use crate::model::push_alert;
use crate::notch::{Alert, AlertKind};
use crate::services::{calendar, radios, settings, sound, todos};
use crate::i18n::tr;

// Not in the crate's enabled feature set (they need Win32_System_IO); declared here.
#[link(name = "kernel32")]
extern "system" {
    fn ReadDirectoryChangesW(
        dir: HANDLE,
        buf: *mut c_void,
        len: u32,
        subtree: BOOL,
        filter: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
        routine: *const c_void,
    ) -> BOOL;
    fn LCIDToLocaleName(lcid: u32, name: PWSTR, cch: i32, flags: u32) -> i32;
    fn GetLocaleInfoEx(locale: PCWSTR, lctype: u32, data: PWSTR, cch: i32) -> i32;
}

pub fn start() {
    spawn(|| {
        if bluetooth().is_ok() {
            // The device watchers live as long as this thread; their callbacks do the work.
            loop {
                std::thread::park();
            }
        }
    });
    spawn(network);
    spawn(downloads);
    spawn(poll_loop);
}

/// Each watcher on its own MTA thread (WinRT calls and callbacks).
fn spawn(f: fn()) {
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        f();
    });
}

fn compact(key: impl Into<String>, icon: &'static str, title: impl Into<String>, sub: impl Into<String>, ms: u64) {
    push_alert(Alert {
        key: key.into(),
        kind: AlertKind::Compact { icon, title: title.into(), sub: sub.into() },
        duration: Duration::from_millis(ms),
    });
}

// ---------------------------------------------------------------------- Bluetooth

const IS_CONNECTED: &str = "System.Devices.Aep.IsConnected";

fn connected_prop(props: windows::core::Result<IMapView<HSTRING, windows::core::IInspectable>>) -> Option<bool> {
    let value = props.ok()?.Lookup(&HSTRING::from(IS_CONNECTED)).ok()?;
    value.cast::<IReference<bool>>().ok()?.Value().ok()
}

/// Paired devices (classic and LE) connecting or disconnecting. Each device's first report is its
/// starting state; only later changes alert.
fn bluetooth() -> windows::core::Result<()> {
    let devices: Arc<Mutex<HashMap<String, (String, bool)>>> = Arc::default();
    let mut watchers: Vec<DeviceWatcher> = Vec::new();
    for selector in [
        BluetoothDevice::GetDeviceSelectorFromPairingState(true)?,
        BluetoothLEDevice::GetDeviceSelectorFromPairingState(true)?,
    ] {
        let props: IIterable<HSTRING> = vec![HSTRING::from(IS_CONNECTED)].try_into()?;
        let watcher = DeviceInformation::CreateWatcherWithKindAqsFilterAndAdditionalProperties(
            &selector,
            &props,
            DeviceInformationKind::AssociationEndpoint,
        )?;
        let d = devices.clone();
        watcher.Added(&TypedEventHandler::<DeviceWatcher, DeviceInformation>::new(move |_, info| {
            if let Some(info) = info {
                let name = info.Name().map(|s| s.to_string_lossy()).unwrap_or_default();
                let connected = connected_prop(info.Properties()).unwrap_or(false);
                d.lock().unwrap().insert(info.Id()?.to_string_lossy(), (name, connected));
            }
            Ok(())
        }))?;
        let d = devices.clone();
        watcher.Updated(&TypedEventHandler::<DeviceWatcher, DeviceInformationUpdate>::new(move |_, update| {
            let Some(update) = update else { return Ok(()) };
            let Some(connected) = connected_prop(update.Properties()) else { return Ok(()) };
            let id = update.Id()?.to_string_lossy();
            let changed = {
                let mut map = d.lock().unwrap();
                match map.get_mut(&id) {
                    Some(dev) if dev.1 != connected => {
                        dev.1 = connected;
                        Some(dev.0.clone())
                    }
                    _ => None,
                }
            };
            if let Some(name) = changed.filter(|n| !n.trim().is_empty()) {
                if settings::get().bluetooth_alerts {
                    let sub = if connected { tr("Connected") } else { tr("Disconnected") };
                    compact(format!("bt:{name}"), "Bluetooth", name, sub, 3000);
                }
            }
            Ok(())
        }))?;
        let d = devices.clone();
        watcher.Removed(&TypedEventHandler::<DeviceWatcher, DeviceInformationUpdate>::new(move |_, update| {
            if let Some(update) = update {
                d.lock().unwrap().remove(&update.Id()?.to_string_lossy());
            }
            Ok(())
        }))?;
        watcher.Start()?;
        watchers.push(watcher);
    }
    // Keep the watchers alive with this thread (the caller parks it).
    std::mem::forget(watchers);
    Ok(())
}

// ---------------------------------------------------------------------- network

/// Online / offline / switched network, once the connection has settled (adapters flap while it comes up).
fn network() {
    let (tx, rx) = mpsc::channel::<()>();
    let tx = Mutex::new(tx);
    let handler = NetworkStatusChangedEventHandler::new(move |_| {
        let _ = tx.lock().unwrap().send(());
        Ok(())
    });
    if NetworkInformation::NetworkStatusChanged(&handler).is_err() {
        return;
    }
    let mut last = radios::connected_name();
    while rx.recv().is_ok() {
        std::thread::sleep(Duration::from_secs(3));
        while rx.try_recv().is_ok() {}
        let now = radios::connected_name();
        if now == last {
            continue;
        }
        let was_online = last.is_some();
        last = now.clone();
        if !settings::get().network_alerts {
            continue;
        }
        match now {
            Some(name) => compact("network", "Wifi", tr("Connected"), name, 2500),
            None if was_online => compact("network", "Wifi", tr("Offline"), tr("No network connection"), 2500),
            None => {}
        }
    }
}

// ---------------------------------------------------------------------- downloads

/// Browsers download to a temporary name and rename it when done; that rename is the signal.
const PARTIAL: [&str; 5] = [".crdownload", ".part", ".partial", ".download", ".tmp"];

fn is_partial(name: &str) -> bool {
    let lower = name.to_lowercase();
    PARTIAL.iter().any(|ext| lower.ends_with(ext))
}

fn downloads_dir() -> Option<String> {
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Downloads, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

fn downloads() {
    let Some(dir) = downloads_dir() else { return };
    let handle = unsafe {
        CreateFileW(
            &HSTRING::from(dir.as_str()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    };
    let Ok(handle) = handle else { return };
    const FILE_NOTIFY_CHANGE_FILE_NAME: u32 = 1;
    const RENAMED_OLD: u32 = 4;
    const RENAMED_NEW: u32 = 5;
    let mut buf = vec![0u32; 16 * 1024]; // 64 KB, DWORD-aligned
    let mut old_partial = false;
    loop {
        let mut got = 0u32;
        let ok = unsafe {
            ReadDirectoryChangesW(
                handle,
                buf.as_mut_ptr().cast(),
                (buf.len() * 4) as u32,
                BOOL(0),
                FILE_NOTIFY_CHANGE_FILE_NAME,
                &mut got,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if !ok.as_bool() {
            break;
        }
        // FILE_NOTIFY_INFORMATION records: next offset, action, name length (bytes), name (UTF-16).
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), got as usize) };
        let mut at = 0usize;
        while at + 12 <= bytes.len() {
            let word = |i: usize| u32::from_le_bytes(bytes[at + i..at + i + 4].try_into().unwrap());
            let (next, action, len) = (word(0) as usize, word(4), word(8) as usize);
            let name: Vec<u16> = bytes[at + 12..(at + 12 + len).min(bytes.len())]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            let name = String::from_utf16_lossy(&name);
            match action {
                RENAMED_OLD => old_partial = is_partial(&name),
                RENAMED_NEW => {
                    if std::mem::take(&mut old_partial) && !is_partial(&name) && settings::get().download_alerts {
                        let file = name.rsplit('\\').next().unwrap_or(&name).to_string();
                        push_alert(Alert {
                            key: format!("download:{name}"),
                            kind: AlertKind::Full { icon: "ArrowDown", title: tr("Download finished").into(), sub: file, open: None, app: None },
                            duration: Duration::from_secs(4),
                        });
                    }
                }
                _ => {}
            }
            if next == 0 {
                break;
            }
            at += next;
        }
    }
    unsafe {
        let _ = CloseHandle(handle);
    }
}

// ---------------------------------------------------------------------- poll loop

#[derive(Default)]
struct Polled {
    // power
    line: Option<bool>,
    low_shown: i32,
    full_shown: bool,
    // keys / layout
    num: Option<bool>,
    layout: Option<(isize, u32)>,
    // reminders
    tick: u64,
    lead_done: HashSet<String>,
    start_done: HashSet<String>,
}

fn poll_loop() {
    let mut st = Polled { low_shown: 101, ..Default::default() };
    loop {
        let s = settings::get();
        // 0.5 s: Num Lock. 1 s: keyboard layout. 2 s: power. 3 s: privacy. 30 s: reminders.
        poll_num_lock(&mut st, s.lock_key_alerts);
        if st.tick % 2 == 0 {
            poll_layout(&mut st, s.keyboard_layout_alerts);
        }
        if st.tick % 4 == 0 {
            poll_power(&mut st, s.power_alerts);
        }
        if st.tick % 6 == 0 {
            poll_privacy(s.privacy_indicator);
        }
        if st.tick % 60 == 0 {
            poll_reminders(&mut st, &s);
        }
        st.tick += 1;
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn poll_num_lock(st: &mut Polled, enabled: bool) {
    if !enabled {
        st.num = None;
        return;
    }
    let num = unsafe { GetKeyState(VK_NUMLOCK.0 as i32) } & 1 != 0;
    if st.num.is_some_and(|was| was != num) {
        let (title, sub) =
            if num { (tr("Num Lock on"), tr("Number pad types digits")) } else { (tr("Num Lock off"), tr("Number pad moves the cursor")) };
        compact("lock", "Keyboard", title, sub, 1500);
    }
    st.num = Some(num);
}

// ------------------------------------------------------------------ power

fn power_status() -> Option<(bool, i32)> {
    let mut ps = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut ps).ok()? };
    if ps.BatteryFlag & 128 != 0 || ps.BatteryFlag == 255 || ps.BatteryLifePercent > 100 || ps.ACLineStatus > 1 {
        return None; // desktop or unknown
    }
    Some((ps.ACLineStatus == 1, ps.BatteryLifePercent as i32))
}

/// Charger plugged in or pulled, low battery (20% and 10%, once each per discharge), fully charged.
fn poll_power(st: &mut Polled, enabled: bool) {
    const LOW: [i32; 2] = [10, 20]; // lowest first
    let Some((charging, pct)) = power_status() else { return };
    let was = st.line.replace(charging);
    if charging {
        st.low_shown = 101;
    } else if was != Some(charging) {
        // Just unplugged: thresholds already passed count as told; the "On battery" alert says the level.
        st.low_shown = LOW.iter().copied().find(|t| pct <= *t).unwrap_or(101);
    }
    let full = charging && pct >= 100;
    if !charging || pct < 95 {
        st.full_shown = false;
    }
    let Some(was) = was else {
        st.full_shown = full;
        return; // first reading: nothing changed yet
    };
    if !enabled {
        st.full_shown |= full;
        return;
    }
    if was != charging {
        st.full_shown = full; // plugged in already full: "Charging · 100%" says it
        let (icon, title) = if charging { ("Bolt", tr("Charging")) } else { ("BatteryFull", tr("On battery")) };
        compact("power", icon, title, format!("{pct}%"), 3000);
        return;
    }
    if charging {
        if full && !st.full_shown {
            st.full_shown = true;
            compact("power", "BatteryFull", tr("Fully charged"), "100%", 3000);
        }
        return;
    }
    for t in LOW {
        if pct > t || st.low_shown <= t {
            continue;
        }
        st.low_shown = t;
        compact("power", "BatteryFull", tr("Battery low"), format!("{pct}% left · plug in soon"), 3000);
        return;
    }
}

// ------------------------------------------------------------------ keyboard layout

fn locale_info(name: &[u16], kind: u32) -> Option<String> {
    let mut buf = [0u16; 128];
    let n = unsafe { GetLocaleInfoEx(PCWSTR(name.as_ptr()), kind, PWSTR(buf.as_mut_ptr()), buf.len() as i32) };
    (n > 1).then(|| String::from_utf16_lossy(&buf[..n as usize - 1]))
}

/// ("EN", "English (United States)") for a language id.
fn language(lang: u32) -> (String, String) {
    const LOCALE_SLOCALIZEDDISPLAYNAME: u32 = 0x2;
    const LOCALE_SISO639LANGNAME: u32 = 0x59;
    let mut name = [0u16; 85];
    let n = unsafe { LCIDToLocaleName(lang, PWSTR(name.as_mut_ptr()), name.len() as i32, 0) };
    if n > 0 {
        if let (Some(code), Some(display)) =
            (locale_info(&name, LOCALE_SISO639LANGNAME), locale_info(&name, LOCALE_SLOCALIZEDDISPLAYNAME))
        {
            return (code.to_uppercase(), display);
        }
    }
    (format!("{lang:04X}"), tr("Keyboard language changed").into())
}

fn current_layout() -> Option<(isize, u32)> {
    unsafe {
        let window = GetForegroundWindow();
        if window.0.is_null() {
            return None;
        }
        let thread = GetWindowThreadProcessId(window, None);
        let lang = (GetKeyboardLayout(thread).0 as usize & 0xFFFF) as u32;
        (lang != 0).then_some((window.0 as isize, lang))
    }
}

/// Only a switch inside the same window counts: moving to another app with its own language is not news.
fn poll_layout(st: &mut Polled, enabled: bool) {
    if !enabled {
        st.layout = None;
        return;
    }
    let Some(now) = current_layout() else { return };
    let was = st.layout.replace(now);
    if let Some((window, lang)) = was {
        if window == now.0 && lang != now.1 {
            let (code, name) = language(now.1);
            compact("layout", "Language", format!("Keyboard · {code}"), name, 1800);
        }
    }
}

// ------------------------------------------------------------------ camera / microphone

static CAMERA: AtomicBool = AtomicBool::new(false);
static MIC: AtomicBool = AtomicBool::new(false);

/// (camera in use, microphone in use), for the privacy dot. Both false while the indicator is off.
pub fn privacy() -> (bool, bool) {
    (CAMERA.load(Ordering::Relaxed), MIC.load(Ordering::Relaxed))
}

const CONSENT_STORE: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\";

fn qword(key: HKEY, sub: &str, value: &str) -> Option<u64> {
    let mut v: u64 = 0;
    let mut size = 8u32;
    let err = unsafe {
        RegGetValueW(
            key,
            &HSTRING::from(sub),
            &HSTRING::from(value),
            RRF_RT_REG_QWORD,
            None,
            Some(&mut v as *mut u64 as *mut c_void),
            Some(&mut size),
        )
    };
    (err == ERROR_SUCCESS).then_some(v)
}

/// The app using a device: packaged apps sit directly under the capability, desktop apps under
/// NonPackaged (named after their exe path, '#' for '\'). In use = LastUsedTimeStart set, Stop 0.
fn scan(path: &str, packaged: bool) -> Option<String> {
    let mut key = HKEY::default();
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), 0, KEY_READ, &mut key) } != ERROR_SUCCESS {
        return None;
    }
    let mut found = None;
    for i in 0.. {
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let err = unsafe { RegEnumKeyExW(key, i, PWSTR(buf.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None) };
        if err != ERROR_SUCCESS {
            break;
        }
        let name = String::from_utf16_lossy(&buf[..len as usize]);
        if packaged && name == "NonPackaged" {
            continue;
        }
        if qword(key, &name, "LastUsedTimeStop") != Some(0) || qword(key, &name, "LastUsedTimeStart").unwrap_or(0) == 0 {
            continue;
        }
        found = Some(if packaged {
            name.split('_').next().unwrap_or("").rsplit('.').next().unwrap_or("").to_string()
        } else {
            let path = name.replace('#', "\\");
            std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(path)
        });
        break;
    }
    unsafe {
        let _ = RegCloseKey(key);
    }
    found
}

fn in_use(capability: &str) -> Option<String> {
    let root = format!("{CONSENT_STORE}{capability}");
    scan(&root, true).or_else(|| scan(&format!(r"{root}\NonPackaged"), false))
}

fn poll_privacy(enabled: bool) {
    let (camera, mic) = if enabled { (in_use("webcam"), in_use("microphone")) } else { (None, None) };
    let was_camera = CAMERA.swap(camera.is_some(), Ordering::Relaxed);
    let was_mic = MIC.swap(mic.is_some(), Ordering::Relaxed);
    if was_camera == camera.is_some() && was_mic == mic.is_some() {
        return;
    }
    crate::bg::refresh(); // the dot
    let label = |app: String| if app.is_empty() { tr("An app is using it").to_string() } else { format!("By {app}") };
    if let (Some(app), false) = (camera, was_camera) {
        compact("privacy:camera", "Photo", tr("Camera in use"), label(app), 3000);
    } else if let (Some(app), false) = (mic, was_mic) {
        compact("privacy:mic", "Microphone", tr("Microphone in use"), label(app), 3000);
    }
}

// ------------------------------------------------------------------ calendar reminders

/// Today's timed events and to-dos due at a time: (start in local seconds, title).
fn timed_today(today: i64) -> Vec<(i64, String)> {
    let (y, m, d) = calendar::civil_from_days(today);
    let mut items: Vec<(i64, String)> = calendar::on_day(y, m, d)
        .into_iter()
        .filter_map(|e| e.time.map(|(h, min)| (today * 86_400 + (h * 3600 + min * 60) as i64, e.title)))
        .collect();
    for t in todos::all() {
        if t.done || t.due_all_day {
            continue;
        }
        if let Some((ty, tm, td, h, min)) = t.due_local() {
            items.push((calendar::days_from_civil(ty, tm, td) * 86_400 + (h * 3600 + min * 60) as i64, t.text));
        }
    }
    items.sort_by_key(|(start, _)| *start);
    items
}

/// One alert (and chime) when an event's reminder window opens, `reminder_lead_min` before it, and one
/// when it starts.
fn poll_reminders(st: &mut Polled, s: &settings::Settings) {
    if !s.reminders_enabled {
        return;
    }
    let lead = s.reminder_lead_min.max(1) as i64 * 60;
    let linger = s.reminder_linger_min.max(1) as i64 * 60;
    let now = todos::local_now_secs();
    let mut chimed = false;
    for (start, title) in timed_today(now.div_euclid(86_400)) {
        if now < start - lead || now > start + linger {
            continue;
        }
        let key = format!("{start}|{title}");
        let is_now = now >= start;
        let fresh = if is_now { st.start_done.insert(key.clone()) } else { st.lead_done.insert(key.clone()) };
        if is_now {
            st.lead_done.insert(key.clone()); // appeared at/after start: don't alert twice
        }
        if !fresh {
            continue;
        }
        let t = start.rem_euclid(86_400);
        let clock = format!("{:02}:{:02}", t / 3600, t % 3600 / 60);
        let minutes = (start - now + 59) / 60;
        let sub = if is_now {
            format!("Starting now · {clock}")
        } else {
            format!("Starts in {} min · {clock}", minutes.max(1))
        };
        push_alert(Alert {
            key: format!("reminder:{key}"),
            kind: AlertKind::Full { icon: "CalendarDays", title, sub, open: Some("calendar"), app: None },
            duration: Duration::from_secs(5),
        });
        if !chimed {
            chimed = true;
            sound::chime(&s.reminder_sound);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn prints_current_state() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        println!("power: {:?}", power_status());
        println!("network: {:?}", radios::connected_name());
        println!("layout: {:?}", current_layout().map(|(_, lang)| (lang, language(lang))));
        println!("num lock: {}", unsafe { GetKeyState(VK_NUMLOCK.0 as i32) } & 1 != 0);
        println!("camera: {:?}  mic: {:?}", in_use("webcam"), in_use("microphone"));
        println!("downloads: {:?}", downloads_dir());
        println!("partial: {} {}", is_partial("a.pdf.crdownload"), is_partial("a.pdf"));
        println!("today timed: {:?}", timed_today(todos::local_now_secs().div_euclid(86_400)));
    }

    #[test]
    #[ignore]
    fn lists_bluetooth_devices() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        println!("paired: {:?}", radios::paired());
        println!("watchers started: {:?}", bluetooth().is_ok());
    }
}
