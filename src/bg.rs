//! Background work. Services expose plain blocking functions; the UI runs them here so the window
//! thread never waits on the network or a device. When a job finishes it asks the window to redraw.

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the window when background state changed and the notch should redraw.
pub const WM_REFRESH: u32 = WM_APP + 2;

static WINDOW: AtomicIsize = AtomicIsize::new(0);

pub fn init(hwnd: HWND) {
    WINDOW.store(hwnd.0 as isize, Ordering::Relaxed);
    // Keep the multithreaded apartment alive for the whole run: windows-rs caches WinRT factories
    // process-wide, and they'd dangle (crash) if the MTA were torn down between background jobs.
    unsafe {
        let _ = windows::Win32::System::Com::CoIncrementMTAUsage();
    }
}

/// The notch window (owner for dialogs opened from background threads).
pub fn window() -> Option<HWND> {
    let h = WINDOW.load(Ordering::Relaxed);
    (h != 0).then(|| HWND(h as _))
}

/// Asks the window to redraw (safe from any thread).
#[track_caller]
pub fn refresh() {
    if std::env::var_os("FATHNOTCH_STATS").is_some() {
        let at = std::panic::Location::caller();
        crate::stat_at(at.file(), at.line());
    }
    let h = WINDOW.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(HWND(h as _), WM_REFRESH, WPARAM(0), LPARAM(0));
        }
    }
}

/// Posted when settings changed (hotkeys, timers and placement are re-applied).
pub const WM_SETTINGS: u32 = WM_APP + 3;

/// Posts `msg` to the window (safe from any thread).
pub fn post(msg: u32) {
    let h = WINDOW.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(HWND(h as _), msg, WPARAM(0), LPARAM(0));
        }
    }
}

/// Runs `job` on its own COM-initialised (MTA) thread, then asks for a redraw.
pub fn spawn<F: FnOnce() + Send + 'static>(job: F) {
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        job();
        refresh();
        unsafe { CoUninitialize() };
    });
}

/// Like `spawn`, for things set while a slider is dragged (volume, brightness): at most one job per `key`
/// runs at a time, and while it runs only the newest waiting one is kept. A slow device (a Bluetooth
/// headset mic, a DDC/CI monitor) then gets the latest value instead of a pile of threads.
pub fn latest<F: FnOnce() + Send + 'static>(key: u64, job: F) {
    use std::collections::HashMap;
    use std::sync::Mutex;
    type Job = Box<dyn FnOnce() + Send>;
    // key → (a worker is running, the job waiting for it)
    static JOBS: Mutex<Option<HashMap<u64, (bool, Option<Job>)>>> = Mutex::new(None);
    {
        let mut g = JOBS.lock().unwrap_or_else(|e| e.into_inner());
        let e = g.get_or_insert_with(HashMap::new).entry(key).or_insert((false, None));
        e.1 = Some(Box::new(job));
        if e.0 {
            return; // the running worker picks it up
        }
        e.0 = true;
    }
    spawn(move || loop {
        let next = {
            let mut g = JOBS.lock().unwrap_or_else(|e| e.into_inner());
            let e = g.get_or_insert_with(HashMap::new).entry(key).or_insert((true, None));
            match e.1.take() {
                Some(j) => j,
                None => {
                    e.0 = false;
                    return;
                }
            }
        };
        next();
    });
}
