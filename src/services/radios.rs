//! Wi-Fi, Bluetooth and mobile-hotspot switches for the Control tab, the Wi-Fi network list, paired
//! Bluetooth devices, and Windows' "notification sounds" switch (silent mode).
//!
//! Everything goes through Windows' own APIs (the same switches as Quick Settings). All calls block; run
//! them on a COM-initialised background thread.

use std::collections::HashMap;
use std::sync::OnceLock;

use windows::core::{Interface, HSTRING};
use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothDevice, BluetoothLEDevice};
use windows::Devices::Enumeration::DeviceInformation;
use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};
use windows::Devices::WiFi::{
    WiFiAccessStatus, WiFiAdapter, WiFiAvailableNetwork, WiFiConnectionStatus, WiFiReconnectionKind,
};
use windows::Foundation::IReference;
use windows::Networking::Connectivity::{ConnectionProfile, NetworkAuthenticationType, NetworkInformation};
use windows::Networking::NetworkOperators::{
    NetworkOperatorTetheringManager, TetheringOperationStatus, TetheringOperationalState,
};
use windows::Security::Credentials::PasswordCredential;
use windows::Win32::System::Registry::{RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD};
use crate::i18n::tr;

// ---------------------------------------------------------------- radios (Wi-Fi / Bluetooth on-off)

fn radio(kind: RadioKind) -> Option<Radio> {
    static ACCESS: OnceLock<bool> = OnceLock::new();
    let allowed = *ACCESS.get_or_init(|| {
        Radio::RequestAccessAsync()
            .and_then(|op| op.get())
            .map(|s| s == RadioAccessStatus::Allowed)
            .unwrap_or(false)
    });
    if !allowed {
        return None;
    }
    let radios = Radio::GetRadiosAsync().ok()?.get().ok()?;
    radios.into_iter().find(|r| r.Kind().ok() == Some(kind))
}

fn radio_on(kind: RadioKind) -> Option<bool> {
    Some(radio(kind)?.State().ok()? == RadioState::On)
}

fn set_radio(kind: RadioKind, on: bool) -> bool {
    let Some(r) = radio(kind) else { return false };
    let state = if on { RadioState::On } else { RadioState::Off };
    r.SetStateAsync(state)
        .and_then(|op| op.get())
        .map(|s| s == RadioAccessStatus::Allowed)
        .unwrap_or(false)
}

/// Whether the Wi-Fi radio is on; None when there's no Wi-Fi radio (or access was refused).
pub fn wifi() -> Option<bool> {
    radio_on(RadioKind::WiFi)
}

/// Turns Wi-Fi on or off; false when Windows refused (airplane mode, policy, no radio).
pub fn set_wifi(on: bool) -> bool {
    set_radio(RadioKind::WiFi, on)
}

pub fn bluetooth() -> Option<bool> {
    radio_on(RadioKind::Bluetooth)
}

pub fn set_bluetooth(on: bool) -> bool {
    set_radio(RadioKind::Bluetooth, on)
}

// ---------------------------------------------------------------- Wi-Fi networks

#[derive(Clone, Debug)]
pub struct Network {
    pub ssid: String,
    /// 0..=4 bars.
    pub signal: u8,
    pub secured: bool,
    pub connected: bool,
}

/// The first Wi-Fi adapter, when apps may use it. On Windows 11 scanning needs location access for
/// desktop apps ("Let desktop apps access your location"); without it the scan comes back empty.
fn adapter() -> Option<WiFiAdapter> {
    let access = WiFiAdapter::RequestAccessAsync().ok()?.get().ok()?;
    if access != WiFiAccessStatus::Allowed {
        return None;
    }
    WiFiAdapter::FindAllAdaptersAsync().ok()?.get().ok()?.into_iter().next()
}

fn connected_ssid(adapter: &WiFiAdapter) -> Option<String> {
    let profile = adapter.NetworkAdapter().ok()?.GetConnectedProfileAsync().ok()?.get().ok()?;
    wlan_ssid(&profile)
}

