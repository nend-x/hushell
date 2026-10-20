use tauri::AppHandle;

use crate::shared::Shared;

pub const BEAT_MS: u32 = 100;

/// Full re-check of window titles every N beats (catches title-only changes,
/// e.g. browser tab switches) — 50 beats = 5 s.
pub const TITLE_RECHECK_BEATS: u64 = 50;

/// A periodic function. `beats` is the current beat count (for duty-cycling).
pub type BeatFn = unsafe fn(&AppHandle, &mut Shared, u64);

pub struct Cycle {
    pub beats: u64,
    pub fns: Vec<(&'static str, BeatFn)>,
}

impl Cycle {
    pub fn new() -> Self {
        Cycle {
            beats: 0,
            fns: Vec::new(),
        }
    }

    pub fn register(&mut self, name: &'static str, f: BeatFn) {
        self.fns.push((name, f));
    }

    pub fn run(&mut self, app: &AppHandle, s: &mut Shared) {
        self.beats += 1;
        for (_name, f) in &self.fns {
            unsafe { f(app, s, self.beats) };
        }
    }
}
