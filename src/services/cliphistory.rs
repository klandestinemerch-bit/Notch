//! Clipboard history in clipboard.json, the .NET build's schema:
//! [{ Id (Guid "D"), Kind (0 text, 1 link, 2 image, 3 files), Text, ImagePath, Files, Time, Pinned }], newest first.
//! A background thread polls the clipboard sequence number (every 500 ms) and records text, links, copied
//! files and images (saved as PNGs in the data folder's `clipboard` directory). Content apps mark as private
//! (password managers) is skipped when "skip sensitive" is on.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use windows::core::w;
use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

use super::store;
use super::todos::{local_now_secs, parse_local};

const FILE: &str = "clipboard.json";
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const CF_DIB: u32 = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Text = 0,
    Link = 1,
    Image = 2,
    Files = 3,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Clip {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub kind: i32,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub image_path: Option<String>,
    #[serde(default)]
    pub files: Option<Vec<String>>,
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Clip {
    pub fn kind(&self) -> Kind {
        match self.kind {
            1 => Kind::Link,
            2 => Kind::Image,
            3 => Kind::Files,
            _ => Kind::Text,
        }
    }

    /// One line of text for the card (240 chars max).
    pub fn preview(&self) -> String {
        let s: String = match self.kind() {
            Kind::Files => self
                .files
                .iter()
                .flatten()
                .map(|f| f.rsplit(['\\', '/']).next().unwrap_or(f).to_string())
                .collect::<Vec<_>>()
                .join(", "),
            Kind::Image => crate::i18n::tr("Image").into(),
            _ => self.text.as_deref().unwrap_or("").trim().replace(['\r', '\n', '\t'], " "),
        };
        if s.chars().count() > 240 {
            s.chars().take(240).collect::<String>() + "…"
        } else {
            s
        }
    }

    /// Link cards: ("github.com", "/user/repo").
    pub fn link_parts(&self) -> (String, String) {
        let t = self.text.as_deref().unwrap_or("").trim();
        let rest = t.split_once("://").map_or(t, |(_, r)| r);
        let (host, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let host = host.rsplit('@').next().unwrap_or(host);
        let host = host.strip_prefix("www.").unwrap_or(host);
        (host.to_string(), path.trim_end_matches('/').to_string())
    }

    pub fn icon(&self) -> &'static str {
        match self.kind() {
            Kind::Link => "Link",
            Kind::Image => "Photo",
            Kind::Files => "Folder",
            Kind::Text => "DocText",
        }
    }

    pub fn time_local(&self) -> i64 {
        parse_local(&self.time).map_or(0, |t| t.0)
    }

    /// "now", "5m", "3h", "2d".
    pub fn time_text(&self) -> String {
        let d = (local_now_secs() - self.time_local()).max(0);
        if d < 60 {
            "now".into()
        } else if d < 3600 {
            format!("{}m", d / 60)
        } else if d < 86_400 {
            format!("{}h", d / 3600)
        } else {
            format!("{}d", d / 86_400)
        }
    }
}

fn cell() -> &'static Mutex<Vec<Clip>> {
    static CELL: OnceLock<Mutex<Vec<Clip>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(store::load(FILE)))
}

fn change<R>(f: impl FnOnce(&mut Vec<Clip>) -> R) -> R {
    let r = {
        let mut items = cell().lock().unwrap_or_else(|e| e.into_inner());
        let r = f(&mut items);
        store::save(FILE, &*items);
        r
    };
    crate::bg::refresh();
    r
}

/// Pinned first, then newest first.
pub fn all() -> Vec<Clip> {
    let mut v = cell().lock().unwrap_or_else(|e| e.into_inner()).clone();
    v.sort_by_key(|c| (std::cmp::Reverse(c.pinned), std::cmp::Reverse(c.time_local())));
    v
}

// ---------------------------------------------------------------- ids

/// Cryptographically random bytes (BCryptGenRandom).
pub(crate) fn random_bytes(n: usize) -> Vec<u8> {
    use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_ALG_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
    let mut b = vec![0u8; n];
    unsafe {
        let _ = BCryptGenRandom(BCRYPT_ALG_HANDLE::default(), &mut b, BCRYPT_USE_SYSTEM_PREFERRED_RNG);
    }
    b
}

