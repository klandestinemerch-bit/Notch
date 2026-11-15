//! Music files on this PC: a queue played with Windows' own MediaPlayer (Media Foundation: MP3, AAC/M4A,
//! WAV, FLAC, WMA, ALAC; Ogg and Opus once Microsoft's Web Media Extensions are installed).
//!
//! While a song is loaded here, `media::current()` reports it and the transport buttons drive it, so the
//! Music tool, the Home card, the pill, "Announce new songs" and synced lyrics all work as they do for any
//! other player. The queue (with each song's tags), the place in it, shuffle and repeat live in player.json.
//!
//! Everything here blocks (file access, WinRT async calls): run it from `bg::spawn`, never on the window
//! thread, except the cheap readers `current`, `view` and `is_audio`. Player methods are never called with
//! the state locked: MediaPlayer may raise its events on the calling thread, and they lock it too.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use windows::core::{IInspectable, HSTRING};
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Media::Core::MediaSource;
use windows::Media::MediaPlaybackType;
use windows::Media::Playback::{
    MediaCommandEnablingRule, MediaPlaybackCommandManager, MediaPlaybackCommandManagerNextReceivedEventArgs,
    MediaPlaybackCommandManagerPreviousReceivedEventArgs, MediaPlaybackSession, MediaPlaybackState, MediaPlayer,
    MediaPlayerAudioCategory, MediaPlayerFailedEventArgs,
};
use windows::Storage::FileProperties::{ThumbnailMode, ThumbnailType};
use windows::Storage::StorageFile;
use windows::Storage::Streams::{DataReader, IInputStream};

use super::media::NowPlaying;
use super::store;

const FILE: &str = "player.json";

/// Shown above the title (where other players show their app's name).
pub const APP: &str = "This PC";

/// Audio files the picker offers and drops accept.
pub const EXTENSIONS: &[&str] = &["mp3", "m4a", "aac", "wav", "flac", "wma", "ogg", "oga", "opus", "aif", "aiff"];

/// Folders are searched this deep, for at most this many songs per add.
const MAX_DEPTH: usize = 8;
const MAX_ADD: usize = 5000;
/// Previous / shuffle remember this many songs.
const MAX_HISTORY: usize = 500;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    pub fn next(self) -> Self {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Track {
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Seconds (0 = unknown).
    pub duration: f64,
    /// The file's tags were read (until then the title is the file name).
    pub tagged: bool,
}

impl Track {
    fn new(path: &str) -> Self {
        let title = Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string());
        Track { path: path.to_string(), title, ..Default::default() }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    queue: Vec<Track>,
    index: usize,
    shuffle: bool,
    repeat: Repeat,
}

/// What the Music tool draws (a cheap snapshot).
#[derive(Clone, Default)]
pub struct View {
    pub queue: Arc<Vec<Track>>,
    /// The current song in the queue (or the one Play starts with).
    pub index: usize,
    /// A song is loaded here (playing or paused).
    pub loaded: bool,
    pub playing: bool,
    pub shuffle: bool,
    pub repeat: Repeat,
    /// Songs are being read in from a folder or a drop.
    pub adding: bool,
    /// The last file that couldn't be played.
    pub error: Option<String>,
}

struct State {
    queue: Arc<Vec<Track>>,
    index: usize,
    shuffle: bool,
    repeat: Repeat,
    /// Songs played before the current one (newest last): Previous with shuffle, and what shuffle has had.
    history: Vec<usize>,
    player: Option<MediaPlayer>,
    now: Option<NowPlaying>,
    /// Bumped by every load: events and jobs for an older one are ignored.
    generation: u64,
    /// Files in a row that wouldn't open (skipping stops once the whole queue failed).
    failures: usize,
    adding: usize,
    error: Option<String>,
    rng: u64,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| {
        let saved: Saved = store::load(FILE);
        let queue: Vec<Track> = saved.queue.into_iter().filter(|t| !t.path.is_empty()).collect();
        let index = saved.index.min(queue.len().saturating_sub(1));
        let seed = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(7);
        Mutex::new(State {
            queue: Arc::new(queue),
            index,
            shuffle: saved.shuffle,
            repeat: saved.repeat,
            history: Vec::new(),
            player: None,
            now: None,
            generation: 0,
            failures: 0,
            adding: 0,
            error: None,
            rng: seed | 1,
        })
    })
}

