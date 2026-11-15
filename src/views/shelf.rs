//! Shelf: files dropped on the notch, as a grid of tiles (icon + name).
//! Click selects (Ctrl toggles, Shift selects a range, Ctrl+A all); dragging on empty space draws a box that
//! selects the tiles it touches. Dragging a tile drags every selected file out to drop it elsewhere;
//! double-click opens; Delete removes the selection; right-click offers Open, Show in folder, Copy,
//! Send to phone and Remove for the selection; the × on a hovered tile removes it.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::bg;
use crate::gfx::{with_alpha, Align, REGULAR};
use crate::services::{cliphistory, phone, shelf};
use crate::ui::{id, Key, Rect, Ui};
use crate::i18n::tr;

const TILE_W: f32 = 74.0;
const TILE_H: f32 = 82.0;
/// The renderer keeps only a handful of decoded images: real icons are used while the shelf is this small.
const MAX_THUMBS: usize = 6;
/// Pointer travel (DIP) before a press on a tile becomes a drag.
const DRAG_START: f32 = 5.0;

/// Posted to the window to run a drag out of the Shelf on the window thread.
pub const WM_SHELF_DRAG: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 9;

/// What the background loader found out about a path.
#[derive(Clone, Default)]
struct Info {
    folder: bool,
    /// PNG icon/thumbnail (None = not loaded or none).
    png: Option<Vec<u8>>,
    /// The thumbnail was attempted.
    thumb_tried: bool,
}

/// A press on a tile that may turn into a drag.
struct Press {
    path: String,
    x: f32,
    y: f32,
    /// The tile was already selected: a plain click (no drag) then narrows the selection to it.
    narrow: bool,
}

/// Box selection in progress: start point in content coordinates and the selection it started from.
struct Band {
    x: f32,
    y: f32,
    base: HashSet<String>,
}

#[derive(Default)]
pub struct State {
    info: Arc<Mutex<HashMap<String, Info>>>,
    loading: Arc<Mutex<Vec<String>>>,
    last_click: Option<(String, Instant)>,
    /// Right-click menu: the paths it acts on and where it opened.
    menu: Option<(Vec<String>, f32, f32)>,
    press: Option<Press>,
    band: Option<Band>,
    selected: HashSet<String>,
    /// Where a Shift+click range starts.
    anchor: Option<String>,
}

pub fn shown(s: &mut State) {
    s.menu = None;
    s.press = None;
    s.band = None;
    bg::spawn(shelf::prune_missing);
}

fn key(path: &str) -> u64 {
    id("shelf.thumb") ^ crate::ui::id(path).rotate_left(7)
}

fn modifier(vk: u16) -> bool {
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(vk as i32) < 0 }
}

/// Tile `k` of the grid whose content starts at (`x0`, `top`).
fn tile_rect(k: usize, per_row: usize, x0: f32, top: f32) -> Rect {
    Rect::new(x0 + (k % per_row) as f32 * (TILE_W + 4.0) + 2.0, top + (k / per_row) as f32 * (TILE_H + 4.0) + 2.0, TILE_W, TILE_H)
}