fn wlan_ssid(profile: &ConnectionProfile) -> Option<String> {
    if !profile.IsWlanConnectionProfile().unwrap_or(false) {
        return None;
    }
    let ssid = profile
        .WlanConnectionProfileDetails()
        .and_then(|d| d.GetConnectedSsid())
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    if !ssid.is_empty() {
        return Some(ssid);
    }
    profile.ProfileName().ok().map(|s| s.to_string_lossy()).filter(|s| !s.is_empty())
}

fn is_secured(n: &WiFiAvailableNetwork) -> bool {
    let auth = n
        .SecuritySettings()
        .and_then(|s| s.NetworkAuthenticationType())
        .unwrap_or(NetworkAuthenticationType::None);
    !matches!(auth, NetworkAuthenticationType::None | NetworkAuthenticationType::Open80211 | NetworkAuthenticationType::Owe)
}

/// The adapter's latest report, strongest entry per SSID (hidden networks skipped).
fn report(adapter: &WiFiAdapter) -> HashMap<String, WiFiAvailableNetwork> {
    let mut best: HashMap<String, WiFiAvailableNetwork> = HashMap::new();
    let Ok(list) = adapter.NetworkReport().and_then(|r| r.AvailableNetworks()) else { return best };
    for n in list {
        let ssid = n.Ssid().map(|s| s.to_string_lossy()).unwrap_or_default();
        if ssid.trim().is_empty() {
            continue;
        }
        let bars = n.SignalBars().unwrap_or(0);
        match best.get(&ssid) {
            Some(old) if old.SignalBars().unwrap_or(0) >= bars => {}
            _ => {
                best.insert(ssid, n);
            }
        }
    }
    best
}

/// Scans and lists networks in range: the connected one first, then strongest, then by name.
/// Empty when there's no adapter or Windows won't let the app scan.
pub fn networks() -> Vec<Network> {
    let Some(adapter) = adapter() else { return Vec::new() };
    if let Ok(op) = adapter.ScanAsync() {
        let _ = op.get();
    }
    let connected = connected_ssid(&adapter).or_else(|| {
        NetworkInformation::GetInternetConnectionProfile().ok().and_then(|p| wlan_ssid(&p))
    });
    let mut list: Vec<Network> = report(&adapter)
        .into_iter()
        .map(|(ssid, n)| Network {
            signal: n.SignalBars().unwrap_or(0).min(4),
            secured: is_secured(&n),
            connected: connected.as_deref() == Some(ssid.as_str()),
            ssid,
        })
        .collect();
    list.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.signal.cmp(&a.signal))
            .then_with(|| a.ssid.to_lowercase().cmp(&b.ssid.to_lowercase()))
    });
    list
}

/// Connects to `ssid` and reconnects automatically later. Saved and open networks need no password.
/// Errors: "password" when the network needs a (correct) password, otherwise a short reason.
pub fn connect(ssid: &str, password: Option<&str>) -> Result<(), String> {
    let adapter = adapter().ok_or(tr("Wi-Fi isn't available"))?;
    let mut found = report(&adapter).remove(ssid);
    if found.is_none() {
        if let Ok(op) = adapter.ScanAsync() {
            let _ = op.get();
        }
        found = report(&adapter).remove(ssid);
    }
    let network = found.ok_or(tr("Out of range"))?;
    let result = match password {
        Some(p) => {
            let cred = PasswordCredential::new().map_err(|e| e.message())?;
            cred.SetPassword(&HSTRING::from(p)).map_err(|e| e.message())?;
            adapter.ConnectWithPasswordCredentialAsync(&network, WiFiReconnectionKind::Automatic, &cred)
        }
        None => adapter.ConnectAsync(&network, WiFiReconnectionKind::Automatic),
    }
    .and_then(|op| op.get())
    .and_then(|r| r.ConnectionStatus())
    .map_err(|_| "Couldn't connect".to_string())?;
    match result {
        WiFiConnectionStatus::Success => Ok(()),
        WiFiConnectionStatus::InvalidCredential => Err("password".into()),
        WiFiConnectionStatus::Timeout => Err(tr("The network didn't answer").into()),
        WiFiConnectionStatus::NetworkNotAvailable => Err(tr("Out of range").into()),
        WiFiConnectionStatus::AccessRevoked => Err(tr("Wi-Fi access is turned off for apps").into()),
        WiFiConnectionStatus::UnsupportedAuthenticationProtocol => Err(tr("This network's security isn't supported").into()),
        _ => Err(tr("Couldn't connect").into()),
    }
}

