//! Transport state and playback scheduler.
//!
//! Each bar is split into a *hold* phase (note_length × bar_duration) and a
//! *rest* phase (the remainder). The scheduler always sends StopChord at the
//! end of the hold, so chords never sustain past their intended length.
//!
//! Chimes fire on a 4-bar cadence while the transport is idle. Once the user
//! has produced a live chord, `chime_suppressed` latches on and chimes never
//! fire again for the session.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::chime::{CHIMES, CHIME_GLIDE_SECS, CHIME_HOLD_SECS};
use crate::music::{Key, Scale};
use crate::progression::Progression;

// -----------------------------------------------------------------------------
// Transport
// -----------------------------------------------------------------------------

pub struct Transport {
    pub bpm: AtomicU32,
    pub playing: AtomicBool,
    pub looping: AtomicBool,
    pub current_bar: AtomicUsize,
    pub seek_to: AtomicI64,
    pub restart: AtomicBool,
    pub progression_len: AtomicUsize,
    pub track_key_tonic: AtomicU32,
    pub track_key_minor: AtomicBool,
    pub chime_index: AtomicUsize,
    pub chime_bar_count: AtomicUsize,
    pub chime_suppressed: AtomicBool,
    pub note_length_bits: AtomicU32,
    pub live_chord: Mutex<Option<Vec<u8>>>,
}

impl Transport {
    pub fn new(key: Key) -> Arc<Self> {
        Arc::new(Transport {
            bpm: AtomicU32::new(120),
            playing: AtomicBool::new(false),
            looping: AtomicBool::new(true),
            current_bar: AtomicUsize::new(0),
            seek_to: AtomicI64::new(-1),
            restart: AtomicBool::new(false),
            progression_len: AtomicUsize::new(0),
            track_key_tonic: AtomicU32::new(key.tonic as u32),
            track_key_minor: AtomicBool::new(key.scale == Scale::Minor),
            chime_index: AtomicUsize::new(0),
            chime_bar_count: AtomicUsize::new(0),
            chime_suppressed: AtomicBool::new(false),
            note_length_bits: AtomicU32::new(1.0f32.to_bits()),
            live_chord: Mutex::new(None),
        })
    }

    pub fn key(&self) -> Key {
        let tonic = self.track_key_tonic.load(Ordering::Relaxed) as u8;
        let scale = if self.track_key_minor.load(Ordering::Relaxed) {
            Scale::Minor
        } else {
            Scale::Major
        };
        Key::new(tonic, scale)
    }

    pub fn set_key(&self, k: Key) {
        self.track_key_tonic
            .store(k.tonic as u32, Ordering::Relaxed);
        self.track_key_minor
            .store(k.scale == Scale::Minor, Ordering::Relaxed);
    }

    pub fn bpm(&self) -> u16 {
        self.bpm.load(Ordering::Relaxed).min(u16::MAX as u32) as u16
    }

    pub fn set_bpm(&self, v: u16) {
        self.bpm.store(v as u32, Ordering::Relaxed);
    }

    pub fn bar_duration(&self) -> Duration {
        let bpm = self.bpm().max(1) as u64;
        Duration::from_micros(60_000_000 / bpm * 4)
    }

    pub fn set_live_chord(&self, notes: Option<Vec<u8>>) {
        *self.live_chord.lock().unwrap() = notes;
    }

    /// Fraction of a bar a chord sustains before releasing (0..1).
    pub fn note_length(&self) -> f32 {
        f32::from_bits(self.note_length_bits.load(Ordering::Relaxed)).clamp(0.05, 1.0)
    }

    pub fn set_note_length(&self, v: f32) {
        self.note_length_bits
            .store(v.clamp(0.05, 1.0).to_bits(), Ordering::Relaxed);
    }
}

// -----------------------------------------------------------------------------
// Scheduler
// -----------------------------------------------------------------------------