fn intersects(a: &Rect, b: &Rect) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, go: &mut Option<&'static str>) {
    let t = ui.t;
    let items = shelf::all();
    // Forget selected paths that left the shelf.
    if !s.selected.is_empty() {
        s.selected.retain(|p| items.iter().any(|i| &i.path == p));
    }
    let menu_hit = menu_input(ui, s, r, go);

    // Header: count (or selection) on the left; hint and Clear (24 tall) on the right; 8 below.
    let (top_r, body) = r.take_top(24.0, 8.0);
    if !items.is_empty() {
        let n = items.len();
        let label = if s.selected.is_empty() {
            crate::i18n::tf!("{n} item{}", "{n} item{}", if n == 1 || crate::i18n::is_id() { "" } else { "s" })
        } else {
            crate::i18n::tf!("{} of {n} selected", "{} dari {n} dipilih", s.selected.len())
        };
        ui.text(&label, top_r, 12.5, crate::gfx::SEMIBOLD, t.subtext, Align::Left);
    }
    let clear_w = ui.measure(tr("Clear"), 13.0, crate::gfx::MEDIUM).ceil() + 28.0;
    let (clear_r, hint_r) = top_r.take_right(clear_w, 10.0);
    ui.text(tr("Drag out · double-click to open"), hint_r, 12.5, crate::gfx::SEMIBOLD, t.subtext, Align::Right);
    if button(ui, clear_r, tr("Clear")) && !menu_hit {
        shelf::clear();
        s.selected.clear();
    }

    ui.fill(body, 16.0, t.surface);
    if items.is_empty() {
        dashed_round(ui, body.inset(0.6, 0.6), 12.0, t.track, 1.2, 6.0, 4.8);
        let cy = body.cy() - (26.0 + 8.0 + 17.0) / 2.0;
        ui.icon("ArchiveBox", body.cx(), cy + 13.0, 26.0, t.subtext);
        ui.text(tr("Drop files or text onto the notch"), Rect::new(body.x, cy + 34.0, body.w, 17.0), 12.5, crate::gfx::SEMIBOLD, t.subtext, Align::Center);
        draw_menu(ui, s, r);
        return;
    }

    // Folder flags for every item, icons for a small shelf: loaded in the background, never in the frame.
    let thumbs_on = items.len() <= MAX_THUMBS;
    {
        let info = s.info.lock().unwrap();
        let loading = s.loading.lock().unwrap();
        let missing: Vec<String> = items
            .iter()
            .map(|i| i.path.clone())
            .filter(|p| !loading.contains(p) && info.get(p).map_or(true, |x| thumbs_on && !x.thumb_tried))
            .collect();
        drop(loading);
        drop(info);
        if !missing.is_empty() {
            s.loading.lock().unwrap().extend(missing.iter().cloned());
            let (info, loading) = (s.info.clone(), s.loading.clone());
            bg::spawn(move || {
                for p in missing {
                    let folder = std::path::Path::new(&p).is_dir();
                    let png = if thumbs_on { shelf::thumbnail(&p, 96) } else { None };
                    info.lock().unwrap().insert(p.clone(), Info { folder, png, thumb_tried: thumbs_on });
                    loading.lock().unwrap().retain(|x| *x != p);
                }
            });
        }
    }

    // Keyboard: Ctrl+A selects all, Delete removes the selection, Esc clears it.
    let keys = std::mem::take(&mut ui.input.keys);
    for k in &keys {
        match k {
            Key::SelectAll => s.selected = items.iter().map(|i| i.path.clone()).collect(),
            Key::Delete | Key::Backspace if !s.selected.is_empty() => {
                for p in s.selected.drain() {
                    shelf::remove(&p);
                }
            }
            Key::Escape if !s.selected.is_empty() => s.selected.clear(),
            _ => {}
        }
    }
    ui.input.keys = keys;

    // WrapPanel inside a ScrollViewer with 6 padding; tiles 74×82 with a 2 margin.
    let inner = body.inset(6.0, 6.0);
    let per_row = (inner.w / (TILE_W + 4.0)).floor().max(1.0) as usize;
    let rows = items.len().div_ceil(per_row);
    let content_h = rows as f32 * (TILE_H + 4.0);
    let info_cell = s.info.clone();
    let selected = s.selected.clone();
    let mut remove: Option<String> = None;
    let mut menu_at = None;
    let mut grid_top = inner.y;
    ui.scroll(id("shelf.grid"), inner, content_h, |ui, top| {
        grid_top = top;
        for (k, item) in items.iter().enumerate() {
            let tile = tile_rect(k, per_row, inner.x, top);
            if tile.bottom() < inner.y || tile.y > inner.bottom() {
                continue;
            }
            let hov = ui.hovered(tile);
            let sel = selected.contains(&item.path);
            if sel {
                ui.fill(tile, 9.0, t.accent_soft);
                ui.g.stroke_round(tile.inset(0.5, 0.5).d2d(), 8.5, ui.c(with_alpha(t.accent, 0.55)), 1.0);
            } else if hov {
                ui.fill(tile, 9.0, t.surface2);
            }
            let k = key(&item.path);
            // Only the folder flag per frame; icon bytes are copied out just once, to decode.
            let (folder, png) = {
                let g = info_cell.lock().unwrap();
                let it = g.get(&item.path);
                let need = thumbs_on && !ui.g.has_image(k);
                (it.is_some_and(|x| x.folder), if need { it.and_then(|x| x.png.clone()) } else { None })
            };
            let icon = Rect::new(tile.cx() - 17.0, tile.y + 9.0, 34.0, 34.0);
            let drawn = thumbs_on
                && (ui.g.has_image(k) || png.is_some_and(|bytes| ui.g.load_image(k, &bytes)))
                && {
                    ui.g.image(k, icon.d2d(), 0.0, ui.alpha);
                    true
                };
            if !drawn {
                let glyph = if folder { "Folder" } else { "File" };
                ui.icon(glyph, icon.cx(), icon.cy(), 28.0, if folder { t.accent } else { t.subtext });
            }
            // Name: 11 px, centred, wraps to two lines (MaxHeight 27), ellipsis.
            let (l1, l2) = two_lines(ui, &item.name(), 11.0, tile.w - 8.0);
            let ny = icon.bottom() + 5.0;
            ui.text(&l1, Rect::new(tile.x + 4.0, ny, tile.w - 8.0, 13.5), 11.0, REGULAR, t.text, Align::Center);
            if !l2.is_empty() {
                ui.text(&l2, Rect::new(tile.x + 4.0, ny + 13.5, tile.w - 8.0, 13.5), 11.0, REGULAR, t.text, Align::Center);
            }

            if hov && s_band_idle(ui) {
                let x = Rect::new(tile.right() - 22.0, tile.y + 2.0, 20.0, 20.0);
                ui.fill(x, 8.0, t.hover);
                ui.icon("XMark", x.cx(), x.cy(), 11.0, t.text);
                if ui.clicked(x) {
                    remove = Some(item.path.clone());
                }
            }
            if ui.right_clicked(tile) {
                let (mx, my) = ui.mouse();
                menu_at = Some((item.path.clone(), mx, my));
            }
        }
    });

    if menu_hit {
        draw_menu(ui, s, r);
        return;
    }
    if let Some(p) = remove {
        s.selected.remove(&p);
        shelf::remove(&p);
    }
    // Right-click: on a selected tile the menu acts on the whole selection, else on that tile alone.
    if let Some((p, mx, my)) = menu_at {
        if !s.selected.contains(&p) {
            s.selected = HashSet::from([p.clone()]);
            s.anchor = Some(p);
        }
        let paths: Vec<String> = items.iter().filter(|i| s.selected.contains(&i.path)).map(|i| i.path.clone()).collect();
        s.menu = Some((paths, mx, my));
    }

    let (mx, my) = ui.mouse();
    let ctrl = modifier(0x11);
    let shift = modifier(0x10);
    let in_grid = inner.contains((mx, my)) && !ui.input.consumed;

    // A press in the grid: on a tile it selects (and may start a drag); on empty space it starts a box.
    if ui.input.pressed && in_grid {
        ui.input.consumed = true;
        ui.wants_keyboard = true; // Ctrl+A / Delete / Esc
        let hit = items.iter().enumerate().find(|(k, _)| tile_rect(*k, per_row, inner.x, grid_top).contains((mx, my)));
        match hit {
            Some((k, item)) => {
                let p = item.path.clone();
                let was = s.selected.contains(&p);
                if shift {
                    // Range from the anchor (Ctrl+Shift adds it to the selection).
                    let a = s.anchor.as_ref().and_then(|a| items.iter().position(|i| &i.path == a)).unwrap_or(k);
                    if !ctrl {
                        s.selected.clear();
                    }
                    for i in &items[a.min(k)..=a.max(k)] {
                        s.selected.insert(i.path.clone());
                    }
                } else if ctrl {
                    if was {
                        s.selected.remove(&p);
                    } else {
                        s.selected.insert(p.clone());
                    }
                    s.anchor = Some(p.clone());
                } else {
                    if !was {
                        s.selected = HashSet::from([p.clone()]);
                    }
                    s.anchor = Some(p.clone());
                    let double = s.last_click.as_ref().is_some_and(|(q, at)| *q == p && at.elapsed().as_millis() < 500);
                    if double {
                        s.last_click = None;
                        crate::shell::open(&p);
                    } else {
                        s.last_click = Some((p.clone(), Instant::now()));
                        s.press = Some(Press { path: p, x: mx, y: my, narrow: was });
                    }
                }
                if (shift || ctrl) && s.selected.contains(&item.path) {
                    s.press = Some(Press { path: item.path.clone(), x: mx, y: my, narrow: false });
                }
            }
            None => {
                let base = if ctrl || shift { s.selected.clone() } else { HashSet::new() };
                s.selected = base.clone();
                s.band = Some(Band { x: mx - inner.x, y: my - grid_top, base });
            }
        }
    }

    // A press on a tile that travels a few DIPs drags the selection out (done on the window thread).
    if let Some(p) = &s.press {
        if !ui.input.down {
            // Released without a drag: a plain click on a selected tile narrows the selection to it.
            if p.narrow {
                s.selected = HashSet::from([p.path.clone()]);
            }
            s.press = None;
        } else if (mx - p.x).hypot(my - p.y) > DRAG_START {
            let mut paths: Vec<String> = items.iter().filter(|i| s.selected.contains(&i.path)).map(|i| i.path.clone()).collect();
            if !paths.contains(&p.path) {
                paths = vec![p.path.clone()];
            }
            s.press = None;
            s.last_click = None;
            start_drag(paths);
        }
    }

    // Box selection: everything the box touches (plus what was selected before with Ctrl/Shift).
    if let Some(b) = &s.band {
        if !ui.input.down {
            s.band = None;
        } else {
            let (x0, y0) = (inner.x + b.x, grid_top + b.y);
            let bx = Rect::new(x0.min(mx), y0.min(my), (mx - x0).abs(), (my - y0).abs());
            let mut sel = b.base.clone();
            if bx.w > 2.0 || bx.h > 2.0 {
                for (k, item) in items.iter().enumerate() {
                    if intersects(&bx, &tile_rect(k, per_row, inner.x, grid_top)) {
                        sel.insert(item.path.clone());
                    }
                }
                ui.push_clip(inner);
                ui.fill(bx, 4.0, with_alpha(t.accent, 0.14));
                ui.g.stroke_round(bx.inset(0.5, 0.5).d2d(), 3.5, ui.c(with_alpha(t.accent, 0.7)), 1.0);
                ui.pop_clip();
            }
            s.selected = sel;
            ui.animate = true;
        }
    }
    draw_menu(ui, s, r);
}

