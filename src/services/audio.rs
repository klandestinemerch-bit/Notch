//! Core Audio: the default endpoints' master volume and mute, the list of playback / recording devices
//! (and switching the default one), the per-app volume mixer of the default output, and a watcher that
//! reports master volume changes of whatever device is currently the default output.
//!
//! Every function is blocking and self-contained (it creates its own COM objects), so it can be called
//! from any thread. COM is initialised for the duration of the call when the thread hasn't done it yet.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::mpsc::{channel, Sender};
use std::sync::Once;
use std::time::Duration;

use windows::core::{implement, interface, Interface, IUnknown, IUnknown_Vtbl, GUID, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, S_OK};
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    eCapture, eCommunications, eConsole, eMultimedia, eRender, AudioSessionStateActive, AudioSessionStateExpired,
    EDataFlow, ERole, IAudioSessionControl2, IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator,
    IMMNotificationClient, IMMNotificationClient_Impl, ISimpleAudioVolume, MMDeviceEnumerator,
    AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, DEVICE_STATE_ACTIVE,
};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;
use crate::i18n::tr;

#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppSession {
    /// Process id; 0 for the "System sounds" session.
    pub pid: u32,
    pub name: String,
    pub volume: f32,
    pub muted: bool,
    /// Playing sound right now (idle apps are shown dimmer, as in the C# app).
    pub active: bool,
}

// ---------------------------------------------------------------- COM helpers

/// Initialises COM (MTA) for the current scope if the thread hasn't done it; harmless if it already has
/// (same or other apartment). Must be declared before any COM object so it is dropped after them.
pub(crate) struct ComScope(bool);

impl ComScope {
    pub(crate) fn new() -> Self {
        // S_OK / S_FALSE need a matching CoUninitialize; RPC_E_CHANGED_MODE (an STA thread) does not.
        ComScope(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn flow(input: bool) -> EDataFlow {
    if input {
        eCapture
    } else {
        eRender
    }
}

fn enumerator() -> Option<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok() }
}

fn default_device(e: &IMMDeviceEnumerator, input: bool) -> Option<IMMDevice> {
    unsafe { e.GetDefaultAudioEndpoint(flow(input), eMultimedia).ok() }
}

fn endpoint_volume(device: &IMMDevice) -> Option<IAudioEndpointVolume> {
    unsafe { device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok() }
}

fn default_volume(input: bool) -> Option<IAudioEndpointVolume> {
    endpoint_volume(&default_device(&enumerator()?, input)?)
}

/// Takes ownership of a CoTaskMem string.
unsafe fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const c_void));
    s
}

fn device_id(device: &IMMDevice) -> Option<String> {
    unsafe { device.GetId().ok().map(|p| take_pwstr(p)) }
}