fn lock() -> MutexGuard<'static, State> {
    state().lock().unwrap_or_else(|e| e.into_inner())
}

/// The player, if one was made (cloned out so it's used unlocked).
fn handle() -> Option<MediaPlayer> {
    lock().player.clone()
}

fn persist() {
    static SAVING: Mutex<()> = Mutex::new(());
    let _one = SAVING.lock().unwrap_or_else(|e| e.into_inner());
    let saved = {
        let s = lock();
        Saved { queue: s.queue.to_vec(), index: s.index, shuffle: s.shuffle, repeat: s.repeat }
    };
    store::save(FILE, &saved);
}

fn secs(t: TimeSpan) -> f64 {
    t.Duration as f64 / 10_000_000.0
}

fn random(s: &mut State) -> u64 {
    // xorshift64*
    let mut x = s.rng;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    s.rng = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

// ---------------------------------------------------------------------- reading

/// What's playing here, or None when no song is loaded.
pub fn current() -> Option<NowPlaying> {
    lock().now.clone()
}

pub fn view() -> View {
    let s = lock();
    View {
        queue: s.queue.clone(),
        index: s.index,
        loaded: s.now.is_some(),
        playing: s.now.as_ref().is_some_and(|n| n.playing),
        shuffle: s.shuffle,
        repeat: s.repeat,
        adding: s.adding > 0,
        error: s.error.clone(),
    }
}

/// A file with one of the audio extensions.
pub fn is_audio(path: &str) -> bool {
    Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| EXTENSIONS.contains(&e.as_str()))
}

/// Something a drop could add: an audio file, or a folder (searched when added).
pub fn droppable(path: &str) -> bool {
    is_audio(path) || Path::new(path).is_dir()
}

/// The songs under `p` (a file or a folder), in name order, files before sub-folders.
fn collect(p: &Path, depth: usize, out: &mut Vec<String>) {
    if out.len() >= MAX_ADD {
        return;
    }
    if p.is_dir() {
        if depth > MAX_DEPTH {
            return;
        }
        let Ok(rd) = std::fs::read_dir(p) else { return };
        let mut entries: Vec<_> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|e| !e.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
            .collect();
        entries.sort_by_key(|e| e.to_string_lossy().to_lowercase());
        let (dirs, files): (Vec<_>, Vec<_>) = entries.into_iter().partition(|e| e.is_dir());
        for f in files.iter().chain(dirs.iter()) {
            collect(f, depth + 1, out);
        }
    } else {
        let s = p.to_string_lossy();
        if is_audio(&s) {
            out.push(s.into_owned());
        }
    }
}

/// Title, artist, album and length from the file's tags (the file name when it has none).
fn read_tags(path: &str) -> Track {
    let mut t = Track::new(path);
    t.tagged = true;
    let props = StorageFile::GetFileFromPathAsync(&HSTRING::from(path))
        .and_then(|op| op.get())
        .and_then(|f| f.Properties())
        .and_then(|p| p.GetMusicPropertiesAsync())
        .and_then(|op| op.get());
    if let Ok(m) = props {
        let text = |r: windows::core::Result<HSTRING>| r.map(|s| s.to_string_lossy().trim().to_string()).unwrap_or_default();
        let title = text(m.Title());
        if !title.is_empty() {
            t.title = title;
        }
        t.artist = text(m.Artist());
        if t.artist.is_empty() {
            t.artist = text(m.AlbumArtist());
        }
        t.album = text(m.Album());
        t.duration = m.Duration().map(secs).unwrap_or(0.0);
    }
    t
}

/// Puts read tags back into the queue (the song may have moved since).
fn store_tags(near: usize, tags: Track) {
    let mut s = lock();
    let at = if s.queue.get(near).is_some_and(|t| t.path == tags.path) {
        Some(near)
    } else {
        s.queue.iter().position(|t| t.path == tags.path && !t.tagged)
    };
    if let Some(i) = at {
        Arc::make_mut(&mut s.queue)[i] = tags;
    }
}

