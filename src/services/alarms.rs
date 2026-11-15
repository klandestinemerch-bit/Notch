//! Alarms (the Timers tab's Alarm segment): a time of day, a label and the days it repeats on (none: once).
//! Saved in alarms.json. A due alarm keeps ringing until Snooze (again in 5 min) or Stop (a one-time one turns off).

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::services::store;
use crate::i18n::tr;

const FILE: &str = "alarms.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Alarm {
    pub id: u64,
    pub hour: u32,
    pub minute: u32,
    pub label: String,
    pub enabled: bool,
    /// Days it repeats on, 0 = Sunday … 6 = Saturday. Empty: once.
    pub days: Vec<u8>,
}

impl Alarm {
    pub fn time(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }

    /// "Once", "Every day", "Weekdays", "Weekends" or "Mon, Wed, Fri".
    pub fn repeat_text(&self) -> String {
        let mut d = self.days.clone();
        d.sort_unstable();
        d.dedup();
        match d.as_slice() {
            [] => tr("Once").into(),
            [0, 1, 2, 3, 4, 5, 6] => tr("Every day").into(),
            [1, 2, 3, 4, 5] => tr("Weekdays").into(),
            [0, 6] => tr("Weekends").into(),
            _ => d.iter().map(|&i| crate::i18n::day3(i as usize)).collect::<Vec<_>>().join(", "),
        }
    }

    /// Minutes from now (`dow`, `hour`, `minute`) until it next rings.
    pub fn minutes_until(&self, dow: usize, hour: u32, minute: u32) -> u32 {
        let now = hour * 60 + minute;
        let at = self.hour * 60 + self.minute;
        for ahead in 0..8u32 {
            let day = (dow + ahead as usize) % 7;
            if !self.days.is_empty() && !self.days.contains(&(day as u8)) {
                continue;
            }
            let mins = ahead * 1440 + at;
            if mins > now {
                return mins - now;
            }
        }
        7 * 1440
    }
}

/// Snooze length.
pub const SNOOZE_MIN: u64 = 5;

struct State {
    list: Vec<Alarm>,
    /// The last minute an alarm rang ("id@yyyy-mm-dd hh:mm"), so it rings once per minute.
    rang: Vec<String>,
    /// The alarm ringing now: it keeps ringing until Snooze or Stop.
    ringing: Option<u64>,
    /// Snoozed alarms and when they ring again.
    snoozed: Vec<(u64, Instant)>,
}

fn state() -> std::sync::MutexGuard<'static, State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State { list: store::load(FILE), rang: Vec::new(), ringing: None, snoozed: Vec::new() }))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn changed(s: &State) {
    store::save(FILE, &s.list);
    crate::bg::refresh();
}

/// All alarms, by time of day.
pub fn all() -> Vec<Alarm> {
    let mut v = state().list.clone();
    v.sort_by_key(|a| (a.hour, a.minute, a.id));
    v
}

pub fn add(hour: u32, minute: u32, label: &str, days: Vec<u8>) {
    let mut s = state();
    let id = s.list.iter().map(|a| a.id).max().unwrap_or(0) + 1;
    s.list.push(Alarm { id, hour: hour % 24, minute: minute % 60, label: label.trim().to_string(), enabled: true, days });
    changed(&s);
}

pub fn set_enabled(id: u64, on: bool) {
    let mut s = state();
    if let Some(a) = s.list.iter_mut().find(|a| a.id == id) {
        a.enabled = on;
    }
    if !on {
        s.snoozed.retain(|(i, _)| *i != id);
    }
    changed(&s);
}

pub fn remove(id: u64) {
    let mut s = state();
    s.list.retain(|a| a.id != id);
    s.snoozed.retain(|(i, _)| *i != id);
    if s.ringing == Some(id) {
        s.ringing = None;
    }
    changed(&s);
}

/// An alarm that starts ringing now: its minute came (once per minute), or its snooze ran out. It stays
/// the ringing one (and on, even a one-time alarm) until `snooze` or `stop`.
pub fn due() -> Option<Alarm> {
    let n = crate::notch::now();
    let mut s = state();
    if s.ringing.is_some() {
        return None;
    }
    let now = Instant::now();
    if let Some(pos) = s.snoozed.iter().position(|(_, at)| *at <= now) {
        let (id, _) = s.snoozed.remove(pos);
        if let Some(a) = s.list.iter().find(|a| a.id == id).cloned() {
            s.ringing = Some(id);
            return Some(a);
        }
    }
    let stamp = format!("{}-{:02}-{:02} {:02}:{:02}", n.year, n.month, n.day, n.hour, n.minute);
    let hit = s
        .list
        .iter()
        .find(|a| {
            a.enabled
                && a.hour == n.hour as u32
                && a.minute == n.minute as u32
                && (a.days.is_empty() || a.days.contains(&(n.dow as u8)))
                && !s.rang.contains(&format!("{}@{stamp}", a.id))
        })
        .cloned()?;
    s.rang.push(format!("{}@{stamp}", hit.id));
    if s.rang.len() > 20 {
        s.rang.remove(0);
    }
    s.snoozed.retain(|(i, _)| *i != hit.id);
    s.ringing = Some(hit.id);
    Some(hit)
}

/// The alarm ringing now.
pub fn ringing() -> Option<Alarm> {
    let s = state();
    let id = s.ringing?;
    s.list.iter().find(|a| a.id == id).cloned()
}

/// When a snoozed alarm rings again (minutes from now, rounded up).
pub fn snoozed_for(id: u64) -> Option<u64> {
    let s = state();
    let at = s.snoozed.iter().find(|(i, _)| *i == id)?.1;
    Some((at.saturating_duration_since(Instant::now()).as_secs() + 59) / 60)
}

/// Quiet now, rings again in `SNOOZE_MIN` minutes.
pub fn snooze() {
    let mut s = state();
    if let Some(id) = s.ringing.take() {
        s.snoozed.retain(|(i, _)| *i != id);
        s.snoozed.push((id, Instant::now() + std::time::Duration::from_secs(SNOOZE_MIN * 60)));
    }
    crate::bg::refresh();
}

/// Done with it: a one-time alarm turns off; a repeating one waits for its next day.
pub fn stop() {
    let mut s = state();
    if let Some(id) = s.ringing.take() {
        s.snoozed.retain(|(i, _)| *i != id);
        if let Some(a) = s.list.iter_mut().find(|a| a.id == id) {
            if a.days.is_empty() {
                a.enabled = false;
            }
        }
        changed(&s);
    }
}
