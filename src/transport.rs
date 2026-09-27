//! Transport state and playback scheduler.
//!
//! Each bar is played from the timings plan `crate::arrangement` produces — the
//! same plan the MIDI exporter reads — so a pattern's onsets, a hold that
//! crosses the bar line and a chord offset across the loop boundary all land
//! where the file says they do. Events are walked against **absolute**
//! deadlines, so a bar of 64th notes cannot accumulate drift.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::arrangement::{self, BarEvent};
use crate::music::{Key, Scale, BAR_TICKS};
use crate::progression::Progression;

/// Slowest and fastest tempo the UI will set, in bpm.
///
/// Here rather than in the panel because they are a property of the *transport*:
/// the settings file clamps a hand-edited tempo to the same range the arrows
/// reach, and a panel constant it could not see would let the two disagree.
pub const BPM_MIN: u16 = 40;
pub const BPM_MAX: u16 = 240;

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
    pub note_length_bits: AtomicU32,
    pub live_chord: Mutex<Option<Vec<u8>>>,
    /// When the current bar began, and which bar it is.
    ///
    /// The scheduler owns the bar clock; tap capture lives on the UI thread and
    /// timestamps key presses against this. Without it a tap has no position in
    /// the bar at all.
    bar_started_at: Mutex<Option<(Instant, usize)>>,
    /// Whether the metronome clicks. Set while a rhythm take is being recorded.
    pub metronome: AtomicBool,
    /// How hard the offbeat subdivision is pushed, `0.0` (straight) to `1.0`
    /// (the triplet feel). The default every pattern without its own follows.
    pub swing_bits: AtomicU32,
    /// Which click timbre the metronome uses, as an index into the synth's
    /// click presets.
    pub metronome_sound: AtomicUsize,
    /// Metronome level, `0.0..=1.0`, independent of the progression's own gain.
    pub metronome_volume_bits: AtomicU32,
    /// Clicks per beat: 1 for the beats, 2 for the "&", 4 for sixteenths.
    pub metronome_subdivision: AtomicUsize,
    /// The slot whose stored chord the computed chord stands in for while the
    /// loop plays — the in-place audition. `AUDITION_OFF` means none.
    audition_slot: AtomicUsize,
    /// Stop now, and silence, without moving the bar.
    ///
    /// Pausing only takes effect at the next bar: the scheduler finishes the one
    /// it is on. That is wrong for a panic stop — a chord would ring on for up
    /// to a bar — so this abandons the bar instead, leaving the position alone
    /// so the next play resumes where you left off.
    pub stop_now: AtomicBool,
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
            note_length_bits: AtomicU32::new(1.0f32.to_bits()),
            live_chord: Mutex::new(None),
            bar_started_at: Mutex::new(None),
            metronome: AtomicBool::new(false),
            swing_bits: AtomicU32::new(0.0f32.to_bits()),
            metronome_sound: AtomicUsize::new(0),
            metronome_volume_bits: AtomicU32::new(0.8f32.to_bits()),
            metronome_subdivision: AtomicUsize::new(1),
            audition_slot: AtomicUsize::new(AUDITION_OFF),
            stop_now: AtomicBool::new(false),
        })
    }

    /// Publish the start of a bar, for tap capture.
    pub fn publish_bar(&self, at: Instant, bar: usize) {
        *self.bar_started_at.lock().unwrap() = Some((at, bar));
    }

    /// The current bar's start instant and index, once the clock has run.
    pub fn bar_started(&self) -> Option<(Instant, usize)> {
        *self.bar_started_at.lock().unwrap()
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

    /// How hard the offbeat subdivision is pushed, `0.0..=1.0`.
    pub fn swing(&self) -> f32 {
        f32::from_bits(self.swing_bits.load(Ordering::Relaxed)).clamp(0.0, 1.0)
    }

    pub fn set_swing(&self, v: f32) {
        self.swing_bits
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// Clicks per beat for the metronome: 1, 2 or 4.
    pub fn metronome_subdivision(&self) -> usize {
        self.metronome_subdivision
            .load(Ordering::Relaxed)
            .clamp(1, 4)
    }

    /// The slot being auditioned in place, if any.
    ///
    /// `None` when nothing is armed, or when the armed slot is past the end of a
    /// progression that has since shrunk.
    pub fn audition_slot(&self) -> Option<usize> {
        match self.audition_slot.load(Ordering::Relaxed) {
            AUDITION_OFF => None,
            slot => Some(slot),
        }
    }

    pub fn set_audition_slot(&self, slot: Option<usize>) {
        self.audition_slot
            .store(slot.unwrap_or(AUDITION_OFF), Ordering::Relaxed);
    }

    pub fn set_metronome_subdivision(&self, v: usize) {
        self.metronome_subdivision
            .store(v.clamp(1, 4), Ordering::Relaxed);
    }

    /// Metronome level, `0.0..=1.0`.
    pub fn metronome_volume(&self) -> f32 {
        f32::from_bits(self.metronome_volume_bits.load(Ordering::Relaxed)).clamp(0.0, 1.0)
    }

    pub fn set_metronome_volume(&self, v: f32) {
        self.metronome_volume_bits
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
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

/// `audition_slot`'s "nothing armed" value. A slot index is never anywhere near
/// this, and an `Option` in an atomic needs a lock this path does not want.
const AUDITION_OFF: usize = usize::MAX;

// -----------------------------------------------------------------------------
// Scheduler
// -----------------------------------------------------------------------------

pub enum SchedulerEvent {
    /// Start a chord on one stab group, at a gain of 0..1.
    Stab {
        group: usize,
        notes: Vec<u8>,
        gain: f32,
        /// How hard the note was hit, 0..1. See `arrangement::Stab::velocity`.
        velocity: f32,
    },
    /// Release one stab group.
    ReleaseStab {
        group: usize,
    },
    /// Release every group: the schedule was abandoned mid-bar.
    Silence,
    /// One metronome tick, while a rhythm take is being recorded.
    Click {
        strong: bool,
        /// Which click preset to play, and how loud.
        sound: usize,
        volume: f32,
    },
}

pub struct Scheduler {
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

/// How long `ticks` lasts at the current tempo.
fn ticks_to_duration(ticks: u64, bar_dur: Duration) -> Duration {
    let fraction = ticks.min(BAR_TICKS) as f64 / BAR_TICKS as f64;
    Duration::from_secs_f64(bar_dur.as_secs_f64() * fraction)
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
        // A panic stop outranks everything: it must be silent before the bar's
        // remaining events are considered.
        if transport.stop_now.swap(false, Ordering::Relaxed) {
            transport.playing.store(false, Ordering::Relaxed);
        }

        let playing = transport.playing.load(Ordering::Relaxed);
        let live = transport.live_chord.lock().unwrap().clone();
        let prog_len = transport.progression_len.load(Ordering::Relaxed);
        let bar = transport.current_bar.load(Ordering::Relaxed);
        let bar_dur = transport.bar_duration();
        let note_len = transport.note_length();
        let metronome = transport.metronome.load(Ordering::Relaxed);
        let swing = transport.swing();
        let subdivision = transport.metronome_subdivision();
        let total = prog_len;

        let bar_start = Instant::now();
        transport.publish_bar(bar_start, bar);

        // Everything from here to the end of the metronome block is planning,
        // not playing: it is off the audio thread and its cost is the one that
        // matters most, because the plan is rebuilt on every bar.
        let planning = crate::timing::Scope::new("scheduler.bar");

        // Decide what this bar holds. `None` means silence.
        //
        // There is no live bar any more. The computed chord used to be appended
        // after the loop and sounded whole-bar when the transport was stopped;
        // both are gone, replaced by the audition in `tui`. What is left here is
        // the progression, played as written — except that the computed chord
        // can stand in for one slot, which is how a chord modification is
        // auditioned in context.
        let audition_slot = transport.audition_slot();
        let audition = match (&live, audition_slot) {
            (Some(notes), Some(slot)) if !notes.is_empty() => Some(arrangement::Audition {
                slot,
                notes: notes.as_slice(),
            }),
            _ => None,
        };
        let mut content: Option<Vec<BarEvent>> = if playing && total > 0 {
            let index = bar % total;
            // Rebuilt every bar, so a pattern assignment, an offset edit, a swing
            // change or an auditioned chord takes effect within one bar.
            //
            // The entry owns its rhythm, so the progression lock is all the
            // scheduler needs — there is no second lock to take, and no order to
            // keep between them.
            let plan = {
                let prog = progression.lock().unwrap();
                arrangement::arrangement_auditioning(
                    &prog.slots,
                    &transport.key(),
                    note_len,
                    swing,
                    audition,
                )
            };
            Some(arrangement::bar_events(
                &plan,
                index,
                BAR_TICKS,
                arrangement::RHYTHM_LAYERS,
            ))
        } else {
            None
        };

        // The metronome is layered on top of whatever else the bar holds, and it
        // is the *whole* bar when nothing else does — a click to practise
        // against, with the transport stopped and no progression.
        if metronome {
            let clicks = arrangement::metronome_events(BAR_TICKS, subdivision, swing);
            content = Some(match content {
                Some(mut events) => {
                    events.extend(clicks);
                    arrangement::sort_events(&mut events);
                    events
                }
                None => clicks,
            });
        }

        // Dropped here rather than at the end of the iteration: what follows
        // waits out the rest of the bar, and a scope that included the wait
        // would report the tempo.
        drop(planning);

        let mut interrupted = false;
        match content {
            Some(planned) => {
                if !play_bar(&transport, &stop, &events, bar_start, bar_dur, &planned) {
                    interrupted = true;
                }
            }
            None => {
                // Nothing to play this bar: wait it out, watching for a stop.
                if !sleep_until_watching(&transport, &stop, bar_start + bar_dur) {
                    interrupted = true;
                }
            }
        }

        if interrupted {
            // The schedule was abandoned, so the releases it still owed will
            // never be delivered; cut whatever is sounding.
            events.send(SchedulerEvent::Silence).ok();
            continue;
        }

        // Advance the bar counter for progression playback.
        if playing && total > 0 {
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

/// Play one bar's events at their due times, then wait out the rest of the bar.
///
/// Returns false if an interrupt (stop, seek, restart) fired first. Deadlines
/// are absolute against the bar's start, so a long bar of fast onsets cannot
/// accumulate the sleep rounding the old hold/rest split had.
fn play_bar(
    transport: &Arc<Transport>,
    stop: &Arc<AtomicBool>,
    events_out: &Sender<SchedulerEvent>,
    bar_start: Instant,
    bar_dur: Duration,
    planned: &[BarEvent],
) -> bool {
    for event in planned {
        let due = bar_start + ticks_to_duration(event.at(), bar_dur);
        if !sleep_until_watching(transport, stop, due) {
            return false;
        }
        match event {
            BarEvent::On {
                group,
                notes,
                gain,
                velocity,
                ..
            } => {
                events_out
                    .send(SchedulerEvent::Stab {
                        group: *group,
                        notes: notes.clone(),
                        gain: *gain,
                        velocity: *velocity,
                    })
                    .ok();
            }
            BarEvent::Off { group, .. } => {
                events_out
                    .send(SchedulerEvent::ReleaseStab { group: *group })
                    .ok();
            }
            BarEvent::Click { strong, .. } => {
                let sound = transport.metronome_sound.load(Ordering::Relaxed);
                events_out
                    .send(SchedulerEvent::Click {
                        strong: *strong,
                        sound,
                        volume: transport.metronome_volume(),
                    })
                    .ok();
            }
        }
    }
    sleep_until_watching(transport, stop, bar_start + bar_dur)
}

/// Sleep until `deadline`, checking for external interrupts every few ms.
/// Returns false if an interrupt (stop, seek, restart) fired.
fn sleep_until_watching(
    transport: &Arc<Transport>,
    stop: &Arc<AtomicBool>,
    deadline: Instant,
) -> bool {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        if transport.seek_to.load(Ordering::Relaxed) >= 0 {
            return false;
        }
        if transport.restart.load(Ordering::Relaxed) {
            return false;
        }
        if transport.stop_now.load(Ordering::Relaxed) {
            return false;
        }
        thread::sleep((deadline - now).min(Duration::from_millis(5)));
    }
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
    fn a_panic_stop_interrupts_the_wait_immediately() {
        // The point of `stop_now`: a paused transport still finishes its bar, so
        // a note can ring for a whole bar. This must not.
        let t = Transport::new(c_major());
        let stop = Arc::new(AtomicBool::new(false));
        let deadline = Instant::now() + Duration::from_secs(30);

        let flag = t.clone();
        let start = Instant::now();
        let stopper = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            flag.stop_now.store(true, Ordering::Relaxed);
        });

        assert!(
            !sleep_until_watching(&t, &stop, deadline),
            "the wait must report an interrupt, not run to the deadline"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "it returned after {:?}",
            start.elapsed()
        );
        stopper.join().unwrap();
    }

    #[test]
    fn a_restart_interrupts_the_wait_too() {
        let t = Transport::new(c_major());
        let stop = Arc::new(AtomicBool::new(false));
        let deadline = Instant::now() + Duration::from_secs(30);

        let flag = t.clone();
        let stopper = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            flag.restart.store(true, Ordering::Relaxed);
        });

        assert!(!sleep_until_watching(&t, &stop, deadline));
        stopper.join().unwrap();
    }

    #[test]
    fn the_metronome_starts_off() {
        let t = Transport::new(c_major());
        assert!(!t.metronome.load(Ordering::Relaxed));
    }

    // ---- the bar clock ----

    #[test]
    fn the_bar_clock_is_unset_until_the_scheduler_publishes_one() {
        let t = Transport::new(c_major());
        assert!(t.bar_started().is_none());
    }

    #[test]
    fn publishing_a_bar_round_trips_the_instant_and_the_index() {
        let t = Transport::new(c_major());
        let now = Instant::now();
        t.publish_bar(now, 3);
        let (at, bar) = t.bar_started().expect("a published bar");
        assert_eq!(at, now);
        assert_eq!(bar, 3);
    }

    // ---- the bar's timings ----

    #[test]
    fn ticks_map_onto_the_bar_duration() {
        let bar = Duration::from_secs(2);
        assert_eq!(ticks_to_duration(0, bar), Duration::ZERO);
        assert_eq!(ticks_to_duration(BAR_TICKS, bar), bar);
        assert_eq!(ticks_to_duration(BAR_TICKS / 2, bar), Duration::from_secs(1));
        assert_eq!(
            ticks_to_duration(BAR_TICKS / 4, bar),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn ticks_past_the_bar_line_are_clamped() {
        let bar = Duration::from_secs(2);
        assert_eq!(ticks_to_duration(BAR_TICKS * 10, bar), bar);
    }

    #[test]
    fn ticks_land_on_the_same_instant_as_the_bar_arithmetic() {
        // 64th notes are the tightest grid, and their spacing must not drift.
        let bar = Duration::from_secs(2);
        let step = BAR_TICKS / 64;
        for i in 0..=64 {
            let expected = Duration::from_secs_f64(2.0 * (i * step) as f64 / BAR_TICKS as f64);
            let got = ticks_to_duration(i * step, bar);
            assert!(
                (got.as_secs_f64() - expected.as_secs_f64()).abs() < 1e-9,
                "step {} was {:?}, expected {:?}",
                i,
                got,
                expected
            );
        }
    }
}