/// Hover affordances (the × button) are hidden while a box is being drawn.
fn s_band_idle(ui: &Ui) -> bool {
    !ui.input.down
}

thread_local! {
    static NAMES: std::cell::RefCell<HashMap<String, (String, String)>> = Default::default();
}

/// Cached [`split_name`]: names are laid out every frame.
fn two_lines(ui: &Ui, s: &str, size: f32, w: f32) -> (String, String) {
    if let Some(v) = NAMES.with(|c| c.borrow().get(s).cloned()) {
        return v;
    }
    let v = split_name(ui, s, size, w);
    NAMES.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 500 {
            c.clear();
        }
        c.insert(s.to_string(), v.clone());
    });
    v
}

/// Splits a name into at most two centred lines that fit `w` (at a space when possible, else by characters);
/// the second line ends in "…" when it doesn't fit.
fn split_name(ui: &Ui, s: &str, size: f32, w: f32) -> (String, String) {
    if ui.measure(s, size, REGULAR) <= w {
        return (s.to_string(), String::new());
    }
    let chars: Vec<char> = s.chars().collect();
    // Longest prefix that fits.
    let mut n = 1;
    while n < chars.len() && ui.measure(&chars[..n + 1].iter().collect::<String>(), size, REGULAR) <= w {
        n += 1;
    }
    // Prefer a word break (WPF wraps at spaces; long words break by characters).
    let brk = (1..n).rev().find(|&i| chars[i - 1] == ' ').unwrap_or(n);
    let l1: String = chars[..brk].iter().collect::<String>().trim_end().to_string();
    let rest: String = chars[brk..].iter().collect::<String>().trim_start().to_string();
    if ui.measure(&rest, size, REGULAR) <= w {
        return (l1, rest);
    }
    let rc: Vec<char> = rest.chars().collect();
    let mut m = 0;
    while m < rc.len() && ui.measure(&(rc[..m + 1].iter().collect::<String>() + "…"), size, REGULAR) <= w {
        m += 1;
    }
    (l1, rc[..m].iter().collect::<String>().trim_end().to_string() + "…")
}

