//! TUI: chord grammar, synth controls, progression, transport.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Local;
use crossterm::{
    cursor::MoveTo,
    Command,
    event::{
        self, Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    style::{
        Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    },
    terminal::{
        disable_raw_mode, enable_raw_mode, size, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};

use crate::debug_log::{Logger, OutputTap};
use crate::export;
use crate::grammar::{left_hand_degree, right_hand_transformation};
use crate::keyboard::{Hotkey, KeyPosition, PositionSet, ACTIVE_LAYOUT};
use crate::midi;
use crate::music::{
    chord_label, diatonic_triad, diatonic_triad_label, note_name, ChordSpec, Key, Scale,
    ScaleDegree, Transformation, BAR_TICKS, BEATS_PER_BAR,
};
use crate::presets::{default_path, PatchStore};
use crate::progression::{Progression, ProgressionEntry, Registers, Slot};
use crate::project;
use crate::rhythm::{self, bar_phase_ticks, RhythmLayer, RhythmPattern};
use crate::rhythm_store::{self, RhythmStore};
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

/// Transport panel rows, in order.
///
/// Named rather than numeric because inserting the metronome row moved every
/// index after it, and a bare `3` in a key handler is exactly the kind of thing
/// that silently starts editing the wrong row.
const TRANSPORT_ROW_BPM: usize = 0;
const TRANSPORT_ROW_LOOP: usize = 1;
const TRANSPORT_ROW_METRONOME: usize = 2;
/// Read-only: it reports the bar the scheduler is on.
const TRANSPORT_ROW_PLAYING: usize = 3;
const TRANSPORT_ROW_KEY: usize = 4;
const TRANSPORT_ROW_MUTE: usize = 5;
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

/// A signed offset as a note value, the way the panel and the chord rows show
/// it: `-1/8`, or the tick count when it is not a note length.
///
/// The offset moves on the note ladder, so this almost always names a note
/// value — which is the point of the ladder.
fn format_offset(ticks: i32) -> String {
    if ticks == 0 {
        return "0".to_string();
    }
    let sign = if ticks > 0 { "+" } else { "-" };
    match note_value(ticks.unsigned_abs() as u64) {
        Some(name) => format!("{}{}", sign, name),
        None => format!("{}t", ticks),
    }
}

/// The offset with its length spelled out, for the row that edits it.
fn format_offset_with_ticks(ticks: i32) -> String {
    if ticks == 0 {
        return "0".to_string();
    }
    format!(
        "{}  ({} ticks)",
        format_offset(ticks),
        ticks.unsigned_abs()
    )
}

/// A duration as a note value *and* its ticks, or `none` for zero.
///
/// The ticks are shown as well as the note value on purpose: a hold is a
/// fraction of a grid cell, so changing the grid rescales it, and the number is
/// what makes that visible.
fn format_ticks(ticks: u64) -> String {
    if ticks == 0 {
        return "none".to_string();
    }
    match note_value(ticks) {
        Some(name) => format!("{}  —  {} ticks", name, ticks),
        None => format!("{} ticks", ticks),
    }
}

/// The note value a tick count lands on, if any. 2880 is what the player's
/// notes call a 3/4 note.
fn note_value(ticks: u64) -> Option<&'static str> {
    // The names follow the way the ladder was asked for — a 3/16, a 3/4 — so
    // every rung is either the unit or three of the next one down. No dotted
    // notation to translate in your head.
    Some(match ticks {
        60 => "1/64",
        120 => "1/32",
        240 => "1/16",
        480 => "1/8",
        720 => "3/16",
        960 => "1/4",
        1440 => "3/8",
        1920 => "1/2",
        2880 => "3/4",
        3840 => "whole",
        _ => return None,
    })
}

/// The grid resolution as a note value: 4 steps per bar is quarter notes.
fn format_resolution(steps: usize) -> &'static str {
    match steps {
        2 => "1/2",
        4 => "1/4",
        8 => "1/8",
        16 => "1/16",
        32 => "1/32",
        64 => "1/64",
        _ => "custom",
    }
}

/// How wide one cell of the bar grid is drawn, given the columns the grid may
/// use. Never zero, and capped so a two-cell grid does not become a bar chart.
fn sinko_cell_width(steps: usize, budget: usize) -> usize {
    if steps == 0 {
        1
    } else {
        (budget / steps).clamp(1, 8)
    }
}

/// One line of the bar grid, at the working pattern's resolution.
///
/// Cell width is chosen so the widest grid still fits a terminal. The playhead
/// cell is `X`/`+` rather than a colour, so the position is assertable in a test
/// after ANSI stripping.
fn sinko_grid_line(
    steps: &[bool],
    playhead: Option<usize>,
    cursor: Option<usize>,
    budget: usize,
) -> String {
    let count = steps.len();
    if count == 0 {
        return String::new();
    }
    let width = sinko_cell_width(count, budget);
    let beat = (count / BEATS_PER_BAR as usize).max(1);
    let mut out = String::new();
    for (i, on) in steps.iter().enumerate() {
        if i % beat == 0 {
            out.push('|');
        }
        let mark = if Some(i) == playhead {
            if *on {
                'X'
            } else {
                '+'
            }
        } else if *on {
            'x'
        } else {
            '·'
        };
        // The bar clock's cell is coloured as well as marked, because it moves
        // every beat and a colour is findable without reading the row.
        let cell: String = std::iter::repeat(mark).take(width).collect();
        let cell = if Some(i) == playhead {
            styled(&cell, LIVE_FG, true)
        } else {
            cell
        };
        // The edit cursor is shown by inverting the cell rather than by a
        // different character: the cell still has to say whether it is a hit,
        // and `X`/`+` are already the playhead's.
        if Some(i) == cursor {
            out.push_str("\x1b[7m");
        }
        out.push_str(&cell);
        if Some(i) == cursor {
            out.push_str("\x1b[0m");
        }
    }
    out.push('|');
    out
}

/// Which grid cell the bar clock is in, or `None` when it is not running.
fn sinko_playhead(state: &AppState, steps: usize) -> Option<usize> {
    if steps == 0 {
        return None;
    }
    // The playhead is worth seeing whenever there is a bar to be in: playback,
    // or a metronome click you are tapping along to with the transport stopped.
    let clock_running = state.transport.playing.load(Ordering::Relaxed)
        || state.transport.metronome.load(Ordering::Relaxed);
    if !clock_running {
        return None;
    }
    let (bar_start, _) = state.transport.bar_started()?;
    let phase = bar_phase_ticks(Instant::now(), bar_start, state.transport.bar_duration());
    let step_ticks = (BAR_TICKS / steps as u64).max(1);
    Some(((phase as u64 / step_ticks) as usize) % steps)
}

// -----------------------------------------------------------------------------
// Focus
// -----------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Focus {
    Transport,
    Progression,
    /// Rhythm patterns: the settings of the progression entry the Progression
    /// panel has selected, plus the grid being tapped.
    Sinko,
    /// One panel for every setting: the three channels side by side plus the
    /// master block. It replaced four subtab panels, so a single Tab now steps
    /// onto it and a single Tab steps off.
    Synth,
    SynthPresets,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            // The order the panels are laid out in: the chord list and the
            // transport share the first row, list on the left, and the rest are
            // stacked below them. `Tab` therefore moves left to right and then
            // down the screen, from wherever it happens to be.
            Focus::Progression => Focus::Transport,
            Focus::Transport => Focus::Sinko,
            Focus::Sinko => Focus::Synth,
            Focus::Synth => Focus::SynthPresets,
            Focus::SynthPresets => Focus::Progression,
        }
    }

    fn prev(self) -> Self {
        match self {
            Focus::Transport => Focus::Progression,
            Focus::Sinko => Focus::Transport,
            Focus::Synth => Focus::Sinko,
            Focus::SynthPresets => Focus::Synth,
            Focus::Progression => Focus::SynthPresets,
        }
    }
}

// -----------------------------------------------------------------------------
// Tap tracker
// -----------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
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
            // Twice restarts from bar 1: the common "again, from the top".
            2 => Some(TapAction::Restart),
            _ => Some(TapAction::SeekMiddle),
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
    ReverbMix,
    ReverbSize,
    MasterVolume,
    MasterMute,
    PreviewFade,
    NoteLength,
}

/// Canonical enumeration of the master settings, kept for the test that proves
/// `MASTER_ROWS` holds every one exactly once — a pair-based layout would
/// otherwise let a parameter vanish from the UI without a compile error.
#[cfg(test)]
const MIXER_PARAMS: [MixerParam; 6] = [
    MixerParam::ReverbMix,
    MixerParam::ReverbSize,
    MixerParam::MasterVolume,
    MixerParam::MasterMute,
    MixerParam::PreviewFade,
    MixerParam::NoteLength,
];

/// The master settings, two per table row.
///
/// Per-channel volume, reverb send and pan used to live here as nine separate
/// rows; they are the three channel columns now, so the master block holds only
/// what is genuinely global.
const MASTER_ROWS: [(MixerParam, MixerParam); 3] = [
    (MixerParam::ReverbMix, MixerParam::ReverbSize),
    (MixerParam::MasterVolume, MixerParam::MasterMute),
    (MixerParam::PreviewFade, MixerParam::NoteLength),
];

impl MixerParam {
    fn label(self) -> &'static str {
        match self {
            MixerParam::ReverbMix => "reverb level",
            MixerParam::ReverbSize => "reverb size",
            MixerParam::MasterVolume => "master volume",
            MixerParam::MasterMute => "master mute",
            MixerParam::PreviewFade => "preview fade",
            MixerParam::NoteLength => "note length",
        }
    }

    fn display(self, p: &SynthParams, t: &Transport) -> String {
        match self {
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

    /// The raw value behind this row, for an edit's `initial` snapshot.
    fn value(self, p: &SynthParams, t: &Transport) -> f32 {
        match self {
            MixerParam::ReverbMix => p.reverb_mix.get(),
            MixerParam::ReverbSize => p.reverb_size.get(),
            MixerParam::MasterVolume => p.master_volume.get(),
            MixerParam::MasterMute => p.master_mute.get(),
            MixerParam::PreviewFade => p.preview_fade.get(),
            MixerParam::NoteLength => t.note_length(),
        }
    }

    /// Write a raw value back. Only `Esc` uses this, with a value it previously
    /// read, so it is already inside the row's range and needs no clamping.
    fn restore(self, p: &SynthParams, t: &Transport, v: f32) {
        match self {
            MixerParam::ReverbMix => p.reverb_mix.set(v),
            MixerParam::ReverbSize => p.reverb_size.set(v),
            MixerParam::MasterVolume => p.master_volume.set(v),
            MixerParam::MasterMute => p.master_mute.set(v),
            MixerParam::PreviewFade => p.preview_fade.set(v),
            MixerParam::NoteLength => t.set_note_length(v),
        }
    }
}

// -----------------------------------------------------------------------------
// The Synth table
// -----------------------------------------------------------------------------

/// One addressable cell of the Synth table.
///
/// Rows `0..CHANNEL_PARAMS.len()` are channel settings with three columns; the
/// rows after that are the master block, two settings per row.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum SynthCell {
    Channel { row: usize, col: usize },
    Master { row: usize, col: usize },
}

/// Addressable rows: one per channel setting, then one per master pair.
const SYNTH_ROWS: usize = CHANNEL_PARAMS.len() + MASTER_ROWS.len();

/// How many columns the row at `row` has.
fn synth_col_count(row: usize) -> usize {
    if row < CHANNEL_PARAMS.len() {
        CHANNEL_COUNT
    } else {
        2
    }
}

fn synth_cell(row: usize, col: usize) -> SynthCell {
    if row < CHANNEL_PARAMS.len() {
        SynthCell::Channel {
            row,
            col: col.min(CHANNEL_COUNT - 1),
        }
    } else {
        SynthCell::Master {
            row: row - CHANNEL_PARAMS.len(),
            col: col.min(1),
        }
    }
}

fn channel_at<'a>(p: &'a SynthParams, col: usize) -> &'a crate::synth::ChannelParams {
    match col {
        1 => &p.mid,
        2 => &p.high,
        _ => &p.low,
    }
}

const CHANNEL_COLUMN_LABELS: [&str; CHANNEL_COUNT] = ["low", "mid", "high"];

impl SynthCell {
    fn param(self) -> Option<ChannelParam> {
        match self {
            SynthCell::Channel { row, .. } => CHANNEL_PARAMS.get(row).copied(),
            SynthCell::Master { .. } => None,
        }
    }

    fn master(self) -> Option<MixerParam> {
        match self {
            SynthCell::Master { row, col } => MASTER_ROWS
                .get(row)
                .map(|(a, b)| if col == 0 { *a } else { *b }),
            SynthCell::Channel { .. } => None,
        }
    }

    fn label(self) -> &'static str {
        match (self.param(), self.master()) {
            (Some(p), _) => p.label(),
            (_, Some(m)) => m.label(),
            _ => "",
        }
    }

    /// Change this cell's value. The same call backs `Shift+←/→` and the
    /// arrows inside an open edit, so both paths share one clamp.
    fn adjust(self, p: &SynthParams, t: &Transport, delta: i32) {
        match (self.param(), self.master()) {
            (Some(param), _) => {
                if let SynthCell::Channel { col, .. } = self {
                    param.adjust(channel_at(p, col), delta);
                }
            }
            (_, Some(m)) => m.adjust(p, t, delta),
            _ => {}
        }
    }

    /// The raw value, for an edit's `initial` snapshot.
    fn value(self, p: &SynthParams, t: &Transport) -> f32 {
        match (self.param(), self.master()) {
            (Some(param), _) => {
                if let SynthCell::Channel { col, .. } = self {
                    param.value(channel_at(p, col))
                } else {
                    0.0
                }
            }
            (_, Some(m)) => m.value(p, t),
            _ => 0.0,
        }
    }

    /// Undo an edit. `Esc` only, and only with a value previously read here.
    fn restore(self, p: &SynthParams, t: &Transport, v: f32) {
        match (self.param(), self.master()) {
            (Some(param), _) => {
                if let SynthCell::Channel { col, .. } = self {
                    param.restore(channel_at(p, col), v);
                }
            }
            (_, Some(m)) => m.restore(p, t, v),
            _ => {}
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
    Volume,
    Waveform,
    Attack,
    Decay,
    Sustain,
    Release,
    Cutoff,
    Resonance,
    Transpose,
    ReverbSend,
    Pan,
}

/// One row per channel setting, shared by all three columns.
///
/// This is the complete `ChannelPatch`: the volume / reverb send / pan rows used
/// to live in the separate Mixer subtab, which meant a channel's volume and its
/// cutoff could never be seen at the same time.
const CHANNEL_PARAMS: [ChannelParam; 11] = [
    ChannelParam::Volume,
    ChannelParam::Waveform,
    ChannelParam::Attack,
    ChannelParam::Decay,
    ChannelParam::Sustain,
    ChannelParam::Release,
    ChannelParam::Cutoff,
    ChannelParam::Resonance,
    ChannelParam::Transpose,
    ChannelParam::ReverbSend,
    ChannelParam::Pan,
];

/// The three channel columns, in display order.
const CHANNEL_COUNT: usize = 3;

impl ChannelParam {
    fn label(self) -> &'static str {
        match self {
            ChannelParam::Volume => "volume",
            ChannelParam::Waveform => "waveform",
            ChannelParam::Attack => "attack",
            ChannelParam::Decay => "decay",
            ChannelParam::Sustain => "sustain",
            ChannelParam::Release => "release",
            ChannelParam::Cutoff => "cutoff",
            ChannelParam::Resonance => "resonance",
            ChannelParam::Transpose => "transpose",
            ChannelParam::ReverbSend => "reverb send",
            ChannelParam::Pan => "pan",
        }
    }

    fn display(self, ch: &crate::synth::ChannelParams) -> String {
        match self {
            ChannelParam::Volume => format!("{:.0}", ch.volume.get()),
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
            ChannelParam::ReverbSend => format!("{:.0}%", ch.reverb_send.get() * 100.0),
            ChannelParam::Pan => format_pan(ch.pan.get()),
        }
    }

    fn adjust(self, ch: &crate::synth::ChannelParams, delta: i32) {
        let d = delta as f32;
        match self {
            ChannelParam::Volume => ch.volume.set((ch.volume.get() + d).clamp(0.0, 7.0)),
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
            ChannelParam::ReverbSend => ch
                .reverb_send
                .set((ch.reverb_send.get() + d * 0.05).clamp(0.0, 1.0)),
            ChannelParam::Pan => ch.pan.set((ch.pan.get() + d * 0.1).clamp(-1.0, 1.0)),
        }
    }

    /// The raw value behind this row, for an edit's `initial` snapshot.
    fn value(self, ch: &crate::synth::ChannelParams) -> f32 {
        match self {
            ChannelParam::Volume => ch.volume.get(),
            ChannelParam::Waveform => ch.waveform.get(),
            ChannelParam::Attack => ch.attack.get(),
            ChannelParam::Decay => ch.decay.get(),
            ChannelParam::Sustain => ch.sustain.get(),
            ChannelParam::Release => ch.release.get(),
            ChannelParam::Cutoff => ch.cutoff.get(),
            ChannelParam::Resonance => ch.resonance.get(),
            ChannelParam::Transpose => ch.transpose.get(),
            ChannelParam::ReverbSend => ch.reverb_send.get(),
            ChannelParam::Pan => ch.pan.get(),
        }
    }

    /// Write a raw value back. Only `Esc` uses this, with a value it previously
    /// read, so it is already inside the row's range and needs no clamping.
    fn restore(self, ch: &crate::synth::ChannelParams, v: f32) {
        match self {
            ChannelParam::Volume => ch.volume.set(v),
            ChannelParam::Waveform => ch.waveform.set(v),
            ChannelParam::Attack => ch.attack.set(v),
            ChannelParam::Decay => ch.decay.set(v),
            ChannelParam::Sustain => ch.sustain.set(v),
            ChannelParam::Release => ch.release.set(v),
            ChannelParam::Cutoff => ch.cutoff.set(v),
            ChannelParam::Resonance => ch.resonance.set(v),
            ChannelParam::Transpose => ch.transpose.set(v),
            ChannelParam::ReverbSend => ch.reverb_send.set(v),
            ChannelParam::Pan => ch.pan.set(v),
        }
    }
}

// -----------------------------------------------------------------------------
// Transport edit / modals
// -----------------------------------------------------------------------------

#[derive(Clone)]
enum Edit {
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
    /// A Synth table cell. Arrows adjust the live parameter, so the change is
    /// audible as it happens; `initial` is the value `Esc` writes back.
    SynthCell {
        cell: SynthCell,
        initial: f32,
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
    /// Name for the pattern being built in the Sinko panel.
    RhythmNameInput { buffer: String },
}

impl Modal {
    fn pass_through_chords(&self) -> bool {
        !matches!(
            self,
            Modal::PatchNameInput { .. }
                | Modal::ImportPathInput { .. }
                | Modal::RhythmNameInput { .. }
        )
    }
}

// -----------------------------------------------------------------------------
// Sinko panel
// -----------------------------------------------------------------------------

/// Rows of the Sinko panel. A fixed count, unlike the Progression panel: the
/// layer grid always draws every line, so the layout cannot jump as takes
/// arrive.
const SINKO_ROW_CHORD: usize = 0;
const SINKO_ROW_PATTERN: usize = 1;
const SINKO_ROW_OFFSET: usize = 2;
const SINKO_ROW_QUANT: usize = 3;
/// The cell cursor: walk the grid and toggle the hits on it.
const SINKO_ROW_HITS: usize = 4;
/// How long each hit holds, in whole grid cells.
const SINKO_ROW_HOLD: usize = 5;
/// How much of the bar's tail is silent.
const SINKO_ROW_MUTE: usize = 6;
const SINKO_ROW_SMOOTH: usize = 7;
const SINKO_ROW_RECORD: usize = 8;
const SINKO_LAYER_ROWS: usize = crate::arrangement::RHYTHM_LAYERS;
const SINKO_ROW_NEW: usize = SINKO_ROW_RECORD + 1 + SINKO_LAYER_ROWS;
const SINKO_ROW_SAVE: usize = SINKO_ROW_NEW + 1;
/// Copy the selected chord's rhythm to the Sinko clipboard.
const SINKO_ROW_COPY: usize = SINKO_ROW_SAVE + 1;
/// Apply the clipboard to the selected chord, as its own copy.
const SINKO_ROW_PASTE: usize = SINKO_ROW_COPY + 1;
const SINKO_ROWS: usize = SINKO_ROW_PASTE + 1;

/// Taps closer together than this are key repeat, not a second tap.
const TAP_DEBOUNCE_MS: u128 = 30;

/// How long the stop flash covers the title line.
const STOP_FLASH: Duration = Duration::from_millis(300);

/// Two Esc presses inside this window quit; one stops.
///
/// The window starts at the *first* press, so a lone Esc stops immediately
/// rather than waiting to see whether a second one is coming.
const ESC_QUIT_WINDOW: Duration = Duration::from_millis(500);

/// How many recent takes a new pattern averages by default.
///
/// One, so each take is its own layer: that is the overlapping-takes sound this
/// panel is for. Raising it averages the recent takes into the top layer
/// instead, which is how a figure tapped several times converges on one clean
/// line — but it merges *different* figures into one, so it is not the default.
const SINKO_SMOOTH_DEFAULT: usize = 1;

// -----------------------------------------------------------------------------
// App state
// -----------------------------------------------------------------------------

/// How long a successful export/import message stays at full brightness.
const STATUS_HOLD: Duration = Duration::from_secs(5);

/// How long it takes to fade away once the hold is over.
const STATUS_FADE: Duration = Duration::from_millis(1000);

/// Widest a status message may be drawn beside a button.
///
/// The panel is a fixed column of rows, so a long message must never wrap: an
/// import failure or a multi-line TOML parse error would otherwise reflow the
/// whole panel and shove everything below it down the screen.
const STATUS_MAX_CHARS: usize = 48;

/// Flatten status text to a single clipped line.
///
/// Errors from deeper layers are not written for a one-line panel: a TOML parse
/// error, for instance, arrives with newlines and a caret. Collapsing
/// whitespace keeps the row count fixed, and clipping keeps the width fixed.
/// The full text still reaches `debug.log`.
fn single_line_status(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= STATUS_MAX_CHARS {
        return collapsed;
    }
    let mut clipped: String = collapsed.chars().take(STATUS_MAX_CHARS - 1).collect();
    clipped.push('…');
    clipped
}

#[derive(Debug)]
enum ActionOutcome {
    /// Done, and worth a moment on screen: saved, copied, exported.
    Ok,
    /// Refused: the key was pressed and could not do anything. There is nothing
    /// to act on, so it says so and gets out of the way.
    Refused,
    /// Failed: something that should have worked did not, and the reason is
    /// worth reading at whatever pace the user reads.
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

    /// A key press that could not do anything: shown, then gone.
    fn refused(text: String) -> Self {
        Self::new(text, ActionOutcome::Refused, Instant::now())
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
    /// A success and a refusal hold at full brightness for [`STATUS_HOLD`] and
    /// then dim over [`STATUS_FADE`]. Only a failure stays: it is usually telling
    /// you that something you asked for did not happen, and vanishing before it is
    /// read would be unhelpful — whereas a refused key press has nothing to act
    /// on, and a message about it that never leaves is just litter.
    fn appearance_at(&self, now: Instant) -> Option<(Color, &str)> {
        if matches!(self.outcome, ActionOutcome::Failed) {
            return Some((Color::Red, self.text()));
        }

        let elapsed = now.saturating_duration_since(self.shown_at);
        if elapsed < STATUS_HOLD {
            return Some((self.full_colour(), self.text()));
        }

        let faded = (elapsed - STATUS_HOLD).as_secs_f32();
        let total = STATUS_FADE.as_secs_f32();
        if faded >= total {
            return None;
        }
        Some((self.faded_colour(1.0 - faded / total), self.text()))
    }

    /// The colour at full brightness: green for something done, red for something
    /// refused.
    fn full_colour(&self) -> Color {
        if self.is_ok() {
            Color::Green
        } else {
            Color::Red
        }
    }

    /// The same colour, dimmed.
    fn faded_colour(&self, level: f32) -> Color {
        if self.is_ok() {
            faded_green(level)
        } else {
            faded_red(level)
        }
    }
}

/// Green dimmed toward black: what a success fades through.
fn faded_green(level: f32) -> Color {
    faded(0x00, 0xAF, 0x00, level)
}

/// Red dimmed toward black: what a refusal fades through.
fn faded_red(level: f32) -> Color {
    faded(0xAF, 0x00, 0x00, level)
}

/// A colour faded toward black.
///
/// A terminal cannot blend toward an unknown background, so "fade" here means
/// "lose brightness": `level` 1 is the full colour, 0 is black. Truecolor is
/// near-universal on the terminals this targets; a terminal without it will
/// approximate, which still reads as a fade.
fn faded(r: u8, g: u8, b: u8, level: f32) -> Color {
    let level = level.clamp(0.0, 1.0);
    Color::Rgb {
        r: (r as f32 * level).round() as u8,
        g: (g as f32 * level).round() as u8,
        b: (b as f32 * level).round() as u8,
    }
}

struct AppState {
    held: PositionSet,
    registers: Registers,
    focus: Focus,
    progression: Arc<Mutex<Progression>>,
    transport: Arc<Transport>,
    edit: Edit,
    /// Transport panel cursor. Separate from the Synth cursor: they used to
    /// share one index, so leaving either panel deep pushed the other's
    /// selection off the end of its own row list.
    transport_row: usize,
    /// Synth table cursor: a row and, within it, a channel column (or one of
    /// the two master cells).
    synth_row: usize,
    synth_col: usize,
    progression_row: usize,
    preset_row: usize,
    modal: Option<Modal>,
    patch_store: PatchStore,
    /// The rhythm pattern *library*, loaded from and saved to `rhythms.toml`.
    ///
    /// A palette of starting points, not what plays: since patterns became
    /// per-chord copies, the scheduler reads the progression and nothing else.
    /// Assigning a pattern clones it out of here, and `[Save Pattern As...]`
    /// copies one back in.
    rhythm_store: Arc<Mutex<RhythmStore>>,
    /// Sinko panel cursor.
    sinko_row: usize,
    /// Which grid cell the `hits` row is editing.
    sinko_cell: usize,
    /// Committed takes, oldest first. Each is one bar of tick positions.
    takes: Vec<Vec<u32>>,
    /// The take being tapped right now.
    current_take: Vec<u32>,
    /// When Esc was last pressed, for the twice-to-quit gesture.
    last_esc: Option<Instant>,
    /// How long the red stop flash stays up.
    stop_flash_until: Option<Instant>,
    /// Whether the tap key records.
    recording: bool,
    /// The metronome switch, as the user set it.
    ///
    /// The transport's own flag is the *effective* state, because an armed
    /// recording needs the click too — see `sync_metronome`.
    metronome_on: bool,
    /// Guards the debounce, so a repeating key is not a second tap.
    last_tap_at: Option<Instant>,
    /// When the current take's bar began; a take closes once the clock moves on.
    take_started_at: Option<Instant>,
    /// How many of the most recent takes are averaged into the top layer.
    sinko_smooth: usize,
    /// The rhythm being edited: a clone of the selected entry's own pattern, or
    /// a blank page when it has none.
    working: RhythmPattern,
    /// Which progression row `working` was loaded from, so moving the selection
    /// reloads it (and abandons the takes that belonged to the other row).
    working_slot: Option<usize>,
    /// `[Copy Sinko]`'s clipboard: one chord's rhythm, ready to paste onto
    /// another. A pattern rather than a name, because what is pasted is a copy.
    sinko_clipboard: Option<RhythmPattern>,
    /// Feedback for the Sinko panel's own actions.
    rhythm_status: Option<ActionStatus>,
    /// Where the pattern library is written. A field rather than a call to
    /// `default_path()` at save time, so a test never writes into the working
    /// directory — the same reasoning as `export_dir`.
    rhythm_path: PathBuf,
    flash_until: Option<Instant>,
    taps: TapTracker,
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

    /// True while the red stop banner should cover the title line.
    fn stop_flash_active(&self) -> bool {
        matches!(self.stop_flash_until, Some(t) if Instant::now() < t)
    }

    /// The cell the Synth cursor is on.
    fn synth_cell(&self) -> SynthCell {
        synth_cell(self.synth_row, self.synth_col)
    }

    fn row_count(&self) -> usize {
        match self.focus {
            Focus::Transport => TRANSPORT_ROWS,
            Focus::Progression => self.progression.lock().unwrap().len(),
            Focus::Sinko => SINKO_ROWS,
            Focus::Synth => SYNTH_ROWS,
            Focus::SynthPresets => self.patch_store.patches.len() + 1,
        }
    }

    fn current_row(&self) -> usize {
        match self.focus {
            Focus::Transport => self.transport_row.min(TRANSPORT_ROWS - 1),
            Focus::Progression => self.progression_row,
            Focus::Sinko => self.sinko_row.min(SINKO_ROWS - 1),
            Focus::Synth => self.synth_row.min(SYNTH_ROWS - 1),
            Focus::SynthPresets => self.preset_row,
        }
    }

    fn set_current_row(&mut self, row: usize) {
        let count = self.row_count();
        let clamped = if count == 0 { 0 } else { row.min(count - 1) };
        match self.focus {
            Focus::Transport => self.transport_row = clamped,
            Focus::Progression => self.progression_row = clamped,
            Focus::Sinko => self.sinko_row = clamped,
            Focus::Synth => {
                self.synth_row = clamped.min(SYNTH_ROWS - 1);
                // A master row has two columns, a channel row three.
                self.synth_col = self.synth_col.min(synth_col_count(self.synth_row) - 1);
            }
            Focus::SynthPresets => self.preset_row = clamped,
        }
    }