/// A random .NET Guid in "D" format.
pub(crate) fn new_guid() -> String {
    let mut b = random_bytes(16);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

// ---------------------------------------------------------------- watching

static STARTED: AtomicBool = AtomicBool::new(false);
/// Our own clipboard writes are ignored until this time (ms since 1970).
static IGNORE_UNTIL: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn ignore_ours() {
    IGNORE_UNTIL.store(now_ms() + 700, Ordering::Relaxed);
}

/// Starts recording (once per run). Safe to call repeatedly.
pub fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = cell();
    std::thread::spawn(|| {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let mut last = unsafe { GetClipboardSequenceNumber() };
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let seq = unsafe { GetClipboardSequenceNumber() };
            if seq == last {
                continue;
            }
            last = seq;
            if now_ms() < IGNORE_UNTIL.load(Ordering::Relaxed) || !super::settings::get().clipboard_enabled {
                continue;
            }
            // Another app may still hold the clipboard open: retry a few times.
            for _ in 0..5 {
                match capture() {
                    Some(item) => {
                        if let Some(item) = item {
                            record(item);
                        }
                        break;
                    }
                    None => std::thread::sleep(Duration::from_millis(80)),
                }
            }
        }
    });
}

/// `None` = couldn't open the clipboard; `Some(None)` = nothing to record.
fn capture() -> Option<Option<Clip>> {
    unsafe {
        OpenClipboard(None).ok()?;
        let r = read_open_clipboard();
        let _ = CloseClipboard();
        Some(r)
    }
}