/// Default button (Surface2, 13 Medium, 8 radius; hover dims it).
fn button(ui: &mut Ui, r: Rect, label: &str) -> bool {
    let t = ui.t;
    let hov = ui.hovered(r);
    ui.fill(r, 8.0, crate::gfx::with_alpha(t.surface2, if hov { 0.85 } else { 1.0 }));
    ui.text(label, r, 13.0, crate::gfx::MEDIUM, t.text, Align::Center);
    ui.clicked(r)
}

/// A dashed rounded-rectangle outline (WPF StrokeDashArray: `dash`/`gap` in DIPs).
fn dashed_round(ui: &mut Ui, r: Rect, radius: f32, c: crate::gfx::Color, width: f32, dash: f32, gap: f32) {
    // Perimeter as a polyline: straight edges plus 8-step corner arcs.
    let mut pts: Vec<(f32, f32)> = Vec::new();
    let corners = [
        (r.right() - radius, r.y + radius, -90.0f32),
        (r.right() - radius, r.bottom() - radius, 0.0),
        (r.x + radius, r.bottom() - radius, 90.0),
        (r.x + radius, r.y + radius, 180.0),
    ];
    for (cx, cy, a0) in corners {
        for k in 0..=8 {
            let a = (a0 + k as f32 * 90.0 / 8.0).to_radians();
            pts.push((cx + radius * a.cos(), cy + radius * a.sin()));
        }
    }
    pts.push(pts[0]);
    let geo = ui.g.path(|sink| unsafe {
        use windows::Win32::Graphics::Direct2D::Common::{D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN};
        let mut on = true;
        let mut left = dash;
        let mut cur = pts[0];
        sink.BeginFigure(crate::gfx::pt(cur.0, cur.1), D2D1_FIGURE_BEGIN_HOLLOW);
        for &next in &pts[1..] {
            let mut seg = ((next.0 - cur.0).powi(2) + (next.1 - cur.1).powi(2)).sqrt();
            while seg > 0.0 {
                let step = left.min(seg);
                let f = step / seg;
                let p = (cur.0 + (next.0 - cur.0) * f, cur.1 + (next.1 - cur.1) * f);
                if on {
                    sink.AddLine(crate::gfx::pt(p.0, p.1));
                }
                cur = p;
                seg -= step;
                left -= step;
                if left <= 0.0 {
                    if on {
                        sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    } else {
                        sink.BeginFigure(crate::gfx::pt(cur.0, cur.1), D2D1_FIGURE_BEGIN_HOLLOW);
                    }
                    on = !on;
                    left = if on { dash } else { gap };
                }
            }
        }
        if on {
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
        }
    });
    if let Some(geo) = geo {
        ui.g.stroke_geometry(&geo.into(), ui.c(c), width);
    }
}