/// Album art from the file (None for a generic icon).
fn thumbnail(file: &StorageFile) -> Option<Vec<u8>> {
    let thumb = file.GetThumbnailAsyncOverloadDefaultOptions(ThumbnailMode::MusicView, 300).ok()?.get().ok()?;
    let image = thumb.Type().ok()? == ThumbnailType::Image;
    let size = thumb.Size().ok()? as u32;
    let mut bytes = Vec::new();
    if image && size > 0 && size <= 16 * 1024 * 1024 {
        let input: IInputStream = thumb.GetInputStreamAt(0).ok()?;
        let reader = DataReader::CreateDataReader(&input).ok()?;
        let got = reader.LoadAsync(size).ok()?.get().ok()?;
        bytes = vec![0u8; got as usize];
        reader.ReadBytes(&mut bytes).ok()?;
    }
    let _ = thumb.Close();
    (!bytes.is_empty()).then_some(bytes)
}

// ---------------------------------------------------------------------- the player

/// The one MediaPlayer (made on first use), with its events hooked.
fn player() -> Option<MediaPlayer> {
    static MAKING: Mutex<()> = Mutex::new(());
    let _one = MAKING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = handle() {
        return Some(p);
    }
    let p = MediaPlayer::new().ok()?;
    let _ = p.SetAudioCategory(MediaPlayerAudioCategory::Media);
    let _ = p.SetAutoPlay(false);
    let _ = p.MediaOpened(&TypedEventHandler::new(|p: &Option<MediaPlayer>, _: &Option<IInspectable>| {
        if let Some(p) = p {
            opened(p);
        }
        Ok(())
    }));
    let _ = p.MediaEnded(&TypedEventHandler::new(|_: &Option<MediaPlayer>, _: &Option<IInspectable>| {
        let g = lock().generation;
        crate::bg::spawn(move || ended(g));
        Ok(())
    }));
    let _ = p.MediaFailed(&TypedEventHandler::new(|_: &Option<MediaPlayer>, _: &Option<MediaPlayerFailedEventArgs>| {
        let g = lock().generation;
        crate::bg::spawn(move || failed(g));
        Ok(())
    }));
    if let Ok(session) = p.PlaybackSession() {
        let _ = session.PlaybackStateChanged(&TypedEventHandler::new(|s: &Option<MediaPlaybackSession>, _: &Option<IInspectable>| {
            if let Some(s) = s {
                sync(s);
            }
            Ok(())
        }));
        let _ = session.SeekCompleted(&TypedEventHandler::new(|s: &Option<MediaPlaybackSession>, _: &Option<IInspectable>| {
            if let Some(s) = s {
                sync(s);
            }
            Ok(())
        }));
    }
    // Media keys, the Windows media overlay and the lock screen: next / previous go through the queue.
    if let Ok(cm) = p.CommandManager() {
        let _ = cm.SetIsEnabled(true);
        for b in [cm.NextBehavior(), cm.PreviousBehavior()].into_iter().flatten() {
            let _ = b.SetEnablingRule(MediaCommandEnablingRule::Always);
        }
        let _ = cm.NextReceived(&TypedEventHandler::new(
            |_: &Option<MediaPlaybackCommandManager>, a: &Option<MediaPlaybackCommandManagerNextReceivedEventArgs>| {
                if let Some(a) = a {
                    let _ = a.SetHandled(true);
                }
                crate::bg::spawn(next);
                Ok(())
            },
        ));
        let _ = cm.PreviousReceived(&TypedEventHandler::new(
            |_: &Option<MediaPlaybackCommandManager>, a: &Option<MediaPlaybackCommandManagerPreviousReceivedEventArgs>| {
                if let Some(a) = a {
                    let _ = a.SetHandled(true);
                }
                crate::bg::spawn(previous);
                Ok(())
            },
        ));
    }
    lock().player = Some(p.clone());
    Some(p)
}

/// The file opened: its real length.
fn opened(p: &MediaPlayer) {
    let duration = p.PlaybackSession().and_then(|s| s.NaturalDuration()).map(secs).unwrap_or(0.0);
    {
        let mut s = lock();
        s.failures = 0;
        s.error = None;
        if let Some(now) = s.now.as_mut() {
            if duration > 0.0 {
                now.duration = duration;
            }
        }
    }
    crate::bg::refresh();
}