unsafe fn read_open_clipboard() -> Option<Clip> {
    if super::settings::get().clipboard_skip_sensitive && sensitive() {
        return None;
    }
    let mut item = Clip {
        id: new_guid(),
        kind: 0,
        text: None,
        image_path: None,
        files: None,
        time: super::shelf::now_string(),
        pinned: false,
        extra: Map::new(),
    };
    if IsClipboardFormatAvailable(CF_HDROP).is_ok() {
        if let Ok(h) = GetClipboardData(CF_HDROP) {
            let drop = HDROP(h.0);
            let n = DragQueryFileW(drop, u32::MAX, None);
            let mut files = Vec::new();
            for i in 0..n {
                let len = DragQueryFileW(drop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                DragQueryFileW(drop, i, Some(&mut buf));
                files.push(String::from_utf16_lossy(&buf[..len]));
            }
            if !files.is_empty() {
                item.kind = Kind::Files as i32;
                item.files = Some(files);
                return Some(item);
            }
        }
    }
    if IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok() {
        let text = global_text(GetClipboardData(CF_UNICODETEXT).ok()?)?;
        if text.trim().is_empty() {
            return None;
        }
        let t = text.trim();
        let link = !t.contains(char::is_whitespace) && (t.starts_with("http://") || t.starts_with("https://")) && t.len() > 8;
        item.kind = if link { Kind::Link } else { Kind::Text } as i32;
        item.text = Some(text);
        return Some(item);
    }
    if IsClipboardFormatAvailable(CF_DIB).is_ok() {
        let h = GetClipboardData(CF_DIB).ok()?;
        let png = dib_to_png(h)?;
        let dir = store::path("clipboard");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{}.png", item.id.replace('-', "")));
        std::fs::write(&path, png).ok()?;
        item.kind = Kind::Image as i32;
        item.image_path = Some(path.to_string_lossy().into_owned());
        return Some(item);
    }
    None
}

/// Password managers and other apps opt out of clipboard monitoring with these formats.
unsafe fn sensitive() -> bool {
    let exclude = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
    let ignore = RegisterClipboardFormatW(w!("Clipboard Viewer Ignore"));
    let history = RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory"));
    if IsClipboardFormatAvailable(exclude).is_ok() || IsClipboardFormatAvailable(ignore).is_ok() {
        return true;
    }
    if IsClipboardFormatAvailable(history).is_ok() {
        if let Ok(h) = GetClipboardData(history) {
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u32;
            if !p.is_null() {
                let v = if GlobalSize(g) >= 4 { *p } else { 1 };
                let _ = GlobalUnlock(g);
                return v == 0;
            }
        }
    }
    false
}

unsafe fn global_text(h: HANDLE) -> Option<String> {
    let g = HGLOBAL(h.0);
    let p = GlobalLock(g) as *const u16;
    if p.is_null() {
        return None;
    }
    let max = GlobalSize(g) / 2;
    let mut len = 0;
    while len < max && *p.add(len) != 0 {
        len += 1;
    }
    let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
    let _ = GlobalUnlock(g);
    Some(s)
}

/// CF_DIB (24 or 32 bpp, uncompressed or bitfields) → PNG.
unsafe fn dib_to_png(h: HANDLE) -> Option<Vec<u8>> {
    let g = HGLOBAL(h.0);
    let p = GlobalLock(g) as *const u8;
    if p.is_null() {
        return None;
    }
    let size = GlobalSize(g);
    let data = std::slice::from_raw_parts(p, size).to_vec();
    let _ = GlobalUnlock(g);
    let rd32 = |o: usize| -> Option<i32> { Some(i32::from_le_bytes(data.get(o..o + 4)?.try_into().ok()?)) };
    let header = rd32(0)? as usize;
    let w = rd32(4)?;
    let h = rd32(8)?;
    let bpp = u16::from_le_bytes(data.get(14..16)?.try_into().ok()?);
    let compression = rd32(16)?;
    let colors = rd32(32)? as usize;
    if w <= 0 || h == 0 || !(bpp == 24 || bpp == 32) || !(compression == 0 || compression == 3) {
        return None;
    }
    let mut offset = header + colors * 4;
    if compression == 3 && header == 40 {
        offset += 12; // the three colour masks follow a plain BITMAPINFOHEADER
    }
    let (w, hh) = (w as usize, h.unsigned_abs() as usize);
    let stride = (w * bpp as usize / 8 + 3) & !3;
    if data.len() < offset + stride * hh {
        return None;
    }
    let mut out = vec![0u8; w * hh * 4];
    let mut any_alpha = false;
    for y in 0..hh {
        let src_row = if h > 0 { hh - 1 - y } else { y };
        let row = &data[offset + src_row * stride..];
        for x in 0..w {
            let o = (y * w + x) * 4;
            if bpp == 32 {
                out[o..o + 4].copy_from_slice(&row[x * 4..x * 4 + 4]);
                any_alpha |= row[x * 4 + 3] != 0;
            } else {
                out[o..o + 3].copy_from_slice(&row[x * 3..x * 3 + 3]);
                out[o + 3] = 255;
            }
        }
    }
    if bpp == 32 && !any_alpha {
        for px in out.chunks_exact_mut(4) {
            px[3] = 255;
        }
    } else if bpp == 32 {
        // Straight alpha → premultiplied for WIC's PBGRA.
        for px in out.chunks_exact_mut(4) {
            let a = px[3] as u32;
            for c in &mut px[..3] {
                *c = (*c as u32 * a / 255) as u8;
            }
        }
    }
    super::shelf::encode_png(w as u32, hh as u32, &out)
}

fn record(item: Clip) {
    let max = super::settings::get().clipboard_max_items.max(10) as usize;
    change(|items| {
        if item.kind() != Kind::Image {
            items.retain(|x| !(x.kind == item.kind && x.kind() != Kind::Image && x.text == item.text && x.files == item.files));
        }
        items.insert(0, item);
        while items.iter().filter(|i| !i.pinned).count() > max {
            if let Some(i) = items.iter().rposition(|i| !i.pinned) {
                let old = items.remove(i);
                delete_image(&old);
            }
        }
    });
}

fn delete_image(c: &Clip) {
    if let Some(p) = &c.image_path {
        let _ = std::fs::remove_file(p);
    }
}

// ---------------------------------------------------------------- actions

/// Puts a clip back on the clipboard (not recorded again) and moves it to the front.
pub fn recopy(id: &str) -> bool {
    let Some(item) = cell().lock().unwrap_or_else(|e| e.into_inner()).iter().find(|c| c.id == id).cloned() else {
        return false;
    };
    ignore_ours();
    let ok = match item.kind() {
        Kind::Files => set_files(item.files.as_deref().unwrap_or(&[])),
        Kind::Image => item.image_path.as_deref().is_some_and(set_image),
        _ => {
            crate::clipboard::set_text(item.text.as_deref().unwrap_or(""));
            true
        }
    };
    if ok {
        change(|items| {
            if let Some(i) = items.iter().position(|c| c.id == id) {
                let mut c = items.remove(i);
                c.time = super::shelf::now_string();
                items.insert(0, c);
            }
        });
    }
    ok
}

/// Copies text without recording it in history (Emoji, converters, …).
pub fn copy_quiet(text: &str) {
    ignore_ours();
    crate::clipboard::set_text(text);
}

pub fn remove(id: &str) {
    change(|items| {
        if let Some(i) = items.iter().position(|c| c.id == id) {
            let c = items.remove(i);
            delete_image(&c);
        }
    });
}

pub fn toggle_pin(id: &str) {
    change(|items| {
        if let Some(c) = items.iter_mut().find(|c| c.id == id) {
            c.pinned = !c.pinned;
        }
    });
}

/// Removes everything except pinned clips.
pub fn clear() {
    change(|items| {
        items.retain(|c| {
            if !c.pinned {
                delete_image(c);
            }
            c.pinned
        })
    });
}

/// Sets a file list (CF_HDROP) on the clipboard.
pub fn set_files(files: &[String]) -> bool {
    if files.is_empty() {
        return false;
    }
    // DROPFILES { pFiles: u32, pt: POINT, fNC: BOOL, fWide: BOOL } then double-null-terminated UTF-16 paths.
    let mut list: Vec<u16> = Vec::new();
    for f in files {
        list.extend(f.encode_utf16());
        list.push(0);
    }
    list.push(0);
    let mut bytes = Vec::with_capacity(20 + list.len() * 2);
    bytes.extend_from_slice(&20u32.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 12]);
    bytes.extend_from_slice(&1u32.to_le_bytes());
    for u in list {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    set_raw(CF_HDROP, &bytes)
}

/// Decodes a PNG/JPEG file and sets it as CF_DIB.
fn set_image(path: &str) -> bool {
    let Some((w, h, px)) = decode_file(path) else { return false };
    let mut bytes = Vec::with_capacity(40 + px.len());
    bytes.extend_from_slice(&40u32.to_le_bytes());
    bytes.extend_from_slice(&(w as i32).to_le_bytes());
    bytes.extend_from_slice(&(h as i32).to_le_bytes()); // bottom-up
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&32u16.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 24]);
    let stride = w as usize * 4;
    for y in (0..h as usize).rev() {
        bytes.extend_from_slice(&px[y * stride..(y + 1) * stride]);
    }
    set_raw(CF_DIB, &bytes)
}

