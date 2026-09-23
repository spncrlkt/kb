//! TUI: chord grammar, synth controls, progression, transport.

use std::io::{self, Write};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
use crate::grammar::{left_hand_degree, right_hand_transformation};
use crate::keyboard::{Hotkey, KeyPosition, PositionSet, ACTIVE_LAYOUT};
use crate::music::{
    chord_label, diatonic_triad, diatonic_triad_label, note_name, ChordSpec, Key, Scale,
    ScaleDegree, Transformation,
};
use crate::presets::{default_path, PatchStore};
use crate::progression::{Progression, ProgressionEntry, Registers, Slot};
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
}

impl Modal {
    fn pass_through_chords(&self) -> bool {
        !matches!(self, Modal::PatchNameInput { .. })
    }
}

// -----------------------------------------------------------------------------
// App state
// -----------------------------------------------------------------------------

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
            Focus::Transport => 5,
            Focus::Progression => self.progression.lock().unwrap().len(),
            Focus::SynthMixer => MIXER_PARAMS.len(),
            Focus::SynthLow | Focus::SynthMid | Focus::SynthHigh => CHANNEL_PARAMS.len(),
            Focus::SynthPresets => self.patch_store.patches.len() + 1,
        }
    }

    fn current_row(&self) -> usize {
        match self.focus {
            Focus::Transport => self.mixer_row.min(4),
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
        KeyCode::Enter => {
            if ev.modifiers.contains(KeyModifiers::CONTROL) {
                // Ctrl+Enter always appends, even with no chord held.
                add_current_chord(state, true, logger);
            } else if enter_commits_chord(state) {
                // A chord is resolvable (held keys and/or a latched register),
                // so Enter commits it from whichever panel has focus. In the
                // progression panel it lands after the selected row; elsewhere
                // it appends.
                let to_end = state.focus != Focus::Progression;
                add_current_chord(state, to_end, logger);
            } else {
                primary_action(state, synth, logger);
            }
        }
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
            let (label, notes) = match transformation {
                Some(t) => {
                    let spec = ChordSpec::new(d, t);
                    (chord_label(&key, &spec), spec.voice(&key))
                }
                None => (diatonic_triad_label(&key, d), diatonic_triad(&key, d)),
            };
            let note_str: Vec<String> = notes.iter().map(|n| note_name(*n)).collect();
            execute!(stdout, Print(format!("  Chord:       {}\r\n", label)))?;
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
            "\r\n  Esc quit   Tab cycle   Enter: add chord   \
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
    let row = if focused { state.mixer_row } else { usize::MAX };

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
}