fn friendly_name(device: &IMMDevice) -> String {
    unsafe {
        device
            .OpenPropertyStore(STGM_READ)
            .and_then(|store| store.GetValue(&PKEY_Device_FriendlyName as *const PROPERTYKEY))
            .map(|v| v.to_string())
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------- master volume

/// Scalar volume (0..1) and mute of the default output (or input) device.
pub fn master(input: bool) -> Option<(f32, bool)> {
    let _com = ComScope::new();
    let ep = default_volume(input)?;
    unsafe {
        let level = ep.GetMasterVolumeLevelScalar().ok()?;
        let muted = ep.GetMute().ok()?.as_bool();
        Some((level, muted))
    }
}

/// Sets the default device's volume (0..1). A level above zero also unmutes it, like the Windows slider.
pub fn set_master(input: bool, level: f32) {
    let _com = ComScope::new();
    let Some(ep) = default_volume(input) else { return };
    let level = level.clamp(0.0, 1.0);
    unsafe {
        let _ = ep.SetMasterVolumeLevelScalar(level, &GUID::zeroed());
        if level > 0.0 && ep.GetMute().map(|m| m.as_bool()).unwrap_or(false) {
            let _ = ep.SetMute(false, &GUID::zeroed());
        }
    }
}

pub fn set_mute(input: bool, muted: bool) {
    let _com = ComScope::new();
    if let Some(ep) = default_volume(input) {
        unsafe {
            let _ = ep.SetMute(muted, &GUID::zeroed());
        }
    }
}

// ---------------------------------------------------------------- devices

/// Active playback (or recording) devices, the default first, then by name.
pub fn devices(input: bool) -> Vec<Device> {
    let _com = ComScope::new();
    let Some(e) = enumerator() else { return Vec::new() };
    let default_id = default_device(&e, input).and_then(|d| device_id(&d));
    let Ok(collection) = (unsafe { e.EnumAudioEndpoints(flow(input), DEVICE_STATE_ACTIVE) }) else {
        return Vec::new();
    };
    let count = unsafe { collection.GetCount() }.unwrap_or(0);
    let mut list: Vec<Device> = (0..count)
        .filter_map(|i| unsafe { collection.Item(i) }.ok())
        .filter_map(|device| {
            let id = device_id(&device)?;
            let name = friendly_name(&device);
            Some(Device {
                is_default: Some(&id) == default_id.as_ref(),
                name: if name.is_empty() { tr("Audio device").into() } else { name },
                id,
            })
        })
        .collect();
    list.sort_by(|a, b| b.is_default.cmp(&a.is_default).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    list
}

/// Undocumented but stable since Windows 7; every "switch audio device" tool uses it. Only
/// `SetDefaultEndpoint` is called; the methods before it just reserve their vtable slots.
#[allow(non_snake_case)]
mod policy {
    use super::*;
    #[interface("F8679F50-850A-41CF-9C72-430F290290C8")]
    pub unsafe trait IPolicyConfig: IUnknown {
    fn GetMixFormat(&self, a: PCWSTR, b: *mut *mut c_void) -> HRESULT;
    fn GetDeviceFormat(&self, a: PCWSTR, b: i32, c: *mut *mut c_void) -> HRESULT;
    fn ResetDeviceFormat(&self, a: PCWSTR) -> HRESULT;
    fn SetDeviceFormat(&self, a: PCWSTR, b: *mut c_void, c: *mut c_void) -> HRESULT;
    fn GetProcessingPeriod(&self, a: PCWSTR, b: i32, c: *mut i64, d: *mut i64) -> HRESULT;
    fn SetProcessingPeriod(&self, a: PCWSTR, b: *mut i64) -> HRESULT;
    fn GetShareMode(&self, a: PCWSTR, b: *mut c_void) -> HRESULT;
    fn SetShareMode(&self, a: PCWSTR, b: *mut c_void) -> HRESULT;
    fn GetPropertyValue(&self, a: PCWSTR, b: *const c_void, c: *mut c_void) -> HRESULT;
    fn SetPropertyValue(&self, a: PCWSTR, b: *const c_void, c: *mut c_void) -> HRESULT;
    pub fn SetDefaultEndpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
    fn SetEndpointVisibility(&self, id: PCWSTR, visible: i32) -> HRESULT;
}
}
use policy::IPolicyConfig;

const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870AF99C_171D_4F9E_AF0D_E63DF40C2BC9);

/// Makes `id` the default device for every role (console, multimedia, communications), as picking it in
/// the Windows sound flyout does. Returns false if any role could not be set.
pub fn set_default(id: &str) -> bool {
    let _com = ComScope::new();
    let Ok(policy) = (unsafe { CoCreateInstance::<_, IPolicyConfig>(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) }) else {
        return false;
    };
    let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
    [eConsole, eMultimedia, eCommunications]
        .into_iter()
        .map(|role| unsafe { policy.SetDefaultEndpoint(PCWSTR(wide.as_ptr()), role) }.is_ok())
        .fold(true, |all, ok| all & ok)
}

// ---------------------------------------------------------------- per-app sessions

struct RawSession {
    pid: u32,
    system: bool,
    active: bool,
    control: IAudioSessionControl2,
}

/// Live (not expired) sessions on the default output.
fn raw_sessions() -> Vec<RawSession> {
    let Some(device) = enumerator().and_then(|e| default_device(&e, false)) else { return Vec::new() };
    let Ok(manager) = (unsafe { device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) }) else { return Vec::new() };
    let Ok(sessions) = (unsafe { manager.GetSessionEnumerator() }) else { return Vec::new() };
    let count = unsafe { sessions.GetCount() }.unwrap_or(0);
    (0..count)
        .filter_map(|i| unsafe { sessions.GetSession(i) }.ok())
        .filter_map(|s| s.cast::<IAudioSessionControl2>().ok())
        .filter_map(|control| unsafe {
            let state = control.GetState().ok()?;
            if state == AudioSessionStateExpired {
                return None;
            }
            let system = control.IsSystemSoundsSession() == S_OK;
            let pid = if system { 0 } else { control.GetProcessId().ok()? };
            if !system && pid == 0 {
                return None;
            }
            Some(RawSession { pid, system, active: state == AudioSessionStateActive, control })
        })
        .collect()
}

/// Apps with an audio session on the default output, one entry per process: playing apps first, then
/// idle ones, then "System sounds".
pub fn sessions() -> Vec<AppSession> {
    let _com = ComScope::new();
    // pid -> (session, system, active)
    let mut by_pid: HashMap<u32, (AppSession, bool, bool)> = HashMap::new();
    for raw in raw_sessions() {
        if let Some(entry) = by_pid.get_mut(&raw.pid) {
            entry.2 |= raw.active;
            continue;
        }
        let Ok(simple) = raw.control.cast::<ISimpleAudioVolume>() else { continue };
        let (volume, muted) = unsafe {
            (simple.GetMasterVolume().unwrap_or(1.0), simple.GetMute().map(|m| m.as_bool()).unwrap_or(false))
        };
        let name = if raw.system { tr("System sounds").to_string() } else { app_name(raw.pid, &raw.control) };
        by_pid.insert(raw.pid, (AppSession { pid: raw.pid, name, volume, muted, active: raw.active }, raw.system, raw.active));
    }
    // One row per app: a browser and its helper processes (WebView, renderers) share a name.
    let mut list: Vec<(AppSession, bool, bool)> = Vec::new();
    for (s, system, active) in by_pid.into_values() {
        match list.iter_mut().find(|(o, sys, _)| !*sys && !system && o.name == s.name) {
            Some(o) => {
                o.2 |= active;
                o.0.active |= active;
            }
            None => list.push((s, system, active)),
        }
    }
    list.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then(b.2.cmp(&a.2))
            .then_with(|| a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase()))
    });
    list.into_iter().map(|(s, _, _)| s).collect()
}

