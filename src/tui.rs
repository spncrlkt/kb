//! TUI: chord grammar, synth controls, progression, transport.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Local;
use crossterm::{
    cursor::MoveTo,
    event::{
        self, Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};

use crate::chime::CHIMES;
use crate::debug_log::{Logger, OutputTap};
use crate::export;
use crate::grammar::{left_hand_degree, right_hand_transformation};
use crate::keyboard::{Hotkey, KeyPosition, PositionSet, ACTIVE_LAYOUT};
use crate::midi;
use crate::music::{
    chord_label, diatonic_triad, diatonic_triad_label, note_name, ChordSpec, Key, Scale,
    ScaleDegree, Transformation,
};
use crate::presets::{default_path, PatchStore};
use crate::progression::{Progression, ProgressionEntry, Registers, Slot};
use crate::project;
use crate::synth::{Synth, SynthParams, Waveform};
use crate::transport::{Scheduler, SchedulerEvent, Transport};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

const LEFT_HAND_POSITIONS: [KeyPosition; 5] = [
    KeyPosition::LeftPinky,
    KeyPosition::LeftRing,
    KeyPosition::LeftMiddle,
    KeyPosition::LeftIndex,
    KeyPosition::LeftInner,
];

const RIGHT_HAND_POSITIONS: [KeyPosition; 5] = [
    KeyPosition::RightInner,
    KeyPosition::RightIndex,
    KeyPosition::RightMiddle,
    KeyPosition::RightRing,
    KeyPosition::RightPinky,
];

const BPM_MIN: u16 = 40;
const BPM_MAX: u16 = 240;
const TAP_THRESHOLD_MS: u128 = 300;

/// Note length options, cycled by left/right on the mixer row.
const NOTE_LENGTHS: [(f32, &str); 4] = [
    (0.25, "1/4"),
    (0.5, "1/2"),
    (0.75, "3/4"),
    (1.0, "whole"),
];

/// Transport panel rows: bpm, loop, playing, track key, mute progression,
/// last chime, `[Export MIDI]`, `[Import MIDI]`. Rows 2 and 5 are read-only but
/// still occupy an index, as they always have.
const TRANSPORT_ROW_EXPORT: usize = 6;
const TRANSPORT_ROW_IMPORT: usize = 7;
const TRANSPORT_ROWS: usize = 8;

fn format_note_length(v: f32) -> String {
    let closest = NOTE_LENGTHS
        .iter()
        .min_by(|a, b| {
            (a.0 - v)
                .abs()
                .partial_cmp(&(b.0 - v).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, name)| *name)
        .unwrap_or("whole");
    closest.to_string()
}

// -----------------------------------------------------------------------------
// Focus
// -----------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Focus {
    Transport,
    Progression,
    SynthMixer,
    SynthLow,
    SynthMid,
    SynthHigh,
    SynthPresets,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Focus::Transport => Focus::Progression,
            Focus::Progression => Focus::SynthMixer,
            Focus::SynthMixer => Focus::SynthLow,
            Focus::SynthLow => Focus::SynthMid,
            Focus::SynthMid => Focus::SynthHigh,
            Focus::SynthHigh => Focus::SynthPresets,
            Focus::SynthPresets => Focus::Transport,
        }
    }

    fn prev(self) -> Self {
        match self {
            Focus::Transport => Focus::SynthPresets,
            Focus::Progression => Focus::Transport,
            Focus::SynthMixer => Focus::Progression,
            Focus::SynthLow => Focus::SynthMixer,
            Focus::SynthMid => Focus::SynthLow,
            Focus::SynthHigh => Focus::SynthMid,
            Focus::SynthPresets => Focus::SynthHigh,
        }
    }
}

// -----------------------------------------------------------------------------
// Tap tracker
// -----------------------------------------------------------------------------

#[derive(Copy, Clone, Debug)]
enum TapAction {
    Toggle,
    SeekMiddle,
    Restart,
}

#[derive(Default)]
struct TapTracker {
    count: u8,
    last_tap: Option<Instant>,
}

impl TapTracker {
    fn tap(&mut self) {
        let now = Instant::now();
        let continuation = match self.last_tap {
            Some(t) => now.duration_since(t).as_millis() < TAP_THRESHOLD_MS,
            None => false,
        };
        self.count = if continuation { self.count + 1 } else { 1 };
        self.last_tap = Some(now);
    }

    fn resolve(&mut self) -> Option<TapAction> {
        let last = self.last_tap?;
        if last.elapsed().as_millis() < TAP_THRESHOLD_MS {
            return None;
        }
        let action = match self.count {
            1 => Some(TapAction::Toggle),
            2 => Some(TapAction::SeekMiddle),
            _ => Some(TapAction::Restart),
        };
        self.count = 0;
        self.last_tap = None;
        action
    }
}

// -----------------------------------------------------------------------------
// Mixer params
// -----------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum MixerParam {
    LowVolume,
    MidVolume,
    HighVolume,
    LowSend,
    MidSend,
    HighSend,
    LowPan,
    MidPan,
    HighPan,
    ReverbMix,
    ReverbSize,
    MasterVolume,
    MasterMute,
    PreviewFade,
    NoteLength,
}

const MIXER_PARAMS: [MixerParam; 15] = [
    MixerParam::LowVolume,
    MixerParam::MidVolume,
    MixerParam::HighVolume,
    MixerParam::LowSend,
    MixerParam::MidSend,
    MixerParam::HighSend,
    MixerParam::LowPan,
    MixerParam::MidPan,
    MixerParam::HighPan,
    MixerParam::ReverbMix,
    MixerParam::ReverbSize,
    MixerParam::MasterVolume,
    MixerParam::MasterMute,
    MixerParam::PreviewFade,
    MixerParam::NoteLength,
];

impl MixerParam {
    fn label(self) -> &'static str {
        match self {
            MixerParam::LowVolume => "low volume",
            MixerParam::MidVolume => "mid volume",
            MixerParam::HighVolume => "high volume",
            MixerParam::LowSend => "low reverb",
            MixerParam::MidSend => "mid reverb",
            MixerParam::HighSend => "high reverb",
            MixerParam::LowPan => "low pan",
            MixerParam::MidPan => "mid pan",
            MixerParam::HighPan => "high pan",
            MixerParam::ReverbMix => "reverb mix",
            MixerParam::ReverbSize => "reverb size",
            MixerParam::MasterVolume => "master volume",
            MixerParam::MasterMute => "master mute",
            MixerParam::PreviewFade => "preview fade",
            MixerParam::NoteLength => "note length",
        }
    }

    fn display(self, p: &SynthParams, t: &Transport) -> String {
        match self {
            MixerParam::LowVolume => format!("{:.0}", p.low.volume.get()),
            MixerParam::MidVolume => format!("{:.0}", p.mid.volume.get()),
            MixerParam::HighVolume => format!("{:.0}", p.high.volume.get()),
            MixerParam::LowSend => format!("{:.0}%", p.low.reverb_send.get() * 100.0),
            MixerParam::MidSend => format!("{:.0}%", p.mid.reverb_send.get() * 100.0),
            MixerParam::HighSend => format!("{:.0}%", p.high.reverb_send.get() * 100.0),
            MixerParam::LowPan => format_pan(p.low.pan.get()),
            MixerParam::MidPan => format_pan(p.mid.pan.get()),
            MixerParam::HighPan => format_pan(p.high.pan.get()),
            MixerParam::ReverbMix => format!("{:.0}%", p.reverb_mix.get() * 100.0),
            MixerParam::ReverbSize => format!("{:.0}%", p.reverb_size.get() * 100.0),
            MixerParam::MasterVolume => format!("{:.0}", p.master_volume.get()),
            MixerParam::MasterMute => {
                if p.master_mute.get() > 0.5 {
                    "on".to_string()
                } else {
                    "off".to_string()
                }
            }
            MixerParam::PreviewFade => format!("{:.0} ms", p.preview_fade.get() * 1000.0),
            MixerParam::NoteLength => format_note_length(t.note_length()),
        }
    }

    fn adjust(self, p: &SynthParams, t: &Transport, delta: i32) {
        let d = delta as f32;
        match self {
            MixerParam::LowVolume => p.low.volume.set((p.low.volume.get() + d).clamp(0.0, 7.0)),
            MixerParam::MidVolume => p.mid.volume.set((p.mid.volume.get() + d).clamp(0.0, 7.0)),
            MixerParam::HighVolume => p.high.volume.set((p.high.volume.get() + d).clamp(0.0, 7.0)),
            MixerParam::LowSend => p
                .low
                .reverb_send
                .set((p.low.reverb_send.get() + d * 0.05).clamp(0.0, 1.0)),
            MixerParam::MidSend => p
                .mid
                .reverb_send
                .set((p.mid.reverb_send.get() + d * 0.05).clamp(0.0, 1.0)),
            MixerParam::HighSend => p
                .high
                .reverb_send
                .set((p.high.reverb_send.get() + d * 0.05).clamp(0.0, 1.0)),
            MixerParam::LowPan => p.low.pan.set((p.low.pan.get() + d * 0.1).clamp(-1.0, 1.0)),
            MixerParam::MidPan => p.mid.pan.set((p.mid.pan.get() + d * 0.1).clamp(-1.0, 1.0)),
            MixerParam::HighPan => p.high.pan.set((p.high.pan.get() + d * 0.1).clamp(-1.0, 1.0)),
            MixerParam::ReverbMix => p
                .reverb_mix
                .set((p.reverb_mix.get() + d * 0.05).clamp(0.0, 1.0)),
            MixerParam::ReverbSize => p
                .reverb_size
                .set((p.reverb_size.get() + d * 0.05).clamp(0.0, 1.0)),
            MixerParam::MasterVolume => p
                .master_volume
                .set((p.master_volume.get() + d).clamp(0.0, 7.0)),
            MixerParam::MasterMute => {
                let cur = p.master_mute.get() > 0.5;
                p.master_mute.set(if cur { 0.0 } else { 1.0 });
            }
            MixerParam::PreviewFade => p
                .preview_fade
                .set((p.preview_fade.get() * 1.15f32.powf(d)).clamp(0.005, 0.5)),
            MixerParam::NoteLength => {
                let cur = t.note_length();
                let idx = NOTE_LENGTHS
                    .iter()
                    .position(|(v, _)| (v - cur).abs() < 0.01)
                    .unwrap_or(3);
                let new_idx = ((idx as i32 + delta).rem_euclid(NOTE_LENGTHS.len() as i32))
                    as usize;
                t.set_note_length(NOTE_LENGTHS[new_idx].0);
            }
        }
    }
}

fn format_pan(v: f32) -> String {
    if v.abs() < 0.05 {
        "C".to_string()
    } else if v < 0.0 {
        format!("L{:.0}", -v * 100.0)
    } else {
        format!("R{:.0}", v * 100.0)
    }
}

// -----------------------------------------------------------------------------
// Channel params
// -----------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ChannelParam {
    Waveform,
    Attack,
    Decay,
    Sustain,
    Release,
    Cutoff,
    Resonance,
    Transpose,
}

const CHANNEL_PARAMS: [ChannelParam; 8] = [
    ChannelParam::Waveform,
    ChannelParam::Attack,
    ChannelParam::Decay,
    ChannelParam::Sustain,
    ChannelParam::Release,
    ChannelParam::Cutoff,
    ChannelParam::Resonance,
    ChannelParam::Transpose,
];