pub enum SchedulerEvent {
    PlayChord(Vec<u8>),
    StopChord,
    PlayChime {
        start: [u8; 3],
        end: [u8; 3],
        glide_secs: f32,
        hold_secs: f32,
    },
    RecordChime {
        index: usize,
    },
}

pub struct Scheduler {
    pub transport: Arc<Transport>,
    events: Receiver<SchedulerEvent>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Scheduler {
    pub fn start(transport: Arc<Transport>, progression: Arc<Mutex<Progression>>) -> Self {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));

        let t = transport.clone();
        let p = progression;
        let s = stop.clone();
        let handle = thread::spawn(move || scheduler_loop(t, p, tx, s));

        Scheduler {
            transport,
            events: rx,
            stop,
            handle: Some(handle),
        }
    }

    pub fn try_recv(&self) -> Option<SchedulerEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// What the scheduler should do for the current bar.
enum BarAction {
    Play(Vec<u8>),
    Chime,
    Silent,
}

fn scheduler_loop(
    transport: Arc<Transport>,
    progression: Arc<Mutex<Progression>>,
    events: Sender<SchedulerEvent>,
    stop: Arc<AtomicBool>,
) {
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }

        // Consume seeks and restarts before computing this bar.
        let seek = transport.seek_to.swap(-1, Ordering::Relaxed);
        if seek >= 0 {
            transport
                .current_bar
                .store(seek as usize, Ordering::Relaxed);
        }
        if transport.restart.swap(false, Ordering::Relaxed) {
            transport.current_bar.store(0, Ordering::Relaxed);
        }

        let playing = transport.playing.load(Ordering::Relaxed);
        let live = transport.live_chord.lock().unwrap().clone();
        let prog_len = transport.progression_len.load(Ordering::Relaxed);
        let bar = transport.current_bar.load(Ordering::Relaxed);
        let bar_dur = transport.bar_duration();
        let note_len = transport.note_length();
        let hold_dur = bar_dur.mul_f64(note_len as f64);
        let rest_dur = bar_dur.saturating_sub(hold_dur);
        let chime_off = transport.chime_suppressed.load(Ordering::Relaxed);

        // Decide what this bar is.
        let action = if playing {
            let total = prog_len + if live.is_some() { 1 } else { 0 };
            if total == 0 {
                if chime_off {
                    BarAction::Silent
                } else {
                    BarAction::Chime
                }
            } else {
                let idx = bar % total;
                if idx < prog_len {
                    let prog = progression.lock().unwrap();
                    let key = transport.key();
                    match prog.slots.get(idx).and_then(|s| s.notes(&key)) {
                        Some(n) => BarAction::Play(n),
                        None => BarAction::Silent,
                    }
                } else {
                    match live {
                        Some(n) => BarAction::Play(n),
                        None => BarAction::Silent,
                    }
                }
            }
        } else if let Some(notes) = live {
            BarAction::Play(notes)
        } else if chime_off {
            BarAction::Silent
        } else {
            BarAction::Chime
        };

        // Execute the bar.
        let mut interrupted = false;
        match action {
            BarAction::Play(notes) => {
                events.send(SchedulerEvent::PlayChord(notes)).ok();
                if !sleep_watching(&transport, &stop, hold_dur) {
                    interrupted = true;
                }
                // Always stop, even if interrupted, to clean up the note.
                events.send(SchedulerEvent::StopChord).ok();
                if !interrupted && !sleep_watching(&transport, &stop, rest_dur) {
                    interrupted = true;
                }
            }
            BarAction::Chime => {
                fire_idle_tick(&transport, &events);
                if !sleep_watching(&transport, &stop, bar_dur) {
                    interrupted = true;
                }
            }
            BarAction::Silent => {
                if !sleep_watching(&transport, &stop, bar_dur) {
                    interrupted = true;
                }
            }
        }

        if interrupted {
            continue;
        }

        // Advance the bar counter for progression playback.
        if playing {
            let total = prog_len + if live.is_some() { 1 } else { 0 };
            if total > 0 {
                let next = (bar + 1) % total;
                if next == 0 && !transport.looping.load(Ordering::Relaxed) {
                    transport.playing.store(false, Ordering::Relaxed);
                    transport.current_bar.store(0, Ordering::Relaxed);
                } else {
                    transport.current_bar.store(next, Ordering::Relaxed);
                }
            }
        }
    }
}