/// The sessions of process `pid` and of any other process of the same app (merged rows).
fn app_sessions(pid: u32) -> Vec<RawSession> {
    let all = raw_sessions();
    let Some(me) = all.iter().find(|s| s.pid == pid) else { return Vec::new() };
    if me.system {
        return all.into_iter().filter(|s| s.pid == pid).collect();
    }
    let name = app_name(pid, &me.control);
    all.into_iter().filter(|s| s.pid == pid || (!s.system && app_name(s.pid, &s.control) == name)).collect()
}

/// Sets every session of process `pid` (0 = system sounds) and its app to `level`; above zero also unmutes.
pub fn set_session_volume(pid: u32, level: f32) {
    let _com = ComScope::new();
    let level = level.clamp(0.0, 1.0);
    for raw in app_sessions(pid) {
        if let Ok(simple) = raw.control.cast::<ISimpleAudioVolume>() {
            unsafe {
                let _ = simple.SetMasterVolume(level, &GUID::zeroed());
                if level > 0.0 {
                    let _ = simple.SetMute(false, &GUID::zeroed());
                }
            }
        }
    }
}

pub fn set_session_mute(pid: u32, muted: bool) {
    let _com = ComScope::new();
    for raw in app_sessions(pid) {
        if let Ok(simple) = raw.control.cast::<ISimpleAudioVolume>() {
            unsafe {
                let _ = simple.SetMute(muted, &GUID::zeroed());
            }
        }
    }
}

/// Friendly name of a process: its exe's FileDescription, else the exe name, else whatever the session
/// itself says (display name, or the exe in its identifier), else "App <pid>".
fn app_name(pid: u32, control: &IAudioSessionControl2) -> String {
    if let Some(path) = process_path(pid) {
        if let Some(desc) = file_description(&path).filter(|d| !d.trim().is_empty()) {
            return desc.trim().to_string();
        }
        return exe_stem(&path);
    }
    unsafe {
        if let Ok(p) = control.GetDisplayName() {
            let name = take_pwstr(p);
            if !name.is_empty() && !name.starts_with('@') {
                return name;
            }
        }
        if let Ok(p) = control.GetSessionIdentifier() {
            // "{...}|\Device\HarddiskVolume3\Program Files\App\app.exe%b{...}"
            let ident = take_pwstr(p);
            if let Some(exe) = ident.split('|').nth(1).and_then(|s| s.split('%').next()).filter(|s| !s.is_empty()) {
                return exe_stem(exe);
            }
        }
    }
    format!("App {pid}")
}

fn exe_stem(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    match file.rfind('.') {
        Some(dot) if dot > 0 => file[..dot].to_string(),
        _ => file.to_string(),
    }
}

fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(handle);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// The FileDescription string of an exe's version resource.
pub(crate) fn file_description(path: &str) -> Option<String> {
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(wide.as_ptr()), 0, size, data.as_mut_ptr() as *mut c_void).ok()?;

        let query = |sub: &str| -> Option<(*const u8, u32)> {
            let sub: Vec<u16> = sub.encode_utf16().chain(Some(0)).collect();
            let mut ptr: *mut c_void = std::ptr::null_mut();
            let mut len = 0u32;
            VerQueryValueW(data.as_ptr() as *const c_void, PCWSTR(sub.as_ptr()), &mut ptr, &mut len)
                .as_bool()
                .then_some((ptr as *const u8, len))
                .filter(|(p, l)| !p.is_null() && *l > 0)
        };

        let mut langs: Vec<String> = Vec::new();
        if let Some((ptr, len)) = query("\\VarFileInfo\\Translation") {
            let pairs = std::slice::from_raw_parts(ptr as *const u16, (len / 2) as usize);
            for pair in pairs.chunks_exact(2) {
                langs.push(format!("{:04x}{:04x}", pair[0], pair[1]));
            }
        }
        langs.extend(["040904b0".into(), "040904e4".into(), "04090000".into()]);
        for lang in langs {
            if let Some((ptr, len)) = query(&format!("\\StringFileInfo\\{lang}\\FileDescription")) {
                // len is in characters, including the terminator
                let chars = std::slice::from_raw_parts(ptr as *const u16, len as usize);
                let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
                let text = String::from_utf16_lossy(&chars[..end]);
                if !text.trim().is_empty() {
                    return Some(text);
                }
            }
        }
        None
    }
}

// ---------------------------------------------------------------- change watcher

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeCallback {
    on_change: fn(f32, bool),
}

impl IAudioEndpointVolumeCallback_Impl for VolumeCallback_Impl {
    fn OnNotify(&self, data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows::core::Result<()> {
        if let Some(data) = unsafe { data.as_ref() } {
            (self.on_change)(data.fMasterVolume, data.bMuted.as_bool());
        }
        Ok(())
    }
}

#[implement(IMMNotificationClient)]
struct DeviceCallback {
    default_changed: Sender<()>,
}

impl IMMNotificationClient_Impl for DeviceCallback_Impl {
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceAdded(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _id: &PCWSTR) -> windows::core::Result<()> {
        // Never touch the audio API from inside this callback (it can deadlock); just wake the watcher.
        if flow == eRender && role == eMultimedia {
            let _ = self.default_changed.send(());
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Starts (once) a background thread that calls `on_change(level, muted)` whenever the default output's
/// volume or mute changes, and again with the new device's values when the default output changes.
/// The callback runs on a Core Audio worker thread or the watcher thread.
pub fn watch(on_change: fn(f32, bool)) {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("audio-watch".into())
            .spawn(move || watch_loop(on_change));
    });
}

fn watch_loop(on_change: fn(f32, bool)) {
    let _com = ComScope::new();
    let Some(e) = enumerator() else { return };
    let (tx, rx) = channel();
    let client: IMMNotificationClient = DeviceCallback { default_changed: tx }.into();
    if unsafe { e.RegisterEndpointNotificationCallback(&client) }.is_err() {
        return;
    }
    let callback: IAudioEndpointVolumeCallback = VolumeCallback { on_change }.into();
    loop {
        let current = default_device(&e, false).and_then(|d| endpoint_volume(&d));
        if let Some(ep) = &current {
            unsafe {
                let _ = ep.RegisterControlChangeNotify(&callback);
                if let (Ok(level), Ok(muted)) = (ep.GetMasterVolumeLevelScalar(), ep.GetMute()) {
                    on_change(level, muted.as_bool());
                }
            }
        }
        if rx.recv().is_err() {
            break;
        }
        // Switching devices fires a burst of notifications; settle, then follow the final default.
        std::thread::sleep(Duration::from_millis(150));
        while rx.try_recv().is_ok() {}
        if let Some(ep) = &current {
            unsafe {
                let _ = ep.UnregisterControlChangeNotify(&callback);
            }
        }
    }
    unsafe {
        let _ = e.UnregisterEndpointNotificationCallback(&client);
    }
}

// ---------------------------------------------------------------- tests (run manually)

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn audio_print_state() {
        println!("output master: {:?}", master(false));
        println!("input master:  {:?}", master(true));
        for input in [false, true] {
            println!("{} devices:", if input { "input" } else { "output" });
            for d in devices(input) {
                println!("  {}{} [{}]", if d.is_default { "* " } else { "  " }, d.name, d.id);
            }
        }
        println!("sessions:");
        for s in sessions() {
            println!("  pid {:>6}  {:<30} vol {:.2} muted {}", s.pid, s.name, s.volume, s.muted);
        }
    }