impl ChannelParam {
    fn label(self) -> &'static str {
        match self {
            ChannelParam::Waveform => "waveform",
            ChannelParam::Attack => "attack",
            ChannelParam::Decay => "decay",
            ChannelParam::Sustain => "sustain",
            ChannelParam::Release => "release",
            ChannelParam::Cutoff => "cutoff",
            ChannelParam::Resonance => "resonance",
            ChannelParam::Transpose => "transpose",
        }
    }

    fn display(self, ch: &crate::synth::ChannelParams) -> String {
        match self {
            ChannelParam::Waveform => Waveform::from_f32(ch.waveform.get()).name().to_string(),
            ChannelParam::Attack => format!("{:.0} ms", ch.attack.get() * 1000.0),
            ChannelParam::Decay => format!("{:.0} ms", ch.decay.get() * 1000.0),
            ChannelParam::Sustain => format!("{:.0}%", ch.sustain.get() * 100.0),
            ChannelParam::Release => format!("{:.0} ms", ch.release.get() * 1000.0),
            ChannelParam::Cutoff => format!("{:.0} Hz", ch.cutoff.get()),
            ChannelParam::Resonance => format!("{:.0}%", ch.resonance.get() * 100.0),
            ChannelParam::Transpose => {
                let v = ch.transpose.get();
                if v > 0.5 {
                    format!("+{:.0} st", v)
                } else if v < -0.5 {
                    format!("{:.0} st", v)
                } else {
                    "0 st".to_string()
                }
            }
        }
    }

    fn adjust(self, ch: &crate::synth::ChannelParams, delta: i32) {
        let d = delta as f32;
        match self {
            ChannelParam::Waveform => {
                let n = Waveform::ALL.len() as f32;
                let cur = ch.waveform.get();
                ch.waveform.set((cur + d).rem_euclid(n));
            }
            ChannelParam::Attack => ch
                .attack
                .set((ch.attack.get() * 1.15f32.powf(d)).clamp(0.001, 0.5)),
            ChannelParam::Decay => ch
                .decay
                .set((ch.decay.get() * 1.15f32.powf(d)).clamp(0.001, 2.0)),
            ChannelParam::Sustain => ch
                .sustain
                .set((ch.sustain.get() + d * 0.05).clamp(0.0, 1.0)),
            ChannelParam::Release => ch
                .release
                .set((ch.release.get() * 1.15f32.powf(d)).clamp(0.001, 2.0)),
            ChannelParam::Cutoff => ch
                .cutoff
                .set((ch.cutoff.get() * 1.15f32.powf(d)).clamp(200.0, 8000.0)),
            ChannelParam::Resonance => ch
                .resonance
                .set((ch.resonance.get() + d * 0.02).clamp(0.0, 0.99)),
            ChannelParam::Transpose => ch
                .transpose
                .set((ch.transpose.get() + d).clamp(-24.0, 24.0)),
        }
    }
}

// -----------------------------------------------------------------------------
// Transport edit / modals
// -----------------------------------------------------------------------------

#[derive(Clone)]
enum TransportEdit {
    None,
    Bpm {
        initial: u16,
        current: u16,
        buffer: String,
    },
    TrackKey {
        initial: Key,
        current: Key,
    },
}

enum Modal {
    /// Two-stage confirmation for clearing the whole progression. Single-chord
    /// deletion is a hotkey backed by undo instead, so it needs no prompt.
    ConfirmDeleteAllStage1,
    ConfirmDeleteAllStage2,
    AddRest,
    PatchNameInput { buffer: String },
    /// Filename to import, pre-filled with the newest export.
    ImportPathInput { buffer: String },
}

impl Modal {
    fn pass_through_chords(&self) -> bool {
        !matches!(
            self,
            Modal::PatchNameInput { .. } | Modal::ImportPathInput { .. }
        )
    }
}

// -----------------------------------------------------------------------------
// App state
// -----------------------------------------------------------------------------

/// How long a successful export/import message stays at full brightness.
const STATUS_HOLD: Duration = Duration::from_secs(5);

/// How long it takes to fade away once the hold is over.
const STATUS_FADE: Duration = Duration::from_millis(1000);

#[derive(Debug)]
enum ActionOutcome {
    Ok,
    Failed,
}

/// Outcome of the most recent MIDI export or import, rendered beside the
/// relevant button so the user gets feedback without reading `debug.log`.
///
/// A success carries a `shown_at` stamp so the filename can fade away on its
/// own; see [`ActionStatus::appearance_at`].
#[derive(Debug)]
struct ActionStatus {
    text: String,
    outcome: ActionOutcome,
    shown_at: Instant,
}

impl ActionStatus {
    fn new(text: String, outcome: ActionOutcome, shown_at: Instant) -> Self {
        ActionStatus {
            text,
            outcome,
            shown_at,
        }
    }

    fn ok(text: String) -> Self {
        Self::new(text, ActionOutcome::Ok, Instant::now())
    }

    fn failed(text: String) -> Self {
        Self::new(text, ActionOutcome::Failed, Instant::now())
    }

    fn text(&self) -> &str {
        &self.text
    }

    fn is_ok(&self) -> bool {
        matches!(self.outcome, ActionOutcome::Ok)
    }

    /// How to draw this at `now`: the colour and text, or `None` once it has
    /// faded away entirely.
    ///
    /// A success holds at full brightness for [`STATUS_HOLD`] and then dims
    /// over [`STATUS_FADE`]. A failure never fades: it is usually telling you
    /// to do something, and vanishing before it is read would be unhelpful.
    fn appearance_at(&self, now: Instant) -> Option<(Color, &str)> {
        if !self.is_ok() {
            return Some((Color::Red, self.text()));
        }

        let elapsed = now.saturating_duration_since(self.shown_at);
        if elapsed < STATUS_HOLD {
            return Some((Color::Green, self.text()));
        }

        let faded = (elapsed - STATUS_HOLD).as_secs_f32();
        let total = STATUS_FADE.as_secs_f32();
        if faded >= total {
            return None;
        }
        Some((faded_green(1.0 - faded / total), self.text()))
    }
}

/// Green dimmed toward black.
///
/// A terminal cannot blend toward an unknown background, so "fade" here means
/// "lose brightness": `level` 1 is the normal green, 0 is black. Truecolor is
/// near-universal on the terminals this targets; a terminal without it will
/// approximate, which still reads as a fade.
fn faded_green(level: f32) -> Color {
    let level = level.clamp(0.0, 1.0);
    Color::Rgb {
        r: 0,
        g: (0xAF as f32 * level).round() as u8,
        b: 0,
    }
}

struct AppState {
    held: PositionSet,
    registers: Registers,
    focus: Focus,
    progression: Arc<Mutex<Progression>>,
    transport: Arc<Transport>,
    edit: TransportEdit,
    mixer_row: usize,
    channel_row: [usize; 3],
    progression_row: usize,
    preset_row: usize,
    modal: Option<Modal>,
    patch_store: PatchStore,
    flash_until: Option<Instant>,
    taps: TapTracker,
    last_chime_index: Option<usize>,
    /// Where MIDI exports are written. A field rather than a call to
    /// `current_dir()` at export time, so it is testable and can later become
    /// a setting.
    export_dir: PathBuf,
    export_status: Option<ActionStatus>,
    import_status: Option<ActionStatus>,
}

impl AppState {
    fn flash(&mut self, duration_ms: u64) {
        self.flash_until = Some(Instant::now() + Duration::from_millis(duration_ms));
    }

    fn is_flashing(&self) -> bool {
        matches!(self.flash_until, Some(t) if Instant::now() < t)
    }

    fn focused_channel(&self) -> Option<usize> {
        match self.focus {
            Focus::SynthLow => Some(0),
            Focus::SynthMid => Some(1),
            Focus::SynthHigh => Some(2),
            _ => None,
        }
    }

    fn row_count(&self) -> usize {
        match self.focus {
            Focus::Transport => TRANSPORT_ROWS,
            Focus::Progression => self.progression.lock().unwrap().len(),
            Focus::SynthMixer => MIXER_PARAMS.len(),
            Focus::SynthLow | Focus::SynthMid | Focus::SynthHigh => CHANNEL_PARAMS.len(),
            Focus::SynthPresets => self.patch_store.patches.len() + 1,
        }
    }

    fn current_row(&self) -> usize {
        match self.focus {
            Focus::Transport => self.mixer_row.min(TRANSPORT_ROWS - 1),
            Focus::Progression => self.progression_row,
            Focus::SynthMixer => self.mixer_row,
            Focus::SynthLow => self.channel_row[0],
            Focus::SynthMid => self.channel_row[1],
            Focus::SynthHigh => self.channel_row[2],
            Focus::SynthPresets => self.preset_row,
        }
    }

    fn set_current_row(&mut self, row: usize) {
        let count = self.row_count();
        let clamped = if count == 0 { 0 } else { row.min(count - 1) };
        match self.focus {
            Focus::Transport => self.mixer_row = clamped,
            Focus::Progression => self.progression_row = clamped,
            Focus::SynthMixer => self.mixer_row = clamped,
            Focus::SynthLow => self.channel_row[0] = clamped,
            Focus::SynthMid => self.channel_row[1] = clamped,
            Focus::SynthHigh => self.channel_row[2] = clamped,
            Focus::SynthPresets => self.preset_row = clamped,
        }
    }

    fn update_progression_len(&self) {
        let len = self.progression.lock().unwrap().len();
        self.transport.progression_len.store(len, Ordering::Relaxed);
    }

    /// Keep the progression cursor inside the list after a structural edit.
    fn clamp_progression_row(&mut self) {
        let len = self.progression.lock().unwrap().len();
        self.progression_row = if len == 0 { 0 } else { self.progression_row.min(len - 1) };
    }

    fn update_live_chord(&self) {
        let notes = chord_notes(resolved_chord(self), &self.transport.key());
        if notes.is_some() {
            // First time the user produces a chord, suppress the chime
            // for the rest of the session.
            self.transport
                .chime_suppressed
                .store(true, Ordering::Relaxed);
        }
        self.transport.set_live_chord(notes);
    }
}

// -----------------------------------------------------------------------------
// Entry point
// -----------------------------------------------------------------------------

pub fn run_interactive() -> io::Result<()> {
    let logger = Logger::create("debug.log")?;
    let output_tap = OutputTap::create(logger.clone());
    let synth = Synth::new(Some(output_tap.peak()))
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    let patch_store = PatchStore::load(&default_path())?;
    let initial_key = Key::new(60, Scale::Major);

    if let Some(p) = patch_store.find("Default") {
        synth.apply_patch(p);
    }

    let progression = Arc::new(Mutex::new(Progression::new()));
    let transport = Transport::new(initial_key);

    // Seed note_length from the default patch.
    if let Some(p) = patch_store.find("Default") {
        transport.set_note_length(p.mixer.note_length);
    }

    let scheduler = Scheduler::start(transport.clone(), progression.clone());

    // Exported progressions go to a gitignored `progressions/` directory. If it
    // cannot be created (a read-only checkout, say) fall back to the working
    // directory rather than refusing to start; the per-export error explains it.
    let export_dir = export::ensure_export_dir().unwrap_or_else(|err| {
        logger.input(&format!(
            "EXPORT directory unavailable ({}); using the working directory",
            err
        ));
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    });

    let mut state = AppState {
        held: PositionSet::new(),
        registers: Registers::default(),
        focus: Focus::Transport,
        progression,
        transport,
        edit: TransportEdit::None,
        mixer_row: 0,
        channel_row: [0, 0, 0],
        progression_row: 0,
        preset_row: 0,
        modal: None,
        patch_store,
        flash_until: None,
        taps: TapTracker::default(),
        last_chime_index: None,
        export_dir,
        export_status: None,
        import_status: None,
    };

    state.update_live_chord();

    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;

    let enhancement_enabled = execute!(
        stdout,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES,
        ),
    )
    .is_ok();

    let result = event_loop(&mut stdout, &synth, &scheduler, &mut state, &logger);

    if enhancement_enabled {
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(stdout, LeaveAlternateScreen);
    let _ = disable_raw_mode();
    drop(scheduler);
    drop(output_tap);
    logger.flush();
    result
}

// -----------------------------------------------------------------------------
// Event loop
// -----------------------------------------------------------------------------

