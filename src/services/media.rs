//! What's playing, from Windows' global media session (Spotify, browsers, Media Player, …), and the
//! transport controls. `start` subscribes once; the state is kept here and the UI reads `current()`.
//! Songs played from this PC (`player`) take over while they play, or while loaded and nothing else plays.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use windows::Foundation::{EventRegistrationToken, TypedEventHandler};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::{DataReader, IInputStream};
use windows::core::Interface;

#[derive(Clone, Debug)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Friendly name of the app playing ("Spotify", "Microsoft Edge", …).
    pub app: String,
    pub playing: bool,
    /// Seconds, as of `updated`.
    pub position: f64,
    /// Seconds; 0 when the player doesn't say.
    pub duration: f64,
    pub updated: Instant,
    /// Encoded thumbnail (PNG/JPEG) as the player supplied it.
    pub art: Option<Arc<Vec<u8>>>,
    /// Changes whenever the art changes (0 = no art).
    pub art_id: u64,
}

impl NowPlaying {
    fn empty(app: String) -> Self {
        NowPlaying {
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            app,
            playing: false,
            position: 0.0,
            duration: 0.0,
            updated: Instant::now(),
            art: None,
            art_id: 0,
        }
    }

    /// The playhead now, extrapolated while playing.
    pub fn position_now(&self) -> f64 {
        let mut p = self.position;
        if self.playing {
            p += self.updated.elapsed().as_secs_f64();
        }
        if self.duration > 0.0 {
            p.clamp(0.0, self.duration)
        } else {
            p.max(0.0)
        }
    }
}

struct Hooked {
    session: Session,
    tokens: [EventRegistrationToken; 3],
}

struct State {
    manager: Option<Manager>,
    hooked: Option<Hooked>,
    now: Option<NowPlaying>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State { manager: None, hooked: None, now: None }))
}

fn lock() -> std::sync::MutexGuard<'static, State> {
    state().lock().unwrap_or_else(|e| e.into_inner())
}

/// Subscribes to the media session (once). Safe to call again; later calls do nothing.
pub fn start() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let Ok(manager) = Manager::RequestAsync().and_then(|op| op.get()) else { return };
    let _ = manager.CurrentSessionChanged(&TypedEventHandler::new(|m: &Option<Manager>, _| {
        if let Some(m) = m {
            hook(m.GetCurrentSession().ok());
        }
        Ok(())
    }));
    // Some players come and go without the current session changing; re-check then too.
    let _ = manager.SessionsChanged(&TypedEventHandler::new(|m: &Option<Manager>, _| {
        if let Some(m) = m {
            let now = m.GetCurrentSession().ok();
            let same = {
                let s = lock();
                match (&s.hooked, &now) {
                    (Some(h), Some(n)) => h.session.as_raw() == n.as_raw(),
                    (None, None) => true,
                    _ => false,
                }
            };
            if !same {
                hook(now);
            }
        }
        Ok(())
    }));
    let current = manager.GetCurrentSession().ok();
    lock().manager = Some(manager);
    hook(current);
}

/// Follows `session` (or nothing), dropping the previous one's subscriptions.
fn hook(session: Option<Session>) {
    static HOOKING: Mutex<()> = Mutex::new(());
    let _serial = HOOKING.lock().unwrap_or_else(|e| e.into_inner());
    let old = {
        let mut s = lock();
        s.now = session.as_ref().map(|x| NowPlaying::empty(app_name(x)));
        s.hooked.take()
    };
    if let Some(old) = old {
        let _ = old.session.RemoveMediaPropertiesChanged(old.tokens[0]);
        let _ = old.session.RemovePlaybackInfoChanged(old.tokens[1]);
        let _ = old.session.RemoveTimelinePropertiesChanged(old.tokens[2]);
    }
    let Some(session) = session else {
        crate::bg::refresh();
        return;
    };
    let t0 = session
        .MediaPropertiesChanged(&TypedEventHandler::new(|s: &Option<Session>, _| {
            if let Some(s) = s {
                refresh_media(s);
            }
            Ok(())
        }))
        .unwrap_or_default();
    let t1 = session
        .PlaybackInfoChanged(&TypedEventHandler::new(|s: &Option<Session>, _| {
            if let Some(s) = s {
                refresh_playback(s);
            }
            Ok(())
        }))
        .unwrap_or_default();
    let t2 = session
        .TimelinePropertiesChanged(&TypedEventHandler::new(|s: &Option<Session>, _| {
            if let Some(s) = s {
                refresh_timeline(s);
            }
            Ok(())
        }))
        .unwrap_or_default();
    lock().hooked = Some(Hooked { session: session.clone(), tokens: [t0, t1, t2] });
    refresh_playback(&session);
    refresh_timeline(&session);
    refresh_media(&session);
}

/// Runs `f` on the shared state if `session` is still the one being followed, then asks for a redraw.
fn update(session: &Session, f: impl FnOnce(&mut NowPlaying)) {
    let mut s = lock();
    let current = s.hooked.as_ref().is_some_and(|h| h.session.as_raw() == session.as_raw());
    if !current {
        return;
    }
    if let Some(now) = s.now.as_mut() {
        f(now);
    }
    drop(s);
    crate::stat("media update");
    crate::bg::refresh();
}