/// Sleep for `dur`, checking for external interrupts every few ms.
/// Returns false if an interrupt (stop, seek, restart) fired.
fn sleep_watching(transport: &Arc<Transport>, stop: &Arc<AtomicBool>, dur: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < dur {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        if transport.seek_to.load(Ordering::Relaxed) >= 0 {
            return false;
        }
        if transport.restart.load(Ordering::Relaxed) {
            return false;
        }
        let remaining = dur.saturating_sub(start.elapsed());
        thread::sleep(remaining.min(Duration::from_millis(5)));
    }
    true
}

/// Fire an idle chime if the 4-bar cadence says it's time.
fn fire_idle_tick(transport: &Arc<Transport>, events: &Sender<SchedulerEvent>) {
    let count = transport.chime_bar_count.load(Ordering::Relaxed);
    if count % 4 == 0 {
        let idx = transport.chime_index.load(Ordering::Relaxed) % CHIMES.len();
        let chime = &CHIMES[idx];
        events
            .send(SchedulerEvent::PlayChime {
                start: chime.start,
                end: chime.end,
                glide_secs: CHIME_GLIDE_SECS,
                hold_secs: CHIME_HOLD_SECS,
            })
            .ok();
        events.send(SchedulerEvent::RecordChime { index: idx }).ok();
        transport.chime_index.store(idx + 1, Ordering::Relaxed);
    }
    transport
        .chime_bar_count
        .store(count + 1, Ordering::Relaxed);
    transport.current_bar.store(0, Ordering::Relaxed);
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn c_major() -> Key {
        Key::new(60, Scale::Major)
    }

    #[test]
    fn key_round_trips_major() {
        let t = Transport::new(c_major());
        assert_eq!(t.key().tonic, 60);
        assert_eq!(t.key().scale, Scale::Major);
    }

    #[test]
    fn key_round_trips_minor() {
        let t = Transport::new(Key::new(57, Scale::Minor));
        assert_eq!(t.key().tonic, 57);
        assert_eq!(t.key().scale, Scale::Minor);
    }

    #[test]
    fn set_key_updates_both_fields() {
        let t = Transport::new(c_major());
        t.set_key(Key::new(62, Scale::Minor));
        assert_eq!(t.key().tonic, 62);
        assert_eq!(t.key().scale, Scale::Minor);
    }

    #[test]
    fn bar_duration_at_120_bpm_is_2_seconds() {
        let t = Transport::new(c_major());
        t.set_bpm(120);
        assert_eq!(t.bar_duration(), Duration::from_secs(2));
    }

    #[test]
    fn bar_duration_at_60_bpm_is_4_seconds() {
        let t = Transport::new(c_major());
        t.set_bpm(60);
        assert_eq!(t.bar_duration(), Duration::from_secs(4));
    }

    #[test]
    fn bpm_round_trips() {
        let t = Transport::new(c_major());
        t.set_bpm(90);
        assert_eq!(t.bpm(), 90);
    }

    #[test]
    fn note_length_round_trips() {
        let t = Transport::new(c_major());
        t.set_note_length(0.5);
        assert_eq!(t.note_length(), 0.5);
    }

    #[test]
    fn note_length_clamps() {
        let t = Transport::new(c_major());
        t.set_note_length(0.0);
        assert_eq!(t.note_length(), 0.05);
        t.set_note_length(2.0);
        assert_eq!(t.note_length(), 1.0);
    }

    #[test]
    fn chime_suppressed_starts_false() {
        let t = Transport::new(c_major());
        assert!(!t.chime_suppressed.load(Ordering::Relaxed));
    }
}