    /// Move within the Synth table's columns, clamped to the row's width.
    fn move_synth_col(&mut self, delta: i32) {
        let count = synth_col_count(self.synth_row) as i32;
        self.synth_col = (self.synth_col as i32 + delta).clamp(0, count - 1) as usize;
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
        self.transport.set_live_chord(notes);
    }
}

// -----------------------------------------------------------------------------
// Entry point
// -----------------------------------------------------------------------------

pub fn run_interactive() -> io::Result<()> {
    // Colour here is *state*, not decoration: it is how the focused panel, the
    // selected row, the chord sounding now and the bar clock's cell are marked.
    // crossterm honours `NO_COLOR` by emitting bare resets, which would drop all
    // of that silently — and `NO_COLOR` is a convention for piped text, not for a
    // full-screen instrument. The weight cues (the focused panel's heavy rule,
    // the bold rows) still carry the same information if a terminal ignores
    // colour anyway.
    crossterm::style::force_color_output(true);

    let logger = Logger::create("debug.log")?;
    let output_tap = OutputTap::create(logger.clone());
    let synth = Synth::new(Some(output_tap.peak()))
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    let patch_store = PatchStore::load(&default_path())?;
    let rhythm_store = Arc::new(Mutex::new(RhythmStore::load(&rhythm_store::default_path())?));
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
        edit: Edit::None,
        transport_row: 0,
        synth_row: 0,
        synth_col: 0,
        progression_row: 0,
        preset_row: 0,
        modal: None,
        patch_store,
        rhythm_store,
        sinko_row: 0,
        sinko_cell: 0,
        takes: Vec::new(),
        current_take: Vec::new(),
        last_esc: None,
        stop_flash_until: None,
        recording: false,
        metronome_on: false,
        last_tap_at: None,
        take_started_at: None,
        sinko_smooth: SINKO_SMOOTH_DEFAULT,
        working: RhythmPattern::draft(),
        working_slot: None,
        sinko_clipboard: None,
        rhythm_status: None,
        rhythm_path: rhythm_store::default_path(),
        flash_until: None,
        taps: TapTracker::default(),
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

fn event_loop<W: io::Write>(
    stdout: &mut W,
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

        render(stdout, synth.params(), state, Screen::read())?;

        while let Some(ev) = scheduler.try_recv() {
            match ev {
                SchedulerEvent::Stab {
                    group,
                    notes,
                    gain,
                } => synth.play_stab(group, &notes, gain),
                SchedulerEvent::ReleaseStab { group } => synth.stop_stab(group),
                SchedulerEvent::Silence => synth.silence(),
                SchedulerEvent::Click { strong } => synth.play_click(strong),
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

        if matches!(state.modal, Some(Modal::RhythmNameInput { .. })) {
            if ev.kind == KeyEventKind::Press {
                match ev.code {
                    KeyCode::Esc => {
                        state.modal = None;
                        logger.input("MODAL cancel pattern name");
                    }
                    KeyCode::Enter => {
                        let typed = match state.modal.take() {
                            Some(Modal::RhythmNameInput { buffer }) => buffer.trim().to_string(),
                            other => {
                                state.modal = other;
                                String::new()
                            }
                        };
                        save_working_pattern(state, &typed, logger);
                    }
                    KeyCode::Backspace => {
                        if let Some(Modal::RhythmNameInput { buffer }) = &mut state.modal {
                            buffer.pop();
                        }
                    }
                    KeyCode::Char(c) => {
                        if let Some(Modal::RhythmNameInput { buffer }) = &mut state.modal {
                            if buffer.len() < 32 && !c.is_control() {
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
                        Modal::RhythmNameInput { .. } => unreachable!(),
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
                // Prompts and edits take Esc first, because there it means
                // "cancel". This has to come *before* the stop/quit gesture or
                // Esc would stop the transport out from under an open editor.
                match state.edit {
                    Edit::Bpm { .. } => {
                        handle_bpm_edit(state, &ev, logger);
                        continue;
                    }
                    Edit::TrackKey { .. } => {
                        handle_track_key_edit(state, &ev, logger);
                        continue;
                    }
                    Edit::SynthCell { .. } => {
                        handle_synth_edit(state, synth.params(), &ev, logger);
                        continue;
                    }
                    Edit::None => {}
                }

                if ev.code == KeyCode::Esc && esc_is_free(state) {
                    if esc_should_quit(state, logger) {
                        return Ok(());
                    }
                    continue;
                }

                // The Sinko panel edits a clone of the selected entry's rhythm,
                // so a change from outside the panel — an undo, an import —
                // has to be picked up before the next key is handled.
                if state.focus == Focus::Sinko {
                    sync_working(state);
                }

                match ev.code {
                    KeyCode::Tab => {
                        state.focus = state.focus.next();
                        if state.focus == Focus::Sinko {
                            sync_working(state);
                        }
                        logger.input(&format!("TAB -> {:?}", state.focus));
                        continue;
                    }
                    KeyCode::BackTab => {
                        state.focus = state.focus.prev();
                        if state.focus == Focus::Sinko {
                            sync_working(state);
                        }
                        logger.input(&format!("BACKTAB -> {:?}", state.focus));
                        continue;
                    }
                    _ => {}
                }

                if let KeyCode::Char(' ') = ev.code {
                    space_pressed(state, logger);
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
                    // Nothing to do: the space bar is a latch, so it has no
                    // release half. Its sibling locks release nothing either.
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
    let edit = std::mem::replace(&mut state.edit, Edit::None);
    let (initial, mut current, mut buffer) = match edit {
        Edit::Bpm {
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
        state.edit = Edit::Bpm {
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
        state.edit = Edit::Bpm {
            initial,
            current,
            buffer,
        };
    }
}

fn handle_track_key_edit(state: &mut AppState, ev: &event::KeyEvent, logger: &Logger) {
    let edit = std::mem::replace(&mut state.edit, Edit::None);
    let (initial, mut current) = match edit {
        Edit::TrackKey { initial, current } => (initial, current),
        other => {
            state.edit = other;
            return;
        }
    };

    if ev.kind != KeyEventKind::Press {
        state.edit = Edit::TrackKey { initial, current };
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
        state.edit = Edit::TrackKey { initial, current };
    }
}

/// What an arrow key means on the Synth table.
///
/// The table is the only two-dimensional surface in the app, so it is the only
/// place where plain `←/→` has to navigate: there, `Shift+←/→` is the live value
/// nudge. Every other panel keeps `←/→` as "adjust".
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum SynthArrow {
    Column(i32),
    Nudge(i32),
}

fn synth_arrow(code: KeyCode, shift: bool) -> Option<SynthArrow> {
    match code {
        KeyCode::Left if !shift => Some(SynthArrow::Column(-1)),
        KeyCode::Right if !shift => Some(SynthArrow::Column(1)),
        KeyCode::Left => Some(SynthArrow::Nudge(-1)),
        KeyCode::Right => Some(SynthArrow::Nudge(1)),
        _ => None,
    }
}

/// Open an edit on the Synth cursor.
///
/// Split out of `primary_action` because everything test-visible needs only
/// `SynthParams`; a `Synth` owns an audio device that no test can create.
fn begin_synth_edit(state: &mut AppState, params: &SynthParams) {
    let cell = state.synth_cell();
    let initial = cell.value(params, &state.transport);
    state.edit = Edit::SynthCell { cell, initial };
}

/// Edit one Synth table cell.
///
/// Arrows adjust the *live* parameter, so the change is audible as it happens —
/// that is the point of editing here rather than typing a value. `Enter` keeps
/// the result; `Esc` writes the pre-edit value back, which makes a sweep you
/// did not like a no-op.
fn handle_synth_edit(state: &mut AppState, params: &SynthParams, ev: &event::KeyEvent, logger: &Logger) {
    let edit = std::mem::replace(&mut state.edit, Edit::None);
    let (cell, initial) = match edit {
        Edit::SynthCell { cell, initial } => (cell, initial),
        other => {
            state.edit = other;
            return;
        }
    };

    if ev.kind != KeyEventKind::Press {
        state.edit = Edit::SynthCell { cell, initial };
        return;
    }

    let transport = state.transport.clone();
    let mut keep_editing = true;

    match ev.code {
        KeyCode::Esc => {
            cell.restore(params, &transport, initial);
            keep_editing = false;
            logger.input(&format!("SYNTH cancel {}", cell.label()));
        }
        KeyCode::Enter => {
            keep_editing = false;
            logger.input(&format!("SYNTH commit {}", cell.label()));
        }
        // Fine and coarse steps on the two axes, so one gesture covers both
        // "nudge it" and "sweep it" without leaving the cell.
        KeyCode::Left => cell.adjust(params, &transport, -1),
        KeyCode::Right => cell.adjust(params, &transport, 1),
        KeyCode::Down => cell.adjust(params, &transport, -5),
        KeyCode::Up => cell.adjust(params, &transport, 5),
        _ => {}
    }

    if keep_editing {
        state.edit = Edit::SynthCell { cell, initial };
    }
}

fn handle_panel_key(
    state: &mut AppState,
    synth: &Synth,
    ev: &event::KeyEvent,
    logger: &Logger,
) {
    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
    match ev.code {
        KeyCode::Up => {
            let row = state.current_row();
            state.set_current_row(row.saturating_sub(1));
        }
        KeyCode::Down => {
            let row = state.current_row();
            state.set_current_row(row + 1);
        }
        // The Synth table is the only two-dimensional surface in the app, so it
        // is the only panel where plain `←/→` navigates: there, `Shift+←/→` is
        // the live value nudge. Every other panel keeps `←/→` as "adjust".
        KeyCode::Left | KeyCode::Right => {
            let delta = if matches!(ev.code, KeyCode::Left) { -1 } else { 1 };
            match if state.focus == Focus::Synth {
                synth_arrow(ev.code, shift)
            } else {
                None
            } {
                Some(SynthArrow::Column(c)) => state.move_synth_col(c),
                Some(SynthArrow::Nudge(n)) => adjust_current(state, synth.params(), n, logger),
                None => adjust_current(state, synth.params(), delta, logger),
            }
        }
        KeyCode::Enter => match enter_intent(state, ev.modifiers.contains(KeyModifiers::CONTROL)) {
            EnterIntent::CommitChord { to_end } => add_current_chord(state, to_end, logger),
            EnterIntent::PanelAction => primary_action(state, synth, logger),
        },
        _ => {}
    }
}

fn adjust_current(state: &mut AppState, params: &SynthParams, delta: i32, _logger: &Logger) {
    let row = state.current_row();
    match state.focus {
        Focus::Transport => match row {
            TRANSPORT_ROW_BPM => {
                let v = state.transport.bpm() as i32 + delta;
                state.transport.set_bpm(v.clamp(BPM_MIN as i32, BPM_MAX as i32) as u16);
            }
            TRANSPORT_ROW_LOOP => {
                let cur = state.transport.looping.load(Ordering::Relaxed);
                state.transport.looping.store(!cur, Ordering::Relaxed);
            }
            TRANSPORT_ROW_METRONOME => toggle_metronome(state, _logger),
            TRANSPORT_ROW_PLAYING => toggle_playback(state, _logger),
            TRANSPORT_ROW_KEY => {
                let mut k = state.transport.key();
                k.scale = match k.scale {
                    Scale::Major => Scale::Minor,
                    Scale::Minor => Scale::Major,
                };
                state.transport.set_key(k);
            }
            TRANSPORT_ROW_MUTE => {
                let cur = params.mute_progression.get() > 0.5;
                params.mute_progression.set(if cur { 0.0 } else { 1.0 });
            }
            _ => {}
        },
        // The Progression panel's arrows nudge the selected entry's offset:
        // that is where the chord list is on screen, so that is where a chord
        // gets moved off its downbeat. The Sinko panel's offset row does the
        // same thing to the same value.
        Focus::Progression => nudge_offset(state, delta, _logger),
        Focus::Sinko => match row {
            SINKO_ROW_PATTERN => cycle_assigned_pattern(state, delta, _logger),
            SINKO_ROW_OFFSET => nudge_offset(state, delta, _logger),
            SINKO_ROW_QUANT => cycle_resolution(state, delta, _logger),
            SINKO_ROW_HITS => move_cell_cursor(state, delta, _logger),
            SINKO_ROW_HOLD => {
                if !working_is_assigned(state, _logger, "hold") {
                    return;
                }
                let rungs: Vec<i32> = rhythm::NOTE_LADDER.iter().map(|t| *t as i32).collect();
                let next = rhythm::rung_step(&rungs, state.working.hold as i32, delta);
                state.working.hold = next as u32;
                commit_working(state, _logger);
                _logger.input(&format!("SINKO hold -> {} ticks", next));
            }
            SINKO_ROW_MUTE => {
                if !working_is_assigned(state, _logger, "mute") {
                    return;
                }
                let rungs: Vec<i32> = rhythm::MUTE_LADDER.iter().map(|t| *t as i32).collect();
                let next = rhythm::rung_step(&rungs, state.working.mute_ticks as i32, delta);
                state.working.mute_ticks = next as u32;
                commit_working(state, _logger);
                _logger.input(&format!("SINKO mute -> {} ticks", next));
            }
            SINKO_ROW_SMOOTH => {
                let next = state.sinko_smooth as i32 + delta;
                state.sinko_smooth = next.clamp(1, rhythm::MAX_TAKES as i32) as usize;
                // Rebuilds the layer stack, so it is a pattern edit like the rest
                // and has to reach the entry — otherwise the next Tab would
                // silently put the old stack back.
                rebuild_working(state);
                commit_working(state, _logger);
                _logger.input(&format!("SINKO smooth = {}", state.sinko_smooth));
            }
            _ => {}
        },
        Focus::Synth => state.synth_cell().adjust(params, &state.transport, delta),
        Focus::SynthPresets => {}
    }
}

fn primary_action(state: &mut AppState, synth: &Synth, logger: &Logger) {
    let row = state.current_row();
    match state.focus {
        Focus::Transport => match row {
            TRANSPORT_ROW_BPM => {
                state.edit = Edit::Bpm {
                    initial: state.transport.bpm(),
                    current: state.transport.bpm(),
                    buffer: String::new(),
                };
                logger.input("BPM edit begin");
            }
            TRANSPORT_ROW_LOOP => {
                let cur = state.transport.looping.load(Ordering::Relaxed);
                state.transport.looping.store(!cur, Ordering::Relaxed);
            }
            TRANSPORT_ROW_METRONOME => toggle_metronome(state, logger),
            TRANSPORT_ROW_PLAYING => toggle_playback(state, logger),
            TRANSPORT_ROW_KEY => {
                state.edit = Edit::TrackKey {
                    initial: state.transport.key(),
                    current: state.transport.key(),
                };
                logger.input("KEY edit begin");
            }
            TRANSPORT_ROW_MUTE => {
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
        Focus::Sinko => sinko_action(state, row, logger),
        Focus::Synth => {
            begin_synth_edit(state, synth.params());
            logger.input(&format!("SYNTH edit {}", state.synth_cell().label()));
        }
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
            let rhythms = state.rhythm_store.lock().unwrap();
            project::encode(&prog, &rhythms, key, bpm, note_length).map(|document| {
                // The patterns travel inside the slots, so the score and the
                // document are built from the same entries.
                let score = midi::render_progression(&prog.slots, &key, bpm, note_length);
                Some((score, document))
            })
        }
    };

    let (score, document) = match prepared {
        Ok(Some(prepared)) => prepared,
        Ok(None) => {
            state.export_status = Some(ActionStatus::refused("nothing to export".to_string()));
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
        state.import_status = Some(ActionStatus::refused("no file name".to_string()));
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

            // Bring the file's patterns into the library, so the imported
            // progression resolves instead of falling back to whole bars.
            let imported = restored.rhythms.len();
            {
                let mut rhythms = state.rhythm_store.lock().unwrap();
                for pattern in restored.rhythms {
                    rhythms.add(pattern);
                }
                if imported > 0 {
                    if let Err(e) = rhythms.save(&state.rhythm_path) {
                        logger.input(&format!("RHYTHM SAVE ERROR: {}", e));
                    }
                }
            }

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
        // Capture the resolved gesture, not the latch state. Storing
        // `registers` here would miss chords played live with both hands and
        // record an empty snapshot, which nothing could usefully replay.
        registers: capture_registers(&effective),
        ..ProgressionEntry::new(degree, transformation)
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
            // The play control is a button now that the space bar latches
            // registers instead: it is the only on-screen way to start playback.
            TRANSPORT_ROW_PLAYING | TRANSPORT_ROW_EXPORT | TRANSPORT_ROW_IMPORT
        ),
        Focus::Sinko => matches!(
            state.current_row(),
            // The hits row toggles a cell, so like the other buttons it takes
            // Enter even while a chord is held.
            SINKO_ROW_HITS
                | SINKO_ROW_RECORD
                | SINKO_ROW_NEW
                | SINKO_ROW_SAVE
                | SINKO_ROW_COPY
                | SINKO_ROW_PASTE
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
    // Shift selects the second action on a position, keeping
    // `KeyPosition::hotkey` a pure function of the physical key: redo rides on
    // undo, and the rhythm clipboard rides on the chord clipboard. So `q`/`j`
    // move whole entries and `Shift+Q`/`Shift+J` move rhythms.
    let hotkey = match hotkey {
        Hotkey::Undo if shift => Hotkey::Redo,
        Hotkey::CopyChord if shift => Hotkey::CopySinko,
        Hotkey::PasteChord if shift => Hotkey::PasteSinko,
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
        Hotkey::ReplaceChord => {
            if state.focus == Focus::Progression {
                replace_selected_chord(state, logger);
            } else {
                state.flash(200);
                logger.input("REPLACE outside progression -> flash");
            }
        }
        // Global on purpose: the tap key is reachable without moving either
        // hand, and the point is to tap while watching the progression.
        Hotkey::SinkoTap => sinko_tap(state, logger),
        // Also a performance control, and global for the same reason.
        Hotkey::MetronomeToggle => toggle_metronome(state, logger),
        // Fed into the same tracker the space bar used, so the single/double/
        // triple-tap behaviour is unchanged; only the key moved.
        Hotkey::TransportTap => state.taps.tap(),
        // The rhythm clipboard acts on the chord under the cursor, so it is
        // offered wherever that cursor is on screen: the Progression panel,
        // which is where chords get chosen to replicate between, and the Sinko
        // panel itself.
        Hotkey::CopySinko | Hotkey::PasteSinko => {
            if matches!(state.focus, Focus::Progression | Focus::Sinko) {
                if hotkey == Hotkey::CopySinko {
                    copy_sinko(state, logger);
                } else {
                    paste_sinko(state, logger);
                }
            } else {
                state.flash(200);
                logger.input(&format!("{:?} outside progression/sinko -> flash", hotkey));
            }
        }
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

/// Set the selected slot to the chord in the registers, keeping its rhythm.
///
/// The counterpart to `Enter`, which *inserts* a chord: this one changes the slot
/// you are looking at. The pattern and offset stay with the entry, so fixing a
/// chord never costs you its syncopation.
fn replace_selected_chord(state: &mut AppState, logger: &Logger) {
    let Some((degree, transformation)) = resolved_chord(state) else {
        state.flash(200);
        logger.input("REPLACE: nothing resolves to replace with");
        return;
    };

    // The same snapshot `Enter` takes: the resolved gesture, not the raw latches.
    let registers = capture_registers(&state.registers.resolve(&state.held));
    let row = state.progression_row;

    let changed = state
        .progression
        .lock()
        .unwrap()
        .replace_chord(row, degree, transformation, registers);

    if changed {
        state.update_progression_len();
        logger.input(&format!("PROG replace at {}", row));
    } else {
        state.flash(200);
        logger.input(&format!("PROG replace no-op at {}", row));
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
            // Handled before this point: the locks, the performance controls,
            // and the rhythm clipboard, which edits the entry rather than the
            // slot list.
            Hotkey::LockRightRegister
            | Hotkey::LockLeftRegister
            | Hotkey::LoadSelectedChord
            | Hotkey::SinkoTap
            | Hotkey::MetronomeToggle
            | Hotkey::TransportTap
            | Hotkey::ReplaceChord
            | Hotkey::CopySinko
            | Hotkey::PasteSinko => unreachable!(),
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


// -----------------------------------------------------------------------------
// Sinko: tap capture and the pattern being built
// -----------------------------------------------------------------------------

/// The progression row the Sinko panel is showing, when it holds a chord.
///
/// The panel keeps no cursor of its own: it always describes whatever the
/// Progression panel has selected, so moving that cursor re-targets it.
fn sinko_target(state: &AppState) -> Option<usize> {
    let prog = state.progression.lock().unwrap();
    match prog.slots.get(state.progression_row) {
        Some(Slot::Chord(_)) => Some(state.progression_row),
        _ => None,
    }
}

/// The rhythm the selected row owns, if it owns one.
///
/// A clone, because the panel edits a draft and writes it back; the entry is the
/// single source the scheduler reads.
fn sinko_pattern(state: &AppState) -> Option<RhythmPattern> {
    let prog = state.progression.lock().unwrap();
    match prog.slots.get(state.progression_row) {
        Some(Slot::Chord(entry)) => entry.pattern.clone(),
        _ => None,
    }
}

/// The selected row's rhythm name, for the `pattern` row.
///
/// Marked `(edited)` when it no longer matches the library pattern it is named
/// after: two chords can both start from `Quarters`, and this is what says which
/// one has been changed since.
fn sinko_pattern_label(state: &AppState) -> String {
    let Some(pattern) = sinko_pattern(state) else {
        return "(none)".to_string();
    };
    let pristine = state
        .rhythm_store
        .lock()
        .unwrap()
        .find(&pattern.name)
        .cloned();
    match pristine {
        Some(template) if template != pattern => format!("{}  (edited)", pattern.name),
        _ => pattern.name.clone(),
    }
}

/// The offset of the selected row, if it holds a chord.
fn sinko_offset(state: &AppState) -> Option<i32> {
    let prog = state.progression.lock().unwrap();
    match prog.slots.get(state.progression_row) {
        Some(Slot::Chord(entry)) => Some(entry.offset_ticks),
        _ => None,
    }
}

/// One nudge of the offset for the selected row.
///
/// The offset walks the same note ladder as the hold, so it lands on a musical
/// value, and it is deliberately *independent of the pattern*: what a 3/16
/// offset means does not change because the chord happened to be given an
/// eighth-note pattern.
fn sinko_offset_step(state: &AppState, delta: i32) -> i32 {
    let current = sinko_offset(state).unwrap_or(0);
    let capped: Vec<i32> = rhythm::signed_ladder()
        .into_iter()
        .filter(|ticks| ticks.abs() <= crate::progression::MAX_OFFSET_TICKS)
        .collect();
    rhythm::rung_step(&capped, current, delta)
}

/// Load the selected row's own rhythm into the working draft.
///
/// The grid should show what the chord actually plays, and editing works on a
/// clone that [`commit_working`] writes straight back. Idempotent, so it costs
/// nothing to call on every Tab.
///
/// The draft is reloaded when the selection moves to another row, and when the
/// row's own pattern no longer matches the draft — which is how an undo, an
/// import or a paste shows up on screen. A row with no rhythm of its own leaves
/// the blank draft alone, because there is nothing to load.
fn sync_working(state: &mut AppState) {
    let row = sinko_target(state);
    let owned = sinko_pattern(state);

    // Stale when the selection moved, or when the row's own rhythm no longer
    // matches the draft. A row that owns nothing is *not* stale on its own: a
    // blank draft is what it should have, and reloading would throw away the
    // takes being recorded into it.
    let stale = state.working_slot != row
        || owned
            .as_ref()
            .is_some_and(|pattern| *pattern != state.working);
    if !stale {
        return;
    }

    state.working = owned.unwrap_or_else(RhythmPattern::draft);
    state.working_slot = row;
    state.sinko_cell = 0;
    state.takes.clear();
    state.current_take.clear();
    state.take_started_at = None;
}

/// True when the pattern plays the cell at `cell`, in any take.
///
/// The layers are a stack, so what sounds is their union.
fn cell_is_on(state: &AppState, cell: usize) -> bool {
    state
        .working
        .layers
        .iter()
        .any(|layer| layer.hit(cell))
}

/// The cell the `hits` cursor is on, clamped to the grid.
fn cursor_cell(state: &AppState) -> usize {
    let cells = state.working.steps_per_bar();
    if cells == 0 {
        0
    } else {
        state.sinko_cell.min(cells - 1)
    }
}

/// Move the `hits` cursor one cell.
fn move_cell_cursor(state: &mut AppState, delta: i32, logger: &Logger) {
    if !working_is_assigned(state, logger, "hits") {
        return;
    }
    let cells = state.working.steps_per_bar();
    if cells == 0 {
        return;
    }
    let next = (cursor_cell(state) as i32 + delta).clamp(0, cells as i32 - 1);
    if next as usize == state.sinko_cell {
        return;
    }
    state.sinko_cell = next as usize;
    logger.input(&format!("SINKO cell {} of {}", next + 1, cells));
}

/// Turn the cell under the cursor on or off.
///
/// Off clears the cell in *every* take, because a hit left in a lower layer
/// would still sound and the press would look like it did nothing. On puts the
/// hit in the newest take, which is the one playing at full level.
fn toggle_cell(state: &mut AppState, logger: &Logger) {
    if !working_is_assigned(state, logger, "hits") {
        return;
    }
    let cells = state.working.steps_per_bar();
    if cells == 0 {
        return;
    }
    let cell = cursor_cell(state);

    if cell_is_on(state, cell) {
        for layer in &mut state.working.layers {
            if cell < layer.len() {
                layer.steps[cell] = false;
            }
        }
        logger.input(&format!("SINKO cell {} off", cell + 1));
    } else {
        let enabled = match state.working.layers.first_mut() {
            Some(top) if cell < top.len() => {
                top.steps[cell] = true;
                true
            }
            _ => false,
        };
        if !enabled {
            state.flash(200);
            logger.input("SINKO cell: the pattern has no take to put it in");
            return;
        }
        logger.input(&format!("SINKO cell {} on", cell + 1));
    }

    commit_working(state, logger);
}

/// What the `hits` row shows: where the cursor is, and whether the pattern
/// plays that cell.
fn hits_row(state: &AppState) -> String {
    let cells = state.working.steps_per_bar();
    if cells == 0 {
        return "—".to_string();
    }
    let cell = cursor_cell(state);
    format!(
        "{:<10}{} of {}",
        if cell_is_on(state, cell) { "on" } else { "off" },
        cell + 1,
        cells
    )
}

/// True when the selected row owns a rhythm, so the shape rows have something to
/// edit.
///
/// A row with no rhythm plays the whole-bar default: nothing about it is a
/// pattern, so editing one would look like it worked and sound like nothing —
/// exactly the confusion the write-through exists to remove. The shape rows
/// refuse instead and say why.
fn working_is_assigned(state: &mut AppState, logger: &Logger, what: &str) -> bool {
    if sinko_pattern(state).is_some() {
        return true;
    }
    state.flash(200);
    logger.input(&format!(
        "SINKO {}: no pattern on this chord (press [New Pattern] or pick one)",
        what
    ));
    false
}

/// Write the working draft into the entry the selected row holds.
///
/// This is the whole write-through, and it is now one step: the entry owns its
/// rhythm, the scheduler reads the entry, so a hold or a mute lands where it is
/// heard without a library write or a disk write. It is one undoable edit, like
/// every other change to the progression.
fn commit_working(state: &mut AppState, logger: &Logger) {
    let Some(row) = sinko_target(state) else {
        return;
    };
    // A rhythm that arrived by recording has no name yet — the draft is a blank
    // page until something is played into it. Name it here, against the palette
    // so it cannot shadow a library pattern and read as `(edited)` by accident.
    if state.working.name.trim().is_empty() {
        state.working.name = state.rhythm_store.lock().unwrap().unique_name("Pattern");
    }
    let changed = state
        .progression
        .lock()
        .unwrap()
        .assign_pattern(row, Some(state.working.clone()));
    if changed {
        logger.input(&format!("SINKO row {} rhythm updated", row));
    }
}

/// Rebuild the working pattern from the takes recorded so far.
///
/// The newest performance goes on top at full level; every layer already there
/// is decayed one step first. That is the overlapping-takes sound: each new pass
/// is loudest and the older ones sit underneath, quieter every measure.
///
/// When `sinko_smooth` is above 1 the top layer is instead the *average* of that
/// many recent takes, which is how a figure tapped several times converges on
/// one clean line rather than stacking up.
fn rebuild_working(state: &mut AppState) {
    let resolution = state.working.steps_per_bar();
    let smooth = state.sinko_smooth.max(1);

    let mut layers: Vec<RhythmLayer> = Vec::new();
    if !state.takes.is_empty() {
        let head_positions = rhythm::average_takes(&state.takes, smooth);
        layers.push(RhythmLayer::new(
            1.0,
            rhythm::quantize(&head_positions, resolution),
        ));

        // Every take outside the averaging window sits underneath as a layer of
        // its own, most recent first. Each position further down the stack is
        // one more step of decay, so the oldest take is the quietest.
        let settled = state.takes.len().saturating_sub(smooth);
        let mut bed: Vec<RhythmLayer> = state.takes[..settled]
            .iter()
            .rev()
            .take(SINKO_LAYER_ROWS - 1)
            .map(|take| RhythmLayer::new(1.0, rhythm::quantize(take, resolution)))
            .collect();
        for i in 0..bed.len() {
            rhythm::decay_layer_gains(&mut bed[i..], rhythm::TAKE_DECAY);
        }
        layers.extend(bed);
    }

    state.working.layers = if layers.is_empty() {
        // A pattern that has never sounded is a blank page, not a layer.
        vec![RhythmLayer::new(1.0, vec![false; resolution])]
    } else {
        layers
    };
}

/// The latest grid of the take in progress, snapped to the working resolution.
fn sinko_live_steps(state: &AppState) -> Vec<bool> {
    let resolution = state.working.steps_per_bar();
    let mut taps = state.current_take.clone();
    if taps.is_empty() {
        return vec![false; resolution];
    }
    taps.sort_unstable();
    rhythm::quantize(&taps, resolution)
}

/// Close the take in progress and fold it into the pattern.
fn close_take(state: &mut AppState, logger: &Logger) {
    let take = std::mem::take(&mut state.current_take);
    state.take_started_at = None;
    if take.is_empty() {
        return;
    }
    state.takes.push(take);
    if state.takes.len() > rhythm::MAX_TAKES {
        state.takes.remove(0);
    }
    rebuild_working(state);
    commit_working(state, logger);
    logger.input(&format!("SINKO take {} recorded", state.takes.len()));
}

/// One tap of the sinko key.
///
/// The tap is timestamped against the bar the scheduler published, so its
/// position *is* the performance. Taps closer together than the debounce are
/// dropped: a held key repeats, and a terminal without the keyboard-enhancement
/// flags reports that repeat as an ordinary press.
fn sinko_tap(state: &mut AppState, logger: &Logger) {
    if !state.recording {
        state.flash(200);
        logger.input("SINKO tap ignored: not recording");
        return;
    }
    let Some((bar_start, _)) = state.transport.bar_started() else {
        state.flash(200);
        logger.input("SINKO tap ignored: no bar clock");
        return;
    };

    let now = Instant::now();
    if let Some(last) = state.last_tap_at {
        if now.duration_since(last).as_millis() < TAP_DEBOUNCE_MS {
            return;
        }
    }
    state.last_tap_at = Some(now);

    // A new bar began since this take started, so the take is over.
    if let Some(started) = state.take_started_at {
        if bar_start > started {
            close_take(state, logger);
        }
    }
    state.take_started_at = Some(bar_start);

    let phase = bar_phase_ticks(now, bar_start, state.transport.bar_duration());
    state.current_take.push(phase);
    logger.input(&format!("SINKO tap at {} ticks", phase));
}

/// The space bar: latch both hands, unless a prompt is open.
///
/// Text prompts swallow the key first, but a *pass-through* prompt (the "add a
/// rest?" question, a delete confirmation) falls through to here — and latching
/// registers behind a question would change the performance state invisibly.
fn space_pressed(state: &mut AppState, logger: &Logger) {
    if state.modal.is_some() {
        return;
    }
    lock_both_registers(state, logger);
}

/// True when Esc means the stop/quit gesture rather than cancelling something.
///
/// A prompt never reaches here (it handles Esc and continues), and an edit is
/// dispatched above, so this is the belt to that pair of braces — and the one
/// place the rule is written down for a test.
fn esc_is_free(state: &AppState) -> bool {
    state.modal.is_none() && matches!(state.edit, Edit::None)
}

/// What a free Esc does: stop now, flash, and quit on a quick second press.
///
/// Returns true when the program should exit.
fn esc_should_quit(state: &mut AppState, logger: &Logger) -> bool {
    let now = Instant::now();
    if let Some(last) = state.last_esc {
        if now.duration_since(last) < ESC_QUIT_WINDOW {
            logger.input("ESC quit");
            return true;
        }
    }

    state.last_esc = Some(now);
    panic_stop(state, logger);
    false
}

/// Stop the transport and silence it, without moving the bar.
fn panic_stop(state: &mut AppState, logger: &Logger) {
    state.transport.playing.store(false, Ordering::Relaxed);
    // Pausing would let the current bar run to its end, so a chord could ring
    // for up to a bar. `stop_now` abandons the bar instead and releases every
    // stab group on the way through the scheduler.
    state.transport.stop_now.store(true, Ordering::Relaxed);
    state.stop_flash_until = Some(Instant::now() + STOP_FLASH);
    logger.input("TRANSPORT stop (esc)");
}

/// Latch both hands at once.
///
/// One press instead of the two register-lock keys, so a two-handed chord can be
/// captured and both hands freed. Live input still wins per side afterwards,
/// exactly as the per-side locks behave.
fn lock_both_registers(state: &mut AppState, logger: &Logger) {
    state.registers.lock_both(&state.held);
    state.update_live_chord();
    logger.input("LOCK BOTH registers");
}

/// Push the effective metronome state to the transport.
///
/// The click runs when the user asked for it *or* a take is armed, because
/// recording needs the beat. Keeping the two apart is what stops arming a take
/// from forgetting a metronome you had already switched on.
fn sync_metronome(state: &AppState) {
    state
        .transport
        .metronome
        .store(state.metronome_on || state.recording, Ordering::Relaxed);
}

/// Start or pause the transport.
///
/// Its own function because three things reach it: the tap key's tracker,
/// `Enter` on the transport's `playing` row, and `←`/`→` on that row.
fn toggle_playback(state: &mut AppState, logger: &Logger) {
    let playing = state.transport.playing.load(Ordering::Relaxed);
    state.transport.playing.store(!playing, Ordering::Relaxed);
    logger.input(if playing {
        "TRANSPORT pause"
    } else {
        "TRANSPORT play"
    });
}

/// Flip the metronome switch.
fn toggle_metronome(state: &mut AppState, logger: &Logger) {
    state.metronome_on = !state.metronome_on;
    sync_metronome(state);
    logger.input(&format!(
        "METRONOME {}",
        if state.metronome_on { "on" } else { "off" }
    ));
}

/// Arm or disarm tap recording.
fn toggle_recording(state: &mut AppState, logger: &Logger) {
    if state.recording {
        state.recording = false;
        close_take(state, logger);
        sync_metronome(state);
        logger.input("SINKO record off");
        return;
    }
    // The click is the only way to feel the beat, and it needs the bar clock,
    // which the scheduler runs whether or not the transport is playing.
    state.recording = true;
    state.last_tap_at = None;
    state.take_started_at = None;
    state.current_take.clear();
    sync_metronome(state);
    logger.input("SINKO record on");
}

/// Point the selected row at the next pattern in the library.
///
/// The list is "none" first and then every saved pattern, so clearing an
/// assignment is reachable without a row of its own.
fn cycle_assigned_pattern(state: &mut AppState, delta: i32, logger: &Logger) {
    if sinko_target(state).is_none() {
        state.flash(200);
        logger.input("SINKO assign: no chord selected");
        return;
    }
    let names: Vec<String> = {
        let store = state.rhythm_store.lock().unwrap();
        if store.patterns.is_empty() {
            drop(store);
            state.flash(200);
            logger.input("SINKO assign: the library is empty");
            return;
        }
        store.patterns.iter().map(|p| p.name.clone()).collect()
    };

    let current = sinko_pattern(state).map(|p| p.name);
    let index = match &current {
        None => 0,
        Some(name) => names
            .iter()
            .position(|n| n == name)
            .map(|i| i + 1)
            .unwrap_or(0),
    };
    let options = names.len() as i32 + 1; // "none", then every pattern
    let next = (index as i32 + delta).rem_euclid(options) as usize;
    // A *copy* of the library pattern, not a reference to it: this is what makes
    // the hits per chord. Two rows may both start from `Quarters` and then drift
    // apart without either hearing the other.
    let chosen = if next == 0 {
        None
    } else {
        state
            .rhythm_store
            .lock()
            .unwrap()
            .find(&names[next - 1])
            .cloned()
    };
    let label = chosen.as_ref().map(|p| p.name.clone());

    let changed = state
        .progression
        .lock()
        .unwrap()
        .assign_pattern(state.progression_row, chosen);
    if changed {
        sync_working(state);
        logger.input(&format!(
            "SINKO row {} assigned {:?} of {} patterns",
            state.progression_row,
            label,
            names.len()
        ));
    } else {
        state.flash(200);
    }
}

/// Move the selected row off its downbeat by one grid step.
fn nudge_offset(state: &mut AppState, delta: i32, logger: &Logger) {
    if sinko_offset(state).is_none() {
        state.flash(200);
        logger.input("SINKO offset: no chord selected");
        return;
    }
    let target = sinko_offset_step(state, delta);
    let changed = state
        .progression
        .lock()
        .unwrap()
        .set_offset(state.progression_row, target);
    if changed {
        logger.input(&format!("SINKO row {} offset -> {}", state.progression_row, target));
    } else {
        // Either the ladder end held it or nothing changed; both are worth
        // showing, because a press that does nothing should say so.
        state.flash(200);
    }
}

/// Re-quantize the working pattern onto the next grid resolution.
fn cycle_resolution(state: &mut AppState, delta: i32, logger: &Logger) {
    if !working_is_assigned(state, logger, "quant") {
        return;
    }
    let options = rhythm::VALID_STEPS;
    let current = state.working.steps_per_bar();
    let index = options.iter().position(|n| *n == current).unwrap_or(1) as i32;
    let next = options[((index + delta).rem_euclid(options.len() as i32)) as usize];
    if next == current {
        return;
    }

    let hold = state.working.hold;
    let mute = state.working.mute_ticks;
    let name = state.working.name.clone();
    match RhythmPattern::blank(name, next) {
        Ok(mut pattern) => {
            // The hold is ticks, so it survives a grid change — the whole point
            // of measuring it in ticks rather than in cells.
            pattern.hold = hold;
            pattern.mute_ticks = mute;
            state.working = pattern;
            rebuild_working(state);
            commit_working(state, logger);
            logger.input(&format!("SINKO quant -> {} steps per bar", next));
        }
        Err(err) => logger.input(&format!("SINKO quant refused: {}", err)),
    }
}

/// Start a new pattern, named and already playing on the selected row.
///
/// It is not a draft: it lands in the library and is assigned straight away, so
/// every edit from here — a resolution, a hold, a muted tail, a tapped take —
/// is heard on the next bar rather than waiting for a save.
fn new_pattern(state: &mut AppState, logger: &Logger) {
    let Some(row) = sinko_target(state) else {
        state.rhythm_status = Some(ActionStatus::refused("select a chord first".to_string()));
        state.flash(300);
        logger.input("SINKO new pattern refused: no chord selected");
        return;
    };

    // Named after nothing in particular, but not colliding with the library:
    // the row reads better than "(none)" and a later save prefills the name.
    let name = state
        .rhythm_store
        .lock()
        .unwrap()
        .unique_name("New Pattern");
    let mut pattern = RhythmPattern::draft();
    pattern.name = name.clone();

    // The entry owns it; the library is untouched until it is saved there.
    state
        .progression
        .lock()
        .unwrap()
        .assign_pattern(row, Some(pattern.clone()));
    state.working = pattern;
    state.working_slot = Some(row);
    state.sinko_cell = 0;
    state.takes.clear();
    state.current_take.clear();
    state.take_started_at = None;
    state.rhythm_status = Some(ActionStatus::ok(name.clone()));
    logger.input(&format!("SINKO new pattern '{}' on row {}", name, row));
}

/// Save the working rhythm into the library under a new name.
///
/// The library is the palette, so this is how a rhythm you built on one chord
/// becomes something other chords can start from. The *entry's* copy is renamed
/// to match, so the row and the library entry it came from read the same and the
/// `(edited)` marker clears.
fn save_working_pattern(state: &mut AppState, typed: &str, logger: &Logger) {
    if sinko_target(state).is_none() {
        state.rhythm_status = Some(ActionStatus::refused("select a chord first".to_string()));
        state.flash(300);
        logger.input("SINKO save refused: no chord selected");
        return;
    }
    if !state.working.any_hit() {
        state.rhythm_status = Some(ActionStatus::refused("nothing tapped yet".to_string()));
        state.flash(300);
        logger.input("SINKO save refused: the pattern is silent");
        return;
    }

    let base = if typed.is_empty() {
        state.working.name.clone()
    } else {
        typed.to_string()
    };
    let name = state.rhythm_store.lock().unwrap().unique_name(&base);

    let mut pattern = state.working.clone();
    pattern.name = name.clone();

    let saved = {
        let mut store = state.rhythm_store.lock().unwrap();
        store.add(pattern.clone());
        store.save(&state.rhythm_path)
    };
    if let Err(err) = saved {
        state.rhythm_status = Some(ActionStatus::failed(err.to_string()));
        state.flash(300);
        logger.input(&format!("RHYTHM SAVE ERROR: {}", err));
        return;
    }

    // The entry keeps a copy of its own, now named after the library entry, so
    // the row reads `Cut` rather than `Cut  (edited)`.
    state
        .progression
        .lock()
        .unwrap()
        .assign_pattern(state.progression_row, Some(pattern.clone()));
    state.working = pattern;
    state.rhythm_status = Some(ActionStatus::ok(name.clone()));
    state.flash(600);
    logger.input(&format!(
        "SINKO saved a copy '{}' on row {}",
        name, state.progression_row
    ));
}

/// The Sinko panel's Enter action for a row.
fn sinko_action(state: &mut AppState, row: usize, logger: &Logger) {
    match row {
        SINKO_ROW_HITS => toggle_cell(state, logger),
        SINKO_ROW_RECORD => toggle_recording(state, logger),
        SINKO_ROW_NEW => new_pattern(state, logger),
        SINKO_ROW_SAVE => {
            let prefill = state.working.name.clone();
            state.modal = Some(Modal::RhythmNameInput { buffer: prefill });
            logger.input("MODAL pattern name");
        }
        SINKO_ROW_COPY => copy_sinko(state, logger),
        SINKO_ROW_PASTE => paste_sinko(state, logger),
        _ => {}
    }
}

/// Copy the selected chord's rhythm, so it can be pasted onto others.
///
/// The rhythm only — the grid, the hits, the hold and the muted tail. The offset
/// stays with each chord, because where a chord sits in the bar is placement
/// rather than rhythm, and copying it would move chords nobody asked to move.
fn copy_sinko(state: &mut AppState, logger: &Logger) {
    match sinko_pattern(state) {
        Some(pattern) => {
            let name = pattern.name.clone();
            state.sinko_clipboard = Some(pattern);
            state.rhythm_status = Some(ActionStatus::ok(format!("copied {}", name)));
            state.flash(400);
            logger.input(&format!("SINKO copied rhythm '{}'", name));
        }
        None => {
            state.rhythm_status = Some(ActionStatus::refused("nothing to copy".to_string()));
            state.flash(300);
            logger.input("SINKO copy refused: this chord has no pattern");
        }
    }
}

/// Give the selected chord a copy of the clipboard's rhythm.
///
/// One undoable edit, and safe to repeat: each paste is an independent copy, so
/// editing the chord afterwards cannot reach any of the others.
fn paste_sinko(state: &mut AppState, logger: &Logger) {
    let Some(pattern) = state.sinko_clipboard.clone() else {
        state.rhythm_status = Some(ActionStatus::refused("nothing copied yet".to_string()));
        state.flash(300);
        logger.input("SINKO paste refused: the clipboard is empty");
        return;
    };
    let Some(row) = sinko_target(state) else {
        state.rhythm_status = Some(ActionStatus::refused("select a chord first".to_string()));
        state.flash(300);
        logger.input("SINKO paste refused: no chord selected");
        return;
    };

    let name = pattern.name.clone();
    let changed = state
        .progression
        .lock()
        .unwrap()
        .assign_pattern(row, Some(pattern.clone()));
    if !changed {
        state.flash(200);
        return;
    }
    // Show it straight away rather than waiting for the next Tab.
    state.working = pattern;
    state.working_slot = Some(row);
    state.sinko_cell = 0;
    state.takes.clear();
    state.current_take.clear();
    state.take_started_at = None;
    state.rhythm_status = Some(ActionStatus::ok(format!("pasted {}", name)));
    state.flash(600);
    logger.input(&format!("SINKO pasted rhythm '{}' onto row {}", name, row));
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

fn render<W: io::Write>(
    stdout: &mut W,
    params: &SynthParams,
    state: &AppState,
    screen: Screen,
) -> io::Result<()> {
    // Everything below draws to this, so nothing can wrap whatever it does.
    let stdout = &mut WidthLimited::new(stdout, screen.width);
    execute!(stdout, Clear(ClearType::All), MoveTo(0, 0))?;

    // A frame drawn into a window this small would wrap into a mess that hides
    // the very keys it is trying to show, so say what is missing instead.
    if !screen.fits() {
        execute!(stdout, Print(too_small_banner(screen)))?;
        stdout.finish()?;
        return Ok(());
    }

    // The stop flash is a full-width red bar over the title, rather than a
    // screen-wide background: the panels below set and reset their own colours,
    // so a background set here would be punched out by the first of them.
    let key = state.transport.key();
    if state.stop_flash_active() {
        execute!(stdout, Print(stop_banner(screen.width)), ResetColor)?;
    } else {
        // The title line carries the two facts that hold for the whole screen —
        // the layout and the track key — rather than either having a line of its
        // own. The key belongs up here because the readout below is a *degree*,
        // and the Transport panel that also shows it may be collapsed.
        execute!(
            stdout,
            Print(format!(
                "{}\r\n",
                centre_line(
                    &format!(
                        "Chord Tool  ·  {}  ·  {}",
                        ACTIVE_LAYOUT.name(),
                        key.name()
                    ),
                    screen.width
                )
            ))
        )?;
    }

    // Three rows: what each hand is holding, what each register means, and what
    // that adds up to. Everything else the screen used to say was a reminder of
    // a key the player already knows, and each reminder cost a row.
    //
    // The hands are drawn on one line, each register under its own hand, and the
    // separator column is shared so the three rows read as one table.
    // The keyboard and its registers are one block: the registers belong *under*
    // the hands that fill them, so both rows start in the same column, and the
    // block's width is a constant so neither of them moves as chords change.
    let keys = panel_block(|out| render_key_row(out, state));
    let keys = keys.trim_end_matches(['\r', '\n']).to_string();
    let registers = register_row(state, &key);
    let offset = " ".repeat(screen.width.saturating_sub(HEADER_BLOCK_WIDTH) / 2);
    execute!(stdout, Print(format!("{}{}\r\n", offset, keys)))?;
    execute!(stdout, Print(format!("{}{}\r\n", offset, registers)))?;

    // One row of data: both registers side by side, then what they add up to.
    // Fixed-width cells keep the columns still as keys come and go, which is what
    // makes a block this dense readable at a glance.
    let readout = match resolved_chord(state) {
        Some((d, transformation)) => {
            let (label, degree, notes) = chord_readout(d, transformation, &key);
            let note_str: Vec<String> = notes.iter().map(|n| note_name(*n)).collect();
            // The chord sounding right now is the one value in this block that
            // changes as you play, so it is the one that is coloured.
            styled(
                &format!("{}  ({})   {}", label, degree, note_str.join(" ")),
                LIVE_FG,
                true,
            )
        }
        None => "—".to_string(),
    };
    // What the registers add up to, at the left margin: it is the heading of the
    // chord list below it, which is the panel that plays it.
    execute!(
        stdout,
        Print(format!("{}{}\r\n", KEY_ROW_INDENT, readout))
    )?;

    // Drawn in the order `Tab` walks, so focus moves *down* the screen — with the
    // first two side by side, because the chord list is what the transport plays
    // and both are short: Transport in the left column, Progression in the right.
    let muted = params.mute_progression.get() > 0.5;
    // The chord list on the left, the transport on the right: the list is what
    // you read and edit, the transport is a row of settings at the far edge, and
    // the panels below take the full width. Each block has its header centred
    // over its own body on the way out.
    draw_columns(
        stdout,
        &centre_block_header(&panel_block(|out| render_progression_panel(out, state))),
        &centre_block_header(&panel_block(|out| render_transport_panel(out, muted, state))),
        screen,
    )?;
    execute!(
        stdout,
        Print(centre_block_header(&panel_block(|out| render_sinko_panel(
            out,
            state,
            screen.grid_budget()
        ))))
    )?;
    execute!(
        stdout,
        Print(centre_block_header(&panel_block(|out| {
            render_synth_panel(out, params, state)
        })))
    )?;

    if let Some(ref modal) = state.modal {
        // One blank line, because a prompt is not another panel.
        execute!(stdout, Print("\r\n"))?;
        render_modal(stdout, modal, screen.width)?;
    }

    // No key reminders down here: every line of them was a row the panels could
    // have had, and the README is the reference. The status lines inside each
    // panel say what just happened, which is the part worth screen space.
    stdout.finish()?;
    Ok(())
}

/// The red stop banner: a full terminal width, so it reads as the screen
/// flashing rather than as a message appearing.
fn stop_banner(width: usize) -> String {
    let mut text = String::from("  STOPPED  —  esc again to quit");
    let visible = text.chars().count();
    if visible < width {
        text.push_str(&" ".repeat(width - visible));
    }
    format!(
        "{}{}{}\r\n",
        crossterm::style::SetBackgroundColor(Color::Red),
        crossterm::style::SetForegroundColor(Color::White),
        text
    )
}

fn render_synth_panel<W: io::Write>(
    stdout: &mut W,
    params: &SynthParams,
    state: &AppState,
) -> io::Result<()> {
    let focused = state.focus == Focus::Synth;
    let presets_focused = state.focus == Focus::SynthPresets;

    let header = if focused && matches!(state.edit, Edit::SynthCell { .. }) {
        " Synth  [editing] "
    } else if presets_focused {
        " Synth [Presets] "
    } else {
        " Synth "
    };
    draw_panel_header(stdout, header, focused || presets_focused)?;

    if focused {
        render_synth_table(stdout, params, state)?;
    } else if presets_focused {
        render_presets_body(stdout, state)?;
    } else {
        render_synth_summary(stdout, params, state)?;
    }
    Ok(())
}

/// Widths for the Synth table.
///
/// Every field is padded to a fixed width and clipped, so no value — a long
/// waveform name, say — can reflow the grid and shove the panels below it down
/// the screen. The same reasoning as `single_line_status` on the Transport.
const SYNTH_LABEL_WIDTH: usize = 14;
const SYNTH_VALUE_WIDTH: usize = 11;

fn clip_to(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
}

/// One table field, padded to a fixed width.
///
/// The selected cell is bracketed rather than only coloured: colour is
/// invisible to a test after ANSI stripping, and a bracketed value also tells
/// the player which column `Enter` will edit.
fn synth_field(value: &str, width: usize, selected: bool) -> String {
    if selected {
        let inner = clip_to(value, width.saturating_sub(2));
        let pad = width.saturating_sub(inner.chars().count() + 2);
        format!("[{}]{}", inner, " ".repeat(pad))
    } else {
        format!("{:<width$}", clip_to(value, width), width = width)
    }
}

/// The whole sound design on one screen: three channel columns plus the master
/// block, so a channel's volume and its cutoff are visible together.
fn render_synth_table<W: io::Write>(
    stdout: &mut W,
    params: &SynthParams,
    state: &AppState,
) -> io::Result<()> {
    let focused = state.focus == Focus::Synth;
    let selected = if focused {
        Some(state.synth_cell())
    } else {
        None
    };

    let mut header = format!("    {:<SYNTH_LABEL_WIDTH$}", "param");
    for name in CHANNEL_COLUMN_LABELS {
        header.push_str(&format!("{:<SYNTH_VALUE_WIDTH$}", name));
    }
    execute!(stdout, Print(format!("{}\r\n", header.trim_end())))?;

    for (row, param) in CHANNEL_PARAMS.iter().enumerate() {
        let marker = if focused && state.synth_row == row {
            "▸"
        } else {
            " "
        };
        let mut line = format!("  {} {:<SYNTH_LABEL_WIDTH$}", marker, param.label());
        for col in 0..CHANNEL_COUNT {
            let cell = SynthCell::Channel { row, col };
            line.push_str(&synth_field(
                &param.display(channel_at(params, col)),
                SYNTH_VALUE_WIDTH,
                selected == Some(cell),
            ));
        }
        draw_row(stdout, line.trim_end(), focused && state.synth_row == row)?;
    }

    for (pair, (a, b)) in MASTER_ROWS.iter().enumerate() {
        let row = CHANNEL_PARAMS.len() + pair;
        let marker = if focused && state.synth_row == row {
            "▸"
        } else {
            " "
        };
        let mut line = format!("  {} {:<SYNTH_LABEL_WIDTH$}", marker, a.label());
        line.push_str(&synth_field(
            &a.display(params, &state.transport),
            SYNTH_VALUE_WIDTH,
            selected == Some(SynthCell::Master { row: pair, col: 0 }),
        ));
        line.push_str(&format!("{:<SYNTH_LABEL_WIDTH$}", b.label()));
        line.push_str(&synth_field(
            &b.display(params, &state.transport),
            SYNTH_VALUE_WIDTH,
            selected == Some(SynthCell::Master { row: pair, col: 1 }),
        ));
        draw_row(stdout, line.trim_end(), focused && state.synth_row == row)?;
    }
    Ok(())
}

/// One-line stand-in while the Synth is not focused, so the default view stays
/// short enough for a 27-row terminal.
fn render_synth_summary<W: io::Write>(
    stdout: &mut W,
    params: &SynthParams,
    state: &AppState,
) -> io::Result<()> {
    let mut line = String::new();
    for (col, name) in CHANNEL_COLUMN_LABELS.iter().enumerate() {
        let ch = channel_at(params, col);
        line.push_str(&format!(
            "{:<5} {:<9}",
            name,
            Waveform::from_f32(ch.waveform.get()).name()
        ));
    }
    line.push_str(&format!(
        "| master {:.0}  reverb {:.0}%  {}",
        params.master_volume.get(),
        params.reverb_mix.get() * 100.0,
        format_note_length(state.transport.note_length())
    ));
    execute!(stdout, Print(format!("  {}\r\n", line)))?;
    Ok(())
}

fn render_presets_body<W: io::Write>(stdout: &mut W, state: &AppState) -> io::Result<()> {
    let focused = state.focus == Focus::SynthPresets;
    for (i, patch) in state.patch_store.patches.iter().enumerate() {
        let marker = if focused && i == state.preset_row {
            "▸"
        } else {
            " "
        };
        draw_row(
            stdout,
            &format!("  {} {}", marker, patch.name),
            focused && i == state.preset_row,
        )?;
    }
    let save_row = state.patch_store.patches.len();
    let marker = if focused && state.preset_row == save_row {
        "▸"
    } else {
        " "
    };
    draw_row(
        stdout,
        &format!("  {} [Save As...]", marker),
        focused && state.preset_row == save_row,
    )?;
    Ok(())
}


/// The Sinko panel: the selected entry's rhythm settings and the grid being
/// tapped.
///
/// It keeps no cursor of its own — `chord`, `pattern` and `offset` all describe
/// whatever the Progression panel has selected — so moving that cursor
/// re-targets this panel. Unfocused it collapses to one line, which is part of
/// what keeps the default view inside a 27-row terminal.
fn render_sinko_panel<W: io::Write>(
    stdout: &mut W,
    state: &AppState,
    budget: usize,
) -> io::Result<()> {
    let focused = state.focus == Focus::Sinko;
    if !focused {
        // One line, header and summary together: unfocused panels earn their
        // rows or they do not get them.
        execute!(
            stdout,
            Print(format!("── Sinko ── {}\r\n", sinko_summary(state)))
        )?;
        return Ok(());
    }

    let header = if state.recording {
        " Sinko  [recording] "
    } else {
        " Sinko "
    };
    draw_panel_header(stdout, header, true)?;

    let row = |at: usize| if state.sinko_row == at { "▸" } else { " " };

    draw_row(
        stdout,
        &format!(
            "  {} {:<9}{}",
            row(SINKO_ROW_CHORD),
            "chord",
            sinko_chord_label(state)
        ),
        state.sinko_row == SINKO_ROW_CHORD,
    )?;

    draw_row(
        stdout,
        &format!(
            "  {} {:<9}{}",
            row(SINKO_ROW_PATTERN),
            "pattern",
            sinko_pattern_label(state)
        ),
        state.sinko_row == SINKO_ROW_PATTERN,
    )?;

    let offset = sinko_offset(state).unwrap_or(0);
    draw_row(
        stdout,
        &format!(
            "  {} {:<9}{}",
            row(SINKO_ROW_OFFSET),
            "offset",
            format_offset_with_ticks(offset)
        ),
        state.sinko_row == SINKO_ROW_OFFSET,
    )?;

    let steps = state.working.steps_per_bar();
    draw_row(
        stdout,
        &format!(
            "  {} {:<9}{}  —  {} steps per bar, {} hit{}",
            row(SINKO_ROW_QUANT),
            "quant",
            format_resolution(steps),
            steps,
            state.working.hit_count(),
            if state.working.hit_count() == 1 { "" } else { "s" }
        ),
        state.sinko_row == SINKO_ROW_QUANT,
    )?;

    draw_row(
        stdout,
        &format!(
            "  {} {:<9}{}",
            row(SINKO_ROW_HITS),
            "hits",
            hits_row(state)
        ),
        state.sinko_row == SINKO_ROW_HITS,
    )?;

    draw_row(
        stdout,
        &format!(
            "  {} {:<9}{}",
            row(SINKO_ROW_HOLD),
            "hold",
            format_ticks(state.working.hold_ticks())
        ),
        state.sinko_row == SINKO_ROW_HOLD,
    )?;

    draw_row(
        stdout,
        &format!(
            "  {} {:<9}{}",
            row(SINKO_ROW_MUTE),
            "mute",
            format_ticks(state.working.mute_ticks as u64)
        ),
        state.sinko_row == SINKO_ROW_MUTE,
    )?;

    let smooth = state.sinko_smooth;
    draw_row(
        stdout,
        &format!(
            "  {} {:<9}{}",
            row(SINKO_ROW_SMOOTH),
            "smooth",
            if smooth == 1 {
                "1 take per layer".to_string()
            } else {
                format!("{} takes averaged", smooth)
            }
        ),
        state.sinko_row == SINKO_ROW_SMOOTH,
    )?;

    draw_row(
        stdout,
        &format!(
            "  {} {:<9}{}",
            row(SINKO_ROW_RECORD),
            "record",
            if state.recording {
                "[● recording]"
            } else {
                "[record]"
            }
        ),
        state.sinko_row == SINKO_ROW_RECORD,
    )?;

    // One grid line per layer, so the decaying stack is visible as it builds,
    // and one for the take being tapped right now.
    let playhead = sinko_playhead(state, steps);
    let live = sinko_live_steps(state);
    let blank = vec![false; steps];
    // The cursor is drawn on the newest take's line, because that is the take a
    // new hit goes into.
    let cursor = if state.focus == Focus::Sinko && state.sinko_row == SINKO_ROW_HITS {
        Some(cursor_cell(state))
    } else {
        None
    };
    for i in 0..SINKO_LAYER_ROWS {
        let at = SINKO_ROW_RECORD + 1 + i;
        let line_cursor = if i == 0 { cursor } else { None };
        match state.working.layers.get(i) {
            Some(layer) => execute!(
                stdout,
                Print(format!(
                    "  {} {:>5.2}  {}\r\n",
                    row(at),
                    layer.gain,
                    sinko_grid_line(&layer.steps, playhead, line_cursor, budget)
                ))
            )?,
            None if i == state.working.layers.len() => execute!(
                stdout,
                Print(format!(
                    "  {} {:>5}  {}\r\n",
                    row(at),
                    "live",
                    sinko_grid_line(&live, playhead, line_cursor, budget)
                ))
            )?,
            None => execute!(
                stdout,
                Print(format!(
                    "  {} {:>5}  {}\r\n",
                    row(at),
                    "·",
                    sinko_grid_line(&blank, playhead, line_cursor, budget)
                ))
            )?,
        }
    }

    draw_row(
        stdout,
        &format!("  {} [New Pattern]", row(SINKO_ROW_NEW)),
        state.sinko_row == SINKO_ROW_NEW,
    )?;

    let status = state
        .rhythm_status
        .as_ref()
        .and_then(|s| s.appearance_at(Instant::now()));
    let save = format!(
        "  {} {:<22}",
        row(SINKO_ROW_SAVE),
        "[Save Pattern As...]"
    );
    match status {
        Some((color, text)) => draw_row_with(
            stdout,
            &save,
            state.sinko_row == SINKO_ROW_SAVE,
            Some((color, single_line_status(text))),
        )?,
        None => draw_row(stdout, &save, state.sinko_row == SINKO_ROW_SAVE)?,
    }

    // Copy and paste are a pair: copy one chord's rhythm, move to another and
    // paste it there. The paste row names what is waiting, so an empty clipboard
    // is visible rather than a press that does nothing.
    draw_row(
        stdout,
        &format!("  {} [Copy Sinko]", row(SINKO_ROW_COPY)),
        state.sinko_row == SINKO_ROW_COPY,
    )?;
    let paste = match &state.sinko_clipboard {
        Some(pattern) => format!("[Paste Sinko: {}]", pattern.name),
        None => "[Paste Sinko]".to_string(),
    };
    draw_row(
        stdout,
        &format!("  {} {}", row(SINKO_ROW_PASTE), paste),
        state.sinko_row == SINKO_ROW_PASTE,
    )?;

    Ok(())
}

/// The home row: what each hand is holding, and which of those keys is a hotkey.
fn render_key_row<W: io::Write>(stdout: &mut W, state: &AppState) -> io::Result<()> {
    execute!(stdout, Print(KEY_ROW_INDENT))?;
    for pos in LEFT_HAND_POSITIONS {
        draw_key(stdout, pos, state.held.contains(&pos))?;
    }
    execute!(stdout, Print(KEY_COLUMN_GAP))?;
    for pos in RIGHT_HAND_POSITIONS {
        draw_key(stdout, pos, state.held.contains(&pos))?;
    }
    Ok(())
}

/// The selected entry, as the panel's first row.
fn sinko_chord_label(state: &AppState) -> String {
    let key = state.transport.key();
    let prog = state.progression.lock().unwrap();
    match prog.slots.get(state.progression_row) {
        Some(Slot::Chord(entry)) => format!(
            "#{}  {}",
            state.progression_row + 1,
            entry.label(&key)
        ),
        Some(Slot::Rest) => format!("#{}  —  (a rest)", state.progression_row + 1),
        None => "no chord selected".to_string(),
    }
}

/// The one line the panel shows while it is not focused.
fn sinko_summary(state: &AppState) -> String {
    let assigned = sinko_pattern_label(state);
    let offset = format_offset(sinko_offset(state).unwrap_or(0));
    format!(
        "{}   {}   {}",
        sinko_chord_label(state),
        assigned,
        offset
    )
}

fn render_progression_panel<W: io::Write>(stdout: &mut W, state: &AppState) -> io::Result<()> {
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
    let rhythm_status = state
        .rhythm_status
        .as_ref()
        .and_then(|status| status.appearance_at(Instant::now()));
    // The header line is the rule and nothing else; the clipboard status — which
    // the rhythm keys report into, because this panel is where they are pressed —
    // is a row of its own below it. A row that comes and goes costs nothing here:
    // the chord list is the shorter of the two columns, so the transport's
    // padding absorbs it.
    draw_panel_header(stdout, &format!(" Progression{} ", history), focused)?;
    if let Some((colour, text)) = rhythm_status {
        execute!(
            stdout,
            SetForegroundColor(colour),
            Print(format!("  {}\r\n", single_line_status(text))),
            ResetColor,
        )?;
    }

    let prog = state.progression.lock().unwrap();
    if prog.is_empty() {
        if state.is_flashing() {
            execute!(
                stdout,
                SetForegroundColor(Color::Yellow),
                Print("  (empty)\r\n"),
                ResetColor,
            )?;
        } else {
            execute!(stdout, Print("  (empty)\r\n"))?;
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

        // The rhythm annotation is what makes the offset visible where it is
        // edited: on the chord row itself.
        let rhythm = match slot {
            Slot::Chord(entry) => {
                let pattern = entry
                    .pattern
                    .as_ref()
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                if pattern.is_empty() && entry.offset_ticks == 0 {
                    String::new()
                } else {
                    // The offset is on the note ladder, so it reads the same
                    // wherever it is shown.
                    format!(
                        "   {}   {}",
                        if pattern.is_empty() { "—" } else { &pattern },
                        format_offset(entry.offset_ticks)
                    )
                }
            }
            Slot::Rest => String::new(),
        };

        // Three states on one row: sounding (red and bold), under the cursor
        // (yellow and bold), or neither. The chord being played is the thing to
        // find at a glance, so it is the loudest of the three.
        let emphasis = if is_play {
            Some((Color::Red, true))
        } else if is_sel {
            Some((Color::Yellow, false))
        } else {
            None
        };
        match emphasis {
            Some((colour, _)) => execute!(
                stdout,
                SetForegroundColor(colour),
                SetAttribute(Attribute::Bold),
                Print(format!(
                    "  {} {} {}{}",
                    sel_marker, play_marker, label, rhythm
                )),
                SetAttribute(Attribute::Reset),
                Print("\r\n"),
            )?,
            None => execute!(
                stdout,
                Print(format!(
                    "  {} {} {}{}\r\n",
                    sel_marker, play_marker, label, rhythm
                ))
            )?,
        }
    }
    Ok(())
}

fn render_transport_panel<W: io::Write>(
    stdout: &mut W,
    progression_muted: bool,
    state: &AppState,
) -> io::Result<()> {
    let focused = state.focus == Focus::Transport;
    // `current_row` applies the Transport row clamp; reading `mixer_row`
    // directly would highlight nothing after the mixer cursor was left deep.
    let row = if focused {
        state.current_row()
    } else {
        usize::MAX
    };

    draw_panel_header(stdout, " Transport ", focused)?;

    let bpm_display = match &state.edit {
        Edit::Bpm { current, buffer, .. } => {
            if buffer.is_empty() {
                format!("{}", current)
            } else {
                format!("{}_", buffer)
            }
        }
        _ => format!("{}", state.transport.bpm()),
    };
    draw_transport_row(
        stdout,
        focused && row == TRANSPORT_ROW_BPM,
        "bpm",
        &bpm_display,
    )?;

    draw_transport_row(
        stdout,
        focused && row == TRANSPORT_ROW_LOOP,
        "loop",
        if state.transport.looping.load(Ordering::Relaxed) {
            "on"
        } else {
            "off"
        },
    )?;

    // The metronome shows its *effective* state, so a click forced on by an
    // armed take is legible rather than looking like a switch that did nothing.
    draw_transport_row(
        stdout,
        focused && row == TRANSPORT_ROW_METRONOME,
        "metronome",
        if state.recording {
            "on (recording)"
        } else if state.metronome_on {
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
    // The read-only rows still take the cursor marker: it is the only way to
    // see where the selection is when stepping through them.
    draw_transport_row(
        stdout,
        focused && row == TRANSPORT_ROW_PLAYING,
        "playing",
        &playing_display,
    )?;

    let key_display = match &state.edit {
        Edit::TrackKey { current, .. } => format!("{} (edit)", current.name()),
        _ => state.transport.key().name(),
    };
    draw_transport_row(
        stdout,
        focused && row == TRANSPORT_ROW_KEY,
        "track key",
        &key_display,
    )?;

    draw_transport_row(
        stdout,
        focused && row == TRANSPORT_ROW_MUTE,
        "mute progression",
        if progression_muted { "on" } else { "off" },
    )?;

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
fn draw_action_row<W: io::Write>(
    stdout: &mut W,
    selected: bool,
    label: &str,
    status: Option<&ActionStatus>,
) -> io::Result<()> {
    let marker = if selected { "▸" } else { " " };
    let head = format!("  {} {:<16}", marker, label);
    match status.and_then(|status| status.appearance_at(Instant::now())) {
        Some((colour, text)) => draw_row_with(
            stdout,
            &head,
            selected,
            Some((colour, format!(" {}", single_line_status(text)))),
        ),
        None => draw_row_with(stdout, &head, selected, None),
    }
}

fn draw_transport_row<W: io::Write>(
    stdout: &mut W,
    selected: bool,
    label: &str,
    value: &str,
) -> io::Result<()> {
    let marker = if selected { "▸" } else { " " };
    draw_row(
        stdout,
        &format!("  {} {:<16} {}", marker, label, value),
        selected,
    )
}

fn render_modal<W: io::Write>(stdout: &mut W, modal: &Modal, width: usize) -> io::Result<()> {
    execute!(
        stdout,
        SetForegroundColor(Color::Black),
        SetBackgroundColor(Color::Yellow),
    )?;
    let shown = |buffer: &String| {
        if buffer.is_empty() {
            "_".to_string()
        } else {
            buffer.clone()
        }
    };
    let row = match modal {
        Modal::ConfirmDeleteAllStage1 => {
            "  Delete all chords?  [Enter] continue  [Esc] cancel  ".to_string()
        }
        Modal::ConfirmDeleteAllStage2 => {
            "  Are you sure?  This cannot be undone.  [Enter] confirm  [Esc] cancel  ".to_string()
        }
        Modal::AddRest => "  Add a rest (silent bar)?  [Enter] yes  [Esc] no  ".to_string(),
        Modal::PatchNameInput { buffer } => format!(
            "  Save patch as: {}     [Enter] save  [Esc] cancel  ",
            shown(buffer)
        ),
        Modal::RhythmNameInput { buffer } => format!(
            "  Save rhythm pattern as: {}   [Enter] save + assign  [Esc] cancel  ",
            shown(buffer)
        ),
        Modal::ImportPathInput { buffer } => format!(
            "  Import MIDI: {}     [Enter] import  [Esc] cancel  ",
            shown(buffer)
        ),
    };
    // A prompt is the one thing that must be readable, so it is cut to the
    // window rather than wrapping around the panel below it.
    execute!(stdout, Print(clip_cell(&row, width)))?;
    execute!(stdout, ResetColor, Print("\r\n"))?;
    Ok(())
}

/// The terminal, in columns and rows.
///
/// Read once per frame rather than assumed: the layout clips to the width it
/// actually has, stacks the paired panels when they will not sit side by side,
/// and says so when there is not enough room to draw anything sensible. A
/// hardcoded 100 was wrong on every terminal that was not one.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Screen {
    width: usize,
    height: usize,
}

/// The width the layout is designed for, and the least it will draw at. Below
/// this it says so instead: the panels have fixed grids, and reflowing them into
/// less room would mean cutting data rather than arranging it.
///
/// 80 is where the widest fixed row still fits — the bar grid at its smallest
/// budget, the synth table, a pair of columns with the transport at its floor.
const MIN_SCREEN_WIDTH: usize = 80;
/// Enough rows for the default view (15) with a line to spare. The tallest view
/// wants 34; shorter than that and the bottom panel scrolls.
const MIN_SCREEN_HEIGHT: usize = 16;
/// Where a window wider than [`MIN_SCREEN_WIDTH`] puts the extra columns: into
/// the bar grid, which is the one thing on screen that is a drawing.
const GRID_BUDGET_MAX: usize = 120;
/// Assumed only when the terminal will not say (a redirected stdout). Exactly the
/// width the layout is designed for.
const DEFAULT_SCREEN: Screen = Screen {
    width: MIN_SCREEN_WIDTH,
    height: 34,
};

impl Screen {
    /// What the terminal reports, or [`DEFAULT_SCREEN`] when it cannot be asked.
    fn read() -> Self {
        match size() {
            Ok((cols, rows)) => Screen {
                width: cols as usize,
                height: rows as usize,
            },
            Err(_) => DEFAULT_SCREEN,
        }
    }

    /// Whether there is room to draw the layout at all.
    fn fits(&self) -> bool {
        self.width >= MIN_SCREEN_WIDTH && self.height >= MIN_SCREEN_HEIGHT
    }

    /// The columns the bar grid may use: everything but the row labels and a
    /// margin, so a wider window gets a wider grid rather than a wider gap.
    fn grid_budget(&self) -> usize {
        self.width
            .saturating_sub(12)
            .clamp(MIN_SCREEN_WIDTH - 52, GRID_BUDGET_MAX)
    }
}

/// What to say instead of a frame that will not fit.
fn too_small_banner(screen: Screen) -> String {
    format!(
        "  Terminal too narrow for the layout.\r\n\r\n\
         \x20 This window:  {} × {}\r\n\
         \x20 Needs:        {} columns, {} rows (the default view)\r\n\
         \x20 Every panel:  {} × {}\r\n",
        screen.width,
        screen.height,
        MIN_SCREEN_WIDTH,
        MIN_SCREEN_HEIGHT,
        MIN_SCREEN_WIDTH,
        DEFAULT_SCREEN.height
    )
}
/// Between the panel columns of the transport/progression block.
const PANEL_COLUMN_GAP: usize = 2;
/// What the right-hand column keeps however wide the left one gets: enough for
/// the transport's widest *data* row, so only a status can be cut.
const MIN_RIGHT_COLUMN: usize = 24;

/// A rendered line with its colour codes removed.
///
/// Nothing that pads or measures a column can use `len()`: an ANSI sequence has
/// no visible width, and a colour left unterminated would bleed into the next
/// column.
fn visible_text(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        // A CSI sequence ends at the first byte in the final-byte range, which
        // `[` itself is inside — so the introducer is stepped over explicitly.
        if let Some('[') = chars.next() {
            for c in chars.by_ref() {
                if ('\x40'..='\x7e').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

/// How many columns a rendered line actually occupies.
fn visible_width(line: &str) -> usize {
    visible_text(line).chars().count()
}

/// `text` padded out to `width` *visible* columns.
///
/// A format width counts the characters it is handed, and a colour is characters
/// — so any column holding styled text has to be padded by what it looks like,
/// not by what it is.
fn pad_to(text: &str, width: usize) -> String {
    format!(
        "{}{}",
        text,
        " ".repeat(width.saturating_sub(visible_width(text)))
    )
}

/// One escape sequence, as text, so a row composed as a `String` can carry one.
fn ansi(command: impl Command) -> String {
    let mut out = String::new();
    // Writing into a `String` cannot fail.
    let _ = command.write_ansi(&mut out);
    out
}

/// `text` in a colour, optionally bold, ready to be embedded in a row.
///
/// For the values that are *live* — the chord sounding now, the cell the clock is
/// in — which are drawn inside a composed row rather than through `execute!`.
fn styled(text: &str, colour: Color, bold: bool) -> String {
    let mut out = ansi(SetForegroundColor(colour));
    if bold {
        out.push_str(&ansi(SetAttribute(Attribute::Bold)));
    }
    out.push_str(text);
    out.push_str(&ansi(SetAttribute(Attribute::Reset)));
    out
}

/// One panel's output, captured so it can be laid out beside another.
fn panel_block<F>(render: F) -> String
where
    F: FnOnce(&mut Vec<u8>) -> io::Result<()>,
{
    let mut out: Vec<u8> = Vec::new();
    // Writing into a `Vec` cannot fail, so there is no error here to report.
    let _ = render(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

/// Two panels side by side, or one above the other when content will not fit.
///
/// The left column is as wide as its own content, so its rows never move as
/// values change; the right one takes what is left, with a floor of
/// [`MIN_RIGHT_COLUMN`]. A long enough rhythm name can push the pair past the
/// width even at the design width, and then they stack rather than clipping
/// either panel to nothing. Either way a row that still will not fit is cut
/// (with an ellipsis) instead of wrapping.
fn draw_columns<W: io::Write>(
    stdout: &mut W,
    left: &str,
    right: &str,
    screen: Screen,
) -> io::Result<()> {
    let left_lines: Vec<&str> = left.lines().collect();
    let right_lines: Vec<&str> = right.lines().collect();
    let left_needed = left_lines
        .iter()
        .map(|line| visible_width(line))
        .max()
        .unwrap_or(0);
    let width = screen.width;

    // Side by side only if the right column keeps its floor.
    if left_needed + PANEL_COLUMN_GAP + MIN_RIGHT_COLUMN > width {
        for line in &left_lines {
            execute!(
                stdout,
                Print(format!("{}\r\n", clip_cell(line, width)))
            )?;
        }
        for line in &right_lines {
            execute!(
                stdout,
                Print(format!("{}\r\n", clip_cell(line, width)))
            )?;
        }
        return Ok(());
    }

    let left_width = left_needed.min(width.saturating_sub(PANEL_COLUMN_GAP + MIN_RIGHT_COLUMN));
    // The right-hand panel sits against the far edge, so the pair uses the whole
    // window however wide it is: the chord list at the margin, the transport at
    // the edge, and the gap between them belonging to neither.
    let right_width = right_lines
        .iter()
        .map(|line| visible_width(line))
        .max()
        .unwrap_or(0)
        .min(width.saturating_sub(left_width + PANEL_COLUMN_GAP));
    let right_start = width.saturating_sub(right_width);

    for index in 0..left_lines.len().max(right_lines.len()) {
        let left_cell = clip_cell(left_lines.get(index).copied().unwrap_or(""), left_width);
        let right_cell = clip_cell(right_lines.get(index).copied().unwrap_or(""), right_width);
        let mut row = pad_to(&left_cell, left_width);
        // Pad up to where the right column starts, keeping at least the column
        // gap between the two: a row whose right-hand panel has run out ends
        // where it ends, which is why the result is trimmed.
        if !right_cell.is_empty() {
            let start = right_start.max(left_width + PANEL_COLUMN_GAP);
            row.push_str(&" ".repeat(start.saturating_sub(visible_width(&row))));
            row.push_str(&right_cell);
        }
        execute!(stdout, Print(format!("{}\r\n", row.trim_end())))?;
    }
    Ok(())
}

/// `text` centred in `width` columns.
fn centre_line(text: &str, width: usize) -> String {
    let visible = visible_width(text);
    if visible >= width {
        return text.to_string();
    }
    let pad = width - visible;
    let left = pad / 2;
    format!("{}{}{}", " ".repeat(left), text, " ".repeat(pad - left))
}

/// A panel's header rule centred in `width`, filled with its own rule character.
///
/// The fill goes *outside* the styling rather than inside it: a focused panel's
/// header is a coloured band around its name, and that band should stay the width
/// of the name rather than being smeared across the screen.
fn centre_rule(line: &str, width: usize) -> String {
    let visible = visible_width(line);
    if visible >= width {
        return line.to_string();
    }
    let pad = width - visible;
    let left = pad / 2;
    let right = pad - left;
    // A heavy rule keeps its weight; anything else fills with the light one.
    let fill = if line.contains('━') { '━' } else { '─' };
    format!(
        "{}{}{}{}",
        fill.to_string().repeat(left),
        line,
        fill.to_string().repeat(right),
        ""
    )
}

/// A rendered block with its header rule centred over the block's own width.
///
/// A panel's first line is its header, and centring it over the body below is
/// what makes a wide rule read as *that panel's* title instead of as a line left
/// at the margin.
fn centre_block_header(block: &str) -> String {
    let mut lines = block.lines();
    let Some(header) = lines.next() else {
        return block.to_string();
    };
    let width = block
        .lines()
        .map(|line| visible_width(line))
        .max()
        .unwrap_or(0);
    let mut out = format!("{}\r\n", centre_rule(header, width));
    for line in lines {
        out.push_str(line);
        out.push_str("\r\n");
    }
    out
}

/// A line cut to `width` visible columns, with an ellipsis when it had to be.
///
/// Colour codes are carried across: the cut is a layout decision, and the row
/// keeps whatever it was saying about itself. That matters because every line on
/// screen goes through here — see [`WidthLimited`].
fn clip_cell(line: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if visible_width(line) <= width {
        return line.to_string();
    }
    let mut out = String::new();
    let mut visible = 0;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            // Copy the whole escape sequence, whatever its length.
            if let Some('[') = chars.clone().next() {
                out.push(chars.next().unwrap_or('['));
                for c in chars.by_ref() {
                    out.push(c);
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        // One column is held back for the ellipsis.
        if visible + 1 >= width {
            out.push('…');
            break;
        }
        out.push(c);
        visible += 1;
    }
    out.push_str(&ansi(SetAttribute(Attribute::Reset)));
    out
}

/// A writer that cuts every line to the window's width.
///
/// Nothing on screen may wrap: a wrapped row costs two, shifts everything below
/// it, and pushes the panels off the bottom. Panels draw fixed grids of their
/// own, so rather than asking every one of them to measure, the cut happens once
/// — here — and a panel that outgrows the window is clipped (with an ellipsis)
/// instead of quietly breaking the frame.
struct WidthLimited<W: io::Write> {
    inner: W,
    width: usize,
    line: Vec<u8>,
}

impl<W: io::Write> WidthLimited<W> {
    fn new(inner: W, width: usize) -> Self {
        WidthLimited {
            inner,
            width,
            line: Vec::new(),
        }
    }

    /// Emit the frame's last line, which may not have ended with a newline (a
    /// modal prompt does not), and flush.
    fn finish(&mut self) -> io::Result<()> {
        if !self.line.is_empty() {
            self.end_line()?;
        }
        self.inner.flush()
    }

    /// Emit whatever has been buffered, clipped, as one line.
    fn end_line(&mut self) -> io::Result<()> {
        let text = String::from_utf8_lossy(&self.line).into_owned();
        // Trailing padding is not content: cutting it would put an ellipsis on a
        // row that had nothing to say, which is exactly what a fixed-width field
        // would otherwise earn.
        let text = text.trim_end_matches(['\r', ' ']);
        self.inner
            .write_all(clip_cell(text, self.width).as_bytes())?;
        self.inner.write_all(b"\r\n")?;
        self.line.clear();
        Ok(())
    }
}

impl<W: io::Write> io::Write for WidthLimited<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for &byte in buf {
            if byte == b'\n' {
                self.end_line()?;
            } else {
                self.line.push(byte);
            }
        }
        Ok(buf.len())
    }

    /// Pass-through deliberately: `execute!` flushes after every command, and a
    /// flush is not the end of a line. Ending the buffered row here would turn
    /// each escape sequence and each mid-row `Print` into a line of its own.
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The band a focused panel's header wears, and the band its selected row wears.
///
/// Two levels rather than one colour: the header says *which panel* has the
/// cursor, the row says *where in it*, and they have to be tellable apart at a
/// glance. The row band is deliberately quieter than the header's, because it
/// moves with every arrow press.
const FOCUS_HEADER_BG: Color = Color::Cyan;
const FOCUS_ROW_BG: Color = Color::DarkGrey;
/// Headers of the panels that do not have the cursor recede.
const IDLE_HEADER_FG: Color = Color::DarkGrey;
/// The cell the bar clock is in, and the chord sounding right now.
const LIVE_FG: Color = Color::Yellow;

/// A panel's header rule.
///
/// Focused it is a filled, bold band under a heavier rule; idle it is dim. Both
/// the colour and the weight of the rule change, so which panel has the cursor
/// does not depend on telling two shades of grey apart.
fn draw_header_rule<W: io::Write>(stdout: &mut W, text: &str, focused: bool) -> io::Result<()> {
    if focused {
        execute!(
            stdout,
            SetAttribute(Attribute::Bold),
            SetForegroundColor(Color::Black),
            SetBackgroundColor(FOCUS_HEADER_BG),
            Print(format!("━━{}━━", text)),
            SetAttribute(Attribute::Reset),
        )
    } else {
        execute!(
            stdout,
            SetForegroundColor(IDLE_HEADER_FG),
            Print(format!("──{}──", text)),
            ResetColor,
        )
    }
}

/// A panel's header, rule and newline. A panel with something to append to the
/// rule (the progression's clipboard status) calls [`draw_header_rule`] instead.
fn draw_panel_header<W: io::Write>(stdout: &mut W, text: &str, focused: bool) -> io::Result<()> {
    draw_header_rule(stdout, text, focused)?;
    execute!(stdout, Print("\r\n"))
}

/// One row of a panel, banded when the cursor is on it.
///
/// The band is applied around the whole row rather than the marker alone: a
/// `▸` is a single glyph to hunt for, a coloured row is where the eye already is.
/// `Attribute::Reset` rather than `ResetColor`, because the band also sets bold.
fn draw_row<W: io::Write>(stdout: &mut W, text: &str, focused_row: bool) -> io::Result<()> {
    if focused_row {
        execute!(
            stdout,
            SetAttribute(Attribute::Bold),
            SetForegroundColor(Color::White),
            SetBackgroundColor(FOCUS_ROW_BG),
            Print(text),
            SetAttribute(Attribute::Reset),
            Print("\r\n"),
        )
    } else {
        execute!(stdout, Print(format!("{}\r\n", text)))
    }
}

/// One row of a panel, banded when the cursor is on it, with the row's value
/// printed in the live colour while it is still being edited.
fn draw_row_with<W: io::Write>(
    stdout: &mut W,
    text: &str,
    focused_row: bool,
    tail: Option<(Color, String)>,
) -> io::Result<()> {
    if focused_row {
        execute!(
            stdout,
            SetAttribute(Attribute::Bold),
            SetForegroundColor(Color::White),
            SetBackgroundColor(FOCUS_ROW_BG),
            Print(text),
        )?;
    } else {
        execute!(stdout, Print(text))?;
    }
    match tail {
        Some((colour, tail)) => execute!(
            stdout,
            SetForegroundColor(colour),
            Print(tail),
            SetAttribute(Attribute::Reset),
            Print("\r\n"),
        )?,
        None => execute!(stdout, SetAttribute(Attribute::Reset), Print("\r\n"))?,
    }
    Ok(())
}

/// The indent every row of the header table starts with.
const KEY_ROW_INDENT: &str = "  ";
/// Columns one key takes in the keyboard row: its three-character cell and the
/// space after it. `draw_key` is what draws them, so the two have to agree.
const KEY_WIDTH: usize = 4;
/// The column between the two hands.
const KEY_COLUMN_GAP: &str = " │ ";
/// Its width in columns. Written down rather than taken from `str::len`, which
/// counts *bytes* — and the bar is three of them. The test
/// `the_keyboard_layout_adds_up` keeps the two in step.
const KEY_COLUMN_GAP_WIDTH: usize = 3;
/// The width the latched keys are padded to, so both registers' arrows line up
/// inside their own cell.
const REGISTER_KEYS_WIDTH: usize = 7;
/// Which column the right-hand register starts in, inside the header block: past
/// the left hand's five keys and the gap between the hands.
const REGISTER_RIGHT_COLUMN: usize =
    KEY_ROW_INDENT.len() + 5 * KEY_WIDTH + KEY_COLUMN_GAP_WIDTH;
/// The width one register's cell may occupy — wide enough for the longest set and
/// the longest label together (`[adfsg] → maj7#11 (J)`).
const REGISTER_CELL_WIDTH: usize = 24;
/// How wide the keyboard-and-registers block is: both hands, the gap between
/// them, and the right-hand register's cell.
///
/// A constant rather than a measurement of the two rows, because the right-hand
/// cell changes length with every chord and a block that resized to fit it would
/// shuffle the keyboard sideways while you play.
const HEADER_BLOCK_WIDTH: usize =
    REGISTER_RIGHT_COLUMN + REGISTER_CELL_WIDTH;

/// The keys latched into a register.
///
/// `—` (never set) and `(empty)` (explicitly cleared) are different states —
/// they resolve differently, and a document keeps the difference — so they read
/// differently here rather than collapsing into one blank.
fn register_keys(register: &Option<PositionSet>) -> String {
    match register {
        None => "—".to_string(),
        Some(set) if set.is_empty() => "(empty)".to_string(),
        Some(set) => {
            let mut labels: Vec<char> = set.iter().map(|p| p.qwerty_label()).collect();
            labels.sort_unstable();
            format!("[{}]", labels.into_iter().collect::<String>())
        }
    }
}

/// Both registers on one row, each under the hand that fills it.
///
/// The hand's keys decide the columns, so the row reads as two labels under the
/// keyboard rather than as two sentences.
fn register_row(state: &AppState, key: &Key) -> String {
    let left = register_cell(&state.registers.left, "L", key);
    let right = register_cell(&state.registers.right, "R", key);
    let mut row = format!("{}{}", KEY_ROW_INDENT, left);
    row.push_str(&" ".repeat(REGISTER_RIGHT_COLUMN.saturating_sub(visible_width(&row))));
    row.push_str(&right);
    row
}

/// One register as a cell: the keys latched into it and what they mean, or just
/// its state.
fn register_cell(register: &Option<PositionSet>, side: &str, key: &Key) -> String {
    // Padded *before* it is coloured: a format width counts the characters it is
    // given, and an escape sequence is characters.
    let keys = format!(
        "{:<REGISTER_KEYS_WIDTH$}",
        register_keys(register)
    );
    // A register that holds something is the one thing in this block you cannot
    // work out from the keyboard row, because latching survives letting go — so
    // the keys it holds are drawn in white and hard to miss, and a register that
    // holds nothing stays dim.
    let keys = if register.as_ref().is_some_and(|set| !set.is_empty()) {
        styled(&keys, Color::White, true)
    } else {
        ansi(SetForegroundColor(IDLE_HEADER_FG)) + &keys + &ansi(SetAttribute(Attribute::Reset))
    };
    format!(
        "{} {}→ {}",
        side,
        keys,
        register_meaning(register, side, key)
    )
}

/// What a register means: a scale degree and its root note on the left hand, a
/// transformation and its mode on the right. `—` when there is nothing to read.
fn register_meaning(register: &Option<PositionSet>, side: &str, key: &Key) -> String {
    let Some(set) = register.as_ref().filter(|set| !set.is_empty()) else {
        return "—".to_string();
    };
    match side {
        "R" => right_hand_transformation(set)
            .map(|t| format!("{} ({:?})", t.label(), t.mode()))
            .unwrap_or_else(|| "?".to_string()),
        _ => left_hand_degree(set)
            .map(|d| format!("{} ({})", d.label(), note_name(key.degree_root(d))))
            .unwrap_or_else(|| "?".to_string()),
    }
}

fn draw_key<W: io::Write>(stdout: &mut W, pos: KeyPosition, active: bool) -> io::Result<()> {
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
            edit: Edit::None,
            transport_row: 0,
            synth_row: 0,
            synth_col: 0,
            progression_row: 0,
            preset_row: 0,
            modal: None,
            patch_store: PatchStore {
                patches: Vec::new(),
            },
            rhythm_store: Arc::new(Mutex::new(RhythmStore::default())),
            sinko_row: 0,
            sinko_cell: 0,
            takes: Vec::new(),
            current_take: Vec::new(),
            last_esc: None,
            stop_flash_until: None,
            recording: false,
            metronome_on: false,
            last_tap_at: None,
            take_started_at: None,
            sinko_smooth: SINKO_SMOOTH_DEFAULT,
            working: RhythmPattern::draft(),
            working_slot: None,
        sinko_clipboard: None,
            rhythm_status: None,
            rhythm_path: std::env::temp_dir().join("chord-tool-unused-rhythms.toml"),
            flash_until: None,
            taps: TapTracker::default(),
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
            Focus::Synth,
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
        ProgressionEntry::new(degree, t)
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
        let mut s = state(Focus::Synth);
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
        // the reserved right-hand slots fall through to the chord path.
        // `RightInnerBelow` left this list when it became "replace the selected
        // chord".
        for p in [
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
        let mut s = state(Focus::Synth);
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
    fn a_deep_synth_cursor_does_not_touch_the_transport_selection() {
        // These panels used to share one row index, so leaving the cursor deep
        // in the mixer pushed the Transport selection off the end of its own
        // (shorter) row list. They are separate fields now; assert that.
        let mut s = state(Focus::Synth);
        s.set_current_row(SYNTH_ROWS - 1);
        assert_eq!(s.synth_row, SYNTH_ROWS - 1);

        s.focus = Focus::Transport;
        assert_eq!(s.current_row(), 0);
        assert!(s.current_row() < TRANSPORT_ROWS);
    }

    #[test]
    fn the_transport_cursor_is_still_clamped_to_its_own_rows() {
        let mut s = state(Focus::Transport);
        s.set_current_row(TRANSPORT_ROWS + 5);
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
        let score = midi::render_progression(&[], &Key::new(60, Scale::Major), 120, 1.0);
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






    // ---- replacing a slot's chord ----

    #[test]
    fn a_refusal_fades_away_like_any_other_message() {
        // A refusal is feedback about a key press, not an error to act on, so it
        // holds and then dims out exactly as a success does.
        let start = Instant::now();
        let refused =
            ActionStatus::new("nothing copied yet".to_string(), ActionOutcome::Refused, start);

        assert_eq!(
            refused.appearance_at(start),
            Some((Color::Red, "nothing copied yet"))
        );
        let mid = start + STATUS_HOLD + STATUS_FADE / 2;
        match refused.appearance_at(mid) {
            Some((Color::Rgb { r, .. }, _)) => assert!(r < 0xAF, "dimming: {:?}", refused),
            other => panic!("should be dimming, got {:?}", other),
        }
        assert_eq!(refused.appearance_at(start + STATUS_HOLD + STATUS_FADE), None);
    }

    #[test]
    fn a_refused_paste_does_not_linger_on_screen() {
        // The regression: a red "nothing copied yet" stayed on the panel for the
        // rest of the session, because every non-success was made to persist.
        // Right for an export that could not write; wrong for a key press with
        // nothing to paste.
        let log = logger();
        let mut s = sinko_state();
        paste_sinko(&mut s, &log);

        let status = s.rhythm_status.as_ref().expect("a refusal is reported");
        assert_eq!(status.text(), "nothing copied yet");
        assert!(
            status.appearance_at(Instant::now() + STATUS_HOLD + STATUS_FADE).is_none(),
            "it should have faded"
        );

        // A failure proper still stays: this one is a path that cannot be
        // written, so the reason is worth reading for as long as it takes.
        let mut s = sinko_state();
        s.rhythm_path = unique_export_dir("rhythm-failure")
            .join("no-such-directory")
            .join("rhythms.toml");
        new_pattern(&mut s, &log);
        s.current_take = vec![0];
        close_take(&mut s, &log);
        save_working_pattern(&mut s, "Tapped", &log);
        let status = s.rhythm_status.as_ref().expect("a failure is reported");
        assert!(
            status.appearance_at(Instant::now() + Duration::from_secs(600)).is_some(),
            "an error with a reason to read should stay"
        );
    }

    #[test]
    fn replace_swaps_the_chord_and_keeps_the_syncopation() {
        let log = logger();
        let mut s = sinko_state();
        // A slot with rhythm and an offset, and the chord to replace it with
        // latched in the registers.
        assign(&mut s, 2, "Offbeat Eighths");
        s.progression.lock().unwrap().set_offset(2, -480);
        s.held.insert(KeyPosition::LeftPinky);
        s.held.insert(KeyPosition::RightMiddle);
        s.registers.lock_both(&s.held);
        s.held.clear();

        let before = resolved_chord(&s);
        let (pattern, offset) = {
            let prog = s.progression.lock().unwrap();
            match &prog.slots[2] {
                Slot::Chord(e) => (e.pattern.clone(), e.offset_ticks),
                other => panic!("expected a chord, got {:?}", other),
            }
        };

        replace_selected_chord(&mut s, &log);

        let after = {
            let prog = s.progression.lock().unwrap();
            match &prog.slots[2] {
                Slot::Chord(e) => (e.degree, e.transformation, e.pattern.clone(), e.offset_ticks),
                other => panic!("expected a chord, got {:?}", other),
            }
        };
        let (degree, transformation) = before.expect("a latched chord");
        assert_eq!(after.0, degree, "the chord changed");
        assert_eq!(after.1, transformation);
        assert_eq!(after.2, pattern, "the pattern stayed with the slot");
        assert_eq!(after.3, offset, "and so did the offset");
    }

    #[test]
    fn replace_needs_a_chord_to_replace_with() {
        let log = logger();
        let mut s = sinko_state();
        let before = degrees(&s);

        replace_selected_chord(&mut s, &log);
        assert_eq!(degrees(&s), before, "nothing changes");
        assert!(s.is_flashing(), "and it says so");
    }

    #[test]
    fn replacing_twice_records_only_one_edit() {
        let log = logger();
        let mut s = sinko_state();
        s.progression_row = 2;
        s.held.insert(KeyPosition::LeftIndex);
        s.held.insert(KeyPosition::RightMiddle);
        s.registers.lock_both(&s.held);
        s.held.clear();

        replace_selected_chord(&mut s, &log);
        // Same chord, same register snapshot: nothing left to record.
        replace_selected_chord(&mut s, &log);

        assert!(s.progression.lock().unwrap().undo());
        match &s.progression.lock().unwrap().slots[2] {
            Slot::Chord(e) => {
                assert_eq!(e.degree, ScaleDegree::VI, "back to the seeded chord");
                assert_eq!(e.registers, Registers::default(), "and its registers");
            }
            other => panic!("expected a chord, got {:?}", other),
        }
        assert!(
            !s.progression.lock().unwrap().can_undo(),
            "two presses recorded one edit, so one undo is all there is"
        );
    }

    #[test]
    fn replace_turns_a_selected_rest_into_a_chord() {
        let log = logger();
        let mut s = sinko_state();
        s.progression.lock().unwrap().slots[2] = Slot::Rest;
        s.held.insert(KeyPosition::LeftIndex);
        s.registers.lock_both(&s.held);
        s.held.clear();

        replace_selected_chord(&mut s, &log);
        let (degree, pattern) = {
            let prog = s.progression.lock().unwrap();
            match &prog.slots[2] {
                Slot::Chord(e) => (e.degree, e.pattern.clone()),
                other => panic!("expected a chord, got {:?}", other),
            }
        };
        assert_eq!(degree, ScaleDegree::I);
        assert_eq!(pattern, None, "a rest had no rhythm to keep");
    }

    #[test]
    fn replace_is_scoped_to_the_progression_panel() {
        // Like copy and paste: it acts on the cursor, so it cannot run where the
        // cursor is not on screen.
        let log = logger();
        let mut s = sinko_state();
        s.focus = Focus::Sinko;
        s.held.insert(KeyPosition::LeftIndex);
        s.registers.lock_both(&s.held);
        s.held.clear();

        let before = degrees(&s);
        handle_hotkey(&mut s, Hotkey::ReplaceChord, false, &log);
        assert_eq!(degrees(&s), before, "nothing was replaced");
        assert!(s.is_flashing(), "it flashes instead of acting on a hidden row");
    }

    #[test]
    fn replace_goes_through_the_hotkey_in_the_progression_panel() {
        let log = logger();
        let mut s = sinko_state();
        s.focus = Focus::Progression;
        s.progression_row = 0;
        s.held.insert(KeyPosition::LeftMiddle);
        s.registers.lock_both(&s.held);
        s.held.clear();

        handle_hotkey(&mut s, Hotkey::ReplaceChord, false, &log);
        match &s.progression.lock().unwrap().slots[0] {
            Slot::Chord(e) => assert_eq!(e.degree, ScaleDegree::V),
            other => panic!("expected a chord, got {:?}", other),
        }
        assert!(s.progression.lock().unwrap().can_undo(), "and it is undoable");
    }

    #[test]
    fn the_rhythm_annotation_survives_a_replace() {
        // What the panel shows about the rhythm must not change when only the
        // chord is swapped.
        let log = logger();
        let mut s = sinko_state();
        s.progression_row = 2;
        assign(&mut s, 2, "Offbeat Eighths");
        s.progression.lock().unwrap().set_offset(2, -480);

        let before = render_frame(&s);
        assert!(before.contains("Offbeat Eighths"), "{}", before);
        assert!(before.contains("Am"), "the selected slot is vi: {}", before);

        s.held.insert(KeyPosition::LeftPinky);
        s.registers.lock_both(&s.held);
        s.held.clear();
        replace_selected_chord(&mut s, &log);

        let after = render_frame(&s);
        assert!(
            after.contains("Offbeat Eighths"),
            "the pattern column is unchanged: {}",
            after
        );
        assert!(after.contains("-1/8"), "and so is the offset column");
        assert!(
            !after.contains("Am"),
            "while the chord itself changed: {}",
            after
        );
    }

    // ---- esc: stop, then quit ----

    #[test]
    fn the_first_esc_stops_and_flashes_without_quitting() {
        let log = logger();
        let mut s = sinko_state();
        s.transport.playing.store(true, Ordering::Relaxed);

        assert!(!esc_should_quit(&mut s, &log), "one press never quits");
        assert!(!s.transport.playing.load(Ordering::Relaxed), "the music stops");
        assert!(
            s.transport.stop_now.load(Ordering::Relaxed),
            "and it stops *now*, rather than at the end of the bar"
        );
        assert!(s.stop_flash_active(), "the screen flashes");
    }

    #[test]
    fn a_quick_second_esc_quits() {
        let log = logger();
        let mut s = sinko_state();
        assert!(!esc_should_quit(&mut s, &log));
        assert!(esc_should_quit(&mut s, &log), "the second press quits");
    }

    #[test]
    fn a_slow_second_esc_only_stops_again() {
        let log = logger();
        let mut s = sinko_state();
        s.transport.playing.store(true, Ordering::Relaxed);
        assert!(!esc_should_quit(&mut s, &log));

        // Past the window, the gesture starts over rather than quitting.
        s.last_esc = Some(Instant::now() - ESC_QUIT_WINDOW - Duration::from_millis(10));
        s.transport.playing.store(true, Ordering::Relaxed);
        assert!(!esc_should_quit(&mut s, &log), "not a double press");
        assert!(!s.transport.playing.load(Ordering::Relaxed));
    }

    #[test]
    fn the_stop_leaves_the_bar_where_it_was() {
        // A panic stop must not rewind: the next play resumes where you were.
        let log = logger();
        let mut s = sinko_state();
        s.transport.current_bar.store(3, Ordering::Relaxed);
        s.transport.playing.store(true, Ordering::Relaxed);

        esc_should_quit(&mut s, &log);
        assert_eq!(s.transport.current_bar.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn an_open_editor_keeps_esc_for_itself() {
        // The regression: Esc used to be tested for "quit" *before* the edit
        // dispatch, so it left the program instead of cancelling the edit, and
        // the handlers' own Esc arms were unreachable.
        let log = logger();
        let mut s = sinko_state();

        s.working = RhythmPattern::from_step_string("g", 1.0, "xxxx").unwrap();
        begin_synth_edit(&mut s, &SynthParams::defaults());
        assert!(!esc_is_free(&s), "an edit owns Esc");
        handle_synth_edit(&mut s, &SynthParams::defaults(), &key(KeyCode::Esc), &log);
        assert!(matches!(s.edit, Edit::None), "and it cancels the edit");

        s.modal = Some(Modal::AddRest);
        assert!(!esc_is_free(&s), "so does a prompt");

        s.modal = None;
        assert!(esc_is_free(&s), "with nothing open, Esc is the stop gesture");
    }

    #[test]
    fn the_stop_banner_replaces_the_title_while_it_flashes() {
        let log = logger();
        let mut s = state(Focus::Transport);
        let before = render_frame(&s);
        assert!(before.contains("Chord Tool"), "normally the title");

        esc_should_quit(&mut s, &log);
        let during = render_frame(&s);
        assert!(during.contains("STOPPED"), "{}", during);
        assert!(!during.contains("Chord Tool"), "the title is covered");

        // A full width of red, so it reads as the screen flashing.
        let banner = stop_banner(DEFAULT_SCREEN.width);
        assert!(banner.contains("STOPPED"));
        assert!(banner.chars().count() > 80, "spans the terminal: {}", banner.len());

        // And it goes away on its own.
        s.stop_flash_until = Some(Instant::now() - Duration::from_millis(1));
        assert!(!s.stop_flash_active());
        assert!(render_frame(&s).contains("Chord Tool"));
    }

    #[test]
    fn zz_dump_hands() {
        let chords = [
            ScaleDegree::I,
            ScaleDegree::V,
            ScaleDegree::VI,
            ScaleDegree::IV,
        ];
        for width in [80, 120] {
            let mut s = state(Focus::Transport);
            seed(&mut s, &chords);
            for pos in [
                KeyPosition::LeftPinky,
                KeyPosition::LeftRing,
                KeyPosition::LeftMiddle,
                KeyPosition::RightIndex,
                KeyPosition::RightMiddle,
            ] {
                s.held.insert(pos);
            }
            s.registers.lock_both(&s.held);
            s.held.clear();
            println!("===== {} =====", width);
            for (i, line) in render_frame_at(&s, width_screen(width)).lines().take(6).enumerate() {
                println!("{:>3}|{}|", i, line);
            }
        }
    }

    #[test]
    fn the_screen_carries_no_key_reminders() {
        // Every reminder was prose about a key the player already knows, and
        // each line of it cost a row that the panels could have had. The README
        // is the reference now; this test is what keeps the screen honest about
        // it, because a reminder creeping back in is a layout change.
        let chords = [
            ScaleDegree::I,
            ScaleDegree::V,
            ScaleDegree::VI,
            ScaleDegree::IV,
        ];
        for focus in [
            Focus::Transport,
            Focus::Progression,
            Focus::Sinko,
            Focus::Synth,
            Focus::SynthPresets,
        ] {
            let mut s = state(focus);
            seed(&mut s, &chords);
            let text = render_frame(&s);
            for reminder in [
                "tab to switch",
                "tab out",
                "-> right register",
                "-> left register",
                "space -> both",
                "Esc stop",
                "play/pause",
                "Log: debug.log",
                "enter toggles",
                "enter keeps",
                "press enter",
                "press a left-hand key",
                "taps a beat",
                "shift+←/→",
            ] {
                assert!(
                    !text.contains(reminder),
                    "{:?} still says {:?}:\n{}",
                    focus,
                    reminder,
                    text
                );
            }
        }
    }


    // ---- editing the hits ----

    /// A live pattern on the selected slot, with the `hits` row focused.
    fn hits_state(steps: usize, steps_str: &str) -> AppState {
        let log = logger();
        let mut s = sinko_state();
        s.sinko_row = SINKO_ROW_HITS;
        new_pattern(&mut s, &log);

        let name = s.working.name.clone();
        let mut pattern = RhythmPattern::from_step_string("f", 1.0, steps_str).unwrap();
        if pattern.steps_per_bar() != steps {
            pattern = RhythmPattern::blank("f", steps).unwrap();
        }
        pattern.name = name;
        own(&mut s, pattern);
        s
    }

    /// The top take's grid as a step string.
    fn top_grid(s: &AppState) -> String {
        s.working.layers[0].to_step_string()
    }

    #[test]
    fn the_hits_cursor_walks_the_grid_and_clamps() {
        let log = logger();
        let mut s = hits_state(4, "x---");

        move_cell_cursor(&mut s, 1, &log);
        assert_eq!(cursor_cell(&s), 1);
        move_cell_cursor(&mut s, 1, &log);
        assert_eq!(cursor_cell(&s), 2);
        move_cell_cursor(&mut s, -1, &log);
        assert_eq!(cursor_cell(&s), 1);

        // Clamped at both ends rather than wrapping.
        for _ in 0..9 {
            move_cell_cursor(&mut s, 1, &log);
        }
        assert_eq!(cursor_cell(&s), 3, "the last cell");
        for _ in 0..9 {
            move_cell_cursor(&mut s, -1, &log);
        }
        assert_eq!(cursor_cell(&s), 0, "the first cell");
    }

    #[test]
    fn enter_turns_a_hit_on_and_off() {
        let log = logger();
        // A quarter grid with the first cell hit, cursor on the second.
        let mut s = hits_state(4, "x---");
        s.sinko_cell = 1;
        assert!(!cell_is_on(&s, 1));

        toggle_cell(&mut s, &log);
        assert!(cell_is_on(&s, 1), "turned on");
        assert_eq!(top_grid(&s), "xx--");

        toggle_cell(&mut s, &log);
        assert!(!cell_is_on(&s, 1), "and off again");
        assert_eq!(top_grid(&s), "x---");
    }

    #[test]
    fn turning_a_hit_off_clears_it_in_every_take() {
        // The regression this guards: a hit left in a lower take would still
        // sound, so the press would look like it did nothing.
        let log = logger();
        let mut s = hits_state(4, "x---");
        s.sinko_cell = 0;
        // A second take, with the same hit and one of its own.
        let mut lower = RhythmLayer::new(0.7, vec![true, false, false, false]);
        lower.steps[3] = true;
        s.working.layers.push(lower);
        assert!(cell_is_on(&s, 0), "both takes hit cell 1");

        toggle_cell(&mut s, &log);
        assert!(!cell_is_on(&s, 0), "cell 1 is gone from the pattern");
        assert!(
            s.working.layers.iter().all(|layer| !layer.hit(0)),
            "and from every take"
        );
        assert!(
            s.working.layers[1].hit(3),
            "while the other hit in that take survives"
        );
    }

    #[test]
    fn turning_a_hit_on_puts_it_in_the_newest_take() {
        let log = logger();
        let mut s = hits_state(4, "x---");
        s.sinko_cell = 2;
        // A quieter take underneath, without the hit.
        s.working.layers.push(RhythmLayer::new(0.7, vec![true, false, false, false]));
        s.working.layers[0].gain = 1.0;

        toggle_cell(&mut s, &log);
        assert!(s.working.layers[0].hit(2), "the newest take has it");
        assert!(!s.working.layers[1].hit(2), "the older one does not");
        assert_eq!(s.working.layers[0].gain, 1.0, "and it is the loud one");
    }

    #[test]
    fn a_toggled_hit_is_heard_without_saving() {
        let log = logger();
        let mut s = hits_state(4, "x---");
        s.sinko_cell = 2;

        toggle_cell(&mut s, &log);

        // The entry the scheduler reads is the one that changed, with no save
        // and no library write in between.
        let played = sinko_pattern(&s).expect("the row plays a rhythm");
        assert!(played.layers[0].hit(2), "the chord has the new hit");
    }

    /// The `hits` row, trimmed.
    fn hits_line(state: &AppState) -> String {
        render_sinko(state)
            .lines()
            // The label column, not the `quant` row's "N hits" tail.
            .find(|line| {
                line.trim()
                    .trim_start_matches('▸')
                    .trim_start()
                    .starts_with("hits")
            })
            .map(|line| line.trim().to_string())
            .unwrap_or_default()
    }

    #[test]
    fn the_hits_row_reads_the_cell_and_its_state() {
        let log = logger();
        let mut s = hits_state(4, "x---");
        assert_eq!(hits_line(&s), "▸ hits     on        1 of 4");

        s.sinko_cell = 1;
        assert_eq!(hits_line(&s), "▸ hits     off       2 of 4");

        toggle_cell(&mut s, &log);
        assert_eq!(hits_line(&s), "▸ hits     on        2 of 4");
    }

    #[test]
    fn the_grid_inverts_the_cell_the_cursor_is_on() {
        // The row says which cell; the grid has to show where it is. It inverts
        // the cell rather than using another character, so the cell still reads
        // as a hit or a rest.
        let mut s = hits_state(4, "x---");
        s.sinko_cell = 2;

        let raw = {
            let mut out: Vec<u8> = Vec::new();
            render_sinko_panel(&mut out, &s, DEFAULT_SCREEN.grid_budget()).unwrap();
            String::from_utf8(out).unwrap()
        };
        assert_eq!(raw.matches("\x1b[7m").count(), 1, "one inverted cell");
        let line = raw
            .lines()
            .find(|line| line.contains("\x1b[7m"))
            .expect("the inverted line");
        assert!(
            line.contains("\x1b[7m···\x1b[0m") || line.contains("\x1b[7m"),
            "the third cell of a quarter grid: {:?}",
            line
        );

        // And it goes away when the cursor is on another row.
        s.sinko_row = SINKO_ROW_HOLD;
        let raw = {
            let mut out: Vec<u8> = Vec::new();
            render_sinko_panel(&mut out, &s, DEFAULT_SCREEN.grid_budget()).unwrap();
            String::from_utf8(out).unwrap()
        };
        assert!(!raw.contains("\x1b[7m"), "no cursor on a value row");
    }

    #[test]
    fn the_hits_row_takes_enter_even_with_a_chord_held() {
        // Otherwise Enter would add the held chord instead of toggling, which is
        // the same trap the Export button has.
        let mut s = hits_state(4, "x---");
        s.held.insert(KeyPosition::LeftIndex);
        s.set_current_row(SINKO_ROW_HITS);
        assert!(row_is_action_button(&s));
        assert_eq!(enter_intent(&s, false), EnterIntent::PanelAction);
    }

    #[test]
    fn the_cursor_follows_a_resolution_change() {
        // A 16-cell grid with the cursor near the end, then a quarter grid: the
        // cursor has to come back inside.
        let log = logger();
        let mut s = hits_state(16, &"x".repeat(16));
        s.sinko_cell = 15;
        assert_eq!(cursor_cell(&s), 15);

        cycle_resolution(&mut s, -1, &log);
        cycle_resolution(&mut s, -1, &log);
        assert_eq!(s.working.steps_per_bar(), 4);
        assert_eq!(cursor_cell(&s), 3, "clamped to the new grid");
    }

    #[test]
    fn a_silent_pattern_can_still_have_hits_turned_on() {
        // The way to build a figure by hand rather than by tapping.
        let log = logger();
        let mut s = hits_state(4, "----");
        s.sinko_cell = 2;
        toggle_cell(&mut s, &log);
        assert_eq!(top_grid(&s), "--x-");
    }

    // ---- hold and mute ----

    #[test]
    fn the_hold_row_walks_the_note_ladder() {
        let log = logger();
        let mut s = sinko_state();
        s.sinko_row = SINKO_ROW_HOLD;
        // An assigned pattern, because a pattern nothing plays cannot be edited.
        own(&mut s, RhythmPattern::from_step_string("g", 0.25, "xxxx").unwrap());
        assert_eq!(s.working.hold, 240, "a 16th to start");

        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert_eq!(s.working.hold, 480, "an 8th");
        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert_eq!(s.working.hold, 720, "a 3/16");
        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert_eq!(s.working.hold, 960, "a quarter");
        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert_eq!(s.working.hold, 1440, "a dotted quarter");
        adjust_current(&mut s, &SynthParams::defaults(), -1, &log);
        assert_eq!(s.working.hold, 960, "and back down one rung");
    }

    #[test]
    fn the_hold_row_reaches_a_whole_note_and_stops_there() {
        let log = logger();
        let mut s = sinko_state();
        s.sinko_row = SINKO_ROW_HOLD;
        own(&mut s, RhythmPattern::from_step_string("g", 1.0, "x---").unwrap());

        for _ in 0..20 {
            adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        }
        assert_eq!(s.working.hold, BAR_TICKS as u32, "a whole note is the top");
        for _ in 0..20 {
            adjust_current(&mut s, &SynthParams::defaults(), -1, &log);
        }
        assert_eq!(s.working.hold, 120, "and a 32nd is the bottom");
    }

    #[test]
    fn the_hold_row_snaps_a_value_that_is_not_a_note_length() {
        // Quarters ships holding 768 ticks — 0.8 of a cell, which is musical but
        // not a note length. The first press snaps it rather than stepping past.
        let log = logger();
        let mut s = sinko_state();
        s.sinko_row = SINKO_ROW_HOLD;
        own(&mut s, RhythmPattern::from_step_string("Q", 0.8, "xxxx").unwrap());
        assert_eq!(s.working.hold, 768);

        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert_eq!(s.working.hold, 720, "the nearest note length");
        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert_eq!(s.working.hold, 960, "and only then does it step");
    }

    #[test]
    fn the_shape_rows_refuse_an_unassigned_pattern() {
        // Nothing plays a pattern with no name, so editing one would look like it
        // worked and sound like nothing.
        let log = logger();
        for row in [
            SINKO_ROW_HITS,
            SINKO_ROW_HOLD,
            SINKO_ROW_MUTE,
            SINKO_ROW_QUANT,
        ] {
            let mut s = sinko_state();
            s.sinko_row = row;
            assert!(
                sinko_pattern(&s).is_none(),
                "the row under test must own nothing"
            );
            let before = s.working.clone();

            adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
            assert_eq!(s.working, before, "row {} changed something", row);
            assert!(s.is_flashing(), "and it says why");
        }
    }

    #[test]
    fn the_hold_row_reads_its_note_value() {
        let log = logger();
        let mut s = sinko_state();
        // Assigning a held pattern loads it into the draft, so the row shows it.
        cycle_assigned_pattern(&mut s, 1, &log);
        for _ in 0..5 {
            cycle_assigned_pattern(&mut s, 1, &log);
        }
        assert_eq!(
            sinko_pattern(&s).map(|p| p.name),
            Some("Held Half".to_string())
        );

        let text = render_sinko(&s);
        assert!(text.contains("hold     1/2  —  1920 ticks"), "{}", text);
        assert!(text.contains("mute     none"), "{}", text);
    }

    #[test]
    fn the_mute_row_walks_none_then_the_note_lengths_up_to_a_quarter() {
        let log = logger();
        let mut s = sinko_state();
        s.sinko_row = SINKO_ROW_MUTE;
        own(&mut s, RhythmPattern::from_step_string("Cut", 1.0, "xxxx").unwrap());
        assert_eq!(s.working.mute_ticks, 0);

        for expected in [120, 240, 480, 720, 960] {
            adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
            assert_eq!(s.working.mute_ticks, expected);
        }
        assert_eq!(s.working.mute_ticks, rhythm::MAX_MUTE_TICKS, "a quarter");

        // And it stops there.
        for _ in 0..5 {
            adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        }
        assert_eq!(s.working.mute_ticks, rhythm::MAX_MUTE_TICKS);

        for _ in 0..9 {
            adjust_current(&mut s, &SynthParams::defaults(), -1, &log);
        }
        assert_eq!(s.working.mute_ticks, 0, "and never below none");
    }

    // ---- Recording takes ----

    /// A Sinko state that is recording against a bar that has just started.
    fn arm(s: &mut AppState) {
        s.transport.publish_bar(Instant::now(), 0);
        s.recording = true;
        s.last_tap_at = None;
    }

    #[test]
    fn the_live_row_is_empty_before_anything_is_tapped() {
        let s = sinko_state();
        let live = sinko_live_steps(&s);
        assert_eq!(live.len(), rhythm::DEFAULT_STEPS);
        assert!(live.iter().all(|on| !on), "nothing has been played yet");
    }

    #[test]
    fn the_live_row_snaps_the_take_in_progress() {
        let mut s = sinko_state();
        s.current_take = vec![0, 1000];
        let live = sinko_live_steps(&s);
        assert!(live[0], "the downbeat");
        assert!(live[4], "1000 ticks snaps to cell 4 of 16");
        assert_eq!(live.iter().filter(|on| **on).count(), 2);
    }

    #[test]
    fn a_tap_is_timestamped_against_the_published_bar() {
        let log = logger();
        let mut s = sinko_state();
        arm(&mut s);

        sinko_tap(&mut s, &log);
        assert_eq!(s.current_take.len(), 1);
        assert!(
            s.current_take[0] < 200,
            "a tap right after the downbeat lands near it, was {}",
            s.current_take[0]
        );
    }

    #[test]
    fn taps_are_ignored_when_recording_is_off() {
        let log = logger();
        let mut s = sinko_state();
        s.transport.publish_bar(Instant::now(), 0);

        sinko_tap(&mut s, &log);
        assert!(s.current_take.is_empty());
        assert!(s.is_flashing(), "a refused tap must say so");
    }

    #[test]
    fn taps_are_ignored_without_a_bar_clock() {
        let log = logger();
        let mut s = sinko_state();
        s.recording = true;

        sinko_tap(&mut s, &log);
        assert!(s.current_take.is_empty(), "there is no bar to sit inside");
        assert!(s.is_flashing());
    }

    #[test]
    fn taps_closer_than_the_debounce_are_a_key_repeat_not_a_second_tap() {
        let log = logger();
        let mut s = sinko_state();
        arm(&mut s);

        sinko_tap(&mut s, &log);
        sinko_tap(&mut s, &log);
        sinko_tap(&mut s, &log);
        assert_eq!(
            s.current_take.len(),
            1,
            "three presses in the same millisecond are one tap"
        );

        // Past the debounce, it is a real second tap.
        s.last_tap_at = Some(Instant::now() - Duration::from_millis(TAP_DEBOUNCE_MS as u64 + 10));
        sinko_tap(&mut s, &log);
        assert_eq!(s.current_take.len(), 2);
    }

    #[test]
    fn a_take_closes_when_the_bar_moves_on() {
        let log = logger();
        let mut s = sinko_state();
        arm(&mut s);
        sinko_tap(&mut s, &log);
        assert_eq!(s.takes.len(), 0, "still recording the first take");

        // The next bar: the scheduler publishes a later bar start.
        s.transport
            .publish_bar(Instant::now() + Duration::from_secs(2), 1);
        s.last_tap_at = None;
        sinko_tap(&mut s, &log);

        assert_eq!(s.takes.len(), 1, "the first take is committed");
        assert_eq!(s.takes[0].len(), 1);
        assert_eq!(s.current_take.len(), 1, "and a new one has begun");
    }

    #[test]
    fn an_empty_take_is_not_committed() {
        let log = logger();
        let mut s = sinko_state();
        arm(&mut s);
        close_take(&mut s, &log);
        assert!(s.takes.is_empty(), "a bar with no taps is not a take");
    }

    #[test]
    fn committed_takes_stack_into_decaying_layers() {
        let log = logger();
        let mut s = sinko_state();
        assert_eq!(s.sinko_smooth, 1, "one take per layer is the default");

        for i in 0..3u32 {
            s.current_take = vec![i * 240];
            close_take(&mut s, &log);
        }

        // Three takes, three layers, newest loudest and each older one a step
        // quieter: the overlapping-takes sound this panel exists for.
        assert_eq!(s.working.layers.len(), 3);
        assert_eq!(s.working.layers[0].gain, 1.0);
        assert!(s.working.layers[1].gain < s.working.layers[0].gain);
        assert!(s.working.layers[2].gain < s.working.layers[1].gain);
        assert!(s.working.layers[0].hit(2), "the newest take's own hit");
    }

    #[test]
    fn two_takes_inside_the_window_average_into_one_layer() {
        let log = logger();
        let mut s = sinko_state();
        s.sinko_smooth = 2;
        // The same two-hit gesture, played a little late the second time.
        s.current_take = vec![0, 480];
        close_take(&mut s, &log);
        s.current_take = vec![40, 520];
        close_take(&mut s, &log);

        // Averaging is by ordinal, so the layer is the mean gesture: (0+40)/2 =
        // 20 ticks and (480+520)/2 = 500, which are cells 0 and 2 of 16.
        assert_eq!(s.working.layers.len(), 1, "both takes are inside the window");
        assert!(s.working.layers[0].hit(0));
        assert!(s.working.layers[0].hit(2), "500 ticks is cell 2 of 16");
        assert_eq!(s.working.layers[0].gain, 1.0, "an average is not decayed");
    }

    #[test]
    fn smoothing_averages_recent_takes_instead_of_stacking_them() {
        let log = logger();
        let mut s = sinko_state();
        s.sinko_smooth = 3;
        // The same figure three times, a few ticks apart each time.
        for jitter in [0u32, 20, 40] {
            s.current_take = vec![0 + jitter, 960 + jitter];
            close_take(&mut s, &log);
        }
        assert_eq!(s.takes.len(), 3);
        // Averaged into one top layer rather than stacked into three.
        assert_eq!(s.working.layers.len(), 1);
        assert!(s.working.layers[0].hit(0));
        assert!(s.working.layers[0].hit(4), "960 ticks is cell 4 of 16");
        assert_eq!(s.working.layers[0].gain, 1.0);
    }

    #[test]
    fn a_take_outside_the_averaging_window_sits_underneath_quieter() {
        let log = logger();
        let mut s = sinko_state();
        s.sinko_smooth = 2;

        // Two takes, both inside the window: one averaged layer.
        s.current_take = vec![0];
        close_take(&mut s, &log);
        s.current_take = vec![960];
        close_take(&mut s, &log);
        assert_eq!(s.working.layers.len(), 1);

        // A third pushes the first out of the window, where it becomes a
        // quieter layer of its own instead of vanishing.
        s.current_take = vec![1920];
        close_take(&mut s, &log);
        assert_eq!(s.working.layers.len(), 2);
        assert_eq!(s.working.layers[0].gain, 1.0, "the averaged top layer");
        assert!(
            (s.working.layers[1].gain - rhythm::TAKE_DECAY).abs() < 1e-6,
            "the settled take, one step down"
        );
        assert!(s.working.layers[1].hit(0), "and it is the first take");
    }

    #[test]
    fn the_layer_stack_is_capped_at_the_voice_groups() {
        let log = logger();
        let mut s = sinko_state();
        s.sinko_smooth = 1;
        for i in 0..8u32 {
            s.current_take = vec![i * 240];
            close_take(&mut s, &log);
        }
        assert_eq!(
            s.working.layers.len(),
            SINKO_LAYER_ROWS,
            "a deeper stack than the synth has voice groups would be silently cut"
        );
    }

    // ---- Assigning, saving, offsets and the grid ----

    #[test]
    fn assigning_a_pattern_cycles_through_none_and_the_library() {
        let log = logger();
        let mut s = sinko_state();
        assert_eq!(assigned_in(&s), None, "a fresh entry has no pattern");

        cycle_assigned_pattern(&mut s, 1, &log);
        assert_eq!(assigned_in(&s).as_deref(), Some("Quarters"));
        cycle_assigned_pattern(&mut s, 1, &log);
        assert_eq!(assigned_in(&s).as_deref(), Some("Eighths"));

        // Backwards from the first entry lands on "none", so clearing an
        // assignment needs no row of its own.
        cycle_assigned_pattern(&mut s, -1, &log);
        cycle_assigned_pattern(&mut s, -1, &log);
        assert_eq!(assigned_in(&s), None);

        // And each cycle is one undoable edit.
        assert!(s.progression.lock().unwrap().undo());
        assert!(assigned_in(&s).is_some(), "undo brings the pattern back");
    }

    #[test]
    fn assigning_needs_a_chord_to_assign_to() {
        let log = logger();
        let mut s = sinko_state();
        s.progression.lock().unwrap().slots = vec![Slot::Rest];
        s.progression_row = 0;
        cycle_assigned_pattern(&mut s, 1, &log);
        assert!(s.is_flashing(), "a rest must refuse rather than act");
    }

    #[test]
    fn saving_a_pattern_writes_it_to_the_library_and_assigns_it() {
        let log = logger();
        let mut s = sinko_state();
        s.rhythm_path = unique_export_dir("rhythm-save").join("rhythms.toml");
        s.current_take = vec![0, 480, 960];
        close_take(&mut s, &log);

        save_working_pattern(&mut s, "Tapped", &log);

        assert_eq!(assigned_in(&s).as_deref(), Some("Tapped"));
        assert!(s.rhythm_path.exists(), "the library must be written");

        // Read it back: the file is the durable half of the feature.
        let store = RhythmStore::load(&s.rhythm_path).unwrap();
        let saved = store.find("Tapped").expect("the saved pattern");
        assert!(saved.any_hit());
        assert_eq!(saved.steps_per_bar(), rhythm::DEFAULT_STEPS);
    }

    #[test]
    fn saving_needs_a_chord_to_assign_to() {
        let log = logger();
        let mut s = sinko_state();
        s.rhythm_path = unique_export_dir("rhythm-orphan").join("rhythms.toml");
        s.progression.lock().unwrap().slots = vec![Slot::Rest];
        s.progression_row = 0;
        s.current_take = vec![0];
        close_take(&mut s, &log);

        save_working_pattern(&mut s, "Orphan", &log);
        assert!(
            !s.rhythm_store
                .lock()
                .unwrap()
                .patterns
                .iter()
                .any(|p| p.name == "Orphan"),
            "a pattern nobody can hear should not be written"
        );
    }

    #[test]
    fn saving_never_overwrites_a_pattern_of_the_same_name() {
        let log = logger();
        let mut s = sinko_state();
        s.rhythm_path = unique_export_dir("rhythm-unique").join("rhythms.toml");
        {
            let mut store = s.rhythm_store.lock().unwrap();
            store.add(RhythmPattern::from_step_string("Mine", 0.5, "x---").unwrap());
        }
        s.current_take = vec![0];
        close_take(&mut s, &log);

        save_working_pattern(&mut s, "Mine", &log);
        assert_eq!(assigned_in(&s).as_deref(), Some("Mine 2"));
    }

    #[test]
    fn a_silent_pattern_is_refused_rather_than_saved() {
        let log = logger();
        let mut s = sinko_state();
        s.rhythm_path = unique_export_dir("rhythm-silent").join("rhythms.toml");

        save_working_pattern(&mut s, "Nothing", &log);
        assert_eq!(assigned_in(&s), None);
        assert!(!s.rhythm_path.exists(), "nothing should have been written");
        assert!(s.is_flashing());
    }

    #[test]
    fn a_muted_tail_reaches_the_sound_without_saving() {
        // End to end: mute the last quarter in the panel and check the
        // arrangement that playback and export read actually drops the tail.
        let log = logger();
        let mut s = sinko_state();
        s.rhythm_path = unique_export_dir("rhythm-mute").join("rhythms.toml");
        new_pattern(&mut s, &log);
        s.current_take = vec![0, 960, 1920, 2880];
        close_take(&mut s, &log);

        s.sinko_row = SINKO_ROW_MUTE;
        for _ in 0..5 {
            adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        }
        assert_eq!(s.working.mute_ticks, rhythm::MAX_MUTE_TICKS, "a quarter");
        assert_eq!(
            sinko_pattern(&s).expect("the row's own rhythm").mute_ticks,
            rhythm::MAX_MUTE_TICKS,
            "and the entry plays it"
        );

        let (slots, key) = {
            let prog = s.progression.lock().unwrap();
            (prog.slots.clone(), s.transport.key())
        };
        let plan = crate::arrangement::arrangement(&slots, &key, 1.0);
        let vi: Vec<&crate::arrangement::Stab> = plan
            .iter()
            .filter(|stab| stab.notes == vec![69, 72, 76])
            .collect();
        // The hit that starts inside the muted tail is dropped, so four on the
        // floor plays three.
        assert_eq!(vi.len(), 3, "the hit inside the muted tail is dropped");
        let bar_end = 2 * BAR_TICKS + BAR_TICKS - rhythm::MAX_MUTE_TICKS as u64;
        for stab in vi {
            assert!(
                stab.end() <= bar_end,
                "the muted tail must not sound: {:?}",
                stab
            );
        }
    }

    #[test]
    fn the_offset_nudges_along_the_note_ladder_whatever_the_pattern_is() {
        let log = logger();
        let mut s = sinko_state();
        assign(&mut s, 2, "Eighths");

        // Off the beat, a 32nd at a time, and it does not matter that the
        // pattern is eighths.
        nudge_offset(&mut s, 1, &log);
        assert_eq!(offset_in(&s), 120, "a 32nd late");
        nudge_offset(&mut s, 1, &log);
        assert_eq!(offset_in(&s), 240, "a 16th");
        nudge_offset(&mut s, 1, &log);
        assert_eq!(offset_in(&s), 480, "an 8th");
        nudge_offset(&mut s, 1, &log);
        assert_eq!(offset_in(&s), 720, "a 3/16");
        nudge_offset(&mut s, 1, &log);
        assert_eq!(offset_in(&s), 960, "a quarter");

        // And back down the ladder, through the downbeat, to the early side.
        for _ in 0..5 {
            nudge_offset(&mut s, -1, &log);
        }
        assert_eq!(offset_in(&s), 0, "back on the downbeat");
        nudge_offset(&mut s, -1, &log);
        assert_eq!(offset_in(&s), -120, "a 32nd early");

        // The same ladder with no pattern assigned at all.
        s.progression.lock().unwrap().assign_pattern(2, None);
        nudge_offset(&mut s, -1, &log);
        assert_eq!(offset_in(&s), -240, "still the ladder");
    }

    #[test]
    fn the_offset_nudge_is_clamped_to_a_whole_note() {
        let log = logger();
        let mut s = sinko_state();
        for _ in 0..40 {
            nudge_offset(&mut s, 1, &log);
        }
        assert_eq!(offset_in(&s), crate::progression::MAX_OFFSET_TICKS);
        for _ in 0..80 {
            nudge_offset(&mut s, -1, &log);
        }
        assert_eq!(offset_in(&s), -crate::progression::MAX_OFFSET_TICKS);
    }

    #[test]
    fn the_offset_reads_as_a_signed_note_value() {
        assert_eq!(format_offset(0), "0");
        assert_eq!(format_offset(-120), "-1/32");
        assert_eq!(format_offset(240), "+1/16");
        assert_eq!(format_offset(-480), "-1/8");
        assert_eq!(format_offset(720), "+3/16");
        assert_eq!(format_offset(-960), "-1/4");
        assert_eq!(format_offset(1920), "+1/2");
        assert_eq!(format_offset(-3840), "-whole");
        // A value off the ladder (a hand-edited file) falls back to ticks.
        assert_eq!(format_offset(-300), "-300t");
        assert_eq!(format_offset_with_ticks(-480), "-1/8  (480 ticks)");
        assert_eq!(format_offset_with_ticks(0), "0");
    }

    #[test]
    fn the_resolution_reads_as_a_note_value() {
        assert_eq!(format_resolution(2), "1/2");
        assert_eq!(format_resolution(4), "1/4");
        assert_eq!(format_resolution(8), "1/8");
        assert_eq!(format_resolution(16), "1/16");
        assert_eq!(format_resolution(32), "1/32");
        assert_eq!(format_resolution(64), "1/64");
    }

    #[test]
    fn cycling_the_resolution_re_quantizes_the_pattern() {
        let log = logger();
        let mut s = sinko_state();
        new_pattern(&mut s, &log);
        s.current_take = vec![0, 960, 1920, 2880];
        close_take(&mut s, &log);
        assert_eq!(s.working.steps_per_bar(), 16);
        assert!(s.working.layers[0].hit(4), "960 ticks is cell 4 of 16");

        // A hold is ticks, so changing the grid must not change the note.
        s.sinko_row = SINKO_ROW_HOLD;
        s.working.hold = 720;

        s.sinko_row = SINKO_ROW_QUANT;
        adjust_current(&mut s, &SynthParams::defaults(), -1, &log);
        assert_eq!(s.working.steps_per_bar(), 8, "one step coarser");
        assert_eq!(s.working.hold, 720, "and the hold survived the grid change");
        // 960 ticks is cell 2 of 8; the other taps land on 0, 4 and 6.
        assert!(s.working.layers[0].hit(2), "the take was re-quantized");
        let hits: Vec<usize> = (0..8).filter(|i| s.working.layers[0].hit(*i)).collect();
        assert_eq!(hits, vec![0, 2, 4, 6]);
    }

    #[test]
    fn the_sinko_grid_is_drawn_at_the_working_resolution() {
        let mut s = sinko_state();
        // One hit on the downbeat of a four-cell grid.
        s.working = RhythmPattern::from_step_string("g", 0.5, "x---").unwrap();
        let line = sinko_grid_line(&s.working.layers[0].steps, None, None, DEFAULT_SCREEN.grid_budget());
        let width = sinko_cell_width(4, DEFAULT_SCREEN.grid_budget());
        assert_eq!(line.matches('x').count(), width, "one hit cell");
        assert_eq!(line.matches('|').count(), BEATS_PER_BAR as usize + 1);
        assert_eq!(line.matches('·').count(), 3 * width, "and three rests");

        s.working = RhythmPattern::from_step_string("g", 0.5, "xxxxxxxx").unwrap();
        let line = sinko_grid_line(&s.working.layers[0].steps, None, None, DEFAULT_SCREEN.grid_budget());
        assert_eq!(line.matches('x').count(), 8 * sinko_cell_width(8, DEFAULT_SCREEN.grid_budget()));
    }

    #[test]
    fn the_widest_grid_still_fits_the_screen() {
        let steps = vec![true; 64];
        let line = sinko_grid_line(&steps, None, None, DEFAULT_SCREEN.grid_budget());
        assert!(
            line.chars().count() <= 80,
            "a 64-step grid is {} chars",
            line.chars().count()
        );
    }

    #[test]
    fn the_playhead_marks_exactly_one_cell() {
        let steps = vec![true; 16];
        let line = sinko_grid_line(&steps, Some(5), None, DEFAULT_SCREEN.grid_budget());
        let width = sinko_cell_width(16, DEFAULT_SCREEN.grid_budget());
        assert_eq!(
            line.matches('X').count(),
            width,
            "exactly one cell is under the playhead"
        );
        assert_eq!(
            line.matches('x').count(),
            15 * width,
            "every other hit stays plain"
        );
    }

    #[test]
    fn the_playhead_follows_the_click_as_well_as_playback() {
        let log = logger();
        let mut s = sinko_state();
        s.transport.publish_bar(Instant::now(), 0);

        // Stopped, no click: there is no bar to be in the middle of.
        assert_eq!(sinko_playhead(&s, 16), None);

        // A click is a bar clock you can see, so the playhead runs with it.
        toggle_metronome(&mut s, &log);
        assert!(sinko_playhead(&s, 16).is_some());

        // And so is playback, click or no click.
        toggle_metronome(&mut s, &log);
        s.transport.playing.store(true, Ordering::Relaxed);
        assert!(sinko_playhead(&s, 16).is_some());
    }

    #[test]
    fn the_sinko_panel_shows_the_assigned_pattern_and_offset() {
        let mut s = sinko_state();
        assign(&mut s, 2, "Offbeat Eighths");
        s.progression.lock().unwrap().set_offset(2, -480);

        let text = render_sinko(&s);
        assert!(text.contains("Offbeat Eighths"), "rendered:\n{}", text);
        assert!(text.contains("-1/8"), "a 480-tick offset is an eighth");
        assert!(text.contains("(480 ticks)"), "rendered:\n{}", text);
    }

    #[test]
    fn a_saved_pattern_is_what_was_tapped() {
        // End to end: tap a take, save it, and check the arrangement the
        // scheduler and the exporter read plays it back.
        let log = logger();
        let mut s = sinko_state();
        s.rhythm_path = unique_export_dir("rhythm-end-to-end").join("rhythms.toml");
        s.current_take = vec![0, 960, 1920, 2880];
        close_take(&mut s, &log);
        save_working_pattern(&mut s, "Four on the floor", &log);

        let (slots, key) = {
            let prog = s.progression.lock().unwrap();
            (prog.slots.clone(), s.transport.key())
        };
        let plan = crate::arrangement::arrangement(&slots, &key, 1.0);
        // Sixteen steps: the four taps land on cells 0, 4, 8 and 12, and the
        // row that owns them is the third bar of the loop.
        let vi_starts: Vec<u64> = plan
            .iter()
            .filter(|stab| stab.notes == vec![69, 72, 76])
            .map(|stab| stab.start)
            .collect();
        let third_bar = 2 * BAR_TICKS;
        assert_eq!(
            vi_starts,
            vec![
                third_bar,
                third_bar + 960,
                third_bar + 1920,
                third_bar + 2880
            ]
        );
    }

    #[test]
    fn a_panel_edit_reaches_the_sound_without_saving() {
        // The regression: the panel edited a draft that nothing played, so a
        // hold or a mute changed the numbers on screen and nothing else. The
        // entry the scheduler reads is what an edit has to reach — and it has to
        // reach it *now*, without a save.
        let log = logger();
        let mut s = sinko_state();
        s.rhythm_path = unique_export_dir("rhythm-live").join("rhythms.toml");

        // A rhythm owned by the selected slot, as [New Pattern] leaves it.
        new_pattern(&mut s, &log);
        s.current_take = vec![0, 960, 1920, 2880];
        close_take(&mut s, &log);

        let hold_before = sinko_pattern(&s).expect("the row's own rhythm").hold;
        assert_eq!(hold_before, rhythm::DEFAULT_HOLD);

        // Nudge the hold. Nothing is saved by hand.
        s.sinko_row = SINKO_ROW_HOLD;
        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        let hold_after = sinko_pattern(&s).expect("still there").hold;
        assert_ne!(hold_after, hold_before, "the chord's rhythm has changed");
        assert_eq!(hold_after, 480, "and the entry holds it");
        assert!(
            !s.rhythm_path.exists(),
            "an edit belongs to the session, not to the palette file"
        );

        // Which means the arrangement that playback and export read has it too.
        let (slots, key) = {
            let prog = s.progression.lock().unwrap();
            (prog.slots.clone(), s.transport.key())
        };
        let plan = crate::arrangement::arrangement(&slots, &key, 1.0);
        let mine: Vec<u64> = plan
            .iter()
            .filter(|stab| stab.notes == vec![69, 72, 76])
            .map(|stab| stab.duration)
            .collect();
        assert!(!mine.is_empty(), "the row plays the pattern");
        assert!(
            mine.iter().all(|duration| *duration == 480),
            "every hit holds the edited {} ticks: {:?}",
            hold_after,
            mine
        );
    }

    #[test]
    fn a_note_value_is_named_for_the_values_a_hold_lands_on() {
        assert_eq!(note_value(120), Some("1/32"));
        assert_eq!(note_value(960), Some("1/4"));
        assert_eq!(note_value(1920), Some("1/2"));
        assert_eq!(note_value(2880), Some("3/4"));
        assert_eq!(note_value(BAR_TICKS), Some("whole"));
        assert_eq!(note_value(720), Some("3/16"), "the player's own word for it");
        assert_eq!(note_value(1440), Some("3/8"));
        assert_eq!(note_value(300), None);
        assert_eq!(format_ticks(0), "none");
        assert_eq!(format_ticks(1920), "1/2  —  1920 ticks");
        assert_eq!(format_ticks(300), "300 ticks");
    }

    // ---- the transport tap key ----

    /// Age the tracker past the resolve threshold without sleeping.
    fn age_taps(taps: &mut TapTracker) {
        taps.last_tap = taps
            .last_tap
            .map(|_| Instant::now() - Duration::from_millis(TAP_THRESHOLD_MS as u64 + 10));
    }

    #[test]
    fn one_tap_plays_or_pauses() {
        let mut taps = TapTracker::default();
        assert_eq!(taps.resolve(), None, "nothing tapped yet");

        taps.tap();
        assert_eq!(taps.resolve(), None, "a single tap waits for the threshold");
        age_taps(&mut taps);
        assert_eq!(taps.resolve(), Some(TapAction::Toggle));
        assert_eq!(taps.resolve(), None, "the count resets after resolving");
    }

    #[test]
    fn two_taps_restart_from_the_top() {
        let mut taps = TapTracker::default();
        taps.tap();
        taps.tap();
        age_taps(&mut taps);
        assert_eq!(taps.resolve(), Some(TapAction::Restart));
    }

    #[test]
    fn three_or_more_taps_seek_to_the_middle() {
        for count in [3, 4, 5] {
            let mut taps = TapTracker::default();
            for _ in 0..count {
                taps.tap();
            }
            age_taps(&mut taps);
            assert_eq!(taps.resolve(), Some(TapAction::SeekMiddle), "{} taps", count);
        }
    }

    #[test]
    fn taps_apart_by_more_than_the_threshold_do_not_accumulate() {
        let mut taps = TapTracker::default();
        taps.tap();
        age_taps(&mut taps);
        assert_eq!(taps.resolve(), Some(TapAction::Toggle));

        // A later press starts a fresh count rather than becoming a double tap.
        taps.tap();
        age_taps(&mut taps);
        assert_eq!(taps.resolve(), Some(TapAction::Toggle));
    }

    #[test]
    fn the_transport_key_feeds_the_tap_tracker() {
        let log = logger();
        let mut s = state(Focus::Transport);
        handle_hotkey(&mut s, Hotkey::TransportTap, false, &log);
        assert_eq!(s.taps.count, 1);

        handle_hotkey(&mut s, Hotkey::TransportTap, false, &log);
        age_taps(&mut s.taps);
        assert_eq!(
            s.taps.resolve(),
            Some(TapAction::Restart),
            "two presses of {{ restart from bar 1"
        );
    }

    #[test]
    fn both_braces_feed_the_transport_tap() {
        // The input loop's two steps: character to physical position, position
        // to action. `}` is the neighbour of `{` and is bound to the same one.
        let log = logger();
        for typed in ['{', '}'] {
            let mut s = state(Focus::Transport);
            let pos = ACTIVE_LAYOUT
                .position(typed)
                .unwrap_or_else(|| panic!("{:?} is unmapped", typed));
            let hotkey = pos.hotkey().expect("a transport key");
            handle_hotkey(&mut s, hotkey, false, &log);
            assert_eq!(s.taps.count, 1, "{:?} should tap the transport", typed);
        }
    }

    #[test]
    fn the_transport_key_is_inert_outside_the_loop_but_never_a_chord() {
        // It is a hotkey, so it must never reach the held set.
        let log = logger();
        let mut s = state(Focus::Transport);
        s.held.insert(KeyPosition::LeftIndex);
        handle_hotkey(&mut s, Hotkey::TransportTap, false, &log);
        assert_eq!(s.held.len(), 1, "the held set is untouched");
        assert_eq!(resolved_chord(&s), Some((ScaleDegree::I, None)));
    }

    // ---- the space bar ----

    #[test]
    fn space_latches_both_hands_and_frees_them() {
        let log = logger();
        let mut s = state(Focus::Transport);
        s.held.insert(KeyPosition::LeftIndex);
        s.held.insert(KeyPosition::RightMiddle);
        let before = resolved_chord(&s);

        space_pressed(&mut s, &log);
        assert_eq!(s.registers.left, Some([KeyPosition::LeftIndex].into()));
        assert_eq!(s.registers.right, Some([KeyPosition::RightMiddle].into()));

        // Hands off: the same chord keeps sounding from the registers alone.
        s.held.clear();
        s.update_live_chord();
        assert_eq!(resolved_chord(&s), before);
        assert_eq!(
            s.transport.live_chord.lock().unwrap().clone(),
            chord_notes(before, &s.transport.key())
        );
    }

    #[test]
    fn space_twice_clears_both_registers() {
        let log = logger();
        let mut s = state(Focus::Transport);
        s.held.insert(KeyPosition::LeftIndex);
        space_pressed(&mut s, &log);
        assert_eq!(s.registers.left, Some([KeyPosition::LeftIndex].into()));

        // Nothing under the hands now, so the second press clears rather than
        // forgetting: `Some(empty)` is the explicitly-cleared state.
        s.held.clear();
        space_pressed(&mut s, &log);
        assert_eq!(s.registers.left, Some(Default::default()));
        assert_eq!(s.registers.right, Some(Default::default()));
        s.update_live_chord();
        assert_eq!(s.transport.live_chord.lock().unwrap().clone(), None);
    }

    #[test]
    fn space_over_one_hand_leaves_the_other_open() {
        // Pressing it with only the left hand down latches the left and clears
        // the right, which is what makes it a toggle rather than a one-way door.
        let log = logger();
        let mut s = state(Focus::Transport);
        s.held.insert(KeyPosition::RightIndex);
        space_pressed(&mut s, &log);
        assert_eq!(s.registers.right, Some([KeyPosition::RightIndex].into()));
        assert_eq!(s.registers.left, Some(Default::default()));
        assert_eq!(resolved_chord(&s), None, "a right hand alone names no degree");
    }

    #[test]
    fn space_does_nothing_while_a_prompt_is_open() {
        let log = logger();
        let mut s = state(Focus::Transport);
        s.held.insert(KeyPosition::LeftIndex);
        s.modal = Some(Modal::AddRest);

        space_pressed(&mut s, &log);
        assert_eq!(s.registers.left, None, "the latch must not fire behind a prompt");
        assert_eq!(s.registers.right, None);
    }

    #[test]
    fn the_playing_row_is_the_play_control() {
        let log = logger();
        let mut s = state(Focus::Transport);
        s.set_current_row(TRANSPORT_ROW_PLAYING);
        assert!(row_is_action_button(&s), "Enter reaches it with a chord held");

        toggle_playback(&mut s, &log);
        assert!(s.transport.playing.load(Ordering::Relaxed));
        toggle_playback(&mut s, &log);
        assert!(!s.transport.playing.load(Ordering::Relaxed));

        // Left/right on the row does the same thing.
        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert!(s.transport.playing.load(Ordering::Relaxed));
    }

    #[test]
    fn the_registers_sit_under_their_own_hands() {
        // The hand's keys decide the columns: `L` starts under the left hand's
        // first key and `R` under the right hand's, so the row reads as two labels
        // under the keyboard.
        let mut s = state(Focus::Transport);
        for pos in [
            KeyPosition::LeftPinky,
            KeyPosition::LeftRing,
            KeyPosition::LeftMiddle,
            KeyPosition::RightIndex,
            KeyPosition::RightMiddle,
        ] {
            s.held.insert(pos);
        }
        s.registers.lock_both(&s.held);
        s.held.clear();

        let text = render_frame_at(&s, width_screen(120));
        let lines: Vec<&str> = text.lines().take(4).collect();
        let keyboard = lines[1];
        let registers = lines[2];

        let column_of = |line: &str, needle: char| {
            visible_text(line)
                .chars()
                .position(|c| c == needle)
                .unwrap_or_else(|| panic!("no {:?} in {:?}", needle, line))
        };
        // Each label sits at the left edge of the cell holding the key above it: a
        // key's glyph is one column into its three-column cell, and the label is at
        // the cell's edge.
        assert_eq!(
            column_of(registers, 'L') + 1,
            column_of(keyboard, 'a'),
            "L under the left hand:\n{}\n{}",
            keyboard,
            registers
        );
        assert_eq!(
            column_of(registers, 'R') + 1,
            column_of(keyboard, 'h'),
            "R under the right hand:\n{}\n{}",
            keyboard,
            registers
        );
        assert!(
            registers.contains("[ads]") && registers.contains("[jk]"),
            "each hand's keys: {:?}",
            registers
        );
    }

    #[test]
    fn the_chord_readout_sits_above_the_chord_list() {
        // What the registers add up to is the heading of the panel that plays it,
        // so it is at the left margin rather than centred with the keyboard.
        let mut s = state(Focus::Transport);
        for pos in [
            KeyPosition::LeftPinky,
            KeyPosition::LeftRing,
            KeyPosition::LeftMiddle,
        ] {
            s.held.insert(pos);
        }
        let text = render_frame_at(&s, width_screen(120));
        let lines: Vec<&str> = text.lines().take(5).collect();

        // Four header rows: title, keyboard, registers, chord.
        assert!(lines[0].contains("Chord Tool"), "{:?}", lines);
        assert!(lines[1].contains('a'), "{:?}", lines);
        assert!(lines[2].contains('L'), "{:?}", lines);
        assert!(lines[3].contains("(vii)"), "{:?}", lines);

        // The chord is hard left, over the chord list below it, and the progression
        // header follows on the next row.
        let indent = |line: &str| line.chars().take_while(|c| *c == ' ').count();
        assert_eq!(indent(lines[3]), KEY_ROW_INDENT.len(), "{:?}", lines[3]);
        assert!(
            text.lines().nth(4).unwrap().starts_with("── Progression"),
            "{:?}",
            text.lines().nth(4)
        );
    }

    #[test]
    fn the_keyboard_layout_adds_up() {
        // The keyboard's columns are derived from these constants, and one of them
        // cannot be taken from the string it describes: `str::len` counts bytes,
        // and the bar between the hands is three of them. Hence this check.
        assert_eq!(KEY_COLUMN_GAP.chars().count(), KEY_COLUMN_GAP_WIDTH);
        assert_eq!(KEY_ROW_INDENT.chars().count(), KEY_ROW_INDENT.len());
        // One key is a three-character cell plus the space after it. `draw_key`
        // draws them; if it ever draws a different shape, this fails.
        let mut out: Vec<u8> = Vec::new();
        let s = state(Focus::Transport);
        for pos in [KeyPosition::LeftPinky] {
            draw_key(&mut out, pos, false).unwrap();
        }
        assert_eq!(visible_width(&String::from_utf8(out).unwrap()), KEY_WIDTH);

        // And the register row fits the block it is drawn in.
        let row = register_row(&s, &Key::new(60, Scale::Major));
        assert!(
            visible_width(&row) <= HEADER_BLOCK_WIDTH,
            "{} columns: {:?}",
            visible_width(&row),
            row
        );
    }

    #[test]
    fn the_keyboard_and_the_registers_are_centred_together() {
        // One block, one axis: the two rows share a left column, and the pair is
        // centred on the window. The width is a constant, so a chord whose label
        // grows cannot shuffle the keyboard sideways.
        let width = 132;
        let mut s = state(Focus::Transport);
        for pos in [
            KeyPosition::LeftPinky,
            KeyPosition::LeftRing,
            KeyPosition::LeftMiddle,
        ] {
            s.held.insert(pos);
        }
        s.registers.lock_both(&s.held);
        // Released: a held key draws a space before its letter, which would stand
        // in for the row's own indent in the comparison below.
        s.held.clear();
        let text = render_frame_at(&s, width_screen(width));
        let lines: Vec<&str> = text.lines().take(3).collect();

        let indent = |line: &str| line.chars().take_while(|c| *c == ' ').count();
        assert_eq!(
            indent(lines[1]),
            indent(lines[2]),
            "the registers start where the keyboard does:\n{}\n{}",
            lines[1],
            lines[2]
        );
        // The row's own indent is part of its leading space, so the pad is what
        // has to match the centring.
        let want = width.saturating_sub(HEADER_BLOCK_WIDTH) / 2;
        assert_eq!(
            indent(lines[1]) - KEY_ROW_INDENT.len(),
            want,
            "and the block is centred"
        );

        // Twenty columns wider moves it ten right, and a different chord does not
        // move it at all.
        let wider = render_frame_at(&s, width_screen(width + 20));
        assert_eq!(
            indent(wider.lines().nth(1).unwrap()) - KEY_ROW_INDENT.len(),
            want + 10
        );

        s.held.insert(KeyPosition::LeftIndex);
        s.held.insert(KeyPosition::LeftInner);
        let chordier = render_frame_at(&s, width_screen(width));
        assert_eq!(
            indent(chordier.lines().nth(1).unwrap()) - KEY_ROW_INDENT.len(),
            want,
            "the keyboard does not move when the chord changes length"
        );
    }

    // ---- the window ----

    /// The app forces colour on at startup, because `NO_COLOR` is a convention
    /// for piped text rather than for a full-screen instrument. The suite does
    /// the same: otherwise an assertion about styling would pass or fail
    /// depending on the shell the tests were run from.
    fn colour_on() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| crossterm::style::force_color_output(true));
    }

    #[test]
    fn draw_columns_lines_the_two_panels_up() {
        let mut out: Vec<u8> = Vec::new();
        draw_columns(&mut out, "ab\nlonger line", "R1\nR2\nR3", DEFAULT_SCREEN).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();

        // As many rows as the taller column: the pair costs one block, not two.
        assert_eq!(lines.len(), 3, "rendered:\n{}", text);

        // The left column starts at the margin, as wide as its widest line, so the
        // right column begins in the same place on every row.
        assert!(lines[0].starts_with("ab"), "{:?}", lines);
        assert!(lines[1].starts_with("longer line"), "{:?}", lines);
        for (index, line) in lines.iter().enumerate() {
            assert!(
                line.ends_with(&format!("R{}", index + 1)),
                "the right column sits at the edge: {:?}",
                line
            );
            assert_eq!(
                visible_text(line).chars().position(|c| c == 'R'),
                Some(DEFAULT_SCREEN.width - 2),
                "and starts in the same column on every row: {:?}",
                line
            );
            assert_eq!(line.trim_end(), *line, "no trailing space: {:?}", line);
        }
    }

    #[test]
    fn draw_columns_measures_visible_width_not_bytes() {
        // A colour code has no width and must not shift the column; neither may
        // the em dashes elsewhere on screen, which are three bytes each.
        let mut out: Vec<u8> = Vec::new();
        draw_columns(&mut out, "\x1b[36m—ab\x1b[0m\n—cd", "R1\nR2", DEFAULT_SCREEN).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();

        // Visible columns, not characters: the escape sequences are characters
        // too, and counting them is the mistake this test exists to catch.
        let start = |line: &str| visible_text(line).chars().position(|c| c == 'R').unwrap();
        assert_eq!(start(lines[0]), start(lines[1]), "rendered: {}", text);
        assert_eq!(start(lines[0]), DEFAULT_SCREEN.width - 2);
    }

    #[test]
    fn draw_columns_squeezes_the_right_column_first() {
        // The left column is the chord list, so it keeps its width; the transport
        // on the right is what gives, because its long lines are statuses rather
        // than data.
        let mut out: Vec<u8> = Vec::new();
        draw_columns(&mut out, &"L".repeat(40), &"R".repeat(60), DEFAULT_SCREEN).unwrap();
        let text = String::from_utf8(out).unwrap();
        let line = text.lines().next().unwrap();

        assert_eq!(
            line.chars().filter(|c| *c == 'L').count(),
            40,
            "the left column is untouched: {:?}",
            line
        );
        let room = DEFAULT_SCREEN.width - 40 - PANEL_COLUMN_GAP;
        assert_eq!(
            line.chars().filter(|c| *c == 'R').count(),
            room - 1,
            "the right column was cut to what was left (minus its ellipsis): {:?}",
            line
        );
        assert!(line.contains('…'), "and says so: {:?}", line);
        assert_eq!(visible_width(line), DEFAULT_SCREEN.width);
    }

    #[test]
    fn draw_columns_stacks_when_the_pair_will_not_fit() {
        // A long enough rhythm name pushes the chord list past the width, so the
        // transport's floor cannot be met beside it: one above the other beats
        // clipping either of them to nothing.
        let mut out: Vec<u8> = Vec::new();
        draw_columns(&mut out, &"L".repeat(70), &"R".repeat(40), DEFAULT_SCREEN).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();

        assert_eq!(lines.len(), 2, "stacked, not side by side: {:?}", lines);
        assert!(lines[0].starts_with('L'), "{:?}", lines);
        assert!(lines[1].starts_with('R'), "{:?}", lines);
        for line in lines {
            assert!(
                visible_width(line) <= MIN_SCREEN_WIDTH,
                "{:?} wraps",
                line
            );
        }
    }

    #[test]
    fn draw_columns_clips_rather_than_wrapping() {
        // A line too long for any pairing is cut and says so, never pushed into a
        // wrap that would cost rows.
        let mut out: Vec<u8> = Vec::new();
        draw_columns(&mut out, &"x".repeat(200), "R", DEFAULT_SCREEN).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();

        assert_eq!(lines.len(), 2, "stacked: {:?}", lines);
        assert!(visible_width(lines[0]) <= DEFAULT_SCREEN.width);
        assert!(lines[0].contains('…'), "it should say it was cut: {:?}", lines[0]);
        assert_eq!(lines[1], "R");
    }

    #[test]
    fn no_line_exceeds_the_window_at_any_width() {
        // The contract: whatever the window, every row fits it. A wrapped row
        // costs two, shifts everything below it and pushes the bottom panels off
        // the screen — so this is checked from the minimum to a very wide window,
        // and for every panel in turn.
        let chords = [
            ScaleDegree::I,
            ScaleDegree::V,
            ScaleDegree::VI,
            ScaleDegree::IV,
        ];
        for width in [MIN_SCREEN_WIDTH, 100, 140, 200] {
            let screen = width_screen(width);
            for focus in [
                Focus::Transport,
                Focus::Progression,
                Focus::Sinko,
                Focus::Synth,
                Focus::SynthPresets,
            ] {
                let mut s = state(focus);
                seed(&mut s, &chords);
                let text = render_frame_at(&s, screen);
                for line in text.lines() {
                    assert!(
                        line.chars().count() <= width,
                        "{:?} at {} columns: {:?} ({} wide)",
                        focus,
                        width,
                        line,
                        line.chars().count()
                    );
                }
            }
        }
    }

    #[test]
    fn a_window_too_small_is_told_so_instead_of_a_wrapped_frame() {
        // Below the minimum the layout cannot be drawn, so it says what is missing
        // rather than printing a mess that hides its own keys.
        let narrow = Screen {
            width: 72,
            height: 12,
        };
        assert!(!narrow.fits());
        let text = render_frame_at(&state(Focus::Transport), narrow);

        assert!(text.contains("Terminal too narrow"), "{}", text);
        assert!(text.contains("72 × 12"), "it names what it has: {}", text);
        assert!(
            text.contains("80 columns, 16 rows"),
            "and what it needs: {}",
            text
        );
        assert!(!text.contains("Transport"), "no panels are drawn: {}", text);
        for line in text.lines() {
            assert!(visible_width(line) <= 72, "{:?}", line);
        }

        // One column short of the minimum is still too narrow; at it, the layout is
        // drawn.
        assert!(!Screen { width: 79, height: 40 }.fits());
        assert!(Screen { width: 80, height: 16 }.fits());
        assert!(
            render_frame_at(&state(Focus::Transport), width_screen(MIN_SCREEN_WIDTH))
                .contains("Chord Tool")
        );
    }

    #[test]
    fn a_wider_window_gives_the_grid_wider_cells() {
        // Where the extra columns go: not into a wider margin but into the bar
        // grid, which is the one thing on screen that is a drawing.
        let narrow = DEFAULT_SCREEN.grid_budget();
        let wide = width_screen(200).grid_budget();
        assert!(wide > narrow, "{} should beat {}", wide, narrow);

        let steps = vec![true; 16];
        let small = sinko_grid_line(&steps, None, None, narrow);
        let large = sinko_grid_line(&steps, None, None, wide);
        assert!(
            visible_width(&large) > visible_width(&small),
            "{} vs {}",
            visible_width(&large),
            visible_width(&small)
        );
        // The minimum width is where the budget starts, and a very wide window
        // stops growing rather than spilling off the screen.
        assert!(DEFAULT_SCREEN.grid_budget() >= 48, "a usable grid");
        assert_eq!(width_screen(1000).grid_budget(), GRID_BUDGET_MAX);
    }

    #[test]
    fn the_register_row_fits_the_design_width_with_everything_on_it() {
        // Both registers, laid out in the width the layout is designed for. The
        // chord is a row of its own now, so this is only the hands' two cells.
        let mut s = state(Focus::Transport);
        for pos in [
            KeyPosition::LeftPinky,
            KeyPosition::LeftRing,
            KeyPosition::LeftMiddle,
            KeyPosition::RightIndex,
            KeyPosition::RightMiddle,
        ] {
            s.held.insert(pos);
        }
        s.registers.lock_both(&s.held);
        s.held.clear();

        let text = render_frame_at(&s, DEFAULT_SCREEN);
        let row = text
            .lines()
            .find(|line| line.contains(" L "))
            .expect("the register row");

        assert!(row.contains("[ads]"), "the left register: {:?}", row);
        assert!(row.contains("[jk]"), "the right register: {:?}", row);
        assert!(
            visible_width(row) <= MIN_SCREEN_WIDTH,
            "{} columns: {:?}",
            visible_width(row),
            row
        );
    }

    // ---- where the focus is ----

    #[test]
    fn the_focused_panel_is_marked_by_rule_and_colour() {
        // Two cues rather than one, because they fail differently: a heavier rule
        // survives a terminal that drops colour, and the colour survives a glance
        // that does not read punctuation.
        colour_on();
        let mut s = state(Focus::Transport);
        seed(
            &mut s,
            &[
                ScaleDegree::I,
                ScaleDegree::V,
                ScaleDegree::VI,
                ScaleDegree::IV,
            ],
        );
        let text = render_frame(&s);
        let focused = text
            .lines()
            .find(|line| line.contains(" Transport "))
            .expect("the transport header");
        let idle = text
            .lines()
            .find(|line| line.contains(" Sinko "))
            .expect("the sinko header");

        // The two headers share a row, so each is checked by name.
        assert!(
            focused.contains("━━ Transport ━━"),
            "the focused rule is heavy: {:?}",
            focused
        );
        assert!(
            idle.contains("── Sinko ──"),
            "an idle rule is light: {:?}",
            idle
        );

        // The idle header is dimmed; the focused one is a filled band.
        let raw = {
            let mut out: Vec<u8> = Vec::new();
            render(&mut out, &SynthParams::defaults(), &s, DEFAULT_SCREEN).unwrap();
            String::from_utf8(out).unwrap()
        };
        let focus_line = raw.lines().find(|line| line.contains(" Transport ")).unwrap();
        let idle_line = raw.lines().find(|line| line.contains(" Sinko ")).unwrap();
        assert!(
            focus_line.contains("48;5;"),
            "the focused header fills its background: {:?}",
            focus_line
        );
        assert!(
            !idle_line.contains("48;5;"),
            "an idle header has no background: {:?}",
            idle_line
        );
    }

    #[test]
    fn the_selected_row_is_banded_and_the_others_are_not() {
        // Which row the cursor is on has to be readable without hunting for the
        // `▸`, so the whole row is marked.
        colour_on();
        let mut s = state(Focus::Transport);
        s.transport_row = TRANSPORT_ROW_METRONOME;
        let raw = {
            let mut out: Vec<u8> = Vec::new();
            render(&mut out, &SynthParams::defaults(), &s, DEFAULT_SCREEN).unwrap();
            String::from_utf8(out).unwrap()
        };
        let row = |needle: &str| {
            raw.lines()
                .find(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("no row for {:?}", needle))
                .to_string()
        };

        let selected = row("metronome");
        assert!(selected.contains("▸"), "{:?}", selected);
        assert!(
            selected.contains("48;5;"),
            "the selected row fills its background: {:?}",
            selected
        );
        assert!(selected.contains("\x1b[1m"), "and is bold: {:?}", selected);
        // A row the cursor is not on carries no styling at all.
        let idle = row("track key");
        assert!(!idle.contains("▸"), "{:?}", idle);
        assert!(!idle.contains('\x1b'), "an unselected row is plain: {:?}", idle);
    }

    #[test]
    fn the_live_values_are_coloured() {
        // The chord sounding now, the keys a register holds and the cell the clock
        // is in are the values that change while you play.
        colour_on();
        let mut s = sinko_state();
        s.held.insert(KeyPosition::LeftPinky);
        s.held.insert(KeyPosition::LeftRing);
        s.held.insert(KeyPosition::LeftMiddle);
        s.registers.lock_both(&s.held);
        s.held.clear();
        // A bar clock, so the grid actually has a playhead to colour.
        s.transport.publish_bar(Instant::now(), 0);
        s.transport.playing.store(true, Ordering::Relaxed);

        let raw = {
            let mut out: Vec<u8> = Vec::new();
            render(&mut out, &SynthParams::defaults(), &s, DEFAULT_SCREEN).unwrap();
            String::from_utf8(out).unwrap()
        };
        let register_row = raw
            .lines()
            .find(|line| line.contains('→'))
            .expect("the register row");
        assert!(
            register_row.contains("\x1b[38;5;15m"),
            "the latched keys are white: {:?}",
            register_row
        );

        // The grid carries the playhead's colour, so the position is findable
        // without reading the cell characters.
        let grid = raw.lines().find(|line| line.contains('|')).expect("a grid row");
        assert!(grid.contains('\x1b'), "the grid is styled: {:?}", grid);
    }

    #[test]
    fn the_panels_are_laid_out_along_the_tab_walk() {
        // The chord list and the transport share the first row — list on the left —
        // and the panels below them are stacked in the order `Tab` walks, so focus
        // keeps moving down the screen. The pair is side by side, so which of the
        // two comes first is a layout choice rather than a walk order.
        let chords = [
            ScaleDegree::I,
            ScaleDegree::V,
            ScaleDegree::VI,
            ScaleDegree::IV,
        ];
        let mut s = state(Focus::Transport);
        seed(&mut s, &chords);
        let text = render_frame(&s);
        let lines: Vec<&str> = text.lines().collect();

        let pair = lines
            .iter()
            .position(|line| line.contains(" Progression") && line.contains(" Transport "))
            .expect("the pair should share a row");
        let sinko = lines
            .iter()
            .position(|line| line.contains(" Sinko "))
            .expect("the sinko panel");
        let synth = lines
            .iter()
            .position(|line| line.contains(" Synth "))
            .expect("the synth panel");
        assert!(
            pair < sinko && sinko < synth,
            "the stack is out of order: pair {}, sinko {}, synth {}",
            pair,
            sinko,
            synth
        );

        let row = lines[pair];
        assert!(
            row.find(" Progression").unwrap() < row.find(" Transport ").unwrap(),
            "the chord list belongs in the left column: {:?}",
            row
        );

        // And the walk covers exactly those four panels.
        let mut walked = vec![Focus::Transport];
        let mut focus = Focus::Transport;
        while focus.next() != Focus::Transport {
            focus = focus.next();
            walked.push(focus);
        }
        // From the transport: down the stack, around, and back to the chord list on
        // its left — the layout order, rotated to start where the app opens.
        assert_eq!(
            walked,
            vec![
                Focus::Transport,
                Focus::Sinko,
                Focus::Synth,
                Focus::SynthPresets,
                Focus::Progression
            ]
        );
    }

    #[test]
    fn no_rendered_line_exceeds_the_width_budget() {
        // The layout is a fixed grid: a line wider than the terminal wraps and
        // pushes everything below it down, which is how the height budget gets
        // spent without anything being added. The hint line was 141 columns
        // before this check existed.
        let chords = [
            ScaleDegree::I,
            ScaleDegree::V,
            ScaleDegree::VI,
            ScaleDegree::IV,
        ];
        for focus in [
            Focus::Transport,
            Focus::Progression,
            Focus::Sinko,
            Focus::Synth,
            Focus::SynthPresets,
        ] {
            // Seeded, so the chord rows and their rhythm annotations are in the
            // frame: those are the lines that grow with content.
            let mut s = state(focus);
            seed(&mut s, &chords);
            let text = render_frame(&s);
            for line in text.lines() {
                assert!(
                    line.chars().count() <= 100,
                    "{:?} line wraps at {} columns: {:?}",
                    focus,
                    line.chars().count(),
                    line
                );
            }
        }

        // The widest state there is, because the header row is the one line that
        // can grow: every key latched into both registers *and* the biggest
        // chord the grammar has, so the readout is at its longest as well.
        let mut full = state(Focus::Transport);
        seed(&mut full, &chords);
        for pos in [
            KeyPosition::LeftPinky,
            KeyPosition::LeftRing,
            KeyPosition::LeftMiddle,
            KeyPosition::LeftIndex,
            KeyPosition::LeftInner,
            KeyPosition::RightInner,
            KeyPosition::RightIndex,
            KeyPosition::RightMiddle,
            KeyPosition::RightRing,
            KeyPosition::RightPinky,
        ] {
            full.held.insert(pos);
        }
        full.registers.lock_both(&full.held);
        let text = render_frame(&full);
        let widest = text
            .lines()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0);
        assert!(
            widest <= 100,
            "the fullest header wraps at {} columns:\n{}",
            widest,
            text
        );
        assert!(widest > 60, "the fullest header should be a real measurement");
    }

    // ---- metronome ----

    #[test]
    fn the_metronome_key_toggles_the_click() {
        let log = logger();
        let mut s = state(Focus::Transport);
        assert!(!s.metronome_on);
        assert!(!s.transport.metronome.load(Ordering::Relaxed));

        toggle_metronome(&mut s, &log);
        assert!(s.metronome_on);
        assert!(s.transport.metronome.load(Ordering::Relaxed));

        toggle_metronome(&mut s, &log);
        assert!(!s.metronome_on);
        assert!(!s.transport.metronome.load(Ordering::Relaxed));
    }

    #[test]
    fn the_metronome_toggles_from_any_panel() {
        // A performance control, like the register locks and the tap key.
        let log = logger();
        for focus in [
            Focus::Transport,
            Focus::Progression,
            Focus::Sinko,
            Focus::Synth,
            Focus::SynthPresets,
        ] {
            let mut s = state(focus);
            handle_hotkey(&mut s, Hotkey::MetronomeToggle, false, &log);
            assert!(s.metronome_on, "focus {:?}", focus);
        }
    }

    #[test]
    fn arming_a_take_never_forgets_a_metronome_you_turned_on() {
        let log = logger();
        let mut s = sinko_state();
        toggle_metronome(&mut s, &log);
        assert!(s.metronome_on);

        // Recording needs the click, and it is already on.
        toggle_recording(&mut s, &log);
        assert!(s.transport.metronome.load(Ordering::Relaxed));

        // Stopping the take leaves the user's own switch alone.
        toggle_recording(&mut s, &log);
        assert!(s.metronome_on, "the switch survived the take");
        assert!(
            s.transport.metronome.load(Ordering::Relaxed),
            "and the click keeps running"
        );
    }

    #[test]
    fn recording_runs_the_click_and_gives_it_back() {
        let log = logger();
        let mut s = sinko_state();
        assert!(!s.metronome_on);

        toggle_recording(&mut s, &log);
        assert!(
            s.transport.metronome.load(Ordering::Relaxed),
            "a take needs the beat"
        );

        toggle_recording(&mut s, &log);
        assert!(!s.metronome_on, "the switch was never on");
        assert!(
            !s.transport.metronome.load(Ordering::Relaxed),
            "so the click stops with the take"
        );
    }

    /// The Transport panel's metronome row, trimmed.
    fn metronome_row(state: &AppState) -> String {
        render_transport(state)
            .lines()
            .find(|line| line.contains("metronome"))
            .map(|line| line.trim().to_string())
            .unwrap_or_default()
    }

    #[test]
    fn the_transport_shows_the_metronome_state() {
        let log = logger();
        let mut s = state(Focus::Transport);
        assert!(metronome_row(&s).ends_with("off"), "{:?}", metronome_row(&s));

        toggle_metronome(&mut s, &log);
        assert!(metronome_row(&s).ends_with("on"), "{:?}", metronome_row(&s));
        assert!(!metronome_row(&s).contains("recording"));

        // Forced on by a take: legible as forced, not as a switch that did
        // nothing.
        s.metronome_on = false;
        s.recording = true;
        sync_metronome(&s);
        assert!(
            metronome_row(&s).ends_with("on (recording)"),
            "{:?}",
            metronome_row(&s)
        );
    }

    #[test]
    fn the_transport_metronome_row_is_a_toggle_row() {
        let log = logger();
        let mut s = state(Focus::Transport);
        s.set_current_row(TRANSPORT_ROW_METRONOME);

        assert_eq!(s.current_row(), TRANSPORT_ROW_METRONOME);
        assert!(!row_is_action_button(&s), "a value row, like loop");
        assert!(!enter_commits_chord(&s));

        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert!(s.metronome_on, "left/right toggles it");
    }

    #[test]
    fn the_transport_rows_still_report_their_own_actions() {
        // Inserting the metronome row moved every index after it, which is what
        // the named constants are for.
        let mut s = state(Focus::Transport);
        s.set_current_row(TRANSPORT_ROW_EXPORT);
        assert!(row_is_action_button(&s));
        s.set_current_row(TRANSPORT_ROW_IMPORT);
        assert!(row_is_action_button(&s));
        s.set_current_row(TRANSPORT_ROW_METRONOME);
        assert!(!row_is_action_button(&s));
        assert_eq!(s.row_count(), TRANSPORT_ROWS);
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

    // ---- transport panel rendering ----
    //
    // This panel used to take a `Synth`, which no test can construct (it needs
    // an audio device), so none of it was ever rendered by the suite. That is
    // how a 129-character error message shipped and wrapped the layout.

    fn render_transport(state: &AppState) -> String {
        let mut out: Vec<u8> = Vec::new();
        render_transport_panel(&mut out, false, state).unwrap();
        String::from_utf8(out).unwrap()
    }

    /// Visible characters only. Consumes whole CSI sequences: `Clear`/`MoveTo`
    /// end in `J`/`H`, not `m`, and `[` is itself inside the final-byte range,
    /// so the introducer has to be stepped over explicitly.
    fn strip_ansi(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c != '\x1b' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            break;
                        }
                    }
                }
                // Not a CSI (e.g. `ESC c`): the introducer is all there was.
                _ => {}
            }
        }
        out
    }

    #[test]
    fn the_transport_panel_has_a_fixed_number_of_rows() {
        let text = render_transport(&state(Focus::Transport));
        // The header, then one line per row. No leading blank: the panels run
        // together, and the header rules are what separate them.
        assert_eq!(text.lines().count(), TRANSPORT_ROWS + 1);
    }

    #[test]
    fn a_long_failure_message_cannot_widen_the_panel() {
        // The regression: this message was printed verbatim at 129 characters,
        // wrapping the panel and shoving everything below it down the screen.
        let mut s = state(Focus::Transport);
        s.import_status = Some(ActionStatus::failed(
            "not a chord-tool file (no embedded progression; files exported \
             before MIDI import existed will not have one)"
                .to_string(),
        ));

        for line in render_transport(&s).lines() {
            let visible = strip_ansi(line);
            assert!(
                visible.chars().count() <= 80,
                "row wraps the panel: {:?} ({} chars)",
                visible,
                visible.chars().count()
            );
        }
    }

    #[test]
    fn a_multi_line_error_is_flattened_onto_one_row() {
        // A TOML parse error arrives with newlines and a caret; drawn verbatim
        // it would add rows and shred the layout below it.
        let mut s = state(Focus::Transport);
        s.import_status = Some(ActionStatus::failed(
            "TOML parse error at line 1, column 13\n  |\n1 | this is not toml\n\
             \x20 |             ^\ninvalid key"
                .to_string(),
        ));

        let text = render_transport(&s);
        assert_eq!(
            text.lines().count(),
            TRANSPORT_ROWS + 1,
            "rendered:\n{}",
            text
        );
        assert!(strip_ansi(&text).contains("TOML parse error at line 1, column 13"));
    }

    #[test]
    fn status_text_is_flattened_and_clipped() {
        assert_eq!(single_line_status("nothing to export"), "nothing to export");
        // Newlines, tabs and runs of spaces collapse to single spaces.
        assert_eq!(
            single_line_status("first\n\tsecond   third"),
            "first second third"
        );
        // Long text clips to the cap, ellipsis included.
        let clipped = single_line_status(&"x".repeat(200));
        assert_eq!(clipped.chars().count(), STATUS_MAX_CHARS);
        assert!(clipped.ends_with('…'));
        // Exactly at the cap is left alone.
        let exact = "y".repeat(STATUS_MAX_CHARS);
        assert_eq!(single_line_status(&exact), exact);
    }

    /// The whole frame, ANSI stripped.
    fn render_frame(state: &AppState) -> String {
        render_frame_at(state, DEFAULT_SCREEN)
    }

    /// A screen of a given width, tall enough that the layout is drawn.
    fn width_screen(width: usize) -> Screen {
        Screen {
            width,
            height: 40,
        }
    }

    /// The whole frame as it would be drawn in a `screen`-sized terminal.
    fn render_frame_at(state: &AppState, screen: Screen) -> String {
        let params = SynthParams::defaults();
        let mut out: Vec<u8> = Vec::new();
        render(&mut out, &params, state, screen).unwrap();
        strip_ansi(&String::from_utf8(out).unwrap())
    }

    /// Total lines `render` emits for a view.
    fn ui_height(focus: Focus, chords: &[ScaleDegree]) -> usize {
        let mut s = state(focus);
        seed(&mut s, chords);
        let params = SynthParams::defaults();
        let mut out: Vec<u8> = Vec::new();
        render(&mut out, &params, &s, DEFAULT_SCREEN).unwrap();
        String::from_utf8(out).unwrap().lines().count()
    }

    // ---- the Synth table ----

    fn key(code: KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn render_synth_with(params: &SynthParams, state: &AppState) -> String {
        let mut out: Vec<u8> = Vec::new();
        render_synth_panel(&mut out, params, state).unwrap();
        strip_ansi(&String::from_utf8(out).unwrap())
    }

    fn render_synth(state: &AppState) -> String {
        render_synth_with(&SynthParams::defaults(), state)
    }

    #[test]
    fn panel_view_heights_are_tracked() {
        // Every row added anywhere costs the whole screen, and the tallest view
        // decides the terminal the app needs. Asserting all three makes further
        // growth a deliberate decision rather than a surprise.
        //
        // The header is three rows, no key reminders are drawn, and the
        // transport and progression share one block, so the whole layout fits a
        // short terminal. The Sinko panel is still the tallest, and a row added
        // to any panel still costs the whole layout — this is the number to
        // watch.
        let chords = [
            ScaleDegree::I,
            ScaleDegree::V,
            ScaleDegree::VI,
            ScaleDegree::IV,
        ];
        assert_eq!(
            ui_height(Focus::Synth, &chords),
            30,
            "the second tallest view: re-check the README budget"
        );
        assert_eq!(
            ui_height(Focus::Sinko, &chords),
            33,
            "the Sinko panel is the tallest view"
        );
        assert_eq!(
            ui_height(Focus::Transport, &chords),
            16,
            "the default view should stay comfortably short"
        );
    }


    // ---- Sinko panel ----

    fn render_sinko(state: &AppState) -> String {
        let mut out: Vec<u8> = Vec::new();
        render_sinko_panel(&mut out, state, DEFAULT_SCREEN.grid_budget()).unwrap();
        strip_ansi(&String::from_utf8(out).unwrap())
    }

    /// A state with a library, a four-chord progression, and Sinko focused.
    fn sinko_state() -> AppState {
        let mut s = state(Focus::Sinko);
        seed(
            &mut s,
            &[
                ScaleDegree::I,
                ScaleDegree::V,
                ScaleDegree::VI,
                ScaleDegree::IV,
            ],
        );
        {
            let mut store = s.rhythm_store.lock().unwrap();
            for pattern in rhythm::builtin_patterns() {
                store.add(pattern);
            }
        }
        s.progression_row = 2;
        s
    }

    /// Assign a library pattern to a row the way cycling the `pattern` row
    /// does: the entry takes a *copy*, so it owns its rhythm from then on.
    fn assign(s: &mut AppState, row: usize, name: &str) {
        let pattern = s
            .rhythm_store
            .lock()
            .unwrap()
            .find(name)
            .unwrap_or_else(|| panic!("no library pattern named {:?}", name))
            .clone();
        assert!(
            s.progression.lock().unwrap().assign_pattern(row, Some(pattern)),
            "assigning {:?} changed nothing",
            name
        );
    }

    /// Give the selected row a rhythm of its own, as the panel would.
    fn own(s: &mut AppState, pattern: RhythmPattern) {
        s.working = pattern;
        s.working_slot = Some(s.progression_row);
        commit_working(s, &logger());
    }

    fn assigned_in(s: &AppState) -> Option<String> {
        match &s.progression.lock().unwrap().slots[s.progression_row] {
            Slot::Chord(e) => e.pattern.as_ref().map(|p| p.name.clone()),
            other => panic!("expected a chord, got {:?}", other),
        }
    }

    fn offset_in(s: &AppState) -> i32 {
        match &s.progression.lock().unwrap().slots[s.progression_row] {
            Slot::Chord(e) => e.offset_ticks,
            other => panic!("expected a chord, got {:?}", other),
        }
    }

    // ---- one rhythm per chord ----

    /// The plan's stab starts for a chord, as the scheduler reads it.
    fn starts_for(s: &AppState, notes: Vec<u8>) -> Vec<u64> {
        let (slots, key) = {
            let prog = s.progression.lock().unwrap();
            (prog.slots.clone(), s.transport.key())
        };
        crate::arrangement::arrangement(&slots, &key, 1.0)
            .iter()
            .filter(|stab| stab.notes == notes)
            .map(|stab| stab.start)
            .collect()
    }

    #[test]
    fn editing_one_chords_hits_leaves_its_neighbour_alone() {
        // The bug this whole model exists for: two chords given `Quarters`, then
        // the hits changed on one of them. They used to share one library
        // pattern, so both changed and both went silent together.
        let log = logger();
        let mut s = sinko_state();
        assign(&mut s, 2, "Quarters");
        assign(&mut s, 3, "Quarters");
        assert_eq!(sinko_pattern(&s).unwrap().name, "Quarters");

        // Turn off row 2's last hit through the panel's own `hits` row.
        s.progression_row = 2;
        s.sinko_row = SINKO_ROW_HITS;
        sync_working(&mut s);
        s.sinko_cell = 3;
        toggle_cell(&mut s, &log);

        let edited = {
            let prog = s.progression.lock().unwrap();
            match &prog.slots[2] {
                Slot::Chord(e) => e.pattern.clone().expect("row 2 still has one"),
                other => panic!("expected a chord, got {:?}", other),
            }
        };
        let neighbour = {
            let prog = s.progression.lock().unwrap();
            match &prog.slots[3] {
                Slot::Chord(e) => e.pattern.clone().expect("row 3 still has one"),
                other => panic!("expected a chord, got {:?}", other),
            }
        };
        assert_eq!(edited.name, "Quarters", "the name is unchanged");
        assert!(!edited.layers[0].hit(3), "row 2 lost the hit");
        assert!(neighbour.layers[0].hit(3), "row 3 was not touched");
        assert_ne!(edited, neighbour, "they are two rhythms, not one");

        // And the arrangement — what the scheduler plays and the exporter
        // writes — hears the difference. Row 2 is vi (A minor), row 3 is IV.
        assert_eq!(
            starts_for(&s, vec![69, 72, 76]),
            vec![2 * BAR_TICKS, 2 * BAR_TICKS + 960, 2 * BAR_TICKS + 1920]
        );
        assert_eq!(
            starts_for(&s, vec![65, 69, 72]),
            vec![
                3 * BAR_TICKS,
                3 * BAR_TICKS + 960,
                3 * BAR_TICKS + 1920,
                3 * BAR_TICKS + 2880
            ]
        );
    }

    #[test]
    fn a_hit_edit_is_one_undoable_edit() {
        let log = logger();
        let mut s = sinko_state();
        assign(&mut s, 2, "Quarters");
        let before = sinko_pattern(&s).unwrap();

        s.sinko_row = SINKO_ROW_HITS;
        sync_working(&mut s);
        s.sinko_cell = 2;
        toggle_cell(&mut s, &log);
        assert_ne!(sinko_pattern(&s).unwrap(), before, "the edit landed");

        assert!(s.progression.lock().unwrap().undo());
        assert_eq!(
            sinko_pattern(&s).unwrap(),
            before,
            "and one undo puts the hit back"
        );
        // The panel follows the undo rather than showing a stale grid.
        sync_working(&mut s);
        assert!(s.working.layers[0].hit(2));
    }

    #[test]
    fn the_library_pattern_survives_every_edit() {
        // Assigning is a copy, so the palette never changes behind the user's
        // back: `Quarters` still means what it shipped as, and is offered that
        // way to the next chord.
        let log = logger();
        let mut s = sinko_state();
        assign(&mut s, 2, "Quarters");
        let pristine = s.rhythm_store.lock().unwrap().find("Quarters").cloned();

        s.sinko_row = SINKO_ROW_HITS;
        sync_working(&mut s);
        s.sinko_cell = 0;
        toggle_cell(&mut s, &log);

        assert_eq!(
            s.rhythm_store.lock().unwrap().find("Quarters").cloned(),
            pristine,
            "the library copy is untouched"
        );
        assert!(!sinko_pattern(&s).unwrap().layers[0].hit(0));
    }

    #[test]
    fn copy_and_paste_replicates_a_rhythm_onto_another_chord() {
        let log = logger();
        let mut s = sinko_state();
        // Build a rhythm on vi, then give it to IV.
        assign(&mut s, 2, "Quarters");
        s.sinko_row = SINKO_ROW_HITS;
        sync_working(&mut s);
        s.sinko_cell = 1;
        toggle_cell(&mut s, &log);

        copy_sinko(&mut s, &log);
        assert!(s.sinko_clipboard.is_some(), "the clipboard holds the rhythm");

        s.progression_row = 3;
        sync_working(&mut s);
        assert!(sinko_pattern(&s).is_none(), "IV starts with nothing");
        paste_sinko(&mut s, &log);

        let pasted = sinko_pattern(&s).expect("IV now owns a rhythm");
        assert_eq!(pasted, s.sinko_clipboard.clone().unwrap());
        assert!(!pasted.layers[0].hit(1), "the edit came along");
        assert_eq!(pasted.name, "Quarters", "and so did the name");
        assert!(
            s.working == pasted,
            "the grid shows what was pasted, not the next Tab"
        );

        // Independent copies: pasting again onto vi and editing IV must not
        // reach it.
        s.sinko_cell = 2;
        toggle_cell(&mut s, &log);
        let vi = {
            let prog = s.progression.lock().unwrap();
            match &prog.slots[2] {
                Slot::Chord(e) => e.pattern.clone().unwrap(),
                other => panic!("expected a chord, got {:?}", other),
            }
        };
        assert!(vi.layers[0].hit(2), "vi kept its own hit");
        assert!(!sinko_pattern(&s).unwrap().layers[0].hit(2));
    }

    #[test]
    fn shift_q_and_shift_j_move_a_rhythm_between_chords() {
        // The hotkey path, from the Progression panel where chords are chosen:
        // no Tab into Sinko, no panel rows, just the two shifted keys.
        let log = logger();
        let mut s = sinko_state();
        s.focus = Focus::Progression;
        assign(&mut s, 2, "Quarters");

        // `q` copies the whole entry, `Shift+Q` only its rhythm.
        handle_hotkey(&mut s, Hotkey::CopyChord, false, &log);
        let whole_entry = s.progression.lock().unwrap().clipboard.clone();
        assert!(whole_entry.is_some(), "the chord clipboard took the entry");
        assert!(s.sinko_clipboard.is_none(), "the rhythm clipboard did not");

        handle_hotkey(&mut s, Hotkey::CopyChord, true, &log);
        assert!(s.sinko_clipboard.is_some(), "Shift+Q takes the rhythm");
        assert_eq!(
            s.sinko_clipboard.as_ref().unwrap().name,
            "Quarters",
            "and only the rhythm"
        );

        // Shift+J changes the chord under the cursor rather than inserting one.
        let rows_before = s.progression.lock().unwrap().slots.len();
        s.progression_row = 3;
        assert!(sinko_pattern(&s).is_none());
        handle_hotkey(&mut s, Hotkey::PasteChord, true, &log);

        assert_eq!(
            s.progression.lock().unwrap().slots.len(),
            rows_before,
            "no slot was inserted"
        );
        assert_eq!(
            sinko_pattern(&s).map(|p| p.name),
            Some("Quarters".to_string()),
            "and the chord has the copied rhythm"
        );
        assert_eq!(
            s.progression_row, 3,
            "the cursor stayed where the rhythm was pasted"
        );
    }

    #[test]
    fn the_progression_header_reports_the_rhythm_clipboard() {
        // The keys work from this panel, so the result has to be visible here:
        // the Sinko panel's status row is off screen from the Progression panel.
        let log = logger();
        let mut s = sinko_state();
        s.focus = Focus::Progression;
        assign(&mut s, 2, "Quarters");

        handle_hotkey(&mut s, Hotkey::CopyChord, true, &log);
        let text = render_frame(&s);
        assert!(text.contains("copied Quarters"), "rendered:\n{}", text);

        s.progression_row = 3;
        handle_hotkey(&mut s, Hotkey::PasteChord, true, &log);
        let text = render_frame(&s);
        assert!(text.contains("pasted Quarters"), "rendered:\n{}", text);
    }

    #[test]
    fn the_rhythm_clipboard_hotkeys_are_scoped_to_the_two_chord_lists() {
        let log = logger();
        let mut s = sinko_state();
        assign(&mut s, 2, "Quarters");
        handle_hotkey(&mut s, Hotkey::CopyChord, true, &log);
        assert!(s.sinko_clipboard.is_some());

        // A panel with no chord cursor cannot tell which chord is meant.
        for focus in [Focus::Transport, Focus::Synth, Focus::SynthPresets] {
            s.focus = focus;
            s.progression_row = 3;
            handle_hotkey(&mut s, Hotkey::PasteChord, true, &log);
            assert!(s.is_flashing(), "{:?} must refuse", focus);
            assert!(sinko_pattern(&s).is_none(), "{:?} changed a chord", focus);
        }

        // Both chord lists are fine, because both show the same cursor.
        for focus in [Focus::Progression, Focus::Sinko] {
            s.focus = focus;
            handle_hotkey(&mut s, Hotkey::PasteChord, true, &log);
            assert!(sinko_pattern(&s).is_some(), "{:?} should paste", focus);
            s.progression.lock().unwrap().assign_pattern(3, None);
        }
    }

    #[test]
    fn pasting_needs_something_copied() {
        let log = logger();
        let mut s = sinko_state();
        assign(&mut s, 2, "Quarters");

        paste_sinko(&mut s, &log);
        assert!(s.is_flashing(), "an empty clipboard refuses");
        assert_eq!(
            sinko_pattern(&s).unwrap(),
            s.rhythm_store.lock().unwrap().find("Quarters").cloned().unwrap()
        );
    }

    #[test]
    fn copying_needs_a_rhythm_to_copy() {
        let log = logger();
        let mut s = sinko_state();
        assert!(sinko_pattern(&s).is_none());

        copy_sinko(&mut s, &log);
        assert!(s.sinko_clipboard.is_none());
        assert!(s.is_flashing(), "and it says why");
    }

    #[test]
    fn the_copy_and_paste_rows_are_buttons_and_name_the_clipboard() {
        let log = logger();
        let mut s = sinko_state();
        assign(&mut s, 2, "Offbeat Eighths");
        s.sinko_row = SINKO_ROW_COPY;

        assert!(row_is_action_button(&s), "copy takes Enter like the rest");
        let text = render_sinko(&s);
        assert!(text.contains("[Copy Sinko]"), "rendered:\n{}", text);
        assert!(text.contains("[Paste Sinko]"), "empty clipboard:\n{}", text);

        sinko_action(&mut s, SINKO_ROW_COPY, &log);
        s.sinko_row = SINKO_ROW_PASTE;
        assert!(row_is_action_button(&s));
        let text = render_sinko(&s);
        assert!(
            text.contains("[Paste Sinko: Offbeat Eighths]"),
            "the row says what is waiting:\n{}",
            text
        );
    }

    #[test]
    fn the_pattern_row_marks_a_rhythm_that_has_drifted_from_the_library() {
        let log = logger();
        let mut s = sinko_state();
        assign(&mut s, 2, "Quarters");
        let text = render_sinko(&s);
        assert!(text.contains("Quarters"), "{}", text);
        assert!(!text.contains("(edited)"), "fresh from the library: {}", text);

        s.sinko_row = SINKO_ROW_HITS;
        sync_working(&mut s);
        s.sinko_cell = 2;
        toggle_cell(&mut s, &log);

        let text = render_sinko(&s);
        assert!(
            text.contains("Quarters  (edited)"),
            "it no longer matches the library's Quarters:\n{}",
            text
        );
    }

    #[test]
    fn a_new_pattern_belongs_to_the_chord_and_not_to_the_library() {
        let log = logger();
        let mut s = sinko_state();
        s.rhythm_path = unique_export_dir("sinko-new").join("rhythms.toml");
        let library_before = s.rhythm_store.lock().unwrap().patterns.len();

        new_pattern(&mut s, &log);

        assert!(
            sinko_pattern(&s).is_some(),
            "the selected chord owns the new rhythm"
        );
        assert!(assigned_in(&s).is_some(), "and the row shows it");
        assert_eq!(
            s.rhythm_store.lock().unwrap().patterns.len(),
            library_before,
            "the palette gained nothing"
        );
        assert!(!s.rhythm_path.exists(), "and nothing was written to disk");
    }

    #[test]
    fn tapping_a_take_gives_a_chord_with_no_rhythm_one_of_its_own() {
        // Recording is how a rhythm gets onto a chord, so it must not need a
        // pattern to exist first — and it must not need a save afterwards.
        let log = logger();
        let mut s = sinko_state();
        assert!(sinko_pattern(&s).is_none());

        s.recording = true;
        s.transport.publish_bar(Instant::now(), 0);
        sinko_tap(&mut s, &log);
        close_take(&mut s, &log);

        let own = sinko_pattern(&s).expect("the chord now owns a rhythm");
        assert!(own.any_hit(), "with the take in it");
        assert!(
            !own.name.trim().is_empty(),
            "and a name, so the pattern row says something"
        );
        assert_eq!(
            starts_for(&s, vec![69, 72, 76]).len(),
            1,
            "and the arrangement plays it"
        );
    }

    #[test]
    fn moving_the_selection_shows_the_other_chords_rhythm() {
        let mut s = sinko_state();
        assign(&mut s, 2, "Quarters");
        assign(&mut s, 3, "Offbeat Eighths");

        s.sinko_row = SINKO_ROW_HITS;
        sync_working(&mut s);
        assert_eq!(s.working.name, "Quarters");
        s.sinko_cell = 3;

        // Moving the progression cursor is what pulls up another chord's own
        // settings, grid and cursor included.
        s.progression_row = 3;
        sync_working(&mut s);
        assert_eq!(s.working.name, "Offbeat Eighths");
        assert_eq!(s.working.steps_per_bar(), 8);
        assert_eq!(s.sinko_cell, 0, "the cursor belongs to the grid it was on");

        // And a row with nothing of its own is a blank page rather than the
        // rhythm of the chord that was selected before.
        s.progression_row = 0;
        sync_working(&mut s);
        assert!(sinko_pattern(&s).is_none());
        assert!(!s.working.any_hit(), "nothing carried over");
    }

    #[test]
    fn the_sinko_panel_shows_the_chord_the_progression_panel_selected() {
        let mut s = sinko_state();
        let text = render_sinko(&s);
        assert!(text.contains("#3"), "rendered:\n{}", text);
        assert!(text.contains("Am"), "vi in C major is Am: \n{}", text);

        // Moving the progression cursor re-targets the panel: it has no cursor
        // of its own.
        s.progression_row = 0;
        let text = render_sinko(&s);
        assert!(text.contains("#1  C"), "I in C major: \n{}", text);
    }

    #[test]
    fn the_sinko_panel_collapses_to_one_line_when_unfocused() {
        let mut s = sinko_state();
        s.focus = Focus::Transport;
        let text = render_sinko(&s);
        // One combined header and summary, and no leading blank.
        assert_eq!(text.lines().count(), 1, "rendered:\n{}", text);
        assert!(text.contains("Sinko"), "rendered:\n{}", text);
        assert!(text.contains("#3"), "the summary still names the chord");
    }

    #[test]
    fn the_sinko_panel_has_a_fixed_number_of_rows() {
        let s = sinko_state();
        let text = render_sinko(&s);
        assert_eq!(text.lines().count(), SINKO_ROWS + 1, "rendered:\n{}", text);
    }

    #[test]
    fn no_synth_table_line_exceeds_the_screen_budget() {
        let text = render_synth(&state(Focus::Synth));
        for line in text.lines() {
            assert!(
                line.chars().count() <= 80,
                "row wraps the panel: {:?} ({} chars)",
                line,
                line.chars().count()
            );
        }
    }

    #[test]
    fn the_synth_table_shows_every_setting_at_once() {
        // The whole point of the reworked panel: one screen, one tab, no
        // subtab. A setting that is addressable but never drawn would leave a
        // reachable row that renders nothing.
        let text = render_synth(&state(Focus::Synth));
        for param in CHANNEL_PARAMS {
            assert!(
                text.contains(param.label()),
                "missing channel row {:?}",
                param.label()
            );
        }
        for param in MIXER_PARAMS {
            assert!(
                text.contains(param.label()),
                "missing master row {:?}",
                param.label()
            );
        }
        for name in CHANNEL_COLUMN_LABELS {
            assert!(text.contains(name), "missing column {name}");
        }
    }

    #[test]
    fn the_synth_table_has_a_fixed_number_of_rows() {
        // Panel header, column header, then one line per addressable row.
        // Asserting the exact total makes further growth a deliberate decision
        // rather than an accident.
        let text = render_synth(&state(Focus::Synth));
        assert_eq!(text.lines().count(), SYNTH_ROWS + 2);
    }

    #[test]
    fn every_mixer_param_appears_exactly_once_in_the_master_block() {
        // The master block is laid out as pairs, so a forgotten parameter would
        // silently vanish from the UI rather than fail to compile.
        let mut seen: Vec<MixerParam> = MASTER_ROWS
            .iter()
            .flat_map(|(a, b)| [*a, *b])
            .collect();
        assert_eq!(seen.len(), MIXER_PARAMS.len());
        seen.sort_by_key(|p| p.label());
        let mut expected = MIXER_PARAMS.to_vec();
        expected.sort_by_key(|p| p.label());
        assert_eq!(seen, expected);
    }

    #[test]
    fn synth_fields_are_padded_and_clipped_to_a_fixed_width() {
        assert_eq!(synth_field("sine", 11, false), "sine       ");
        assert_eq!(synth_field("sine", 11, true), "[sine]     ");
        // A value longer than the field is clipped *inside* the brackets, so a
        // long waveform name can never widen the grid.
        let clipped = synth_field("averylongwaveformname", 11, true);
        assert_eq!(clipped, "[averylong]");
        assert_eq!(clipped.chars().count(), 11);
        assert_eq!(
            synth_field("averylongwaveformname", 11, false).chars().count(),
            11
        );
    }

    #[test]
    fn the_selected_cell_is_bracketed_in_its_own_column() {
        let mut s = state(Focus::Synth);
        s.synth_row = 0; // volume
        s.synth_col = 1; // mid
        let p = SynthParams::defaults();
        p.low.volume.set(1.0);
        p.mid.volume.set(2.0);
        p.high.volume.set(3.0);

        let text = render_synth_with(&p, &s);
        let row = text
            .lines()
            .find(|l| l.contains("volume"))
            .expect("volume row");
        assert!(row.contains("[2]"), "mid must be selected: {row:?}");
        assert!(
            !row.contains("[1]") && !row.contains("[3]"),
            "only one cell may be bracketed: {row:?}"
        );
        // The values stay in low / mid / high order.
        let (low, mid, high) = (
            row.find('1').unwrap(),
            row.find("[2]").unwrap(),
            row.find('3').unwrap(),
        );
        assert!(low < mid && mid < high, "column order broke: {row:?}");
    }

    #[test]
    fn master_rows_select_between_their_two_cells() {
        let mut s = state(Focus::Synth);
        s.synth_row = CHANNEL_PARAMS.len(); // the reverb level / size pair
        s.synth_col = 1;
        let p = SynthParams::defaults();
        p.reverb_mix.set(0.10);
        p.reverb_size.set(0.80);

        let row = render_synth_with(&p, &s)
            .lines()
            .find(|l| l.contains("reverb level"))
            .expect("reverb row")
            .to_string();
        assert!(row.contains("[80%]"), "the second cell must be selected: {row:?}");
        assert!(row.contains("10%"), "the first cell must still show: {row:?}");
    }

    #[test]
    fn plain_arrows_move_the_column_and_clamp_at_the_row_width() {
        let mut s = state(Focus::Synth);
        s.synth_row = 0;
        s.synth_col = 0;
        s.move_synth_col(-1);
        assert_eq!(s.synth_col, 0, "clamped at the left edge");
        for _ in 0..CHANNEL_COUNT + 2 {
            s.move_synth_col(1);
        }
        assert_eq!(s.synth_col, CHANNEL_COUNT - 1, "clamped at the right edge");
    }

    #[test]
    fn moving_to_a_master_row_clamps_the_column_to_two_cells() {
        let mut s = state(Focus::Synth);
        s.synth_row = 0;
        s.synth_col = 2;
        s.set_current_row(CHANNEL_PARAMS.len());
        assert_eq!(s.synth_col, 1, "master rows hold two cells");
    }

    #[test]
    fn shift_selects_the_nudge_and_plain_arrows_select_the_column() {
        assert_eq!(
            synth_arrow(KeyCode::Left, false),
            Some(SynthArrow::Column(-1))
        );
        assert_eq!(
            synth_arrow(KeyCode::Right, false),
            Some(SynthArrow::Column(1))
        );
        assert_eq!(synth_arrow(KeyCode::Left, true), Some(SynthArrow::Nudge(-1)));
        assert_eq!(synth_arrow(KeyCode::Right, true), Some(SynthArrow::Nudge(1)));
        assert_eq!(synth_arrow(KeyCode::Up, false), None);
        assert_eq!(synth_arrow(KeyCode::Enter, false), None);
    }

    #[test]
    fn one_tab_reaches_the_synth_and_one_tab_leaves_it() {
        assert_eq!(Focus::Sinko.next(), Focus::Synth);
        assert_eq!(Focus::Synth.next(), Focus::SynthPresets);
        assert_eq!(Focus::Synth.prev(), Focus::Sinko);
    }

    #[test]
    fn the_focus_cycle_is_the_layout_order() {
        // Left to right along the shared row, then down the stack. `Tab` and
        // `Shift+Tab` are inverses of each other, and the cycle closes.
        assert_eq!(Focus::Progression.next(), Focus::Transport);
        assert_eq!(Focus::Transport.next(), Focus::Sinko);
        assert_eq!(Focus::Sinko.next(), Focus::Synth);
        assert_eq!(Focus::Synth.next(), Focus::SynthPresets);
        assert_eq!(Focus::SynthPresets.next(), Focus::Progression);

        for focus in [
            Focus::Progression,
            Focus::Transport,
            Focus::Sinko,
            Focus::Synth,
            Focus::SynthPresets,
        ] {
            assert_eq!(focus.next().prev(), focus, "{:?}", focus);
            assert_eq!(focus.prev().next(), focus, "{:?}", focus);
        }
    }

    #[test]
    fn every_panel_is_reachable_by_tabbing() {
        let mut seen = Vec::new();
        let mut focus = Focus::Transport;
        for _ in 0..6 {
            if seen.contains(&focus) {
                break;
            }
            seen.push(focus);
            focus = focus.next();
        }
        assert_eq!(seen.len(), 5, "panels: {:?}", seen);
        assert_eq!(focus, Focus::Transport, "the cycle must close");
        assert_eq!(Focus::Transport.prev(), Focus::Progression);
    }

    #[test]
    fn every_synth_cell_addresses_a_labelled_parameter() {
        for row in 0..SYNTH_ROWS {
            for col in 0..synth_col_count(row) {
                let cell = synth_cell(row, col);
                assert!(!cell.label().is_empty(), "cell {cell:?} has no label");
            }
        }
        assert_eq!(SYNTH_ROWS, CHANNEL_PARAMS.len() + MASTER_ROWS.len());
    }

    #[test]
    fn every_channel_cell_adjusts_only_its_own_column() {
        let p = SynthParams::defaults();
        let t = Transport::new(Key::new(60, Scale::Major));
        let before = (p.low.volume.get(), p.mid.volume.get(), p.high.volume.get());

        synth_cell(0, 1).adjust(&p, &t, 1);
        assert_eq!(p.low.volume.get(), before.0, "low must be untouched");
        assert_eq!(p.mid.volume.get(), before.1 + 1.0);
        assert_eq!(p.high.volume.get(), before.2, "high must be untouched");
    }

    #[test]
    fn a_synth_nudge_uses_the_same_clamp_as_the_old_mixer() {
        let p = SynthParams::defaults();
        let t = Transport::new(Key::new(60, Scale::Major));
        // Master volume saturates at 7.0 rather than running away.
        for _ in 0..20 {
            synth_cell(CHANNEL_PARAMS.len() + 1, 0).adjust(&p, &t, 1);
        }
        assert_eq!(p.master_volume.get(), 7.0);
    }

    #[test]
    fn esc_reverts_a_synth_edit_and_enter_keeps_it() {
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        s.synth_row = 6; // cutoff
        s.synth_col = 0; // low
        let before = p.low.cutoff.get();

        begin_synth_edit(&mut s, &p);
        assert!(matches!(s.edit, Edit::SynthCell { .. }));
        handle_synth_edit(&mut s, &p, &key(KeyCode::Left), &log);
        assert!(p.low.cutoff.get() < before, "left must lower the cutoff");

        handle_synth_edit(&mut s, &p, &key(KeyCode::Esc), &log);
        assert_eq!(p.low.cutoff.get(), before, "esc must write the value back");
        assert!(matches!(s.edit, Edit::None), "esc must close the edit");

        begin_synth_edit(&mut s, &p);
        handle_synth_edit(&mut s, &p, &key(KeyCode::Right), &log);
        let nudged = p.low.cutoff.get();
        handle_synth_edit(&mut s, &p, &key(KeyCode::Enter), &log);
        assert_eq!(p.low.cutoff.get(), nudged, "enter must keep the value");
        assert!(matches!(s.edit, Edit::None), "enter must close the edit");
    }

    #[test]
    fn the_vertical_arrows_are_the_coarse_step() {
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        s.synth_row = 6; // cutoff
        s.synth_col = 0;
        let before = p.low.cutoff.get();

        begin_synth_edit(&mut s, &p);
        handle_synth_edit(&mut s, &p, &key(KeyCode::Right), &log);
        let fine = p.low.cutoff.get() - before;
        p.low.cutoff.set(before);
        handle_synth_edit(&mut s, &p, &key(KeyCode::Up), &log);
        let coarse = p.low.cutoff.get() - before;

        assert!(
            coarse > fine * 2.0,
            "up ({coarse}) should sweep further than right ({fine})"
        );
    }

    #[test]
    fn a_discrete_cell_survives_a_full_edit_cycle() {
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        s.synth_row = 1; // waveform
        s.synth_col = 2; // high
        let before = p.high.waveform.get();

        begin_synth_edit(&mut s, &p);
        handle_synth_edit(&mut s, &p, &key(KeyCode::Right), &log);
        assert_ne!(p.high.waveform.get(), before, "cycling must change it");
        assert_eq!(p.low.waveform.get(), 0.0, "and only that column");

        handle_synth_edit(&mut s, &p, &key(KeyCode::Esc), &log);
        assert_eq!(p.high.waveform.get(), before, "esc must restore");
    }

    #[test]
    fn a_synth_edit_ignores_non_press_events() {
        // Key repeat must not re-trigger the step, or holding an arrow would
        // race away from the value the player aimed at.
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        s.synth_row = 0; // volume
        s.synth_col = 0;
        let before = p.low.volume.get();

        begin_synth_edit(&mut s, &p);
        let mut release = event::KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        handle_synth_edit(&mut s, &p, &release, &log);

        assert_eq!(p.low.volume.get(), before);
        assert!(matches!(s.edit, Edit::SynthCell { .. }), "edit stays open");
    }

    #[test]
    fn note_length_is_drawn_in_the_synth_table() {
        let text = render_synth(&state(Focus::Synth));
        assert!(text.contains("note length"), "rendered:\n{text}");
    }

    #[test]
    fn the_synth_summary_stands_in_while_the_panel_is_unfocused() {
        // The table only draws when focused; everywhere else one line keeps the
        // default view short.
        let text = render_synth(&state(Focus::Transport));
        assert!(!text.contains("note length"), "rendered:\n{text}");
        assert_eq!(text.lines().count(), 2, "header, summary");
        assert!(text.contains("master"), "rendered:\n{text}");
    }
}
