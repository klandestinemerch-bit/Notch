//! Quick notes in notes.json, the .NET build's schema: [{ Id, Created, Text, Updated }], newest first.
//! Before notes.json existed there was one plain note.txt; it's read as a single note when notes.json is missing.
//! The selected note's id lives in settings (LastNoteId).

use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::store;
use super::todos::{format_local, local_now_secs, parse_local};

const FILE: &str = "notes.json";
const LEGACY_FILE: &str = "note.txt";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Note {
    /// 32 hex digits (a .NET Guid "N").
    #[serde(default)]
    pub id: String,
    /// .NET DateTime strings, local with offset.
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub updated: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Note {
    /// First non-blank line, or "New note".
    pub fn title(&self) -> &str {
        self.text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(crate::i18n::tr("New note"))
    }

    /// The next non-blank line (up to 80 characters), or "No additional text".
    pub fn preview(&self) -> String {
        match self.text.lines().map(str::trim).filter(|l| !l.is_empty()).nth(1) {
            Some(l) => l.chars().take(80).collect(),
            None => crate::i18n::tr("No additional text").into(),
        }
    }

    pub fn words(&self) -> usize {
        self.text.split_whitespace().count()
    }

    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Last edit as local seconds (for sorting and "when" labels; see todos.rs for the convention).
    pub fn updated_local(&self) -> i64 {
        parse_local(&self.updated).map_or(0, |d| d.0)
    }

    /// "3:05 PM" today, "Nov 2" this year, else "Nov 2, 2025".
    pub fn when(&self) -> String {
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        let t = self.updated_local();
        let now = local_now_secs();
        let (y, m, d) = super::todos::civil_from_days(t.div_euclid(86_400));
        let (ny, _, _) = super::todos::civil_from_days(now.div_euclid(86_400));
        if t.div_euclid(86_400) == now.div_euclid(86_400) {
            let secs = t.rem_euclid(86_400);
            let (h, min) = (secs / 3600, secs % 3600 / 60);
            let h12 = if h % 12 == 0 { 12 } else { h % 12 };
            format!("{h12}:{min:02} {}", if h < 12 { "AM" } else { "PM" })
        } else if y == ny {
            format!("{} {d}", MONTHS[m as usize - 1])
        } else {
            format!("{} {d}, {y}", MONTHS[m as usize - 1])
        }
    }
}

fn now_string() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let local = super::todos::local_from_utc(d.as_secs() as i64);
    format_local(local, d.subsec_nanos() / 100)
}

fn new_id() -> String {
    // Random enough for a note id: time, a counter and the address of a stack value, hashed.
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for _ in 0..2 {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

fn cell() -> &'static Mutex<Vec<Note>> {
    static CELL: OnceLock<Mutex<Vec<Note>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(load()))
}

fn load() -> Vec<Note> {
    if store::path(FILE).exists() {
        let mut notes: Vec<Note> = store::load(FILE);
        for n in &mut notes {
            if n.id.is_empty() {
                n.id = new_id();
            }
            if n.created.is_empty() {
                n.created = now_string();
            }
            if n.updated.is_empty() {
                n.updated = n.created.clone();
            }
        }
        return notes;
    }
    let legacy = store::path(LEGACY_FILE);
    match std::fs::read_to_string(&legacy) {
        Ok(text) if !text.trim().is_empty() => {
            let at = std::fs::metadata(&legacy)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| format_local(super::todos::local_from_utc(d.as_secs() as i64), 0))
                .unwrap_or_else(now_string);
            vec![Note { id: new_id(), created: at.clone(), text, updated: at, extra: Map::new() }]
        }
        _ => vec![],
    }
}

fn listeners() -> &'static Mutex<Vec<fn()>> {
    static L: OnceLock<Mutex<Vec<fn()>>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn on_change(cb: fn()) {
    listeners().lock().unwrap_or_else(|e| e.into_inner()).push(cb);
}

fn change<R>(f: impl FnOnce(&mut Vec<Note>) -> R) -> R {
    let r = {
        let mut notes = cell().lock().unwrap_or_else(|e| e.into_inner());
        let r = f(&mut notes);
        store::save(FILE, &*notes);
        r
    };
    let list = listeners().lock().unwrap_or_else(|e| e.into_inner()).clone();
    for cb in list {
        cb();
    }
    r
}

/// All notes, most recently edited first.
pub fn all() -> Vec<Note> {
    let mut notes = cell().lock().unwrap_or_else(|e| e.into_inner()).clone();
    notes.sort_by_key(|n| std::cmp::Reverse(n.updated_local()));
    notes
}

pub fn get(id: &str) -> Option<Note> {
    cell().lock().unwrap_or_else(|e| e.into_inner()).iter().find(|n| n.id == id).cloned()
}

/// A new note at the top of the list.
pub fn add(text: &str) -> Note {
    let now = now_string();
    let note = Note { id: new_id(), created: now.clone(), text: text.to_string(), updated: now, extra: Map::new() };
    let copy = note.clone();
    change(|notes| notes.insert(0, note));
    copy
}

/// Replaces a note's text (it moves to the top as the most recently edited).
pub fn update(id: &str, text: &str) {
    change(|notes| {
        if let Some(i) = notes.iter().position(|n| n.id == id) {
            if notes[i].text == text {
                return;
            }
            let mut n = notes.remove(i);
            n.text = text.to_string();
            n.updated = now_string();
            notes.insert(0, n);
        }
    });
}

pub fn delete(id: &str) {
    change(|notes| notes.retain(|n| n.id != id));
}

/// Drops empty notes except `keep` (the one being edited), as the .NET build does when saving.
pub fn prune_blank(keep: Option<&str>) {
    change(|notes| notes.retain(|n| !n.is_blank() || Some(n.id.as_str()) == keep));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::settings::tests::{read_json, same, scratch};

    #[test]
    #[ignore]
    fn notes_round_trip() {
        let dir = scratch();
        let before = read_json(&dir.join(FILE));
        let notes = all();
        println!("{} notes", notes.len());
        for n in &notes {
            println!("  {} | {} chars | {} words | {}", n.id, n.text.chars().count(), n.words(), n.when());
        }
        change(|_| {});
        let mut diffs = vec![];
        same(&before, &read_json(&dir.join(FILE)), "notes", &mut diffs);
        for d in &diffs {
            println!("DIFF {d}");
        }
        assert!(diffs.is_empty());

        let n = add("Title\nSecond line");
        assert_eq!(n.id.len(), 32);
        assert_eq!(n.title(), "Title");
        assert_eq!(n.preview(), "Second line");
        update(&n.id, "Changed");
        let saved = read_json(&dir.join(FILE));
        println!("new note as saved: {}", serde_json::to_string(&saved[0]).unwrap());
        assert_eq!(saved[0]["Text"], "Changed");
        delete(&n.id);
        assert_eq!(all().len(), notes.len());
    }
}