fn refresh_media(session: &Session) {
    let Ok(props) = session.TryGetMediaPropertiesAsync().and_then(|op| op.get()) else { return };
    let text = |r: windows::core::Result<windows::core::HSTRING>| r.map(|s| s.to_string_lossy()).unwrap_or_default();
    let title = text(props.Title());
    let mut artist = text(props.Artist());
    let album = text(props.AlbumTitle());
    if artist.trim().is_empty() {
        artist = text(props.AlbumArtist());
    }
    // YouTube's auto-generated music channels: "Taylor Swift - Topic" → "Taylor Swift".
    if let Some(stripped) = artist.strip_suffix(" - Topic") {
        artist = stripped.to_string();
    }
    let art = props.Thumbnail().ok().and_then(|t| read_thumbnail(&t));
    let art_id = art.as_ref().map(|bytes| {
        let mut h = DefaultHasher::new();
        bytes.hash(&mut h);
        h.finish() | 1
    });
    update(session, |now| {
        now.title = title;
        now.artist = artist;
        now.album = album;
        now.art_id = art_id.unwrap_or(0);
        now.art = art.map(Arc::new);
    });
}

fn read_thumbnail(r: &windows::Storage::Streams::IRandomAccessStreamReference) -> Option<Vec<u8>> {
    let stream = r.OpenReadAsync().ok()?.get().ok()?;
    let size = stream.Size().ok()? as u32;
    if size == 0 || size > 16 * 1024 * 1024 {
        return None;
    }
    let input: IInputStream = stream.GetInputStreamAt(0).ok()?;
    let reader = DataReader::CreateDataReader(&input).ok()?;
    let got = reader.LoadAsync(size).ok()?.get().ok()?;
    let mut bytes = vec![0u8; got as usize];
    reader.ReadBytes(&mut bytes).ok()?;
    let _ = stream.cast::<windows::Foundation::IClosable>().map(|c| c.Close());
    (!bytes.is_empty()).then_some(bytes)
}

fn refresh_playback(session: &Session) {
    let playing = session
        .GetPlaybackInfo()
        .and_then(|i| i.PlaybackStatus())
        .map(|s| s == Status::Playing)
        .unwrap_or(false);
    update(session, |now| {
        if now.playing != playing {
            // Freeze (or restart) the extrapolation at the moment the state flips.
            now.position = now.position_now();
            now.updated = Instant::now();
            now.playing = playing;
        }
    });
}

/// Current time as a Windows FILETIME-style tick count (100 ns since 1601), like WinRT DateTime.
fn now_ticks() -> i64 {
    const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    EPOCH_DIFF + (since.as_nanos() / 100) as i64
}

fn refresh_timeline(session: &Session) {
    let Ok(t) = session.GetTimelineProperties() else { return };
    let ticks = |v: i64| v as f64 / 10_000_000.0;
    let start = t.StartTime().map(|d| d.Duration).unwrap_or(0);
    let end = t.EndTime().map(|d| d.Duration).unwrap_or(0);
    let pos = t.Position().map(|d| d.Duration).unwrap_or(0);
    let stamp = t.LastUpdatedTime().map(|d| d.UniversalTime).unwrap_or(0);
    let duration = ticks((end - start).max(0));
    let mut position = ticks(pos - start).max(0.0);
    let playing = session
        .GetPlaybackInfo()
        .and_then(|i| i.PlaybackStatus())
        .map(|s| s == Status::Playing)
        .unwrap_or(false);
    if playing && stamp > 0 {
        let age = ticks(now_ticks() - stamp);
        if age > 0.0 && age < 24.0 * 3600.0 {
            position += age;
        }
    }
    update(session, |now| {
        now.duration = duration;
        now.position = if duration > 0.0 { position.min(duration) } else { position };
        now.updated = Instant::now();
        now.playing = playing;
    });
}

/// "Spotify.exe" → "Spotify", "MSEdge" → "Microsoft Edge", "Microsoft.ZuneMusic_…!Microsoft.ZuneMusic" → "Media Player".
fn app_name(session: &Session) -> String {
    let aumid = session.SourceAppUserModelId().map(|s| s.to_string_lossy()).unwrap_or_default();
    pretty_app(&aumid)
}