pub fn disconnect() {
    if let Some(a) = adapter() {
        let _ = a.Disconnect();
    }
}

/// The current Wi-Fi network's name, "Ethernet" when online over something else, None when offline.
pub fn connected_name() -> Option<String> {
    let profile = NetworkInformation::GetInternetConnectionProfile().ok()?;
    if profile.IsWlanConnectionProfile().unwrap_or(false) {
        return Some(wlan_ssid(&profile).unwrap_or_else(|| tr("Wi-Fi").into()));
    }
    if profile.IsWwanConnectionProfile().unwrap_or(false) {
        return Some(tr("Cellular").into());
    }
    Some(tr("Ethernet").into())
}

// ---------------------------------------------------------------- Bluetooth devices

#[derive(Clone, Debug)]
pub struct BtDevice {
    pub id: String,
    pub name: String,
    pub connected: bool,
}

const IS_CONNECTED: &str = "System.Devices.Aep.IsConnected";

fn prop_bool(info: &DeviceInformation, key: &str) -> Option<bool> {
    let value = info.Properties().ok()?.Lookup(&HSTRING::from(key)).ok()?;
    value.cast::<IReference<bool>>().ok()?.Value().ok()
}

fn device_connected(id: &HSTRING, le: bool) -> Option<bool> {
    if le {
        let d = BluetoothLEDevice::FromIdAsync(id).ok()?.get().ok()?;
        let c = d.ConnectionStatus().ok()? == BluetoothConnectionStatus::Connected;
        let _ = d.Close();
        Some(c)
    } else {
        let d = BluetoothDevice::FromIdAsync(id).ok()?.get().ok()?;
        let c = d.ConnectionStatus().ok()? == BluetoothConnectionStatus::Connected;
        let _ = d.Close();
        Some(c)
    }
}