// ---------------------------------------------------------------- drag out

static PENDING: Mutex<Option<Vec<String>>> = Mutex::new(None);
static DRAGGING: AtomicBool = AtomicBool::new(false);

/// A drag out of the Shelf is running (the notch's own drop target ignores it).
pub fn dragging() -> bool {
    DRAGGING.load(Ordering::Relaxed)
}

/// Queues a drag of `paths` and asks the window to run it once this frame is done.
fn start_drag(paths: Vec<String>) {
    if paths.is_empty() {
        return;
    }
    *PENDING.lock().unwrap() = Some(paths);
    bg::post(WM_SHELF_DRAG);
}

#[windows::core::implement(windows::Win32::System::Ole::IDropSource)]
struct DropSource;

#[allow(non_snake_case)]
impl windows::Win32::System::Ole::IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(
        &self,
        escape: windows::Win32::Foundation::BOOL,
        keys: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
    ) -> windows::core::HRESULT {
        if escape.as_bool() {
            windows::core::HRESULT(0x0004_0101) // DRAGDROP_S_CANCEL
        } else if keys.0 & 0x0001 == 0 {
            windows::core::HRESULT(0x0004_0100) // DRAGDROP_S_DROP: the left button is up
        } else {
            windows::Win32::Foundation::S_OK
        }
    }

    fn GiveFeedback(&self, _effect: windows::Win32::System::Ole::DROPEFFECT) -> windows::core::HRESULT {
        windows::core::HRESULT(0x0004_0102) // DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Runs the queued drag: Windows' drag and drop of the files (copy), modal until the drop. Window thread only
/// (it owns the mouse capture and is OLE-initialised).
pub fn run_pending_drag() {
    let Some(paths) = PENDING.lock().unwrap().take() else { return };
    DRAGGING.store(true, Ordering::Relaxed);
    unsafe {
        use windows::core::HSTRING;
        use windows::Win32::System::Com::IDataObject;
        use windows::Win32::System::Ole::{DoDragDrop, IDropSource, DROPEFFECT, DROPEFFECT_COPY};
        use windows::Win32::UI::Shell::Common::ITEMIDLIST;
        use windows::Win32::UI::Shell::{BHID_DataObject, ILFree, SHCreateShellItemArrayFromIDLists, SHParseDisplayName};
        // One data object for all the files (CF_HDROP and the shell formats), even across folders.
        let mut pidls: Vec<*const ITEMIDLIST> = Vec::new();
        for p in &paths {
            let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
            if SHParseDisplayName(&HSTRING::from(p.as_str()), None, &mut pidl, 0, None).is_ok() && !pidl.is_null() {
                pidls.push(pidl);
            }
        }
        if !pidls.is_empty() {
            if let Ok(arr) = SHCreateShellItemArrayFromIDLists(&pidls) {
                if let Ok(data) = arr.BindToHandler::<_, IDataObject>(None, &BHID_DataObject) {
                    let src: IDropSource = DropSource.into();
                    let mut effect = DROPEFFECT(0);
                    let _ = DoDragDrop(&data, &src, DROPEFFECT_COPY, &mut effect);
                }
            }
        }
        for p in pidls {
            ILFree(Some(p));
        }
    }
    DRAGGING.store(false, Ordering::Relaxed);
    bg::spawn(shelf::prune_missing);
}

// ---------------------------------------------------------------- right-click menu (.NET ContextMenu look)

const MENU_ROW: f32 = 27.0;
const MENU_ITEMS: [&str; 5] = ["Open", "Show in folder", "Copy", "Send to phone", "Remove from shelf"];

fn menu_rect(ui: &Ui, s: &State, r: Rect) -> Option<Rect> {
    let (_, x, y) = s.menu.as_ref()?;
    let w = MENU_ITEMS.iter().map(|l| ui.measure(l, 13.0, REGULAR).ceil() + 20.0).fold(130.0, f32::max) + 8.0;
    let h = MENU_ROW * MENU_ITEMS.len() as f32 + 8.0;
    let x = x.min(r.right() - w);
    let y = if y + h > r.bottom() + 8.0 { (y - h).max(r.y - 40.0) } else { *y };
    Some(Rect::new(x, y, w, h))
}

fn menu_input(ui: &mut Ui, s: &mut State, r: Rect, go: &mut Option<&'static str>) -> bool {
    if s.menu.is_none() || !(ui.input.pressed || ui.input.right_pressed) || ui.input.consumed {
        return false;
    }
    let m = menu_rect(ui, s, r).unwrap();
    let (paths, _, _) = s.menu.take().unwrap();
    let (mx, my) = ui.mouse();
    if m.contains((mx, my)) && ui.input.pressed && !paths.is_empty() {
        match ((my - m.y - 4.0) / MENU_ROW).floor() as i32 {
            0 => {
                for p in &paths {
                    crate::shell::open(p);
                }
            }
            1 => crate::shell::reveal(&paths[0]),
            2 => {
                let p = paths.clone();
                bg::spawn(move || {
                    cliphistory::set_files(&p);
                });
            }
            3 => {
                phone::send(&paths);
                *go = Some("phone");
            }
            4 => {
                for p in &paths {
                    s.selected.remove(p);
                    shelf::remove(p);
                }
            }
            _ => {}
        }
    }
    ui.input.consumed = true;
    true
}

fn draw_menu(ui: &mut Ui, s: &State, r: Rect) {
    let Some(m) = menu_rect(ui, s, r) else { return };
    let t = ui.t;
    ui.g.fill_round(m.d2d(), 8.0, ui.c(t.surface2));
    ui.g.stroke_round(m.inset(0.5, 0.5).d2d(), 7.5, ui.c(t.stroke), 1.0);
    let n = s.menu.as_ref().map_or(0, |m| m.0.len());
    for (k, label) in MENU_ITEMS.iter().enumerate() {
        let row = Rect::new(m.x + 4.0, m.y + 4.0 + k as f32 * MENU_ROW, m.w - 8.0, MENU_ROW);
        if row.contains(ui.mouse()) {
            ui.fill(row, 5.0, t.hover);
        }
        let label = tr(label);
        let text = if n > 1 && k != 1 { format!("{label} ({n})") } else { label.to_string() };
        ui.text(&text, row.pad(10.0, 0.0, 6.0, 0.0), 13.0, REGULAR, t.text, Align::Left);
    }
    ui.animate = true;
}