/// Playing / paused and the playhead, as the player reports them.
fn sync(session: &MediaPlaybackSession) {
    let state = session.PlaybackState().unwrap_or(MediaPlaybackState::None);
    let position = session.Position().map(secs).unwrap_or(0.0);
    {
        let mut s = lock();
        let Some(now) = s.now.as_mut() else { return };
        if state == MediaPlaybackState::Playing {
            now.playing = true;
        } else if state == MediaPlaybackState::Paused || state == MediaPlaybackState::None {
            now.playing = false;
        }
        // Opening / buffering keep what was asked for, and report no useful position.
        if state != MediaPlaybackState::Opening {
            now.position = position;
            now.updated = Instant::now();
        }
    }
    crate::stat("player update");
    crate::bg::refresh();
}

/// Opens song `index` of the queue (and plays it with `autoplay`). `remember` puts the song it replaces in
/// the history (not when going back to one).
fn load(index: usize, autoplay: bool, remember: bool) {
    let (mut track, gen) = {
        let mut s = lock();
        let Some(t) = s.queue.get(index).cloned() else { return };
        s.generation += 1;
        if remember && s.now.is_some() && s.index != index {
            let was = s.index;
            s.history.push(was);
            if s.history.len() > MAX_HISTORY {
                s.history.remove(0);
            }
        }
        s.index = index;
        (t, s.generation)
    };
    if !track.tagged {
        track = read_tags(&track.path);
        store_tags(index, track.clone());
    }
    {
        let mut s = lock();
        if s.generation != gen {
            return;
        }
        s.now = Some(NowPlaying {
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            app: APP.into(),
            playing: autoplay,
            position: 0.0,
            duration: track.duration,
            updated: Instant::now(),
            art: None,
            art_id: 0,
        });
    }
    crate::bg::refresh();

    let Some(p) = player() else { return failed(gen) };
    let Ok(file) = StorageFile::GetFileFromPathAsync(&HSTRING::from(track.path.as_str())).and_then(|op| op.get()) else {
        return failed(gen);
    };
    let Ok(source) = MediaSource::CreateFromStorageFile(&file) else { return failed(gen) };
    if lock().generation != gen {
        return;
    }
    if p.SetSource(&source).is_err() {
        return failed(gen);
    }
    if autoplay {
        super::media::pause_others();
        let _ = p.Play();
    }
    // Title, artist and art for the Windows media overlay (best effort).
    if let Ok(d) = p.SystemMediaTransportControls().and_then(|c| c.DisplayUpdater()) {
        if d.CopyFromFileAsync(MediaPlaybackType::Music, &file).and_then(|op| op.get()).unwrap_or(false) {
            let _ = d.Update();
        }
    }
    if let Some(art) = thumbnail(&file) {
        let mut h = DefaultHasher::new();
        art.hash(&mut h);
        let id = h.finish() | 1;
        let mut s = lock();
        if s.generation == gen {
            if let Some(now) = s.now.as_mut() {
                now.art_id = id;
                now.art = Some(Arc::new(art));
            }
        }
    }
    crate::bg::refresh();
    persist();
}

/// The song to play after the current one. `user`: Next was pressed (it wraps round even with repeat off).
fn pick_next(s: &mut State, user: bool) -> Option<usize> {
    let n = s.queue.len();
    if n == 0 {
        return None;
    }
    if s.now.is_none() {
        return Some(s.index.min(n - 1));
    }
    let wrap = user || s.repeat != Repeat::Off;
    if s.shuffle && n > 1 {
        // Songs not had yet this round (the current one counts as had).
        let mut had = vec![false; n];
        for &i in &s.history {
            if i < n {
                had[i] = true;
            }
        }
        had[s.index.min(n - 1)] = true;
        let mut left: Vec<usize> = (0..n).filter(|&i| !had[i]).collect();
        if left.is_empty() {
            if !wrap {
                return None;
            }
            s.history.clear();
            left = (0..n).filter(|&i| i != s.index).collect();
        }
        let k = (random(s) % left.len() as u64) as usize;
        return Some(left[k]);
    }
    if s.index + 1 < n {
        Some(s.index + 1)
    } else if wrap {
        Some(0)
    } else {
        None
    }
}