fn decode_file(path: &str) -> Option<(u32, u32, Vec<u8>)> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::GENERIC_READ;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory, WICBitmapDitherTypeNone,
        WICBitmapPaletteTypeMedianCut, WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let dec = wic
            .CreateDecoderFromFilename(&HSTRING::from(path), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let frame = dec.GetFrame(0).ok()?;
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(&frame, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeMedianCut)
            .ok()?;
        let (mut w, mut h) = (0, 0);
        conv.GetSize(&mut w, &mut h).ok()?;
        let mut px = vec![0u8; (w * h * 4) as usize];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut px).ok()?;
        Some((w, h, px))
    }
}

fn set_raw(format: u32, bytes: &[u8]) -> bool {
    unsafe {
        let mut opened = false;
        for _ in 0..5 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        if !opened {
            return false;
        }
        let _ = EmptyClipboard();
        let mut ok = false;
        if let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) {
            let p = GlobalLock(g) as *mut u8;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
                let _ = GlobalUnlock(g);
                ok = SetClipboardData(format, HANDLE(g.0)).is_ok();
            }
        }
        let _ = CloseClipboard();
        ok
    }
}

#[cfg(test)]
mod tests {
    /// Reads (never writes) whatever is on the clipboard now and prints what would be recorded.
    #[test]
    #[ignore]
    fn read_current_clipboard() {
        match super::capture() {
            None => println!("clipboard busy"),
            Some(None) => println!("nothing to record (empty, unsupported or marked sensitive)"),
            Some(Some(c)) => println!(
                "would record kind {:?}, {} chars of preview, files {:?}, image {:?}",
                c.kind(),
                c.preview().chars().count(),
                c.files.as_ref().map(|f| f.len()),
                c.image_path.is_some()
            ),
        }
    }
}