    #[test]
    #[ignore]
    fn audio_set_and_restore() {
        // Master volume: nudge and restore (only if unmuted, since a nonzero level unmutes).
        if let Some((level, muted)) = master(false) {
            if !muted {
                let probe = if level > 0.5 { level - 0.02 } else { level + 0.02 };
                set_master(false, probe);
                let got = master(false).unwrap().0;
                set_master(false, level);
                println!("master {level:.3} -> {got:.3} -> {:.3}", master(false).unwrap().0);
                assert!((got - probe).abs() < 0.01);
            }
            set_mute(false, !muted);
            let flipped = master(false).unwrap().1;
            set_mute(false, muted);
            assert_eq!(flipped, !muted);
            assert_eq!(master(false).unwrap().1, muted);
        }
        // Session volume: nudge and restore the first unmuted app session.
        if let Some(s) = sessions().into_iter().find(|s| !s.muted && s.pid != 0) {
            let probe = if s.volume > 0.5 { s.volume - 0.05 } else { s.volume + 0.05 };
            set_session_volume(s.pid, probe);
            let got = sessions().into_iter().find(|x| x.pid == s.pid).map(|x| x.volume);
            set_session_volume(s.pid, s.volume);
            println!("session {} {:.2} -> {:?} -> restored", s.name, s.volume, got);
            assert!((got.unwrap() - probe).abs() < 0.01);
        }
        // Default device: re-set the current default (a no-op change) to prove IPolicyConfig works.
        if let Some(d) = devices(false).into_iter().find(|d| d.is_default) {
            assert!(set_default(&d.id));
            assert!(devices(false).iter().any(|x| x.is_default && x.id == d.id));
        }
    }