fn ended(gen: u64) {
    let next = {
        let mut s = lock();
        if s.generation != gen {
            return;
        }
        if s.repeat == Repeat::One {
            Some(s.index)
        } else {
            pick_next(&mut s, false)
        }
    };
    match next {
        Some(i) => load(i, true, true),
        None => {
            // The end of the queue (repeat off): stop, back at the song's start.
            {
                let mut s = lock();
                if let Some(now) = s.now.as_mut() {
                    now.playing = false;
                    now.position = 0.0;
                    now.updated = Instant::now();
                }
            }
            if let Some(session) = handle().and_then(|p| p.PlaybackSession().ok()) {
                let _ = session.SetPosition(TimeSpan { Duration: 0 });
            }
            crate::bg::refresh();
        }
    }
}

/// The song couldn't be opened: say so and go on with the next (until the whole queue failed).
fn failed(gen: u64) {
    let next = {
        let mut s = lock();
        if s.generation != gen {
            return;
        }
        s.failures += 1;
        let name = s.queue.get(s.index).map(|t| t.title.clone()).unwrap_or_default();
        s.error = Some(crate::i18n::tf!("Couldn't play “{name}”", "Gagal memutar “{name}”"));
        if s.failures >= s.queue.len() {
            s.now = None;
            s.failures = 0;
            None
        } else {
            pick_next(&mut s, true)
        }
    };
    crate::bg::refresh();
    // On a fresh job: a long run of unplayable files must not nest load → failed → load … deeper and deeper.
    if let Some(i) = next {
        crate::bg::spawn(move || load(i, true, true));
    }
}

// ---------------------------------------------------------------------- controls

/// Adds files, and the songs in folders, to the end of the queue. With `play`, the first new song starts
/// unless one is already playing here.
pub fn add(paths: Vec<String>, play: bool) {
    lock().adding += 1;
    crate::bg::refresh();
    let mut files = Vec::new();
    for p in &paths {
        collect(Path::new(p), 0, &mut files);
    }
    files.truncate(MAX_ADD);
    let (first, start) = {
        let mut s = lock();
        s.adding -= 1;
        let first = s.queue.len();
        if !files.is_empty() {
            Arc::make_mut(&mut s.queue).extend(files.iter().map(|f| Track::new(f)));
        }
        let idle = !s.now.as_ref().is_some_and(|n| n.playing);
        (first, (play && idle && !files.is_empty()).then_some(first))
    };
    persist();
    crate::bg::refresh();
    if let Some(i) = start {
        load(i, true, true);
    }
    // Tags for the rest, a few at a time.
    lock().adding += 1;
    for (k, f) in files.iter().enumerate() {
        let i = first + k;
        if lock().queue.get(i).is_some_and(|t| t.tagged) {
            continue;
        }
        store_tags(i, read_tags(f));
        if k % 25 == 24 {
            crate::bg::refresh();
        }
    }
    lock().adding -= 1;
    persist();
    crate::bg::refresh();
}

/// The Open dialog for songs (several at once), then `add`.
pub fn pick_files(play: bool) {
    let files = dialog(false, true, crate::i18n::tr("Add music"));
    if !files.is_empty() {
        add(files, play);
    }
}

/// The folder dialog, then `add` with every song in it.
pub fn pick_folder(play: bool) {
    let folder = dialog(true, false, crate::i18n::tr("Add a music folder"));
    if !folder.is_empty() {
        add(folder, play);
    }
}

/// The Open dialog for one audio file (the adzan), titled `title`.
pub fn pick_file(title: &str) -> Option<String> {
    dialog(false, false, title).into_iter().next()
}

/// Pauses the song playing here, if one is (the adzan is starting).
pub fn pause_if_playing() {
    let playing = lock().now.as_ref().is_some_and(|n| n.playing);
    if playing {
        play_pause();
    }
}

/// Play: the paused song, or the queue from where it was; with an empty queue, the Open dialog.
pub fn play_or_pick() {
    if lock().queue.is_empty() {
        pick_files(true);
    } else {
        play_pause();
    }
}

