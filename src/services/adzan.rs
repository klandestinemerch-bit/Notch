//! The adzan: an audio file of the call to prayer, played when a prayer time comes (Settings → Prayer →
//! Adzan). Subuh can have its own file (the Subuh adzan has the extra "ash-shalatu khairum minan naum").
//!
//! It has its own MediaPlayer, kept out of Windows' media controls, so it never shows as "now playing" or
//! answers the media keys; songs here and other players are paused while it plays. Blocking (WinRT async
//! calls): run `play` / `play_file` from `bg::spawn`.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

use windows::core::{IInspectable, HSTRING};
use windows::Foundation::TypedEventHandler;
use windows::Media::Core::MediaSource;
use windows::Media::Playback::{MediaPlayer, MediaPlayerAudioCategory, MediaPlayerFailedEventArgs};
use windows::Storage::StorageFile;

use super::settings;

/// The prayers that can have the adzan (`prayer::IDS` without sunrise), with their `prayer::NAMES` index.
pub const PRAYERS: [(&str, usize); 5] = [("fajr", 0), ("dhuhr", 2), ("asr", 3), ("maghrib", 4), ("isha", 5)];

#[derive(Default)]
struct State {
    player: Option<MediaPlayer>,
    /// What's playing (a prayer name, or "" for a test), None when quiet.
    playing: Option<String>,
}

fn lock() -> MutexGuard<'static, State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// The file for prayer `id` ("fajr" may have its own), if one is set and still there.
pub fn file_for(id: &str) -> Option<String> {
    let s = settings::shared();
    let f = if id == "fajr" && !s.adzan_fajr_file.trim().is_empty() { &s.adzan_fajr_file } else { &s.adzan_file };
    let f = f.trim();
    (!f.is_empty() && Path::new(f).is_file()).then(|| f.to_string())
}

/// Whether the adzan should sound for prayer `id`: turned on, chosen for that prayer, a file set, and the
/// Control tab's Silent off.
pub fn wanted(id: &str) -> bool {
    let s = settings::shared();
    s.adzan_enabled && !s.silent_mode && s.adzan_prayers.iter().any(|p| p == id) && file_for(id).is_some()
}

/// What's playing: Some(prayer name, or "" for a test) while the adzan sounds.
pub fn playing() -> Option<String> {
    lock().playing.clone()
}

/// The adzan for prayer `id`; `name` is shown while it plays. False when it couldn't start.
pub fn play(id: &str, name: &str) -> bool {
    match file_for(id) {
        Some(path) => play_file(&path, name),
        None => false,
    }
}

/// Plays `path` as the adzan (also the settings' Test button, with `name` "").
pub fn play_file(path: &str, name: &str) -> bool {
    stop();
    let Some(p) = player() else { return false };
    let Ok(file) = StorageFile::GetFileFromPathAsync(&HSTRING::from(path)).and_then(|op| op.get()) else { return false };
    let Ok(source) = MediaSource::CreateFromStorageFile(&file) else { return false };
    let _ = p.SetVolume(settings::shared().adzan_volume.clamp(0.0, 1.0));
    if p.SetSource(&source).is_err() {
        return false;
    }
    // Quiet the music first.
    super::player::pause_if_playing();
    super::media::pause_others();
    if p.Play().is_err() {
        return false;
    }
    lock().playing = Some(name.to_string());
    crate::bg::refresh();
    true
}

pub fn stop() {
    let p = {
        let mut s = lock();
        s.playing = None;
        s.player.clone()
    };
    if let Some(p) = p {
        let _ = p.Pause();
    }
    crate::bg::refresh();
}

/// Volume 0..1 (applies to an adzan that's playing too).
pub fn set_volume(v: f64) {
    if let Some(p) = lock().player.clone() {
        let _ = p.SetVolume(v.clamp(0.0, 1.0));
    }
}

/// The adzan's player (made once), outside Windows' media controls.
fn player() -> Option<MediaPlayer> {
    if let Some(p) = lock().player.clone() {
        return Some(p);
    }
    let p = MediaPlayer::new().ok()?;
    let _ = p.SetAudioCategory(MediaPlayerAudioCategory::Media);
    let _ = p.SetAutoPlay(false);
    if let Ok(cm) = p.CommandManager() {
        let _ = cm.SetIsEnabled(false);
    }
    let done = |_: &Option<MediaPlayer>, _: &Option<IInspectable>| {
        lock().playing = None;
        crate::bg::refresh();
        Ok(())
    };
    let _ = p.MediaEnded(&TypedEventHandler::new(done));
    let _ = p.MediaFailed(&TypedEventHandler::new(|_: &Option<MediaPlayer>, _: &Option<MediaPlayerFailedEventArgs>| {
        lock().playing = None;
        crate::bg::refresh();
        Ok(())
    }));
    let mut s = lock();
    if let Some(existing) = s.player.clone() {
        return Some(existing);
    }
    s.player = Some(p.clone());
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prayers_line_up_with_the_names() {
        for (id, i) in PRAYERS {
            assert_eq!(crate::services::prayer::IDS[i], id);
        }
    }
}