fn event_loop(
    stdout: &mut io::Stdout,
    synth: &Synth,
    scheduler: &Scheduler,
    state: &mut AppState,
    logger: &Logger,
) -> io::Result<()> {
    loop {
        if let Some(action) = state.taps.resolve() {
            match action {
                TapAction::Toggle => {
                    let p = state.transport.playing.load(Ordering::Relaxed);
                    state.transport.playing.store(!p, Ordering::Relaxed);
                    logger.input(if p {
                        "TRANSPORT pause"
                    } else {
                        "TRANSPORT play"
                    });
                }
                TapAction::SeekMiddle => {
                    let len = state.transport.progression_len.load(Ordering::Relaxed);
                    let mid = (len / 2) as i64;
                    state.transport.seek_to.store(mid, Ordering::Relaxed);
                    state.transport.playing.store(true, Ordering::Relaxed);
                    logger.input(&format!("TRANSPORT seek middle -> bar {}", mid + 1));
                }
                TapAction::Restart => {
                    state.transport.seek_to.store(0, Ordering::Relaxed);
                    state.transport.playing.store(true, Ordering::Relaxed);
                    logger.input("TRANSPORT restart");
                }
            }
        }

        render(stdout, synth, state)?;

        while let Some(ev) = scheduler.try_recv() {
            match ev {
                SchedulerEvent::PlayChord(notes) => synth.play_progression_chord(&notes),
                SchedulerEvent::StopChord => synth.stop_progression(),
                SchedulerEvent::PlayChime {
                    start,
                    end,
                    glide_secs,
                    hold_secs,
                } => {
                    synth.play_chime(start, end, glide_secs, hold_secs);
                }
                SchedulerEvent::RecordChime { index } => {
                    state.last_chime_index = Some(index);
                }
            }
        }

        if !event::poll(Duration::from_millis(5))? {
            continue;
        }

        let Event::Key(ev) = event::read()? else {
            continue;
        };

        if let Some(Modal::PatchNameInput { ref mut buffer }) = state.modal {
            if ev.kind == KeyEventKind::Press {
                match ev.code {
                    KeyCode::Esc => {
                        state.modal = None;
                        logger.input("MODAL cancel patch name");
                    }
                    KeyCode::Enter => {
                        let name = if buffer.trim().is_empty() {
                            "Untitled".to_string()
                        } else {
                            buffer.trim().to_string()
                        };
                        let patch = synth.capture_patch(&name, state.transport.note_length());
                        state.patch_store.add(patch);
                        if let Err(e) = state.patch_store.save(&default_path()) {
                            logger.input(&format!("SAVE ERROR: {}", e));
                        }
                        logger.input(&format!("MODAL save patch '{}'", name));
                        state.modal = None;
                    }
                    KeyCode::Backspace => {
                        buffer.pop();
                    }
                    KeyCode::Char(c) => {
                        if buffer.len() < 32 && !c.is_control() {
                            buffer.push(c);
                        }
                    }
                    _ => {}
                }
            }
            continue;
        }

        if matches!(state.modal, Some(Modal::ImportPathInput { .. })) {
            if ev.kind == KeyEventKind::Press {
                match ev.code {
                    KeyCode::Esc => {
                        state.modal = None;
                        logger.input("MODAL cancel import");
                    }
                    KeyCode::Enter => {
                        // Take the modal first so the buffer borrow is over
                        // before the import mutates state.
                        let filename = match state.modal.take() {
                            Some(Modal::ImportPathInput { buffer }) => buffer.trim().to_string(),
                            other => {
                                state.modal = other;
                                String::new()
                            }
                        };
                        import_midi(state, &filename, logger);
                    }
                    KeyCode::Backspace => {
                        if let Some(Modal::ImportPathInput { buffer }) = &mut state.modal {
                            buffer.pop();
                        }
                    }
                    KeyCode::Char(c) => {
                        if let Some(Modal::ImportPathInput { buffer }) = &mut state.modal {
                            if buffer.len() < 200 && !c.is_control() {
                                buffer.push(c);
                            }
                        }
                    }
                    _ => {}
                }
            }
            continue;
        }

        if let Some(ref modal) = state.modal {
            if ev.kind == KeyEventKind::Press {
                match ev.code {
                    KeyCode::Esc => {
                        state.modal = None;
                        logger.input("MODAL cancel");
                        continue;
                    }
                    KeyCode::Enter => match modal {
                        Modal::ConfirmDeleteAllStage1 => {
                            state.modal = Some(Modal::ConfirmDeleteAllStage2);
                            logger.input("MODAL delete-all stage 2");
                            continue;
                        }
                        Modal::ConfirmDeleteAllStage2 => {
                            state.progression.lock().unwrap().delete_all();
                            state.update_progression_len();
                            logger.input("MODAL confirm delete all");
                            state.modal = None;
                            continue;
                        }
                        Modal::AddRest => {
                            let idx = state.progression.lock().unwrap().append(Slot::Rest);
                            state.update_progression_len();
                            logger.input(&format!("MODAL add rest at {}", idx));
                            state.modal = None;
                            continue;
                        }
                        Modal::PatchNameInput { .. } => unreachable!(),
                        Modal::ImportPathInput { .. } => unreachable!(),
                    },
                    _ => {}
                }
            }
            if !modal.pass_through_chords() {
                continue;
            }
        }

        match ev.kind {
            KeyEventKind::Press => {
                if ev.code == KeyCode::Esc && state.modal.is_none() {
                    logger.input("ESC");
                    return Ok(());
                }

                match state.edit {
                    TransportEdit::Bpm { .. } => {
                        handle_bpm_edit(state, &ev, logger);
                        continue;
                    }
                    TransportEdit::TrackKey { .. } => {
                        handle_track_key_edit(state, &ev, logger);
                        continue;
                    }
                    TransportEdit::None => {}
                }

                match ev.code {
                    KeyCode::Tab => {
                        state.focus = state.focus.next();
                        logger.input(&format!("TAB -> {:?}", state.focus));
                        continue;
                    }
                    KeyCode::BackTab => {
                        state.focus = state.focus.prev();
                        logger.input(&format!("BACKTAB -> {:?}", state.focus));
                        continue;
                    }
                    _ => {}
                }

                if let KeyCode::Char(' ') = ev.code {
                    state.taps.tap();
                    logger.input("SPACE tap");
                    continue;
                }

                if let KeyCode::Char(c) = ev.code {
                    if let Some(pos) = ACTIVE_LAYOUT.position(c) {
                        let shift = ev.modifiers.contains(KeyModifiers::SHIFT)
                            || c.is_ascii_uppercase();
                        if let Some(hotkey) = pos.hotkey() {
                            // Hotkeys are checked first so they never reach the
                            // held set. `LeftInner` is a home-row key the
                            // grammar ignores, and the grammar matches exact
                            // shapes, so inserting it would break whatever
                            // chord it was held with.
                            handle_hotkey(state, hotkey, shift, logger);
                        } else if !ev
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        {
                            // Ctrl/Alt are not part of the chord grammar.
                            // Ignoring them keeps the combination free for
                            // bindings instead of silently sounding a chord.
                            state.held.insert(pos);
                            state.update_live_chord();
                            logger.input(&format!("PRESS '{}'", c));
                        }
                    }
                    continue;
                }

                handle_panel_key(state, synth, &ev, logger);
            }
            KeyEventKind::Release => {
                if let KeyCode::Char(' ') = ev.code {
                    // Tap tracker handles it.
                } else if let KeyCode::Char(c) = ev.code {
                    if let Some(pos) = ACTIVE_LAYOUT.position(c) {
                        if pos.is_home_row() {
                            state.held.remove(&pos);
                            state.update_live_chord();
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

// -----------------------------------------------------------------------------
// Input helpers
// -----------------------------------------------------------------------------

fn handle_bpm_edit(state: &mut AppState, ev: &event::KeyEvent, logger: &Logger) {
    let edit = std::mem::replace(&mut state.edit, TransportEdit::None);
    let (initial, mut current, mut buffer) = match edit {
        TransportEdit::Bpm {
            initial,
            current,
            buffer,
        } => (initial, current, buffer),
        other => {
            state.edit = other;
            return;
        }
    };

    if ev.kind != KeyEventKind::Press {
        state.edit = TransportEdit::Bpm {
            initial,
            current,
            buffer,
        };
        return;
    }

    let mut keep_editing = true;

    match ev.code {
        KeyCode::Esc => {
            state.transport.set_bpm(initial);
            keep_editing = false;
            logger.input("BPM cancel");
        }
        KeyCode::Enter => {
            if !buffer.is_empty() {
                if let Ok(v) = buffer.parse::<u16>() {
                    current = v.clamp(BPM_MIN, BPM_MAX);
                }
            }
            state.transport.set_bpm(current);
            keep_editing = false;
            logger.input(&format!("BPM commit = {}", current));
        }
        KeyCode::Backspace => {
            buffer.pop();
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            if buffer.len() < 3 {
                buffer.push(c);
            }
        }
        KeyCode::Up => {
            let d = if ev.modifiers.contains(KeyModifiers::SHIFT) {
                10
            } else {
                1
            };
            current = current.saturating_add(d).min(BPM_MAX);
            buffer.clear();
        }
        KeyCode::Down => {
            let d = if ev.modifiers.contains(KeyModifiers::SHIFT) {
                10
            } else {
                1
            };
            current = current.saturating_sub(d).max(BPM_MIN);
            buffer.clear();
        }
        _ => {}
    }

    if keep_editing {
        state.edit = TransportEdit::Bpm {
            initial,
            current,
            buffer,
        };
    }
}

fn handle_track_key_edit(state: &mut AppState, ev: &event::KeyEvent, logger: &Logger) {
    let edit = std::mem::replace(&mut state.edit, TransportEdit::None);
    let (initial, mut current) = match edit {
        TransportEdit::TrackKey { initial, current } => (initial, current),
        other => {
            state.edit = other;
            return;
        }
    };

    if ev.kind != KeyEventKind::Press {
        state.edit = TransportEdit::TrackKey { initial, current };
        return;
    }

    let mut keep_editing = true;

    match ev.code {
        KeyCode::Esc => {
            state.transport.set_key(initial);
            keep_editing = false;
            logger.input("KEY cancel");
        }
        KeyCode::Enter => {
            state.transport.set_key(current);
            keep_editing = false;
            logger.input(&format!("KEY commit = {}", current.name()));
        }
        KeyCode::Up => {
            let pc = (current.tonic % 12 + 1) % 12;
            current.tonic = 60 + pc;
        }
        KeyCode::Down => {
            let pc = (current.tonic % 12 + 11) % 12;
            current.tonic = 60 + pc;
        }
        KeyCode::Left | KeyCode::Right => {
            current.scale = match current.scale {
                Scale::Major => Scale::Minor,
                Scale::Minor => Scale::Major,
            };
        }
        _ => {}
    }

    if keep_editing {
        state.edit = TransportEdit::TrackKey { initial, current };
    }
}

fn handle_panel_key(
    state: &mut AppState,
    synth: &Synth,
    ev: &event::KeyEvent,
    logger: &Logger,
) {
    match ev.code {
        KeyCode::Up => {
            let row = state.current_row();
            state.set_current_row(row.saturating_sub(1));
        }
        KeyCode::Down => {
            let row = state.current_row();
            state.set_current_row(row + 1);
        }
        KeyCode::Left => adjust_current(state, synth, -1, logger),
        KeyCode::Right => adjust_current(state, synth, 1, logger),
        KeyCode::Enter => match enter_intent(state, ev.modifiers.contains(KeyModifiers::CONTROL)) {
            EnterIntent::CommitChord { to_end } => add_current_chord(state, to_end, logger),
            EnterIntent::PanelAction => primary_action(state, synth, logger),
        },
        _ => {}
    }
}

fn adjust_current(state: &mut AppState, synth: &Synth, delta: i32, _logger: &Logger) {
    let row = state.current_row();
    match state.focus {
        Focus::Transport => match row {
            0 => {
                let v = state.transport.bpm() as i32 + delta;
                state.transport.set_bpm(v.clamp(BPM_MIN as i32, BPM_MAX as i32) as u16);
            }
            1 => {
                let cur = state.transport.looping.load(Ordering::Relaxed);
                state.transport.looping.store(!cur, Ordering::Relaxed);
            }
            3 => {
                let mut k = state.transport.key();
                k.scale = match k.scale {
                    Scale::Major => Scale::Minor,
                    Scale::Minor => Scale::Major,
                };
                state.transport.set_key(k);
            }
            4 => {
                let p = synth.params();
                let cur = p.mute_progression.get() > 0.5;
                p.mute_progression.set(if cur { 0.0 } else { 1.0 });
            }
            _ => {}
        },
        Focus::Progression => {}
        Focus::SynthMixer => {
            if let Some(param) = MIXER_PARAMS.get(row) {
                param.adjust(synth.params(), &state.transport, delta);
            }
        }
        Focus::SynthLow | Focus::SynthMid | Focus::SynthHigh => {
            let idx = state.focused_channel().unwrap();
            let ch = match idx {
                0 => &synth.params().low,
                1 => &synth.params().mid,
                _ => &synth.params().high,
            };
            if let Some(param) = CHANNEL_PARAMS.get(row) {
                param.adjust(ch, delta);
            }
        }
        Focus::SynthPresets => {}
    }
}

fn primary_action(state: &mut AppState, synth: &Synth, logger: &Logger) {
    let row = state.current_row();
    match state.focus {
        Focus::Transport => match row {
            0 => {
                state.edit = TransportEdit::Bpm {
                    initial: state.transport.bpm(),
                    current: state.transport.bpm(),
                    buffer: String::new(),
                };
                logger.input("BPM edit begin");
            }
            1 => {
                let cur = state.transport.looping.load(Ordering::Relaxed);
                state.transport.looping.store(!cur, Ordering::Relaxed);
            }
            3 => {
                state.edit = TransportEdit::TrackKey {
                    initial: state.transport.key(),
                    current: state.transport.key(),
                };
                logger.input("KEY edit begin");
            }
            4 => {
                let p = synth.params();
                let cur = p.mute_progression.get() > 0.5;
                p.mute_progression.set(if cur { 0.0 } else { 1.0 });
            }
            TRANSPORT_ROW_EXPORT => export_midi(state, logger),
            TRANSPORT_ROW_IMPORT => open_import_modal(state, logger),
            _ => {}
        },
        Focus::Progression => {
            add_current_chord(state, false, logger);
        }
        Focus::SynthMixer | Focus::SynthLow | Focus::SynthMid | Focus::SynthHigh => {}
        Focus::SynthPresets => {
            let preset_count = state.patch_store.patches.len();
            if row < preset_count {
                let name = state.patch_store.patches[row].name.clone();
                if let Some(p) = state.patch_store.find(&name) {
                    let note_length = p.mixer.note_length;
                    synth.apply_patch(p);
                    state.transport.set_note_length(note_length);
                    logger.input(&format!("PRESET load '{}'", name));
                }
            } else {
                state.modal = Some(Modal::PatchNameInput {
                    buffer: String::new(),
                });
                logger.input("PRESET save-as modal");
            }
        }
    }
}

/// Render the progression and write it out as a timestamped `.mid` file.
///
/// The semantic session document is embedded alongside the notes, so the file
/// can be imported back (`import_midi`) rather than only played by a DAW.
///
/// Runs on the UI thread from an explicit keypress, so no file I/O ever
/// touches the audio callback. The outcome is kept on the state for the panel
/// to render, because flashing alone is invisible on the Transport panel.
fn export_midi(state: &mut AppState, logger: &Logger) {
    // Encode and render under one lock, from one snapshot, so the notes and the
    // document can never describe different sessions.
    let prepared = {
        let prog = state.progression.lock().unwrap();
        if prog.is_empty() {
            Ok(None)
        } else {
            let key = state.transport.key();
            let bpm = state.transport.bpm();
            let note_length = state.transport.note_length();
            project::encode(&prog, key, bpm, note_length).map(|document| {
                let score = midi::render_progression(&prog, &key, bpm, note_length);
                Some((score, document))
            })
        }
    };

    let (score, document) = match prepared {
        Ok(Some(prepared)) => prepared,
        Ok(None) => {
            state.export_status = Some(ActionStatus::failed("nothing to export".to_string()));
            state.flash(300);
            logger.input("EXPORT skipped: progression is empty");
            return;
        }
        Err(err) => {
            state.export_status = Some(ActionStatus::failed(err.to_string()));
            state.flash(300);
            logger.input(&format!("EXPORT failed: {}", err));
            return;
        }
    };

    match export::export(&score, &document, &state.export_dir, Local::now()) {
        Ok(path) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            state.export_status = Some(ActionStatus::ok(name));
            state.flash(600);
            logger.input(&format!("EXPORT wrote {}", path.display()));
        }
        Err(err) => {
            state.export_status = Some(ActionStatus::failed(err.to_string()));
            state.flash(300);
            logger.input(&format!("EXPORT failed: {}", err));
        }
    }
}

/// Open the import prompt, pre-filled with the newest export.
///
/// The prompt is what makes a specific file reachable: a terminal UI has no
/// file picker, and the newest export is the overwhelmingly common target.
fn open_import_modal(state: &mut AppState, logger: &Logger) {
    let buffer = newest_export(&state.export_dir).unwrap_or_default();
    logger.input(&format!("IMPORT modal (prefill {:?})", buffer));
    state.modal = Some(Modal::ImportPathInput { buffer });
}

/// Read an exported file and install its progression and session context.
fn import_midi(state: &mut AppState, filename: &str, logger: &Logger) {
    let filename = filename.trim();
    if filename.is_empty() {
        state.import_status = Some(ActionStatus::failed("no file name".to_string()));
        state.flash(300);
        logger.input("IMPORT skipped: no file name");
        return;
    }

    // A relative name resolves against the export directory, which is where
    // exports land; an absolute path is honoured as given.
    let candidate = PathBuf::from(filename);
    let path = if candidate.is_absolute() {
        candidate
    } else {
        state.export_dir.join(candidate)
    };

    match export::import(&path) {
        Ok(restored) => {
            let count = restored.slots.len();
            let changed = state.progression.lock().unwrap().replace(restored.slots);
            state.transport.set_key(restored.key);
            state.transport.set_bpm(restored.bpm.clamp(BPM_MIN, BPM_MAX));
            state.transport.set_note_length(restored.note_length);
            state.update_progression_len();
            state.progression_row = 0;

            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            state.import_status = Some(ActionStatus::ok(name));
            state.flash(600);
            logger.input(&format!(
                "IMPORT loaded {} slots from {} (changed: {})",
                count,
                path.display(),
                changed
            ));
        }
        Err(err) => {
            state.import_status = Some(ActionStatus::failed(err.to_string()));
            state.flash(300);
            logger.input(&format!("IMPORT failed: {}", err));
        }
    }
}

/// The newest export in `dir`, if there is one.
///
/// Timestamped names sort lexicographically in chronological order, so the
/// greatest name is the most recent export.
fn newest_export(dir: &Path) -> Option<String> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with("progression-") && name.ends_with(".mid"))
        .max()
}

fn add_current_chord(state: &mut AppState, to_end: bool, logger: &Logger) {
    let Some((degree, transformation)) = resolved_chord(state) else {
        if state.focus == Focus::Progression {
            state.modal = Some(Modal::AddRest);
            logger.input("ADD no chord -> Add Rest modal");
        } else {
            state.flash(200);
            logger.input("ADD no chord -> flash");
        }
        return;
    };

    let effective = state.registers.resolve(&state.held);
    let entry = ProgressionEntry {
        degree,
        transformation,
        // Capture the resolved gesture, not the latch state. Storing
        // `registers` here would miss chords played live with both hands and
        // record an empty snapshot, which nothing could usefully replay.
        registers: capture_registers(&effective),
    };

    let inserted_at = {
        let mut prog = state.progression.lock().unwrap();
        if to_end || prog.is_empty() {
            prog.append(Slot::Chord(entry))
        } else {
            prog.insert_at(state.progression_row + 1, Slot::Chord(entry))
        }
    };

    state.update_progression_len();
    state.progression_row = inserted_at;
    logger.input(&format!("ADD chord at {}", inserted_at));
}

/// The chord implied by the latched registers plus any keys held right now.
///
/// One hand can come from a register while the other is live, and live input
/// wins per side (see `Registers::resolve`). This is the single source of
/// truth for "what is the user playing": the on-screen readout, the live
/// audio, and Enter-to-add all go through it, so they cannot disagree.
fn resolved_chord(state: &AppState) -> Option<(ScaleDegree, Option<Transformation>)> {
    let effective = state.registers.resolve(&state.held);
    let degree = left_hand_degree(&effective)?;
    Some((degree, right_hand_transformation(&effective)))
}

/// What Enter should do, decided before any side effect.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum EnterIntent {
    /// Commit the chord currently being played.
    CommitChord { to_end: bool },
    /// Fall through to the focused panel's own action.
    PanelAction,
}

/// Decide what Enter does.
///
/// Ctrl+Enter always appends, even with nothing held. Otherwise a chord that
/// resolves is committed from whichever panel has focus — in the Progression
/// panel it lands after the selected row, elsewhere it appends.
///
/// The exception is an explicitly selected **button** row: it takes Enter even
/// while a chord is held. Without that, holding a chord would silently swallow
/// the button press and add a chord instead of exporting (the same latent trap
/// the Presets panel's `[Save As...]` row had).
fn enter_intent(state: &AppState, ctrl: bool) -> EnterIntent {
    if ctrl {
        return EnterIntent::CommitChord { to_end: true };
    }
    if row_is_action_button(state) {
        return EnterIntent::PanelAction;
    }
    if enter_commits_chord(state) {
        return EnterIntent::CommitChord {
            to_end: state.focus != Focus::Progression,
        };
    }
    EnterIntent::PanelAction
}

/// True when the focused row is an explicit button rather than a value row.
fn row_is_action_button(state: &AppState) -> bool {
    match state.focus {
        Focus::Transport => matches!(
            state.current_row(),
            TRANSPORT_ROW_EXPORT | TRANSPORT_ROW_IMPORT
        ),
        Focus::SynthPresets => state.current_row() >= state.patch_store.patches.len(),
        _ => false,
    }
}

/// True when Enter should commit the current chord instead of falling through
/// to the focused panel's primary action.
fn enter_commits_chord(state: &AppState) -> bool {
    resolved_chord(state).is_some()
}

/// Split a resolved position set into the per-hand registers that produced it.
///
/// Both sides are recorded as `Some`, including an empty right hand for a
/// plain triad: `Some(empty)` means "explicitly no right-hand shape" and
/// re-resolves to the triad, whereas `None` would mean "never set".
fn capture_registers(effective: &PositionSet) -> Registers {
    Registers {
        left: Some(effective.iter().filter(|p| p.is_left()).copied().collect()),
        right: Some(effective.iter().filter(|p| p.is_right()).copied().collect()),
    }
}

/// Dispatch a hotkey.
///
/// The register locks are performance controls and work from any panel. The
/// progression actions are scoped to the progression panel, where the
/// selection cursor lives, so they can never act on an invisible row.
fn handle_hotkey(state: &mut AppState, hotkey: Hotkey, shift: bool, logger: &Logger) {
    // Redo shares a position with undo and is selected with Shift, keeping
    // `KeyPosition::hotkey` a pure function of the physical key.
    let hotkey = match hotkey {
        Hotkey::Undo if shift => Hotkey::Redo,
        other => other,
    };

    match hotkey {
        Hotkey::LockRightRegister => {
            state.registers.lock_right(&state.held);
            state.update_live_chord();
            logger.input("LOCK RIGHT register");
        }
        Hotkey::LockLeftRegister => {
            state.registers.lock_left(&state.held);
            state.update_live_chord();
            logger.input("LOCK LEFT register");
        }
        Hotkey::LoadSelectedChord => load_selected_chord(state, logger),
        Hotkey::CopyChord | Hotkey::PasteChord | Hotkey::DeleteChord | Hotkey::Undo | Hotkey::Redo => {
            if state.focus == Focus::Progression {
                edit_progression(state, hotkey, logger);
            } else {
                state.flash(200);
                logger.input(&format!("{:?} outside progression -> flash", hotkey));
            }
        }
    }
}

/// Recall the chord under the progression cursor into the registers.
///
/// Deliberately explicit rather than automatic on selection: scrolling the
/// progression must not clobber a latched register mid-performance. This is
/// the only place the selection's stored registers are read.
fn load_selected_chord(state: &mut AppState, logger: &Logger) {
    if state.focus != Focus::Progression {
        state.flash(200);
        logger.input("RECALL outside progression -> flash");
        return;
    }

    let recalled = {
        let prog = state.progression.lock().unwrap();
        match prog.slots.get(state.progression_row) {
            Some(Slot::Chord(entry)) => Some(entry.registers.clone()),
            _ => None,
        }
    };

    let Some(registers) = recalled else {
        // Nothing to recall: a rest, or an empty progression.
        state.flash(200);
        logger.input("RECALL no chord under cursor -> flash");
        return;
    };

    state.registers = registers;
    state.update_live_chord();
    logger.input(&format!("RECALL chord at {}", state.progression_row));
}

/// Apply a progression edit. Only called with the progression panel focused,
/// so `progression_row` is a live selection.
fn edit_progression(state: &mut AppState, hotkey: Hotkey, logger: &Logger) {
    let row = state.progression_row;

    let changed = {
        let mut prog = state.progression.lock().unwrap();
        match hotkey {
            Hotkey::CopyChord => prog.copy(row),
            Hotkey::PasteChord => prog.paste_after(Some(row)),
            Hotkey::DeleteChord => prog.delete(row),
            Hotkey::Undo => prog.undo(),
            Hotkey::Redo => prog.redo(),
            Hotkey::LockRightRegister
            | Hotkey::LockLeftRegister
            | Hotkey::LoadSelectedChord => unreachable!(),
        }
    };

    if !changed {
        state.flash(200);
        logger.input(&format!("PROG {:?} no-op at {}", hotkey, row));
        return;
    }

    // A paste lands directly after the row it was pasted from, so follow it.
    if hotkey == Hotkey::PasteChord {
        state.progression_row = row + 1;
    }
    state.clamp_progression_row();
    state.update_progression_len();
    logger.input(&format!("PROG {:?} at {}", hotkey, row));
}

/// Sound a resolved chord, or `None` when nothing resolves.
fn chord_notes(
    chord: Option<(ScaleDegree, Option<Transformation>)>,
    key: &Key,
) -> Option<Vec<u8>> {
    let (degree, transformation) = chord?;
    Some(match transformation {
        Some(t) => ChordSpec::new(degree, t).voice(key),
        None => diatonic_triad(key, degree),
    })
}

/// What the `Chord:` readout shows: the chord's name, its scale degree
/// relative to the track key, and its notes.
///
/// The degree belongs here because the register lines already show it for a
/// latched hand, and live playing should read the same way. Both go through
/// `ScaleDegree::label()`, so the two can never disagree.
fn chord_readout(
    degree: ScaleDegree,
    transformation: Option<Transformation>,
    key: &Key,
) -> (String, &'static str, Vec<u8>) {
    let label = match transformation {
        Some(t) => chord_label(key, &ChordSpec::new(degree, t)),
        None => diatonic_triad_label(key, degree),
    };
    let notes = match transformation {
        Some(t) => ChordSpec::new(degree, t).voice(key),
        None => diatonic_triad(key, degree),
    };
    (label, degree.label(), notes)
}

// -----------------------------------------------------------------------------
// Rendering
// -----------------------------------------------------------------------------

fn render(stdout: &mut io::Stdout, synth: &Synth, state: &AppState) -> io::Result<()> {
    execute!(stdout, Clear(ClearType::All), MoveTo(0, 0))?;

    execute!(stdout, Print("  Chord Tool\r\n"))?;
    execute!(
        stdout,
        Print(format!("  Layout: {}\r\n\r\n", ACTIVE_LAYOUT.name()))
    )?;

    execute!(stdout, Print("  "))?;
    for pos in LEFT_HAND_POSITIONS {
        draw_key(stdout, pos, state.held.contains(&pos))?;
    }
    execute!(stdout, Print(" | "))?;
    for pos in RIGHT_HAND_POSITIONS {
        draw_key(stdout, pos, state.held.contains(&pos))?;
    }
    execute!(stdout, Print("\r\n"))?;

    let key = state.transport.key();
    draw_register_line(stdout, "L", &state.registers.left, &key)?;
    draw_register_line(stdout, "R", &state.registers.right, &key)?;
    execute!(
        stdout,
        Print("  Lock:  z -> right register   / -> left register\r\n\r\n")
    )?;

    match resolved_chord(state) {
        Some((d, transformation)) => {
            let (label, degree, notes) = chord_readout(d, transformation, &key);
            let note_str: Vec<String> = notes.iter().map(|n| note_name(*n)).collect();
            execute!(
                stdout,
                Print(format!("  Chord:       {}   ({})\r\n", label, degree))
            )?;
            execute!(
                stdout,
                Print(format!("  Notes:       {}\r\n\r\n", note_str.join(" ")))
            )?;
        }
        None => {
            execute!(
                stdout,
                Print("  Chord:       (press a left-hand key)\r\n\r\n")
            )?;
        }
    }

    render_synth_panel(stdout, synth, state)?;
    render_progression_panel(stdout, state)?;
    render_transport_panel(stdout, synth, state)?;

    if let Some(ref modal) = state.modal {
        execute!(stdout, Print("\r\n"))?;
        render_modal(stdout, modal)?;
    }

    execute!(
        stdout,
        Print(
            "\r\n  Esc quit   Tab cycle   Enter: add chord, or activate a button   \
             Space: play/pause (2: mid, 3: restart)\r\n"
        )
    )?;
    execute!(stdout, Print("  Log: debug.log\r\n"))?;
    stdout.flush()?;
    Ok(())
}

fn render_synth_panel(
    stdout: &mut io::Stdout,
    synth: &Synth,
    state: &AppState,
) -> io::Result<()> {
    let active_subtab = match state.focus {
        Focus::SynthMixer => Some("Mixer"),
        Focus::SynthLow => Some("Low"),
        Focus::SynthMid => Some("Mid"),
        Focus::SynthHigh => Some("High"),
        Focus::SynthPresets => Some("Presets"),
        _ => None,
    };

    let header = match active_subtab {
        Some(name) => format!(" Synth [{}]  (tab to switch) ", name),
        None => " Synth ".to_string(),
    };
    execute!(stdout, Print(format!("\r\n──{}──\r\n", header)))?;

    match active_subtab {
        Some("Mixer") => render_mixer_body(stdout, synth, state)?,
        Some("Low") | Some("Mid") | Some("High") => render_channel_body(stdout, synth, state)?,
        Some("Presets") => render_presets_body(stdout, state)?,
        _ => {
            execute!(stdout, Print("  (tab to switch to a synth subtab)\r\n"))?;
        }
    }
    Ok(())
}

fn render_mixer_body(
    stdout: &mut io::Stdout,
    synth: &Synth,
    state: &AppState,
) -> io::Result<()> {
    let focused = state.focus == Focus::SynthMixer;
    for (i, param) in MIXER_PARAMS.iter().enumerate() {
        let marker = if focused && i == state.mixer_row {
            "▸"
        } else {
            " "
        };
        execute!(
            stdout,
            Print(format!(
                "  {} {:<14} {}\r\n",
                marker,
                param.label(),
                param.display(synth.params(), &state.transport)
            ))
        )?;
    }
    Ok(())
}

fn render_channel_body(
    stdout: &mut io::Stdout,
    synth: &Synth,
    state: &AppState,
) -> io::Result<()> {
    let idx = state.focused_channel().unwrap_or(0);
    let ch = match idx {
        0 => &synth.params().low,
        1 => &synth.params().mid,
        _ => &synth.params().high,
    };
    let row = state.channel_row[idx];
    for (i, param) in CHANNEL_PARAMS.iter().enumerate() {
        let marker = if i == row { "▸" } else { " " };
        execute!(
            stdout,
            Print(format!(
                "  {} {:<14} {}\r\n",
                marker,
                param.label(),
                param.display(ch)
            ))
        )?;
    }
    Ok(())
}

fn render_presets_body(stdout: &mut io::Stdout, state: &AppState) -> io::Result<()> {
    let focused = state.focus == Focus::SynthPresets;
    for (i, patch) in state.patch_store.patches.iter().enumerate() {
        let marker = if focused && i == state.preset_row {
            "▸"
        } else {
            " "
        };
        execute!(stdout, Print(format!("  {} {}\r\n", marker, patch.name)))?;
    }
    let save_row = state.patch_store.patches.len();
    let marker = if focused && state.preset_row == save_row {
        "▸"
    } else {
        " "
    };
    execute!(stdout, Print(format!("  {} [Save As...]\r\n", marker)))?;
    Ok(())
}

fn render_progression_panel(stdout: &mut io::Stdout, state: &AppState) -> io::Result<()> {
    execute!(stdout, Print("\r\n"))?;
    let focused = state.focus == Focus::Progression;
    let history = {
        let prog = state.progression.lock().unwrap();
        match (prog.can_undo(), prog.can_redo()) {
            (false, false) => String::new(),
            (true, false) => "  undo: yes".to_string(),
            (false, true) => "  redo: yes".to_string(),
            (true, true) => "  undo: yes  redo: yes".to_string(),
        }
    };
    let header = if focused {
        format!(" Progression  (tab to switch){} ", history)
    } else {
        format!(" Progression{} ", history)
    };
    execute!(stdout, Print(format!("──{}──\r\n", header)))?;

    let prog = state.progression.lock().unwrap();
    if prog.is_empty() {
        if state.is_flashing() {
            execute!(
                stdout,
                SetForegroundColor(Color::Yellow),
                Print("  (empty — press enter to add chords)\r\n"),
                ResetColor,
            )?;
        } else {
            execute!(stdout, Print("  (empty — press enter to add chords)\r\n"))?;
        }
        return Ok(());
    }

    let playing = state.transport.playing.load(Ordering::Relaxed);
    let playing_bar = if playing {
        Some(state.transport.current_bar.load(Ordering::Relaxed))
    } else {
        None
    };
    let key = state.transport.key();
    let prog_len = prog.len();

    for (i, slot) in prog.slots.iter().enumerate() {
        let label = slot.label(&key);
        let is_play = Some(i) == playing_bar && i < prog_len;
        let is_sel = focused && i == state.progression_row;

        let sel_marker = if is_sel { "▸" } else { " " };
        let play_marker = if is_play { "▶" } else { " " };

        if is_play {
            execute!(
                stdout,
                SetForegroundColor(Color::Red),
                Print(format!("  {} {} {}\r\n", sel_marker, play_marker, label)),
                ResetColor,
            )?;
        } else if is_sel {
            execute!(
                stdout,
                SetForegroundColor(Color::Yellow),
                Print(format!("  {} {} {}\r\n", sel_marker, play_marker, label)),
                ResetColor,
            )?;
        } else {
            execute!(
                stdout,
                Print(format!("  {} {} {}\r\n", sel_marker, play_marker, label))
            )?;
        }
    }
    Ok(())
}

fn render_transport_panel(
    stdout: &mut io::Stdout,
    synth: &Synth,
    state: &AppState,
) -> io::Result<()> {
    execute!(stdout, Print("\r\n"))?;
    let focused = state.focus == Focus::Transport;
    // `current_row` applies the Transport row clamp; reading `mixer_row`
    // directly would highlight nothing after the mixer cursor was left deep.
    let row = if focused {
        state.current_row()
    } else {
        usize::MAX
    };

    let header = if focused {
        " Transport  (tab to switch) "
    } else {
        " Transport "
    };
    execute!(stdout, Print(format!("──{}──\r\n", header)))?;

    let bpm_display = match &state.edit {
        TransportEdit::Bpm { current, buffer, .. } => {
            if buffer.is_empty() {
                format!("{}", current)
            } else {
                format!("{}_", buffer)
            }
        }
        _ => format!("{}", state.transport.bpm()),
    };
    draw_transport_row(stdout, focused && row == 0, "bpm", &bpm_display)?;

    draw_transport_row(
        stdout,
        focused && row == 1,
        "loop",
        if state.transport.looping.load(Ordering::Relaxed) {
            "on"
        } else {
            "off"
        },
    )?;

    let len = state.transport.progression_len.load(Ordering::Relaxed);
    let live = state.transport.live_chord.lock().unwrap().is_some();
    let total = len + if live { 1 } else { 0 };
    let playing_display = if state.transport.playing.load(Ordering::Relaxed) {
        let bar = state.transport.current_bar.load(Ordering::Relaxed);
        format!("▶ bar {}/{}", bar + 1, total.max(1))
    } else if live {
        "live".to_string()
    } else {
        "idle".to_string()
    };
    draw_transport_row(stdout, false, "playing", &playing_display)?;

    let key_display = match &state.edit {
        TransportEdit::TrackKey { current, .. } => format!("{} (edit)", current.name()),
        _ => state.transport.key().name(),
    };
    draw_transport_row(stdout, focused && row == 3, "track key", &key_display)?;

    let muted = synth.params().mute_progression.get() > 0.5;
    draw_transport_row(
        stdout,
        focused && row == 4,
        "mute progression",
        if muted { "on" } else { "off" },
    )?;

    let chime_display = match state.last_chime_index {
        Some(i) if i < CHIMES.len() => {
            let c = &CHIMES[i];
            let fmt = |s: [u8; 3], e: [u8; 3]| -> String {
                let a = format!("{}→{}", note_name(s[0]), note_name(e[0]));
                let b = format!("{}→{}", note_name(s[1]), note_name(e[1]));
                let d = format!("{}→{}", note_name(s[2]), note_name(e[2]));
                format!("{}  {}  {}", a, b, d)
            };
            format!("{}  [{}]", c.name, fmt(c.start, c.end))
        }
        _ => "—".to_string(),
    };
    draw_transport_row(stdout, false, "last chime", &chime_display)?;

    // Action buttons. They are selected like value rows, but
    // `row_is_action_button` makes Enter activate them even while a chord is
    // held. Each shows the outcome of its last run, since flashing is
    // invisible on this panel.
    draw_action_row(
        stdout,
        focused && row == TRANSPORT_ROW_EXPORT,
        "[Export MIDI]",
        state.export_status.as_ref(),
    )?;
    draw_action_row(
        stdout,
        focused && row == TRANSPORT_ROW_IMPORT,
        "[Import MIDI]",
        state.import_status.as_ref(),
    )?;

    Ok(())
}

/// A button row plus the green/red outcome of its last run.
///
/// A successful filename fades away over [`STATUS_FADE`] once it is
/// [`STATUS_HOLD`] old, which is why the appearance is resolved against the
/// current instant rather than drawn unconditionally.
fn draw_action_row(
    stdout: &mut io::Stdout,
    selected: bool,
    label: &str,
    status: Option<&ActionStatus>,
) -> io::Result<()> {
    let marker = if selected { "▸" } else { " " };
    execute!(stdout, Print(format!("  {} {:<16}", marker, label)))?;
    match status.and_then(|status| status.appearance_at(Instant::now())) {
        Some((colour, text)) => execute!(
            stdout,
            SetForegroundColor(colour),
            Print(format!(" {}\r\n", text)),
            ResetColor,
        )?,
        None => execute!(stdout, Print("\r\n"))?,
    }
    Ok(())
}

fn draw_transport_row(
    stdout: &mut io::Stdout,
    selected: bool,
    label: &str,
    value: &str,
) -> io::Result<()> {
    let marker = if selected { "▸" } else { " " };
    execute!(
        stdout,
        Print(format!("  {} {:<16} {}\r\n", marker, label, value))
    )?;
    Ok(())
}

fn render_modal(stdout: &mut io::Stdout, modal: &Modal) -> io::Result<()> {
    execute!(
        stdout,
        SetForegroundColor(Color::Black),
        SetBackgroundColor(Color::Yellow),
    )?;
    match modal {
        Modal::ConfirmDeleteAllStage1 => {
            execute!(
                stdout,
                Print("  Delete all chords?  [Enter] continue  [Esc] cancel  ")
            )?;
        }
        Modal::ConfirmDeleteAllStage2 => {
            execute!(
                stdout,
                Print("  Are you sure?  This cannot be undone.  [Enter] confirm  [Esc] cancel  ")
            )?;
        }
        Modal::AddRest => {
            execute!(
                stdout,
                Print("  Add a rest (silent bar)?  [Enter] yes  [Esc] no  ")
            )?;
        }
        Modal::PatchNameInput { buffer } => {
            let shown = if buffer.is_empty() { "_" } else { buffer };
            execute!(
                stdout,
                Print(format!(
                    "  Save patch as: {}     [Enter] save  [Esc] cancel  ",
                    shown
                ))
            )?;
        }
        Modal::ImportPathInput { buffer } => {
            let shown = if buffer.is_empty() { "_" } else { buffer };
            execute!(
                stdout,
                Print(format!(
                    "  Import MIDI: {}     [Enter] import  [Esc] cancel  ",
                    shown
                ))
            )?;
        }
    }
    execute!(stdout, ResetColor, Print("\r\n"))?;
    Ok(())
}

fn draw_register_line(
    stdout: &mut io::Stdout,
    side: &str,
    register: &Option<PositionSet>,
    key: &Key,
) -> io::Result<()> {
    let (positions_str, resolved_str) = match register {
        None => ("—".to_string(), "—".to_string()),
        Some(set) if set.is_empty() => ("(empty)".to_string(), "—".to_string()),
        Some(set) => {
            let mut chars: Vec<char> = set.iter().map(|p| p.qwerty_label()).collect();
            chars.sort();
            let positions: String = chars.into_iter().collect();

            let resolved = match side {
                "L" => match left_hand_degree(set) {
                    Some(d) => format!("{} ({})", d.label(), note_name(key.degree_root(d))),
                    None => "?".to_string(),
                },
                "R" => match right_hand_transformation(set) {
                    Some(t) => format!("{} ({:?})", t.label(), t.mode()),
                    None => "?".to_string(),
                },
                _ => "?".to_string(),
            };
            (format!("[{}]", positions), resolved)
        }
    };
    execute!(
        stdout,
        Print(format!(
            "  Register {}:  {:<10} -> {}\r\n",
            side, positions_str, resolved_str
        ))
    )?;
    Ok(())
}

fn draw_key(stdout: &mut io::Stdout, pos: KeyPosition, active: bool) -> io::Result<()> {
    let label = pos.qwerty_label();
    if active {
        execute!(
            stdout,
            SetForegroundColor(Color::Black),
            SetBackgroundColor(Color::Green),
            Print(format!(" {} ", label)),
        )?;
    } else if pos.hotkey().is_some() {
        // An action key sitting in the chord row (`g` recalls the selection).
        // Coloured separately so it doesn't read as a chord key.
        execute!(
            stdout,
            SetForegroundColor(Color::Cyan),
            Print(format!("[{}]", label)),
        )?;
    } else {
        execute!(
            stdout,
            SetForegroundColor(Color::DarkGrey),
            Print(format!("[{}]", label)),
        )?;
    }
    execute!(stdout, ResetColor, Print(" "))?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn logger() -> Arc<Logger> {
        let mut path = std::env::temp_dir();
        path.push(format!("chord-tool-tui-{}.log", std::process::id()));
        Logger::create(path.to_str().unwrap()).unwrap()
    }

    fn state(focus: Focus) -> AppState {
        AppState {
            held: PositionSet::new(),
            registers: Registers::default(),
            focus,
            progression: Arc::new(Mutex::new(Progression::new())),
            transport: Transport::new(Key::new(60, Scale::Major)),
            edit: TransportEdit::None,
            mixer_row: 0,
            channel_row: [0, 0, 0],
            progression_row: 0,
            preset_row: 0,
            modal: None,
            patch_store: PatchStore {
                patches: Vec::new(),
            },
            flash_until: None,
            taps: TapTracker::default(),
            last_chime_index: None,
            export_dir: std::env::temp_dir(),
            export_status: None,
        import_status: None,
        }
    }

    fn slots(state: &AppState) -> Vec<Slot> {
        state.progression.lock().unwrap().slots.clone()
    }

    // ---- resolution: register + live override ----

    #[test]
    fn nothing_held_and_no_register_does_not_resolve() {
        let s = state(Focus::Progression);
        assert_eq!(resolved_chord(&s), None);
        assert!(!enter_commits_chord(&s));
    }

    #[test]
    fn live_left_hand_resolves_the_plain_triad() {
        let mut s = state(Focus::Progression);
        s.held.insert(KeyPosition::LeftIndex);
        assert_eq!(resolved_chord(&s), Some((ScaleDegree::I, None)));
        assert!(enter_commits_chord(&s));
    }

    #[test]
    fn register_supplies_one_hand_and_live_supplies_the_other() {
        // The bug report: latch the left hand, hold only the right.
        let mut s = state(Focus::Transport);
        s.held.insert(KeyPosition::LeftIndex);
        s.registers.lock_left(&s.held);
        s.held.clear();

        s.held.insert(KeyPosition::RightIndex);
        assert_eq!(
            resolved_chord(&s),
            Some((ScaleDegree::I, Some(Transformation::Dom7)))
        );
    }

    #[test]
    fn live_input_overrides_a_stale_register_per_side() {
        let mut s = state(Focus::Progression);
        s.held.insert(KeyPosition::LeftIndex);
        s.registers.lock_left(&s.held);
        s.held.clear();

        // A live left-hand key wins over the latch.
        s.held.insert(KeyPosition::LeftMiddle);
        assert_eq!(resolved_chord(&s), Some((ScaleDegree::V, None)));
    }

    // ---- Enter commits from any panel ----

    #[test]
    fn enter_commits_while_holding_a_chord_in_any_panel() {
        for focus in [
            Focus::Transport,
            Focus::Progression,
            Focus::SynthMixer,
            Focus::SynthLow,
            Focus::SynthPresets,
        ] {
            let mut s = state(focus);
            s.held.insert(KeyPosition::LeftIndex);
            assert!(enter_commits_chord(&s), "focus {:?}", focus);
        }
    }

    #[test]
    fn enter_defers_to_the_panel_when_no_chord_resolves() {
        for focus in [Focus::Transport, Focus::SynthPresets] {
            let s = state(focus);
            assert!(!enter_commits_chord(&s), "focus {:?}", focus);
        }
    }

    #[test]
    fn add_appends_from_a_non_progression_panel() {
        let mut s = state(Focus::Transport);
        s.held.insert(KeyPosition::LeftIndex);
        let log = logger();
        let to_end = s.focus != Focus::Progression;
        add_current_chord(&mut s, to_end, &log);

        match slots(&s).as_slice() {
            [Slot::Chord(e)] => {
                assert_eq!(e.degree, ScaleDegree::I);
                assert_eq!(e.transformation, None);
            }
            other => panic!("expected one chord, got {:?}", other),
        }
        // The transport must learn the new length or playback ignores it.
        assert_eq!(s.transport.progression_len.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn add_inserts_after_the_selected_row_in_the_progression_panel() {
        let mut s = state(Focus::Progression);
        let log = logger();

        s.held.insert(KeyPosition::LeftIndex); // I
        add_current_chord(&mut s, false, &log);
        s.held.clear();
        s.held.insert(KeyPosition::LeftMiddle); // V
        add_current_chord(&mut s, false, &log);
        assert_eq!(s.progression_row, 1);

        // Select row 0 and insert: the new chord lands at index 1.
        s.progression_row = 0;
        s.held.clear();
        s.held.insert(KeyPosition::LeftPinky); // ii
        add_current_chord(&mut s, false, &log);

        let degrees: Vec<ScaleDegree> = slots(&s)
            .iter()
            .map(|s| match s {
                Slot::Chord(e) => e.degree,
                Slot::Rest => panic!("unexpected rest"),
            })
            .collect();
        assert_eq!(
            degrees,
            vec![ScaleDegree::I, ScaleDegree::II, ScaleDegree::V]
        );
        assert_eq!(s.progression_row, 1);
    }

    #[test]
    fn add_appends_with_to_end_from_the_progression_panel() {
        let mut s = state(Focus::Progression);
        let log = logger();
        s.held.insert(KeyPosition::LeftIndex);
        add_current_chord(&mut s, true, &log);
        s.progression_row = 0;
        s.held.clear();
        s.held.insert(KeyPosition::LeftMiddle);
        add_current_chord(&mut s, true, &log);
        assert_eq!(s.progression_row, 1);
    }

    #[test]
    fn add_records_the_register_snapshot_from_the_resolved_chord() {
        let mut s = state(Focus::Transport);
        let log = logger();
        s.held.insert(KeyPosition::LeftIndex);
        s.registers.lock_left(&s.held);
        s.held.clear();

        s.held.insert(KeyPosition::RightIndex);
        add_current_chord(&mut s, true, &log);

        match slots(&s).as_slice() {
            [Slot::Chord(e)] => {
                assert_eq!(e.degree, ScaleDegree::I);
                assert_eq!(e.transformation, Some(Transformation::Dom7));
                assert_eq!(e.registers.left, Some([KeyPosition::LeftIndex].into()));
            }
            other => panic!("expected one chord, got {:?}", other),
        }
    }

    #[test]
    fn add_without_a_chord_in_the_progression_panel_offers_a_rest() {
        let mut s = state(Focus::Progression);
        let log = logger();
        add_current_chord(&mut s, false, &log);
        assert!(matches!(s.modal, Some(Modal::AddRest)));
        assert!(slots(&s).is_empty());
    }

    #[test]
    fn add_without_a_chord_elsewhere_flashes_instead_of_prompting() {
        let mut s = state(Focus::Transport);
        let log = logger();
        add_current_chord(&mut s, true, &log);
        assert!(s.modal.is_none());
        assert!(s.is_flashing());
        assert!(slots(&s).is_empty());
    }

    // ---- resolved chord drives the audio too ----

    // ---- below-home-row editing hotkeys ----

    fn chord_entry(degree: ScaleDegree, t: Option<Transformation>) -> ProgressionEntry {
        ProgressionEntry {
            degree,
            transformation: t,
            registers: Registers::default(),
        }
    }

    fn seed(s: &mut AppState, degrees: &[ScaleDegree]) {
        let mut prog = s.progression.lock().unwrap();
        for d in degrees {
            prog.slots.push(Slot::Chord(chord_entry(*d, None)));
        }
    }

    fn degrees(s: &AppState) -> Vec<ScaleDegree> {
        s.progression
            .lock()
            .unwrap()
            .slots
            .iter()
            .map(|slot| match slot {
                Slot::Chord(e) => e.degree,
                Slot::Rest => panic!("unexpected rest"),
            })
            .collect()
    }

    #[test]
    fn delete_hotkey_removes_the_selected_chord_and_is_undoable() {
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::IV, ScaleDegree::V]);
        s.progression_row = 1;

        edit_progression(&mut s, Hotkey::DeleteChord, &log);
        assert_eq!(degrees(&s), vec![ScaleDegree::I, ScaleDegree::V]);
        assert_eq!(s.transport.progression_len.load(Ordering::Relaxed), 2);

        edit_progression(&mut s, Hotkey::Undo, &log);
        assert_eq!(
            degrees(&s),
            vec![ScaleDegree::I, ScaleDegree::IV, ScaleDegree::V]
        );
        assert_eq!(s.transport.progression_len.load(Ordering::Relaxed), 3);

        edit_progression(&mut s, Hotkey::Redo, &log);
        assert_eq!(degrees(&s), vec![ScaleDegree::I, ScaleDegree::V]);
    }

    #[test]
    fn copy_then_paste_duplicates_after_the_selection() {
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        s.progression_row = 0;

        edit_progression(&mut s, Hotkey::CopyChord, &log);
        edit_progression(&mut s, Hotkey::PasteChord, &log);

        assert_eq!(
            degrees(&s),
            vec![ScaleDegree::I, ScaleDegree::I, ScaleDegree::V]
        );
        // The cursor follows the pasted chord.
        assert_eq!(s.progression_row, 1);
    }

    #[test]
    fn paste_without_a_copy_flashes_and_changes_nothing() {
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I]);
        edit_progression(&mut s, Hotkey::PasteChord, &log);
        assert_eq!(degrees(&s), vec![ScaleDegree::I]);
        assert!(s.is_flashing());
    }

    #[test]
    fn undo_and_redo_are_noops_when_history_is_empty() {
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I]);
        edit_progression(&mut s, Hotkey::Undo, &log);
        assert_eq!(degrees(&s), vec![ScaleDegree::I]);
        assert!(s.is_flashing());
    }

    #[test]
    fn delete_past_the_end_of_the_list_flashes() {
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I]);
        s.progression_row = 5;
        edit_progression(&mut s, Hotkey::DeleteChord, &log);
        assert_eq!(degrees(&s), vec![ScaleDegree::I]);
        assert!(s.is_flashing());
    }

    #[test]
    fn deleting_the_last_chord_leaves_the_cursor_valid() {
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        s.progression_row = 1;
        edit_progression(&mut s, Hotkey::DeleteChord, &log);
        assert_eq!(s.progression_row, 0);
    }

    #[test]
    fn shift_turns_undo_into_redo() {
        // `handle_hotkey` resolves the Shift modifier, keeping
        // `KeyPosition::hotkey` a pure function of the physical key.
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        s.progression_row = 1;
        edit_progression(&mut s, Hotkey::DeleteChord, &log);
        assert_eq!(degrees(&s), vec![ScaleDegree::I]);

        handle_hotkey(&mut s, Hotkey::Undo, false, &log);
        assert_eq!(degrees(&s), vec![ScaleDegree::I, ScaleDegree::V]);

        handle_hotkey(&mut s, Hotkey::Undo, true, &log);
        assert_eq!(degrees(&s), vec![ScaleDegree::I]);
    }

    #[test]
    fn progression_hotkeys_are_inert_outside_the_progression_panel() {
        let mut s = state(Focus::Transport);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        s.progression_row = 0;

        handle_hotkey(&mut s, Hotkey::DeleteChord, false, &log);
        assert_eq!(degrees(&s), vec![ScaleDegree::I, ScaleDegree::V]);
        assert!(s.is_flashing());
    }

    #[test]
    fn register_locks_still_work_and_are_not_scoped_to_a_panel() {
        let mut s = state(Focus::SynthMixer);
        let log = logger();
        s.held.insert(KeyPosition::LeftIndex);
        handle_hotkey(&mut s, Hotkey::LockLeftRegister, false, &log);
        s.held.clear();
        s.held.insert(KeyPosition::RightIndex);
        assert_eq!(
            resolved_chord(&s),
            Some((ScaleDegree::I, Some(Transformation::Dom7)))
        );
    }

    #[test]
    fn unassigned_below_home_row_keys_are_never_dispatched() {
        // The dispatcher only fires for positions that resolve to a hotkey, so
        // the four reserved right-hand slots fall through to the chord path.
        for p in [
            KeyPosition::RightInnerBelow,
            KeyPosition::RightIndexBelow,
            KeyPosition::RightMiddleBelow,
            KeyPosition::RightRingBelow,
        ] {
            assert_eq!(p.hotkey(), None, "{:?} should be inert", p);
        }
    }

    // ---- the register snapshot ----

    #[test]
    fn snapshot_captures_the_resolved_gesture_not_just_the_latches() {
        let mut s = state(Focus::Transport);
        s.held.insert(KeyPosition::LeftIndex);
        s.held.insert(KeyPosition::RightIndex);
        // Nothing is latched: the chord is played entirely live, which is the
        // case the old `state.registers.clone()` recorded as empty.
        let captured = capture_registers(&s.registers.resolve(&s.held));
        assert_eq!(captured.left, Some([KeyPosition::LeftIndex].into()));
        assert_eq!(captured.right, Some([KeyPosition::RightIndex].into()));
    }

    #[test]
    fn snapshot_marks_a_plain_triad_as_explicitly_right_hand_empty() {
        let mut s = state(Focus::Transport);
        s.held.insert(KeyPosition::LeftIndex);
        let captured = capture_registers(&s.registers.resolve(&s.held));
        assert_eq!(captured.left, Some([KeyPosition::LeftIndex].into()));
        // Some(empty) re-resolves to the triad; None would mean "never set".
        assert_eq!(captured.right, Some(PositionSet::new()));
    }

    #[test]
    fn add_current_chord_records_the_gesture_for_live_two_handed_chords() {
        let mut s = state(Focus::Transport);
        let log = logger();
        s.held.insert(KeyPosition::LeftIndex);
        s.held.insert(KeyPosition::RightIndex);
        add_current_chord(&mut s, true, &log);

        match slots(&s).as_slice() {
            [Slot::Chord(e)] => {
                assert_eq!(e.degree, ScaleDegree::I);
                assert_eq!(e.transformation, Some(Transformation::Dom7));
                assert_eq!(e.registers.left, Some([KeyPosition::LeftIndex].into()));
                assert_eq!(e.registers.right, Some([KeyPosition::RightIndex].into()));
            }
            other => panic!("expected one chord, got {:?}", other),
        }
    }

    #[test]
    fn recall_restores_the_snapshot_and_voices_it() {
        let mut s = state(Focus::Progression);
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        // Give row 1 a latched left hand plus a live right hand.
        {
            let mut prog = s.progression.lock().unwrap();
            if let Slot::Chord(e) = &mut prog.slots[1] {
                e.registers = Registers {
                    left: Some([KeyPosition::LeftMiddle].into()),
                    right: Some([KeyPosition::RightIndex].into()),
                };
            }
        }

        s.progression_row = 1;
        load_selected_chord(&mut s, &logger());

        assert_eq!(s.registers.left, Some([KeyPosition::LeftMiddle].into()));
        assert_eq!(s.registers.right, Some([KeyPosition::RightIndex].into()));
        // V + dom7 in C major = G B D F.
        assert_eq!(
            s.transport.live_chord.lock().unwrap().clone(),
            Some(vec![67, 71, 74, 77])
        );
    }

    #[test]
    fn scrolling_the_progression_does_not_touch_the_registers() {
        // The whole point of binding recall to `g`: selection must never
        // clobber a latched register mid-performance.
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        s.registers.left = Some([KeyPosition::LeftPinky].into());
        s.registers.right = Some([KeyPosition::RightIndex].into());

        s.set_current_row(1);
        s.set_current_row(0);
        s.set_current_row(1);

        assert_eq!(s.registers.left, Some([KeyPosition::LeftPinky].into()));
        assert_eq!(s.registers.right, Some([KeyPosition::RightIndex].into()));
        // ...and the edits do not either.
        edit_progression(&mut s, Hotkey::DeleteChord, &log);
        assert_eq!(s.registers.left, Some([KeyPosition::LeftPinky].into()));
        assert_eq!(s.registers.right, Some([KeyPosition::RightIndex].into()));
    }

    #[test]
    fn recall_goes_through_the_hotkey_and_updates_the_live_chord() {
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        {
            let mut prog = s.progression.lock().unwrap();
            if let Slot::Chord(e) = &mut prog.slots[1] {
                e.registers = Registers {
                    left: Some([KeyPosition::LeftMiddle].into()),
                    right: Some([KeyPosition::RightIndex].into()),
                };
            }
        }
        s.progression_row = 1;
        handle_hotkey(&mut s, Hotkey::LoadSelectedChord, false, &log);
        assert_eq!(s.registers.left, Some([KeyPosition::LeftMiddle].into()));
        assert_eq!(
            s.transport.live_chord.lock().unwrap().clone(),
            Some(vec![67, 71, 74, 77])
        );
    }

    #[test]
    fn recall_is_silent_outside_the_progression_panel() {
        let mut s = state(Focus::SynthMixer);
        seed(&mut s, &[ScaleDegree::I]);
        load_selected_chord(&mut s, &logger());
        assert_eq!(s.registers.left, None);
        assert_eq!(s.transport.live_chord.lock().unwrap().clone(), None);
        assert!(s.is_flashing());
    }

    #[test]
    fn recall_ignores_a_rest() {
        let mut s = state(Focus::Progression);
        s.registers.left = Some([KeyPosition::LeftPinky].into());
        s.progression.lock().unwrap().slots.push(Slot::Rest);
        load_selected_chord(&mut s, &logger());
        // Untouched: the rest has nothing to recall.
        assert_eq!(s.registers.left, Some([KeyPosition::LeftPinky].into()));
        assert!(s.is_flashing());
    }

    #[test]
    fn chord_notes_follows_the_resolved_chord() {
        let key = Key::new(60, Scale::Major);
        // I with a dominant 7th: C E G Bb.
        let notes = chord_notes(Some((ScaleDegree::I, Some(Transformation::Dom7))), &key);
        assert_eq!(notes, Some(vec![60, 64, 67, 70]));
        // No chord at all.
        assert_eq!(chord_notes(None, &key), None);
        // Degree with no transformation is the plain triad.
        assert_eq!(chord_notes(Some((ScaleDegree::I, None)), &key), Some(vec![60, 64, 67]));
    }

    // ---- chord readout ----

    #[test]
    fn chord_readout_reports_the_relative_degree() {
        let key = Key::new(60, Scale::Major);

        // IV7 in C major is F7.
        let (label, degree, notes) =
            chord_readout(ScaleDegree::IV, Some(Transformation::Dom7), &key);
        assert_eq!(label, "F7");
        assert_eq!(degree, "IV");
        assert_eq!(notes, vec![65, 69, 72, 75]);

        // A plain diatonic triad on vi is Am, and the degree keeps its case.
        let (label, degree, notes) = chord_readout(ScaleDegree::VI, None, &key);
        assert_eq!(label, "Am");
        assert_eq!(degree, "vi");
        assert_eq!(notes, vec![69, 72, 76]);

        // vii is the diminished triad.
        let (label, degree, notes) = chord_readout(ScaleDegree::VII, None, &key);
        assert_eq!(label, "Bdim");
        assert_eq!(degree, "vii");
        assert_eq!(notes, vec![71, 74, 77]);
    }

    #[test]
    fn chord_readout_degree_matches_the_register_line() {
        // Both the register line and the chord readout describe the same
        // gesture, and `ScaleDegree::label` is their shared source.
        let key = Key::new(60, Scale::Major);
        let held: PositionSet = [KeyPosition::LeftMiddle].into(); // V

        let from_grammar = left_hand_degree(&held).expect("a degree");
        let (_, readout_degree, _) = chord_readout(from_grammar, None, &key);
        assert_eq!(readout_degree, from_grammar.label());
    }

    #[test]
    fn chord_readout_follows_a_minor_key() {
        let key = Key::new(57, Scale::Minor); // A minor
        // i in A minor is Am.
        let (label, degree, notes) = chord_readout(ScaleDegree::I, None, &key);
        assert_eq!(label, "Am");
        assert_eq!(degree, "I");
        assert_eq!(notes, vec![57, 60, 64]);
    }

    // ---- MIDI export button ----

    fn unique_export_dir(tag: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "chord-tool-tui-export-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn transport_panel_exposes_the_export_button() {
        let mut s = state(Focus::Transport);
        assert_eq!(s.row_count(), TRANSPORT_ROWS);
        s.set_current_row(TRANSPORT_ROW_EXPORT);
        assert_eq!(s.current_row(), TRANSPORT_ROW_EXPORT);
        assert!(row_is_action_button(&s));
    }

    #[test]
    fn the_export_button_clamps_after_a_deep_mixer_cursor() {
        // The Transport and Synth Mixer panels share `mixer_row`; leaving the
        // cursor deep in the mixer must not push the Transport selection off
        // the end of its own (shorter) row list.
        let mut s = state(Focus::SynthMixer);
        s.mixer_row = MIXER_PARAMS.len() - 1;
        s.focus = Focus::Transport;
        assert_eq!(s.current_row(), TRANSPORT_ROWS - 1);
    }

    #[test]
    fn a_held_chord_does_not_swallow_the_export_button() {
        // The whole point of the buttons-win rule: selecting Export MIDI and
        // pressing Enter must export, not add the chord under the hands.
        let mut s = state(Focus::Transport);
        s.held.insert(KeyPosition::LeftIndex);
        s.set_current_row(TRANSPORT_ROW_EXPORT);
        assert_eq!(enter_intent(&s, false), EnterIntent::PanelAction);
    }

    #[test]
    fn a_value_row_still_commits_a_held_chord() {
        let mut s = state(Focus::Transport);
        s.held.insert(KeyPosition::LeftIndex);
        s.set_current_row(0);
        assert_eq!(
            enter_intent(&s, false),
            EnterIntent::CommitChord { to_end: true }
        );
    }

    #[test]
    fn ctrl_enter_always_commits_even_on_a_button() {
        let mut s = state(Focus::Transport);
        s.set_current_row(TRANSPORT_ROW_EXPORT);
        // No chord resolves, but Ctrl+Enter appends regardless.
        assert_eq!(
            enter_intent(&s, true),
            EnterIntent::CommitChord { to_end: true }
        );
    }

    #[test]
    fn the_presets_save_row_is_a_button_too() {
        // The same latent trap existed here before the rule was introduced.
        let mut s = state(Focus::SynthPresets);
        s.patch_store.patches.clear();
        s.set_current_row(0);
        assert!(row_is_action_button(&s));

        s.held.insert(KeyPosition::LeftIndex);
        assert_eq!(enter_intent(&s, false), EnterIntent::PanelAction);
    }

    #[test]
    fn an_empty_progression_exports_nothing() {
        let mut s = state(Focus::Transport);
        s.export_dir = unique_export_dir("empty");
        export_midi(&mut s, &logger());

        assert!(s.export_status.as_ref().is_some_and(|status| !status.is_ok()));
        assert_eq!(std::fs::read_dir(&s.export_dir).unwrap().count(), 0);

        std::fs::remove_dir_all(&s.export_dir).unwrap();
    }

    #[test]
    fn exporting_writes_a_timestamped_midi_file() {
        let mut s = state(Focus::Transport);
        s.export_dir = unique_export_dir("write");
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        export_midi(&mut s, &logger());

        let name = match &s.export_status {
            Some(status) if status.is_ok() => status.text().to_string(),
            other => panic!("expected a successful export, got {:?}", other),
        };
        assert!(name.starts_with("progression-"), "got {}", name);
        assert!(name.ends_with(".mid"), "got {}", name);

        let bytes = std::fs::read(s.export_dir.join(&name)).unwrap();
        assert_eq!(&bytes[0..4], b"MThd");

        std::fs::remove_dir_all(&s.export_dir).unwrap();
    }

    #[test]
    fn a_failed_export_is_reported_not_hidden() {
        let mut s = state(Focus::Transport);
        // A directory that does not exist makes the write fail.
        s.export_dir = std::env::temp_dir().join("chord-tool-tui-missing-dir");
        let _ = std::fs::remove_dir_all(&s.export_dir);
        seed(&mut s, &[ScaleDegree::I]);
        export_midi(&mut s, &logger());

        assert!(s.export_status.as_ref().is_some_and(|status| !status.is_ok()));
    }

    // ---- MIDI import button ----

    /// Export the current session and return the file name it landed under.
    fn export_current(s: &mut AppState) -> String {
        export_midi(s, &logger());
        match &s.export_status {
            Some(status) if status.is_ok() => status.text().to_string(),
            other => panic!("expected a successful export, got {:?}", other),
        }
    }

    #[test]
    fn transport_panel_exposes_the_import_button() {
        let mut s = state(Focus::Transport);
        s.set_current_row(TRANSPORT_ROW_IMPORT);
        assert_eq!(s.current_row(), TRANSPORT_ROW_IMPORT);
        assert!(row_is_action_button(&s));
        // ...and a held chord must not swallow it.
        s.held.insert(KeyPosition::LeftIndex);
        assert_eq!(enter_intent(&s, false), EnterIntent::PanelAction);
    }

    #[test]
    fn an_export_imports_back_into_the_session() {
        let mut s = state(Focus::Transport);
        s.export_dir = unique_export_dir("import-roundtrip");
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        let name = export_current(&mut s);

        // Wreck the session: different progression, different tempo.
        {
            let mut prog = s.progression.lock().unwrap();
            prog.delete_all();
        }
        s.transport.set_bpm(200);
        s.update_progression_len();

        import_midi(&mut s, &name, &logger());

        assert_eq!(degrees(&s), vec![ScaleDegree::I, ScaleDegree::V]);
        assert_eq!(s.transport.bpm(), 120);
        assert_eq!(s.transport.progression_len.load(Ordering::Relaxed), 2);
        assert!(s.import_status.as_ref().is_some_and(ActionStatus::is_ok));

        std::fs::remove_dir_all(&s.export_dir).unwrap();
    }

    #[test]
    fn an_import_can_be_undone_in_one_step() {
        let mut s = state(Focus::Transport);
        s.export_dir = unique_export_dir("import-undo");
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        let name = export_current(&mut s);
        {
            let mut prog = s.progression.lock().unwrap();
            prog.replace(vec![Slot::Rest]);
        }

        import_midi(&mut s, &name, &logger());
        assert_eq!(s.progression.lock().unwrap().len(), 2);

        // One undo returns the pre-import progression, not a half-imported one.
        assert!(s.progression.lock().unwrap().undo());
        assert_eq!(s.progression.lock().unwrap().len(), 1);

        std::fs::remove_dir_all(&s.export_dir).unwrap();
    }

    #[test]
    fn a_foreign_midi_file_is_refused_and_reported() {
        let mut s = state(Focus::Transport);
        s.export_dir = unique_export_dir("import-foreign");
        // A well-formed MIDI file, but without the embedded session document.
        let score = midi::render_progression(
            &Progression::new(),
            &Key::new(60, Scale::Major),
            120,
            1.0,
        );
        let bytes = crate::smf::write(&score, &crate::smf::SmfOptions::single("Foreign"));
        std::fs::write(s.export_dir.join("foreign.mid"), bytes).unwrap();

        import_midi(&mut s, "foreign.mid", &logger());

        match &s.import_status {
            Some(status) if !status.is_ok() => {
                assert!(
                    status.text().contains("not a chord-tool file"),
                    "got {}",
                    status.text()
                )
            }
            other => panic!("expected a refusal, got {:?}", other),
        }

        std::fs::remove_dir_all(&s.export_dir).unwrap();
    }

    #[test]
    fn importing_a_missing_file_is_reported() {
        let mut s = state(Focus::Transport);
        s.export_dir = unique_export_dir("import-missing");
        import_midi(&mut s, "does-not-exist.mid", &logger());
        assert!(s.import_status.as_ref().is_some_and(|status| !status.is_ok()));
        std::fs::remove_dir_all(&s.export_dir).unwrap();
    }

    #[test]
    fn importing_an_empty_name_is_refused_without_touching_the_disk() {
        let mut s = state(Focus::Transport);
        import_midi(&mut s, "   ", &logger());
        assert!(s.import_status.as_ref().is_some_and(|status| !status.is_ok()));
    }

    #[test]
    fn the_import_prompt_prefills_the_newest_export() {
        let mut s = state(Focus::Transport);
        s.export_dir = unique_export_dir("import-prefill");
        seed(&mut s, &[ScaleDegree::I]);
        let name = export_current(&mut s);

        open_import_modal(&mut s, &logger());

        assert!(matches!(
            &s.modal,
            Some(Modal::ImportPathInput { buffer }) if buffer == &name
        ));

        std::fs::remove_dir_all(&s.export_dir).unwrap();
    }

    #[test]
    fn an_absolute_path_is_honoured_over_the_export_directory() {
        let mut s = state(Focus::Transport);
        let dir = unique_export_dir("import-absolute");
        s.export_dir = dir.clone();
        seed(&mut s, &[ScaleDegree::I]);
        let name = export_current(&mut s);
        let absolute = dir.join(&name);

        // Point export_dir somewhere useless: the absolute path must win.
        s.export_dir = std::env::temp_dir().join("chord-tool-tui-nowhere");
        import_midi(&mut s, absolute.to_str().unwrap(), &logger());

        assert!(s.import_status.as_ref().is_some_and(ActionStatus::is_ok));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ---- status fade ----

    #[test]
    fn a_successful_filename_holds_then_fades_away() {
        let start = Instant::now();
        let status =
            ActionStatus::new("progression-x.mid".to_string(), ActionOutcome::Ok, start);

        // Full brightness for the whole hold.
        assert_eq!(
            status.appearance_at(start),
            Some((Color::Green, "progression-x.mid"))
        );
        assert_eq!(
            status.appearance_at(start + STATUS_HOLD - Duration::from_millis(1)),
            Some((Color::Green, "progression-x.mid"))
        );

        // Dimmer in the middle of the fade, but still readable.
        match status.appearance_at(start + STATUS_HOLD + STATUS_FADE / 2) {
            Some((Color::Rgb { g: green, .. }, text)) => {
                assert!(
                    green > 0 && green < 0xAF,
                    "expected a dimmed green, got {}",
                    green
                );
                assert_eq!(text, "progression-x.mid");
            }
            other => panic!("expected a fading green, got {:?}", other),
        }

        // Gone once the fade completes, and it stays gone.
        assert_eq!(status.appearance_at(start + STATUS_HOLD + STATUS_FADE), None);
        assert_eq!(status.appearance_at(start + Duration::from_secs(600)), None);
    }

    #[test]
    fn a_failure_does_not_fade() {
        // An error is worth reading and usually actionable; only the
        // confirmatory filename goes away on its own.
        let start = Instant::now();
        let status =
            ActionStatus::new("not a chord-tool file".to_string(), ActionOutcome::Failed, start);
        assert_eq!(
            status.appearance_at(start + Duration::from_secs(600)),
            Some((Color::Red, "not a chord-tool file"))
        );
    }

    #[test]
    fn a_fresh_status_is_stamped_and_classified() {
        let before = Instant::now();
        let ok = ActionStatus::ok("name.mid".to_string());
        assert!(ok.shown_at >= before);
        assert!(ok.is_ok());
        assert_eq!(ok.text(), "name.mid");

        let failed = ActionStatus::failed("nope".to_string());
        assert!(!failed.is_ok());
    }
}