pub fn play_pause() {
    let (loaded, playing, index, empty) = {
        let s = lock();
        (s.now.is_some(), s.now.as_ref().is_some_and(|n| n.playing), s.index, s.queue.is_empty())
    };
    if !loaded {
        if !empty {
            load(index, true, false);
        }
        return;
    }
    let Some(p) = handle() else { return };
    if playing {
        let _ = p.Pause();
    } else {
        super::media::pause_others();
        let _ = p.Play();
    }
    let mut s = lock();
    if let Some(now) = s.now.as_mut() {
        now.position = now.position_now();
        now.playing = !playing;
        now.updated = Instant::now();
    }
}

pub fn next() {
    let i = {
        let mut s = lock();
        pick_next(&mut s, true)
    };
    if let Some(i) = i {
        load(i, true, true);
    }
}

/// Back to the song's start, or (in its first 3 seconds) to the song before.
pub fn previous() {
    let back = {
        let mut s = lock();
        let n = s.queue.len();
        let pos = s.now.as_ref().map_or(0.0, |n| n.position_now());
        if s.now.is_none() || pos > 3.0 {
            None
        } else if s.shuffle {
            s.history.pop()
        } else if s.index > 0 {
            Some(s.index - 1)
        } else if s.repeat == Repeat::All {
            n.checked_sub(1)
        } else {
            None
        }
    };
    match back {
        Some(i) => load(i, true, false),
        None => seek(0.0),
    }
}

/// Jumps to `seconds` into the song.
pub fn seek(seconds: f64) {
    let Some(session) = handle().and_then(|p| p.PlaybackSession().ok()) else { return };
    let _ = session.SetPosition(TimeSpan { Duration: (seconds.max(0.0) * 10_000_000.0) as i64 });
    let mut s = lock();
    if let Some(now) = s.now.as_mut() {
        now.position = seconds.max(0.0);
        now.updated = Instant::now();
    }
}

/// Song `index` of the queue (the current one pauses / resumes).
pub fn play_at(index: usize) {
    let current = {
        let s = lock();
        s.now.is_some() && s.index == index
    };
    if current {
        play_pause();
    } else {
        load(index, true, true);
    }
}

/// Takes song `index` out of the queue (the next one takes over if it was playing).
pub fn remove(index: usize) {
    enum Then {
        Nothing,
        Clear,
        Load(usize, bool),
    }
    let then = {
        let mut s = lock();
        if index >= s.queue.len() {
            return;
        }
        Arc::make_mut(&mut s.queue).remove(index);
        s.history.retain(|&h| h != index);
        for h in s.history.iter_mut() {
            if *h > index {
                *h -= 1;
            }
        }
        let n = s.queue.len();
        if n == 0 {
            Then::Clear
        } else if index < s.index {
            s.index -= 1;
            Then::Nothing
        } else if index == s.index {
            if s.index >= n {
                s.index = 0;
            }
            match &s.now {
                Some(now) => Then::Load(s.index, now.playing),
                None => Then::Nothing,
            }
        } else {
            Then::Nothing
        }
    };
    match then {
        Then::Clear => return clear(),
        Then::Load(i, playing) => load(i, playing, false),
        Then::Nothing => {}
    }
    persist();
    crate::bg::refresh();
}

/// Empties the queue and lets the player go (other apps show in the Music tool again).
pub fn clear() {
    let p = {
        let mut s = lock();
        s.queue = Arc::new(Vec::new());
        s.index = 0;
        s.history.clear();
        s.now = None;
        s.generation += 1;
        s.error = None;
        s.player.take()
    };
    if let Some(p) = p {
        let _ = p.Pause();
        let _ = p.Close();
    }
    persist();
    crate::bg::refresh();
}

pub fn set_shuffle(on: bool) {
    lock().shuffle = on;
    persist();
}

pub fn cycle_repeat() {
    {
        let mut s = lock();
        s.repeat = s.repeat.next();
    }
    persist();
}

// ---------------------------------------------------------------------- dialogs