    #[test]
    #[ignore]
    fn audio_watch() {
        fn cb(level: f32, muted: bool) {
            println!("watch: level {level:.3} muted {muted}");
        }
        watch(cb);
        std::thread::sleep(Duration::from_millis(300));
        // Provoke a notification and restore.
        if let Some((_, muted)) = master(false) {
            set_mute(false, !muted);
            std::thread::sleep(Duration::from_millis(200));
            set_mute(false, muted);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

// ---------------------------------------------------------------- Bluetooth audio connect

/// Bluetooth audio driver property set: KSPROPERTY_ONESHOT_RECONNECT (0) / _DISCONNECT (1).
const KSPROPSETID_BT_AUDIO: GUID = GUID::from_u128(0x7fa06c40_b8f6_4c7e_8556_e8c33a12e54d);

/// Connects (or disconnects) a paired Bluetooth audio device (headphones, earbuds, a speaker) right away,
/// the way the Bluetooth quick setting does: a one-shot reconnect on the driver behind its audio endpoints.
/// `name` is the device's Bluetooth name; endpoints are matched by it ("Headphones (WH-1000XM4)").
/// False when no Bluetooth audio endpoint of that name answered (not an audio device, or not in range).
pub fn bt_connect(name: &str, connect: bool) -> bool {
    use windows::Win32::Media::Audio::{IConnector, IDeviceTopology, IPart, eAll, DEVICE_STATEMASK_ALL};
    use windows::Win32::Media::KernelStreaming::{IKsControl, KSIDENTIFIER, KSIDENTIFIER_0, KSIDENTIFIER_0_0, KSPROPERTY_TYPE_GET};

    let _com = ComScope::new();
    let Some(e) = enumerator() else { return false };
    let want = name.trim().to_lowercase();
    if want.is_empty() {
        return false;
    }
    let mut done: Vec<String> = Vec::new();
    let mut ok = false;
    unsafe {
        let Ok(list) = e.EnumAudioEndpoints(eAll, DEVICE_STATE(DEVICE_STATEMASK_ALL)) else { return false };
        let n = list.GetCount().unwrap_or(0);
        for i in 0..n {
            let Ok(dev) = list.Item(i) else { continue };
            if !friendly_name(&dev).to_lowercase().contains(&want) {
                continue;
            }
            let Ok(topo) = dev.Activate::<IDeviceTopology>(CLSCTX_ALL, None) else { continue };
            let Ok(conn) = topo.GetConnector(0) else { continue };
            let Ok(other): windows::core::Result<IConnector> = conn.GetConnectedTo() else { continue };
            let Ok(part) = other.cast::<IPart>() else { continue };
            // The driver (KS filter) behind the endpoint must be a Bluetooth one; each is poked once.
            let filter = part.GetTopologyObject().and_then(|t| t.GetDeviceId()).map(|p| take_pwstr(p)).unwrap_or_default();
            let lower = filter.to_lowercase();
            if !(lower.contains("bthenum") || lower.contains("bthhfenum") || lower.contains("bthledevice")) || done.contains(&lower) {
                continue;
            }
            done.push(lower);
            // The command goes to the driver's own device (the KS filter), not to the topology part.
            let Ok(filter_dev) = e.GetDevice(&windows::core::HSTRING::from(filter.as_str())) else { continue };
            let Ok(ks) = filter_dev.Activate::<IKsControl>(CLSCTX_ALL, None) else { continue };
            let prop = KSIDENTIFIER {
                Anonymous: KSIDENTIFIER_0 {
                    Anonymous: KSIDENTIFIER_0_0 { Set: KSPROPSETID_BT_AUDIO, Id: if connect { 0 } else { 1 }, Flags: KSPROPERTY_TYPE_GET },
                },
            };
            let mut ret = 0u32;
            if ks.KsProperty(&prop, std::mem::size_of::<KSIDENTIFIER>() as u32, std::ptr::null_mut(), 0, &mut ret).is_ok() {
                ok = true;
            }
        }
    }
    ok
}

#[cfg(test)]
mod bt_probe {
    use super::*;

    /// Real round trip on one device: FATHNOTCH_BT=<name> cargo test --release bt_round -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bt_round() {
        let name = std::env::var("FATHNOTCH_BT").expect("FATHNOTCH_BT");
        let state = || crate::services::radios::paired().into_iter().find(|b| b.name.contains(&name)).map(|b| b.connected);
        println!("before: {:?}", state());
        println!("disconnect: {}", bt_connect(&name, false));
        std::thread::sleep(std::time::Duration::from_secs(6));
        println!("after disconnect: {:?}", state());
        println!("connect: {}", bt_connect(&name, true));
        std::thread::sleep(std::time::Duration::from_secs(8));
        println!("after connect: {:?}", state());
    }

    /// Dry run of `bt_connect`: prints every audio endpoint and how far the Bluetooth path gets.
    /// cargo test --release bt_probe -- --ignored --nocapture
    #[test]
    #[ignore]
    fn probe() {
        use windows::Win32::Media::Audio::{IConnector, IDeviceTopology, IPart, eAll, DEVICE_STATEMASK_ALL};
        use windows::Win32::Media::KernelStreaming::IKsControl;
        let _com = ComScope::new();
        let e = enumerator().expect("enumerator");
        unsafe {
            let list = e.EnumAudioEndpoints(eAll, DEVICE_STATE(DEVICE_STATEMASK_ALL)).expect("enum");
            for i in 0..list.GetCount().unwrap_or(0) {
                let Ok(dev) = list.Item(i) else { continue };
                let name = friendly_name(&dev);
                let state = dev.GetState().map(|s| s.0).unwrap_or(99);
                print!("[{state}] {name}: ");
                let topo = match dev.Activate::<IDeviceTopology>(CLSCTX_ALL, None) { Ok(t) => t, Err(e) => { println!("topology {e}"); continue } };
                let conn = match topo.GetConnector(0) { Ok(c) => c, Err(e) => { println!("connector {e}"); continue } };
                let other: IConnector = match conn.GetConnectedTo() { Ok(c) => c, Err(e) => { println!("connectedTo {e}"); continue } };
                let part = match other.cast::<IPart>() { Ok(p) => p, Err(e) => { println!("part {e}"); continue } };
                let filter = part.GetTopologyObject().and_then(|t| t.GetDeviceId()).map(|p| take_pwstr(p)).unwrap_or_default();
                let ks = e.GetDevice(&windows::core::HSTRING::from(filter.as_str())).and_then(|d| d.Activate::<IKsControl>(CLSCTX_ALL, None));
                println!("filter={filter} ks={:?}", ks.map(|_| true));
            }
        }
    }
}