fn pretty_app(aumid: &str) -> String {
    if aumid.is_empty() {
        return String::new();
    }
    let lower = aumid.to_lowercase();
    if lower.contains("zune") {
        return "Media Player".into();
    }
    let mut name = aumid.rsplit('!').next().unwrap_or(aumid);
    name = name.rsplit('\\').next().unwrap_or(name);
    if name.len() > 4 && name[name.len() - 4..].eq_ignore_ascii_case(".exe") {
        name = &name[..name.len() - 4];
    }
    let first = name.split(['.', '_']).find(|s| !s.is_empty()).unwrap_or(name);
    let known = match first.to_lowercase().as_str() {
        "msedge" => Some("Microsoft Edge"),
        "chrome" => Some("Google Chrome"),
        "firefox" => Some("Firefox"),
        "spotify" | "spotifyab" => Some("Spotify"),
        "brave" => Some("Brave"),
        "opera" => Some("Opera"),
        "vlc" => Some("VLC"),
        "applemusic" | "appleinc" if lower.contains("music") => Some("Apple Music"),
        _ => None,
    };
    if let Some(k) = known {
        return k.into();
    }
    if lower.contains("spotify") {
        return "Spotify".into();
    }
    let mut c = first.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => aumid.to_string(),
    }
}

/// Our own player's session, as Windows lists it (the local player reports itself directly instead).
fn is_ours(n: &NowPlaying) -> bool {
    n.app.eq_ignore_ascii_case("fathnotch")
}

/// What another app is playing (Windows' current media session).
fn system() -> Option<NowPlaying> {
    lock().now.clone().filter(|n| !is_ours(n))
}

/// Whether the transport drives the songs played from this PC (`player`): while they play, or while
/// they're loaded and no other app is playing.
fn local_first(local: &Option<NowPlaying>, system: &Option<NowPlaying>) -> bool {
    match local {
        None => false,
        Some(l) => l.playing || !system.as_ref().is_some_and(|s| s.playing && !s.title.is_empty()),
    }
}

/// What's playing now, or None when nothing is (no app has a media session, no song loaded here).
pub fn current() -> Option<NowPlaying> {
    let local = super::player::current();
    let system = system();
    if local_first(&local, &system) {
        local
    } else {
        system
    }
}

fn local_in_charge() -> bool {
    local_first(&super::player::current(), &system())
}

/// Pauses the other app that's playing (songs from this PC are starting).
pub fn pause_others() {
    let playing = {
        let s = lock();
        s.hooked.as_ref().map(|h| h.session.clone()).filter(|_| s.now.as_ref().is_some_and(|n| n.playing && !is_ours(n)))
    };
    if let Some(s) = playing {
        let _ = s.TryPauseAsync().and_then(|op| op.get());
    }
}

fn session() -> Option<Session> {
    lock().hooked.as_ref().map(|h| h.session.clone())
}

pub fn play_pause() {
    if local_in_charge() {
        return super::player::play_pause();
    }
    if let Some(s) = session() {
        let _ = s.TryTogglePlayPauseAsync().and_then(|op| op.get());
    }
}

pub fn next() {
    if local_in_charge() {
        return super::player::next();
    }
    if let Some(s) = session() {
        let _ = s.TrySkipNextAsync().and_then(|op| op.get());
    }
}

pub fn previous() {
    if local_in_charge() {
        return super::player::previous();
    }
    if let Some(s) = session() {
        let _ = s.TrySkipPreviousAsync().and_then(|op| op.get());
    }
}

/// Jumps to `seconds` into the track.
pub fn seek(seconds: f64) {
    if local_in_charge() {
        return super::player::seek(seconds);
    }
    let Some(s) = session() else { return };
    let ticks = (seconds.max(0.0) * 10_000_000.0) as i64;
    let ok = s.TryChangePlaybackPositionAsync(ticks).and_then(|op| op.get()).unwrap_or(false);
    if ok {
        // Show the new position right away; the player's timeline update follows.
        update(&s, |now| {
            now.position = seconds.max(0.0);
            now.updated = Instant::now();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_names() {
        assert_eq!(pretty_app("Spotify.exe"), "Spotify");
        assert_eq!(pretty_app("MSEdge"), "Microsoft Edge");
        assert_eq!(pretty_app("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"), "Media Player");
        assert_eq!(pretty_app("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), "Spotify");
        assert_eq!(pretty_app("Chrome"), "Google Chrome");
        assert!(is_ours(&NowPlaying::empty(pretty_app("fathnotch.exe"))));
        assert!(is_ours(&NowPlaying::empty(pretty_app("C:\\Tools\\FathNotch.exe"))));
    }

    #[test]
    fn local_or_system() {
        let np = |playing: bool| Some(NowPlaying { title: "x".into(), playing, ..NowPlaying::empty("App".into()) });
        assert!(!local_first(&None, &np(true)));
        assert!(local_first(&np(true), &np(true)), "this PC's songs while they play");
        assert!(local_first(&np(false), &np(false)), "paused here, nothing else playing");
        assert!(local_first(&np(false), &None));
        assert!(!local_first(&np(false), &np(true)), "another app playing wins over a paused song here");
    }

    #[test]
    #[ignore]
    fn media_report() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        start();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        match current() {
            Some(n) => println!(
                "app={:?} title={:?} artist={:?} album={:?} playing={} pos={:.1}/{:.1} art={} bytes id={:x}",
                n.app,
                n.title,
                n.artist,
                n.album,
                n.playing,
                n.position_now(),
                n.duration,
                n.art.as_ref().map(|a| a.len()).unwrap_or(0),
                n.art_id
            ),
            None => println!("nothing playing"),
        }
    }
}