/// The Open dialog for audio files (several with `multi`), or the folder picker. Runs on its own STA
/// thread, owned by the notch so it shows above it. Blocking: call from a background thread.
fn dialog(folder: bool, multi: bool, title: &str) -> Vec<String> {
    let owner = crate::bg::window().map(|h| h.0 as isize).unwrap_or(0);
    let title = title.to_string();
    std::thread::spawn(move || unsafe {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
        use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH};
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let mut out = Vec::new();
        if let Ok(dlg) = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) {
            let _ = dlg.SetTitle(&HSTRING::from(title));
            if let Ok(opts) = dlg.GetOptions() {
                let kind = if folder {
                    FOS_PICKFOLDERS
                } else if multi {
                    FOS_ALLOWMULTISELECT | FOS_FILEMUSTEXIST
                } else {
                    FOS_FILEMUSTEXIST
                };
                let _ = dlg.SetOptions(opts | kind | FOS_FORCEFILESYSTEM);
            }
            // The filter strings must outlive Show.
            let audio_name = HSTRING::from(crate::i18n::tr("Audio files"));
            let audio_spec = HSTRING::from(EXTENSIONS.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(";"));
            let all_name = HSTRING::from(crate::i18n::tr("All files"));
            let all_spec = HSTRING::from("*.*");
            if !folder {
                let filters = [
                    COMDLG_FILTERSPEC { pszName: PCWSTR(audio_name.as_ptr()), pszSpec: PCWSTR(audio_spec.as_ptr()) },
                    COMDLG_FILTERSPEC { pszName: PCWSTR(all_name.as_ptr()), pszSpec: PCWSTR(all_spec.as_ptr()) },
                ];
                let _ = dlg.SetFileTypes(&filters);
            }
            if dlg.Show(HWND(owner as _)).is_ok() {
                if let Ok(items) = dlg.GetResults() {
                    for i in 0..items.GetCount().unwrap_or(0) {
                        if let Ok(item) = items.GetItemAt(i) {
                            if let Ok(p) = item.GetDisplayName(SIGDN_FILESYSPATH) {
                                if let Ok(s) = p.to_string() {
                                    out.push(s);
                                }
                                CoTaskMemFree(Some(p.0 as _));
                            }
                        }
                    }
                }
            }
        }
        CoUninitialize();
        out
    })
    .join()
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(n: usize, index: usize, shuffle: bool, repeat: Repeat) -> State {
        State {
            queue: Arc::new((0..n).map(|i| Track::new(&format!("C:\\m\\{i}.mp3"))).collect()),
            index,
            shuffle,
            repeat,
            history: Vec::new(),
            player: None,
            now: Some(NowPlaying {
                title: String::new(),
                artist: String::new(),
                album: String::new(),
                app: APP.into(),
                playing: true,
                position: 0.0,
                duration: 0.0,
                updated: Instant::now(),
                art: None,
                art_id: 0,
            }),
            generation: 0,
            failures: 0,
            adding: 0,
            error: None,
            rng: 0x1234_5678,
        }
    }

    #[test]
    fn audio_files() {
        assert!(is_audio("C:\\Music\\a.MP3"));
        assert!(is_audio("song.flac"));
        assert!(!is_audio("cover.jpg"));
        assert!(!is_audio("noext"));
        assert_eq!(Track::new("C:\\Music\\01 Intro.mp3").title, "01 Intro");
    }

    #[test]
    fn in_order() {
        let mut s = st(3, 0, false, Repeat::Off);
        assert_eq!(pick_next(&mut s, false), Some(1));
        s.index = 2;
        assert_eq!(pick_next(&mut s, false), None, "the queue ends with repeat off");
        assert_eq!(pick_next(&mut s, true), Some(0), "Next wraps round");
        s.repeat = Repeat::All;
        assert_eq!(pick_next(&mut s, false), Some(0));
    }

    #[test]
    fn shuffle_plays_each_once() {
        let mut s = st(6, 0, true, Repeat::Off);
        let mut seen = vec![0];
        while let Some(i) = pick_next(&mut s, false) {
            assert!(!seen.contains(&i), "{i} came twice");
            seen.push(i);
            let was = s.index;
            s.history.push(was);
            s.index = i;
        }
        assert_eq!(seen.len(), 6);
        assert!(pick_next(&mut s, true).is_some(), "Next starts a new round");
    }

    #[test]
    fn repeat_cycles() {
        assert_eq!(Repeat::Off.next(), Repeat::All);
        assert_eq!(Repeat::All.next(), Repeat::One);
        assert_eq!(Repeat::One.next(), Repeat::Off);
    }
}