/// Paired Bluetooth devices (classic and LE), connected ones first. A device paired both ways (some
/// headphones) is listed once.
pub fn paired() -> Vec<BtDevice> {
    let mut list: Vec<BtDevice> = Vec::new();
    let selectors = [
        (BluetoothDevice::GetDeviceSelectorFromPairingState(true), false),
        (BluetoothLEDevice::GetDeviceSelectorFromPairingState(true), true),
    ];
    for (selector, le) in selectors {
        let Ok(selector) = selector else { continue };
        let Ok(found) = DeviceInformation::FindAllAsyncAqsFilter(&selector).and_then(|op| op.get()) else {
            continue;
        };
        for info in found {
            let name = info.Name().map(|s| s.to_string_lossy()).unwrap_or_default();
            if name.trim().is_empty() {
                continue;
            }
            let id = info.Id().unwrap_or_default();
            // The association endpoint usually carries IsConnected; otherwise ask the device itself.
            let connected = prop_bool(&info, IS_CONNECTED)
                .or_else(|| device_connected(&id, le))
                .unwrap_or(false);
            if let Some(d) = list.iter_mut().find(|d| d.name == name) {
                d.connected |= connected;
                continue;
            }
            list.push(BtDevice { id: id.to_string_lossy(), name, connected });
        }
    }
    list.sort_by(|a, b| b.connected.cmp(&a.connected).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    list
}

// ---------------------------------------------------------------- mobile hotspot

/// The hotspot shares the PC's internet connection, so it needs one.
fn tethering() -> Option<NetworkOperatorTetheringManager> {
    let profile = NetworkInformation::GetInternetConnectionProfile().ok()?;
    NetworkOperatorTetheringManager::CreateFromConnectionProfile(&profile).ok()
}

/// Whether the mobile hotspot is on; None when there's no connection to share or no hotspot support.
pub fn hotspot() -> Option<bool> {
    let state = tethering()?.TetheringOperationalState().ok()?;
    match state {
        TetheringOperationalState::On | TetheringOperationalState::InTransition => Some(true),
        TetheringOperationalState::Off => Some(false),
        _ => None,
    }
}

/// Number of devices on the hotspot (0 when off).
pub fn hotspot_clients() -> u32 {
    tethering().and_then(|m| m.ClientCount().ok()).unwrap_or(0)
}

pub fn set_hotspot(on: bool) -> Result<(), String> {
    let manager = tethering().ok_or(tr("No internet connection to share"))?;
    let op = if on { manager.StartTetheringAsync() } else { manager.StopTetheringAsync() };
    let result = op.and_then(|o| o.get()).map_err(|e| e.message())?;
    match result.Status() {
        Ok(TetheringOperationStatus::Success) => Ok(()),
        status => {
            let detail = result.AdditionalErrorMessage().map(|s| s.to_string_lossy()).unwrap_or_default();
            if !detail.is_empty() {
                return Err(detail);
            }
            Err(match status {
                Ok(TetheringOperationStatus::WiFiDeviceOff) => tr("Wi-Fi is off").into(),
                Ok(TetheringOperationStatus::BluetoothDeviceOff) => tr("Bluetooth is off").into(),
                Ok(TetheringOperationStatus::OperationInProgress) => tr("Already switching").into(),
                Ok(TetheringOperationStatus::EntitlementCheckFailure) => tr("Your carrier doesn't allow sharing").into(),
                _ => tr("Windows couldn't switch the hotspot").into(),
            })
        }
    }
}

// ---------------------------------------------------------------- silent notifications

const SOUND_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Notifications\Settings";
const SOUND_VALUE: &str = "NOC_GLOBAL_SETTING_ALLOW_NOTIFICATION_SOUND";

/// True when Windows' "Allow notifications to play sounds" is off.
pub fn silent() -> bool {
    let mut value: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(SOUND_KEY),
            &HSTRING::from(SOUND_VALUE),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    err.is_ok() && value == 0
}

/// Turns notification sounds off (`on` = silent) or back on.
pub fn set_silent(on: bool) {
    let value: u32 = if on { 0 } else { 1 };
    unsafe {
        let _ = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(SOUND_KEY),
            &HSTRING::from(SOUND_VALUE),
            REG_DWORD.0,
            Some(&value as *const u32 as *const _),
            std::mem::size_of::<u32>() as u32,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn com() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
    }

    #[test]
    #[ignore]
    fn radios_report() {
        com();
        println!("wifi on: {:?}", wifi());
        println!("bluetooth on: {:?}", bluetooth());
        println!("connected: {:?}", connected_name());
        println!("hotspot: {:?} ({} clients)", hotspot(), hotspot_clients());
        println!("silent: {}", silent());
        let nets = networks();
        println!("{} networks", nets.len());
        for n in nets.iter().take(10) {
            println!("  {:?}", n);
        }
        for d in paired() {
            println!("  bt {:?}", d);
        }
    }
}

#[cfg(test)]
mod timing {
    /// How long each read takes: cargo test --release radios_timing -- --ignored --nocapture
    #[test]
    #[ignore]
    fn radios_timing() {
        let t = |name: &str, f: &dyn Fn()| {
            let s = std::time::Instant::now();
            f();
            println!("{name}: {} ms", s.elapsed().as_millis());
        };
        for _ in 0..2 {
            t("wifi", &|| drop(super::wifi()));
            t("bluetooth", &|| drop(super::bluetooth()));
            t("hotspot", &|| drop(super::hotspot()));
            t("connected_name", &|| drop(super::connected_name()));
            t("paired", &|| drop(super::paired()));
            t("audio devices", &|| drop(crate::services::audio::devices(false)));
            t("audio sessions", &|| drop(crate::services::audio::sessions()));
        }
    }
}
