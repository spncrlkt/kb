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
use crate::analyzer::{TAPS as ANALYZER_TAPS, SPEEDS as ANALYZER_SPEEDS};
use crate::ensemble::{self, Ensemble, EnsembleStore};
use crate::eq::{EqPreset, EqPresetStore, EqTarget, EQ_BANDS};
use crate::export;
use crate::fx::{Fx, FxKind, FxPreset, FxPresetStore, CHAIN_SLOTS, P0, P1, P2, P4, P5};
use crate::grammar::{left_hand_degree, right_hand_transformation};
use crate::history::{Chord, History, Play};
use crate::instrument::{Instrument, InstrumentStore};
use crate::keyboard::{Hotkey, KeyPosition, PositionSet, ACTIVE_LAYOUT};
use crate::midi;
use crate::music::{
    chord_label, diatonic_triad, diatonic_triad_label, note_name, ChordSpec, Key, Scale,
    ScaleDegree, Transformation, BAR_TICKS, BEATS_PER_BAR,
};
use crate::progression::{Progression, ProgressionEntry, Registers, Slot};
use crate::project;
use crate::rhythm::{self, bar_phase_ticks, RhythmLayer, RhythmPattern};
use crate::rhythm_store::{self, RhythmStore};
use crate::settings::{self, Settings};
use crate::synth::{FilterType, FmMode, FxSlotParams, LfoWave, Synth, SynthParams, Waveform};
use crate::transport::{Scheduler, SchedulerEvent, Transport};
use crate::voice::{ComposedChannel, VoicePatch};

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
/// How far `Shift+←/→` moves the tempo: roughly "one feel", not one click.
const BPM_COARSE_STEP: i32 = 10;
/// How many patterns `Shift+←/→` skips in the palette. The library ships two
/// dozen, so one press at a time is no way to cross it.
const PATTERN_COARSE_STEP: i32 = 5;
const TAP_THRESHOLD_MS: u128 = 300;

/// Note length options, cycled by left/right on the mixer row.
pub(crate) const NOTE_LENGTHS: [(f32, &str); 4] =
    [(0.25, "1/4"), (0.5, "1/2"), (0.75, "3/4"), (1.0, "whole")];

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
/// The master volume, sharing its value with the Synth panel's master block
/// rather than holding a second copy of it.
const TRANSPORT_ROW_VOLUME: usize = 5;
/// One button for both file actions: `Enter` opens it into a chooser and a
/// second `Enter` runs the chosen side.
const TRANSPORT_ROW_MIDI: usize = 6;
const TRANSPORT_ROWS: usize = 7;

/// How long the persisted settings have to sit still before they are written.
///
/// Long enough that holding an arrow down is one write rather than thirty a
/// second, short enough that an unexpected kill loses at most this much.
const SETTINGS_SETTLE: Duration = Duration::from_millis(400);

/// What the `[MIDI]` row can do, left to right, which is the order `←`/`→`
/// picks between.
const MIDI_ACTIONS: [&str; 2] = ["EXPORT", "import"];
const MIDI_EXPORT: usize = 0;
const MIDI_IMPORT: usize = 1;

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
        12 => "1/8T",
        16 => "1/16",
        24 => "1/16T",
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
        let cell = mark.to_string().repeat(width);
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
    SynthEnsembles,
    /// The thirteen-band equaliser, one curve at a time: the three registers and
    /// the mix.
    ///
    /// It borrows the Synth panel's slot rather than taking a row of its own,
    /// which is what `SynthEnsembles` does too. That is not a compromise: an
    /// equaliser is a page of thirteen rows, and a page drawn on every other view
    /// would cost the whole layout for a panel nobody is looking at.
    Eq,
    /// The live spectrum: thirteen band levels for whichever register or the mix
    /// the `target` row is pointed at.
    ///
    /// The fourth view of that same slot. It is a *readout* — nothing here is
    /// editable except how it is drawn — so it is the one panel that shares
    /// another's cursor: it reads the same `target` the EQ panel writes, and
    /// tabbing between them keeps you on the same part.
    Spectrum,
    /// The effect rack: one register's six insert slots, with the slot under the
    /// cursor opened out into its own kind, variant, preset and parameters.
    ///
    /// The fifth view of the Synth slot. Eighteen slots of up to six numbers is
    /// a page of its own — the table's `fx` page is the overview and hands the
    /// cursor over to here.
    Fx,
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
            Focus::Synth => Focus::SynthEnsembles,
            Focus::SynthEnsembles => Focus::Eq,
            Focus::Eq => Focus::Spectrum,
            Focus::Spectrum => Focus::Fx,
            Focus::Fx => Focus::Progression,
        }
    }

    fn prev(self) -> Self {
        match self {
            Focus::Transport => Focus::Progression,
            Focus::Sinko => Focus::Transport,
            Focus::Synth => Focus::Sinko,
            Focus::SynthEnsembles => Focus::Synth,
            Focus::Eq => Focus::SynthEnsembles,
            Focus::Spectrum => Focus::Eq,
            Focus::Fx => Focus::Spectrum,
            Focus::Progression => Focus::Fx,
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

/// Which of the two aux units a master cell belongs to.
///
/// Each send is named after the effect it feeds, so the unit behind it is fixed:
/// the reverb send goes to a reverb, the delay send to a delay. Only the variant
/// is a choice, which is why the master block has a `subtype` row and no `type`
/// row — changing the family would make the row above it a lie.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum AuxUnit {
    Reverb,
    Delay,
}

impl AuxUnit {
    fn kind(self) -> FxKind {
        match self {
            AuxUnit::Reverb => FxKind::Reverb,
            AuxUnit::Delay => FxKind::Delay,
        }
    }

    /// The parameter slots that mean something in this position, given what the
    /// unit is doing right now.
    ///
    /// Two slots are conditional. `mix` is unused everywhere here: an aux unit
    /// runs fully wet, because how much of it you hear is the mixer's return
    /// level — a master row of its own — so its dry/wet is neither written by the
    /// panel nor read by the engine. And the delay's `division` names a note
    /// value only the `sync` switch reads, so it is not compared until that
    /// switch is on. Leaving it in would make the shipped delay — sync off, the
    /// division parked on the quarter note — read as `custom` from the first
    /// frame, and taking it out entirely would make a synced delay ignore the
    /// note value it is locked to.
    ///
    /// Listing the slots that count is also what makes the preset label work;
    /// see [`FxPresetStore::name_for_using`].
    fn params(self, fx: &Fx) -> &'static [usize] {
        // size, damp, predelay
        const REVERB: [usize; 3] = [P0, P1, P2];
        // time, feedback, tone, sync, division
        const DELAY_SYNCED: [usize; 5] = [P0, P1, P2, P4, P5];
        // time, feedback, tone, sync
        const DELAY_FREE: [usize; 4] = [P0, P1, P2, P4];
        match self {
            AuxUnit::Reverb => &REVERB,
            AuxUnit::Delay if fx.param(P4) > 0.5 => &DELAY_SYNCED,
            AuxUnit::Delay => &DELAY_FREE,
        }
    }

    /// The name of one of its parameters, straight from the model's own table so
    /// the two cannot drift apart.
    fn param_label(self, index: usize) -> &'static str {
        self.kind()
            .params()
            .get(index)
            .map(|spec| spec.label)
            .unwrap_or("")
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum MixerParam {
    /// The reverb send's unit.
    ReverbSubtype,
    ReverbPreset,
    ReverbSize,
    ReverbDamp,
    ReverbPredelay,
    ReverbMix,
    /// The delay send's unit.
    DelaySubtype,
    DelayPreset,
    DelayTime,
    DelayFeedback,
    DelayTone,
    DelaySync,
    DelayDivision,
    DelayMix,
    MasterVolume,
    MasterMute,
    /// The LFO is global while its destinations are per channel: one vibrato
    /// for the whole chord is what the ear expects, and three LFOs at three
    /// rates is a chorus, which is a different feature.
    LfoRate,
    LfoWave,
    NoteLength,
}

/// Canonical enumeration of the master settings, kept for the test that proves
/// `MASTER_ROWS` holds every one exactly once — a pair-based layout would
/// otherwise let a parameter vanish from the UI without a compile error.
#[cfg(test)]
const MIXER_PARAMS: [MixerParam; 19] = [
    MixerParam::ReverbSubtype,
    MixerParam::ReverbPreset,
    MixerParam::ReverbSize,
    MixerParam::ReverbDamp,
    MixerParam::ReverbPredelay,
    MixerParam::ReverbMix,
    MixerParam::DelaySubtype,
    MixerParam::DelayPreset,
    MixerParam::DelayTime,
    MixerParam::DelayFeedback,
    MixerParam::DelayTone,
    MixerParam::DelaySync,
    MixerParam::DelayDivision,
    MixerParam::DelayMix,
    MixerParam::MasterVolume,
    MixerParam::MasterMute,
    MixerParam::LfoRate,
    MixerParam::LfoWave,
    MixerParam::NoteLength,
];

/// The master settings, one entry per cell, in table order.
///
/// A grid rather than a list of pairs: a row may hold a single cell, so taking a
/// setting out does not force the rest to be rearranged. Per-channel volume,
/// reverb send and pan used to live here as nine separate rows; they are the
/// three channel columns now.
///
/// The two aux units are the bulk of it. They sit in the mixer rather than in a
/// panel of their own because a send and the effect it feeds are one decision:
/// the level row and the size row that give it something to say belong side by
/// side. The per-register insert racks are a different thing and live on the
/// Synth table's `fx` page.
const MASTER_ROWS: [&[MixerParam]; 10] = [
    &[MixerParam::ReverbSubtype, MixerParam::ReverbPreset],
    &[MixerParam::ReverbSize, MixerParam::ReverbDamp],
    &[MixerParam::ReverbPredelay, MixerParam::ReverbMix],
    &[MixerParam::DelaySubtype, MixerParam::DelayPreset],
    &[MixerParam::DelayTime, MixerParam::DelayFeedback],
    &[MixerParam::DelayTone, MixerParam::DelaySync],
    &[MixerParam::DelayDivision, MixerParam::DelayMix],
    &[MixerParam::MasterVolume, MixerParam::MasterMute],
    &[MixerParam::LfoRate, MixerParam::LfoWave],
    &[MixerParam::NoteLength],
];

/// What a master cell needs besides the value it edits.
///
/// `params` and `transport` were always two arguments; the preset library is the
/// third, needed because `preset` is a row that *applies* a stored effect rather
/// than reading and writing a number. Bundling the three keeps every cell's
/// signature to one argument, and a `Copy` bundle of borrows costs nothing.
#[derive(Copy, Clone)]
struct MixerCtx<'a> {
    params: &'a SynthParams,
    transport: &'a Transport,
    presets: &'a FxPresetStore,
}

impl<'a> MixerCtx<'a> {
    fn new(params: &'a SynthParams, transport: &'a Transport, presets: &'a FxPresetStore) -> Self {
        MixerCtx {
            params,
            transport,
            presets,
        }
    }

    /// The live controls of an aux unit.
    fn aux_slot(self, unit: AuxUnit) -> &'a FxSlotParams {
        match unit {
            AuxUnit::Reverb => &self.params.aux_reverb,
            AuxUnit::Delay => &self.params.aux_delay,
        }
    }

    fn tempo(self) -> f32 {
        self.transport.bpm() as f32
    }
}

/// The preset an aux unit is on, or `custom` once a parameter has moved.
///
/// Compared on the slots the position actually uses — see [`AuxUnit::params`] —
/// so the shipped settings read as the shipped preset rather than as `custom`
/// from the first frame.
fn preset_label(store: &FxPresetStore, fx: &Fx, unit: AuxUnit) -> String {
    store
        .name_for_using(fx, unit.params(fx))
        .map(|name| name.to_string())
        .unwrap_or_else(|| "custom".to_string())
}

impl MixerParam {
    /// The aux unit, for the three settings that address a unit rather than one
    /// of its numbers.
    fn aux_unit(self) -> Option<AuxUnit> {
        match self {
            MixerParam::ReverbSubtype
            | MixerParam::ReverbPreset
            | MixerParam::ReverbSize
            | MixerParam::ReverbDamp
            | MixerParam::ReverbPredelay => Some(AuxUnit::Reverb),
            MixerParam::DelaySubtype
            | MixerParam::DelayPreset
            | MixerParam::DelayTime
            | MixerParam::DelayFeedback
            | MixerParam::DelayTone
            | MixerParam::DelaySync
            | MixerParam::DelayDivision => Some(AuxUnit::Delay),
            _ => None,
        }
    }

    /// The aux unit and the parameter slot, for the rows that edit a number.
    ///
    /// The slot is a constant per row rather than a lookup, because the pair is
    /// what the row *is*: `reverb size` is the reverb's first parameter and would
    /// still be that if the model renumbered everything.
    fn aux_param(self) -> Option<(AuxUnit, usize)> {
        match self {
            MixerParam::ReverbSize => Some((AuxUnit::Reverb, P0)),
            MixerParam::ReverbDamp => Some((AuxUnit::Reverb, P1)),
            MixerParam::ReverbPredelay => Some((AuxUnit::Reverb, P2)),
            MixerParam::DelayTime => Some((AuxUnit::Delay, P0)),
            MixerParam::DelayFeedback => Some((AuxUnit::Delay, P1)),
            MixerParam::DelayTone => Some((AuxUnit::Delay, P2)),
            MixerParam::DelaySync => Some((AuxUnit::Delay, P4)),
            MixerParam::DelayDivision => Some((AuxUnit::Delay, P5)),
            _ => None,
        }
    }

    /// Whether this row applies a stored effect rather than a value.
    fn is_preset(self) -> bool {
        matches!(self, MixerParam::ReverbPreset | MixerParam::DelayPreset)
    }

    fn label(self) -> &'static str {
        if let Some((unit, index)) = self.aux_param() {
            return unit.param_label(index);
        }
        match self {
            MixerParam::ReverbSubtype | MixerParam::DelaySubtype => "subtype",
            MixerParam::ReverbPreset | MixerParam::DelayPreset => "preset",
            MixerParam::ReverbMix => "reverb level",
            MixerParam::DelayMix => "delay level",
            MixerParam::MasterVolume => "master volume",
            MixerParam::MasterMute => "master mute",
            MixerParam::LfoRate => "lfo rate",
            MixerParam::LfoWave => "lfo wave",
            MixerParam::NoteLength => "note length",
            // Unreachable: the arm above covers every remaining variant, and a
            // test walks `MIXER_PARAMS` to prove nothing lands here.
            _ => "",
        }
    }

    fn display(self, c: MixerCtx) -> String {
        if let Some((unit, index)) = self.aux_param() {
            return c.aux_slot(unit).fx().display(index, c.tempo());
        }
        match self {
            MixerParam::ReverbSubtype | MixerParam::DelaySubtype => {
                let unit = self.aux_unit().unwrap_or(AuxUnit::Reverb);
                c.aux_slot(unit).fx().subtype.name().to_string()
            }
            MixerParam::ReverbPreset | MixerParam::DelayPreset => {
                let unit = self.aux_unit().unwrap_or(AuxUnit::Reverb);
                preset_label(c.presets, &c.aux_slot(unit).fx(), unit)
            }
            // The return levels, not the units' own dry/wet: how much reverb you
            // hear is a mixer decision, and the unit is always fully wet so that
            // one number makes it.
            MixerParam::ReverbMix => format!("{:.0}%", c.params.reverb_mix.get() * 100.0),
            MixerParam::DelayMix => format!("{:.0}%", c.params.delay_mix.get() * 100.0),
            MixerParam::MasterVolume => format!("{:.0}", c.params.master_volume.get()),
            MixerParam::MasterMute => {
                if c.params.master_mute.get() > 0.5 {
                    "on".to_string()
                } else {
                    "off".to_string()
                }
            }
            // Two decimals, because the slow end of the range is where the
            // interesting settings are: 0.28 Hz is a drift, and "0 Hz" would be
            // indistinguishable from stopped.
            MixerParam::LfoRate => format!("{:.2} Hz", c.params.lfo_rate.get()),
            MixerParam::LfoWave => LfoWave::from_f32(c.params.lfo_wave.get())
                .name()
                .to_string(),
            MixerParam::NoteLength => format_note_length(c.transport.note_length()),
            _ => String::new(),
        }
    }

    fn adjust(self, c: MixerCtx, delta: i32) {
        use crate::synth::range as r;
        let p = c.params;
        if let Some((unit, index)) = self.aux_param() {
            let slot = c.aux_slot(unit);
            // The step and the range come from the model's own table, so a
            // percent moves by one and a cutoff moves by a ratio without this
            // match knowing which is which.
            let next = slot.fx().stepped(index, delta, false);
            slot.set_param(index, next);
            return;
        }
        match self {
            MixerParam::ReverbSubtype | MixerParam::DelaySubtype => {
                let slot = c.aux_slot(self.aux_unit().unwrap_or(AuxUnit::Reverb));
                let len = slot.fx().kind.subtypes().len() as i32;
                let next = (slot.subtype_index() as i32 + delta).rem_euclid(len);
                slot.set_subtype_index(next as usize);
            }
            MixerParam::ReverbPreset | MixerParam::DelayPreset => {
                let unit = self.aux_unit().unwrap_or(AuxUnit::Reverb);
                let slot = c.aux_slot(unit);
                let current = slot.fx();
                let used = unit.params(&current);
                if let Some(preset) = c.presets.step_using(&current, used, delta) {
                    slot.set(&preset.fx);
                }
            }
            MixerParam::ReverbMix => {
                p.reverb_mix
                    .set(step_to(p.reverb_mix.get(), delta, 0.05, r::REVERB_MIX))
            }
            MixerParam::DelayMix => {
                p.delay_mix
                    .set(step_to(p.delay_mix.get(), delta, 0.05, r::REVERB_MIX))
            }
            MixerParam::MasterVolume => {
                p.master_volume
                    .set(step_to(p.master_volume.get(), delta, 1.0, r::MASTER_VOLUME))
            }
            MixerParam::MasterMute => {
                let cur = p.master_mute.get() > 0.5;
                p.master_mute.set(if cur { 0.0 } else { 1.0 });
            }
            // Proportional, so the top of the range is reachable in as few
            // presses as the bottom.
            MixerParam::LfoRate => {
                p.lfo_rate
                    .set(step_ratio(p.lfo_rate.get(), delta, 1.15, r::LFO_RATE))
            }
            MixerParam::LfoWave => {
                p.lfo_wave.set(
                    cycle(&LfoWave::ALL, LfoWave::from_f32(p.lfo_wave.get()), delta) as i32 as f32,
                )
            }
            MixerParam::NoteLength => {
                let cur = c.transport.note_length();
                let idx = NOTE_LENGTHS
                    .iter()
                    .position(|(v, _)| (v - cur).abs() < 0.01)
                    .unwrap_or(3);
                let new_idx = ((idx as i32 + delta).rem_euclid(NOTE_LENGTHS.len() as i32)) as usize;
                c.transport.set_note_length(NOTE_LENGTHS[new_idx].0);
            }
            _ => {}
        }
    }

    /// The raw value behind this row, for an edit's `initial` snapshot.
    ///
    /// A `preset` row has no number to snapshot; what it replaced is the whole
    /// effect, which the edit carries separately. See [`SynthCell::fx_before`].
    fn value(self, c: MixerCtx) -> f32 {
        let p = c.params;
        if let Some((unit, index)) = self.aux_param() {
            return c.aux_slot(unit).param(index);
        }
        match self {
            MixerParam::ReverbMix => p.reverb_mix.get(),
            MixerParam::DelayMix => p.delay_mix.get(),
            MixerParam::MasterVolume => p.master_volume.get(),
            MixerParam::MasterMute => p.master_mute.get(),
            MixerParam::LfoRate => p.lfo_rate.get(),
            MixerParam::LfoWave => p.lfo_wave.get(),
            MixerParam::NoteLength => c.transport.note_length(),
            _ => 0.0,
        }
    }

    /// Write a raw value back. Only `Esc` uses this, with a value it previously
    /// read, so it is already inside the row's range and needs no clamping.
    fn restore(self, c: MixerCtx, v: f32) {
        let p = c.params;
        if let Some((unit, index)) = self.aux_param() {
            c.aux_slot(unit).set_param(index, v);
            return;
        }
        match self {
            MixerParam::ReverbMix => p.reverb_mix.set(v),
            MixerParam::DelayMix => p.delay_mix.set(v),
            MixerParam::MasterVolume => p.master_volume.set(v),
            MixerParam::MasterMute => p.master_mute.set(v),
            MixerParam::LfoRate => p.lfo_rate.set(v),
            MixerParam::LfoWave => p.lfo_wave.set(v),
            MixerParam::NoteLength => c.transport.set_note_length(v),
            _ => {}
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
/// Which screenful of the Synth panel is showing.
///
/// The table outgrew the screen: two dozen channel settings cannot be on it at
/// once, and an earlier version of this panel had merged its separate mixer
/// sub-tab away precisely so that a channel's volume and its cutoff could be
/// seen together. Paging is the honest way back — every page is short enough to
/// read at a glance, the master block is on all of them, and choosing a page is
/// one keystroke rather than a scroll position you have to remember.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum SynthPage {
    Tone,
    /// The second oscillator, everything it can do to the first one, and the
    /// pitch.
    Osc,
    /// The plucked string's own controls.
    ///
    /// Its own page because it is the one waveform that is a model rather than a
    /// shape: three rows that mean nothing on any other waveform, and a page is
    /// easier to find than three rows scattered among the oscillators.
    Pluck,
    Env,
    Filter,
    Mod,
    /// The effect rack: the two sends, then one row per insert slot.
    ///
    /// The slots are drawn as one cell per register, which is the same question
    /// the rest of the table asks — what does the low register have that the mid
    /// does not — and it is the only view that shows all eighteen slots at once.
    /// Editing one in depth is the FX panel's job; an arrow here swaps the kind,
    /// which is the fast way to find out whether a rack wants a phaser at all.
    Fx,
}

impl SynthPage {
    const ALL: [SynthPage; 7] = [
        SynthPage::Tone,
        SynthPage::Osc,
        SynthPage::Pluck,
        SynthPage::Env,
        SynthPage::Filter,
        SynthPage::Mod,
        SynthPage::Fx,
    ];

    fn name(self) -> &'static str {
        match self {
            SynthPage::Tone => "tone",
            SynthPage::Osc => "osc",
            SynthPage::Pluck => "pluck",
            SynthPage::Env => "env",
            SynthPage::Filter => "filter",
            SynthPage::Mod => "mod",
            SynthPage::Fx => "fx",
        }
    }

    /// The channel rows on this page, in display order.
    ///
    /// Grouped by what they act on rather than by the order the parameters
    /// happen to sit in the struct: the envelope's shape lives with the
    /// envelope, and everything that moves the filter lives together.
    fn rows(self) -> &'static [ChannelParam] {
        match self {
            SynthPage::Tone => &[
                ChannelParam::Volume,
                ChannelParam::Waveform,
                ChannelParam::PulseWidth,
                ChannelParam::Position,
                ChannelParam::PhaseDist,
                ChannelParam::NoiseLevel,
                ChannelParam::Transpose,
                ChannelParam::Pan,
            ],
            SynthPage::Osc => &[
                ChannelParam::Osc2Waveform,
                ChannelParam::Osc2Interval,
                ChannelParam::Osc2Level,
                ChannelParam::Osc2Fm,
                ChannelParam::FmMode,
                ChannelParam::Feedback,
                ChannelParam::Osc2Ring,
                ChannelParam::Glide,
            ],
            SynthPage::Pluck => &[
                ChannelParam::PluckDecay,
                ChannelParam::PluckDamp,
                ChannelParam::PluckBurst,
            ],
            SynthPage::Env => &[
                ChannelParam::Attack,
                ChannelParam::Decay,
                ChannelParam::Sustain,
                ChannelParam::Release,
                ChannelParam::EnvCurve,
            ],
            SynthPage::Filter => &[
                ChannelParam::Cutoff,
                ChannelParam::Resonance,
                ChannelParam::FilterType,
                ChannelParam::Drive,
                ChannelParam::FilterEnv,
                ChannelParam::FilterAttack,
                ChannelParam::FilterDecay,
                ChannelParam::KeyTrack,
            ],
            SynthPage::Mod => &[
                ChannelParam::LfoPitch,
                ChannelParam::LfoCutoff,
                ChannelParam::LfoAmp,
                ChannelParam::LfoPwm,
                ChannelParam::VelCutoff,
                ChannelParam::VelPwm,
                ChannelParam::Unison,
                ChannelParam::Detune,
            ],
            // Both sends, then the rack. The reverb send used to sit on the
            // tone page; the two belong side by side, because the question
            // "how much of this register goes to the reverb" is the same
            // question as "how much goes to the delay".
            SynthPage::Fx => &[
                ChannelParam::ReverbSend,
                ChannelParam::DelaySend,
                ChannelParam::FxSlot(0),
                ChannelParam::FxSlot(1),
                ChannelParam::FxSlot(2),
                ChannelParam::FxSlot(3),
                ChannelParam::FxSlot(4),
                ChannelParam::FxSlot(5),
            ],
        }
    }
}

/// A cell of the Synth table: a channel setting in one of three columns, or one
/// of the master block's cells.
///
/// The channel case holds the parameter itself rather than its row index. With
/// paging there is no one list for an index to mean anything in, and a
/// parameter is a better address than a position anyway.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum SynthCell {
    /// The instrument library row, which acts on whichever register the cursor
    /// is in — so `Shift+←/→` there auditions a whole channel design live.
    Instrument {
        col: usize,
    },
    Channel {
        param: ChannelParam,
        col: usize,
    },
    Master {
        row: usize,
        col: usize,
    },
}

/// The most rows the Synth panel ever draws: the instrument row, the tallest
/// page's channel rows, then the master block.
///
/// The largest page is `fx` at eight rows — the two sends and the six slots —
/// and a test derives the maximum from `SynthPage::rows()` rather than trusting
/// this number, so it cannot quietly stop being the maximum. Every page is
/// padded out to it, which is why the panel is one height whatever page is up.
const SYNTH_ROWS: usize = 1 + 8 + MASTER_ROWS.len();

/// Channel rows drawn on any one page.
///
/// Every page is padded out to this, so paging never changes the height of the
/// layout and the panels drawn after this one never move.
fn page_row_capacity() -> usize {
    SYNTH_ROWS - 1 - MASTER_ROWS.len()
}

/// Rows on one page: its channel settings, then the shared master block.
fn synth_row_count(page: SynthPage) -> usize {
    1 + page.rows().len() + MASTER_ROWS.len()
}

/// Row 0 is the instrument row and is always there; it is above the page's own
/// settings rather than inside them, because swapping an instrument is what you
/// do *to* a register rather than one of its parameters.
const SYNTH_ROW_INSTRUMENT: usize = 0;

/// How many of a page's rows are its own settings.
fn channel_row_start() -> usize {
    SYNTH_ROW_INSTRUMENT + 1
}

/// How many columns the row at `row` of `page` has.
fn synth_col_count(page: SynthPage, row: usize) -> usize {
    if row <= page.rows().len() {
        // The instrument row and the setting rows both have one cell per
        // register.
        CHANNEL_COUNT
    } else {
        master_row(row - channel_row_start() - page.rows().len()).len()
    }
}

/// The master block's row at `row`, or an empty row if it is off the end.
fn master_row(row: usize) -> &'static [MixerParam] {
    MASTER_ROWS.get(row).copied().unwrap_or(&[])
}

/// Where one of the master block's settings sits in the grid.
///
/// The transport's `master volume` row edits the same cell the Synth table's
/// master block does, so it has to name that cell rather than duplicate it — and
/// naming it by position would be a second copy of the layout to keep in step.
/// Returns the first cell; no setting appears twice in the block, which a test
/// holds.
fn master_row_of(param: MixerParam) -> (usize, usize) {
    for (row, cells) in MASTER_ROWS.iter().enumerate() {
        if let Some(col) = cells.iter().position(|cell| *cell == param) {
            return (row, col);
        }
    }
    (0, 0)
}

fn synth_cell(page: SynthPage, row: usize, col: usize) -> SynthCell {
    let rows = page.rows();
    let col = col.min(CHANNEL_COUNT - 1);
    if row == SYNTH_ROW_INSTRUMENT {
        SynthCell::Instrument { col }
    } else if row <= rows.len() {
        SynthCell::Channel {
            param: rows[row - channel_row_start()],
            col,
        }
    } else {
        let master = row - channel_row_start() - rows.len();
        SynthCell::Master {
            row: master,
            col: col.min(master_row(master).len().saturating_sub(1)),
        }
    }
}

fn channel_at(p: &SynthParams, col: usize) -> &crate::synth::ChannelParams {
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
            SynthCell::Channel { param, .. } => Some(param),
            _ => None,
        }
    }

    fn master(self) -> Option<MixerParam> {
        match self {
            SynthCell::Master { row, col } => master_row(row).get(col).copied(),
            _ => None,
        }
    }

    /// Which register this cell acts on, for the rows that have one per column.
    fn col(self) -> Option<usize> {
        match self {
            SynthCell::Instrument { col } | SynthCell::Channel { col, .. } => Some(col),
            SynthCell::Master { .. } => None,
        }
    }

    fn is_instrument(self) -> bool {
        matches!(self, SynthCell::Instrument { .. })
    }

    /// Which insert slot this cell is, when it is one of the `fx` page's.
    ///
    /// Its own accessor because `Enter` on that row does something no other cell
    /// does: it leaves the table for the panel that edits a slot in full.
    fn fx_slot_index(self) -> Option<usize> {
        match self {
            SynthCell::Channel {
                param: ChannelParam::FxSlot(index),
                ..
            } => Some(index),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        if self.is_instrument() {
            return "instrument";
        }
        match (self.param(), self.master()) {
            (Some(p), _) => p.label(),
            (_, Some(m)) => m.label(),
            _ => "",
        }
    }

    /// Change this cell's value. The same call backs `Shift+←/→` and the
    /// arrows inside an open edit, so both paths share one clamp.
    fn adjust(self, c: MixerCtx, delta: i32) {
        match (self.param(), self.master()) {
            (Some(param), _) => {
                if let SynthCell::Channel { col, .. } = self {
                    param.adjust(channel_at(c.params, col), delta);
                }
            }
            (_, Some(m)) => m.adjust(c, delta),
            _ => {}
        }
    }

    /// The aux unit this cell runs through, if it is one of the master block's
    /// effect rows.
    ///
    /// The instrument row and the channel rows are per register and belong to no
    /// unit; the global rows edit a scalar. Everything that answers here is a
    /// row of the reverb or delay block.
    fn aux_unit(self) -> Option<AuxUnit> {
        self.master().and_then(MixerParam::aux_unit)
    }

    /// The aux slot this cell addresses, when its value *is* a whole effect.
    ///
    /// Only a master `preset` row does: it applies a stored effect wholesale, so
    /// there is no scalar to snapshot and `Esc` has to undo an effect. An `fx`
    /// page slot row is also an effect rather than a number, but `Enter` there
    /// leaves the table for the FX panel instead of opening an edit, so there is
    /// no snapshot to take — `Shift+←/→` cycles the kind and the panel is where
    /// it is dialled in.
    fn fx_slot<'a>(self, c: MixerCtx<'a>) -> Option<&'a FxSlotParams> {
        match self.master() {
            Some(m) if m.is_preset() => self.aux_unit().map(|unit| c.aux_slot(unit)),
            _ => None,
        }
    }

    /// The whole effect this cell is about to replace, so `Esc` can put it back.
    ///
    /// A user who walks four presets looking for a reverb should be able to
    /// change their mind. `None` for every other cell, whose value is one number.
    fn fx_before(self, c: MixerCtx) -> Option<Fx> {
        self.fx_slot(c).map(|slot| slot.fx())
    }

    /// The raw value, for an edit's `initial` snapshot.
    fn value(self, c: MixerCtx) -> f32 {
        if self.is_instrument() {
            return 0.0;
        }
        match (self.param(), self.master()) {
            (Some(param), _) => {
                if let SynthCell::Channel { col, .. } = self {
                    param.value(channel_at(c.params, col))
                } else {
                    0.0
                }
            }
            (_, Some(m)) => m.value(c),
            _ => 0.0,
        }
    }

    /// Undo an edit. `Esc` only, and only with a value previously read here.
    ///
    /// `before` is the whole effect this cell replaced; the two are exclusive,
    /// because a cell is either a number or an effect.
    fn restore(self, c: MixerCtx, v: f32, before: Option<Fx>) {
        if self.is_instrument() {
            return;
        }
        if let Some(fx) = before {
            if let Some(slot) = self.fx_slot(c) {
                slot.set(&fx);
            }
            return;
        }
        match (self.param(), self.master()) {
            (Some(param), _) => {
                if let SynthCell::Channel { col, .. } = self {
                    param.restore(channel_at(c.params, col), v);
                }
            }
            (_, Some(m)) => m.restore(c, v),
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
    PulseWidth,
    NoiseLevel,
    Attack,
    Decay,
    Sustain,
    Release,
    EnvCurve,
    Glide,
    Cutoff,
    Resonance,
    FilterType,
    FilterEnv,
    FilterAttack,
    FilterDecay,
    KeyTrack,
    LfoPitch,
    LfoCutoff,
    LfoAmp,
    LfoPwm,
    Unison,
    Detune,
    /// Filter drive, 0..1.
    Drive,
    /// Velocity to cutoff, 0..1.
    VelCutoff,
    /// Velocity to pulse width, 0..1.
    VelPwm,
    /// Wavetable position, 0..1: this waveform blended into the next one in its
    /// octave group.
    Position,
    /// Phase distortion, 0..1.
    PhaseDist,
    /// The second oscillator's waveform.
    Osc2Waveform,
    /// Its interval from the first, in semitones.
    Osc2Interval,
    /// How much of it is mixed in, 0..1.
    Osc2Level,
    /// How hard it bends the first oscillator, 0..1.
    Osc2Fm,
    /// Which domain that depth is spent in.
    FmMode,
    /// The oscillator bending its own phase with its own last sample, 0..1.
    Feedback,
    /// The two oscillators multiplied together, mixed in alongside them, 0..1.
    Osc2Ring,
    /// A plucked string's ring, in seconds to -60 dB.
    PluckDecay,
    /// How fast its upper partials die, 0..1.
    PluckDamp,
    /// How long it is excited, as a fraction of one period.
    PluckBurst,
    Transpose,
    ReverbSend,
    DelaySend,
    /// One insert slot of the register's rack, by position.
    ///
    /// A position rather than the effect itself: the slot's contents change
    /// under the cursor every time the kind cycles, and the cell has to stay the
    /// same cell while it does.
    FxSlot(usize),
    Pan,
}

/// The slot rows, in rack order. Indexed by [`ChannelParam::FxSlot`].
const FX_SLOT_LABELS: [&str; CHAIN_SLOTS] =
    ["slot 1", "slot 2", "slot 3", "slot 4", "slot 5", "slot 6"];

/// The three channel columns, in display order.
const CHANNEL_COUNT: usize = 3;

/// A linear step inside a range.
fn step_to(v: f32, delta: i32, step: f32, (lo, hi): (f32, f32)) -> f32 {
    (v + delta as f32 * step).clamp(lo, hi)
}

/// A proportional step inside a range.
///
/// For the times that span two orders of magnitude — a cutoff can be 200 Hz or
/// 8 kHz — the same *proportion* of movement is the one that feels even. An
/// octave of cutoff is the same number of presses at either end of the row.
fn step_ratio(v: f32, delta: i32, ratio: f32, (lo, hi): (f32, f32)) -> f32 {
    (v * ratio.powf(delta as f32)).clamp(lo, hi)
}

/// The next entry of a fixed list, wrapping.
fn cycle<T: Copy + PartialEq>(all: &[T], current: T, delta: i32) -> T {
    let index = all.iter().position(|&x| x == current).unwrap_or(0) as i32;
    let len = all.len() as i32;
    all[(index + delta).rem_euclid(len) as usize]
}

impl ChannelParam {
    fn label(self) -> &'static str {
        match self {
            ChannelParam::Volume => "volume",
            ChannelParam::Waveform => "waveform",
            ChannelParam::PulseWidth => "pulse width",
            ChannelParam::NoiseLevel => "noise level",
            ChannelParam::Attack => "attack",
            ChannelParam::Decay => "decay",
            ChannelParam::Sustain => "sustain",
            ChannelParam::Release => "release",
            ChannelParam::EnvCurve => "env curve",
            ChannelParam::Glide => "glide",
            ChannelParam::Cutoff => "cutoff",
            ChannelParam::Resonance => "resonance",
            ChannelParam::FilterType => "filter type",
            ChannelParam::FilterEnv => "filter env",
            ChannelParam::FilterAttack => "filter attack",
            ChannelParam::FilterDecay => "filter decay",
            ChannelParam::KeyTrack => "key track",
            ChannelParam::LfoPitch => "lfo pitch",
            ChannelParam::LfoCutoff => "lfo cutoff",
            ChannelParam::LfoAmp => "lfo amp",
            ChannelParam::LfoPwm => "lfo pwm",
            ChannelParam::Unison => "unison",
            ChannelParam::Detune => "detune",
            ChannelParam::Drive => "drive",
            ChannelParam::VelCutoff => "vel cutoff",
            ChannelParam::VelPwm => "vel pwm",
            ChannelParam::Position => "position",
            ChannelParam::PhaseDist => "phase dist",
            ChannelParam::Osc2Waveform => "osc2 waveform",
            ChannelParam::Osc2Interval => "osc2 interval",
            ChannelParam::Osc2Level => "osc2 level",
            ChannelParam::Osc2Fm => "osc2 fm",
            ChannelParam::FmMode => "fm mode",
            ChannelParam::Feedback => "feedback",
            ChannelParam::Osc2Ring => "osc2 ring",
            ChannelParam::PluckDecay => "pluck decay",
            ChannelParam::PluckDamp => "pluck damp",
            ChannelParam::PluckBurst => "pluck burst",
            ChannelParam::Transpose => "transpose",
            ChannelParam::ReverbSend => "reverb send",
            ChannelParam::DelaySend => "delay send",
            ChannelParam::FxSlot(index) => FX_SLOT_LABELS[index.min(CHAIN_SLOTS - 1)],
            ChannelParam::Pan => "pan",
        }
    }

    fn display(self, ch: &crate::synth::ChannelParams) -> String {
        match self {
            ChannelParam::Volume => format!("{:.0}", ch.volume.get()),
            ChannelParam::Waveform => Waveform::from_f32(ch.waveform.get()).name().to_string(),
            ChannelParam::PulseWidth => format!("{:.0}%", ch.pulse_width.get() * 100.0),
            ChannelParam::NoiseLevel => format!("{:.0}%", ch.noise_level.get() * 100.0),
            ChannelParam::Attack => format!("{:.0} ms", ch.attack.get() * 1000.0),
            ChannelParam::Decay => format!("{:.0} ms", ch.decay.get() * 1000.0),
            ChannelParam::Sustain => format!("{:.0}%", ch.sustain.get() * 100.0),
            ChannelParam::Release => format!("{:.0} ms", ch.release.get() * 1000.0),
            ChannelParam::EnvCurve => format!("{:.0}%", ch.env_curve.get() * 100.0),
            ChannelParam::Glide => {
                let v = ch.glide.get();
                if v < 0.005 {
                    "off".to_string()
                } else {
                    format!("{:.0} ms", v * 1000.0)
                }
            }
            ChannelParam::Cutoff => format!("{:.0} Hz", ch.cutoff.get()),
            ChannelParam::Resonance => format!("{:.0}%", ch.resonance.get() * 100.0),
            ChannelParam::FilterType => FilterType::from_f32(ch.filter_type.get())
                .short_name()
                .to_string(),
            ChannelParam::FilterEnv => format!("{:+.0}%", ch.filter_env.get() * 100.0),
            ChannelParam::FilterAttack => format!("{:.0} ms", ch.filter_attack.get() * 1000.0),
            ChannelParam::FilterDecay => format!("{:.0} ms", ch.filter_decay.get() * 1000.0),
            ChannelParam::KeyTrack => format!("{:.0}%", ch.key_track.get() * 100.0),
            ChannelParam::LfoPitch => format!("{:.0}%", ch.lfo_pitch.get() * 100.0),
            ChannelParam::LfoCutoff => format!("{:.0}%", ch.lfo_cutoff.get() * 100.0),
            ChannelParam::LfoAmp => format!("{:.0}%", ch.lfo_amp.get() * 100.0),
            ChannelParam::LfoPwm => format!("{:.0}%", ch.lfo_pwm.get() * 100.0),
            ChannelParam::Unison => format!("{:.0}", ch.unison.get()),
            ChannelParam::Detune => format!("{:.0} ct", ch.detune.get()),
            ChannelParam::Drive => format!("{:.0}%", ch.drive.get() * 100.0),
            ChannelParam::VelCutoff => format!("{:.0}%", ch.vel_cutoff.get() * 100.0),
            ChannelParam::VelPwm => format!("{:.0}%", ch.vel_pwm.get() * 100.0),
            ChannelParam::Position => format!("{:.0}%", ch.position.get() * 100.0),
            ChannelParam::PhaseDist => format!("{:.0}%", ch.phase_dist.get() * 100.0),
            ChannelParam::Osc2Waveform => Waveform::from_f32(ch.osc2_waveform.get())
                .name()
                .to_string(),
            ChannelParam::Osc2Interval => {
                let v = ch.osc2_interval.get();
                if v > 0.5 {
                    format!("+{:.0} st", v)
                } else if v < -0.5 {
                    format!("{:.0} st", v)
                } else {
                    "0 st".to_string()
                }
            }
            ChannelParam::Osc2Level => format!("{:.0}%", ch.osc2_level.get() * 100.0),
            ChannelParam::Osc2Fm => format!("{:.0}%", ch.osc2_fm.get() * 100.0),
            ChannelParam::FmMode => FmMode::from_f32(ch.fm_mode.get()).name().to_string(),
            ChannelParam::Feedback => format!("{:.0}%", ch.feedback.get() * 100.0),
            ChannelParam::Osc2Ring => format!("{:.0}%", ch.osc2_ring.get() * 100.0),
            ChannelParam::PluckDecay => {
                let v = ch.pluck_decay.get();
                if v >= 1.0 {
                    format!("{:.2} s", v)
                } else {
                    format!("{:.0} ms", v * 1000.0)
                }
            }
            ChannelParam::PluckDamp => format!("{:.0}%", ch.pluck_damp.get() * 100.0),
            ChannelParam::PluckBurst => format!("{:.0}%", ch.pluck_burst.get() * 100.0),
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
            ChannelParam::DelaySend => format!("{:.0}%", ch.delay_send.get() * 100.0),
            // The kind, not the whole `distortion · overdrive`: a rack is read
            // as a shape — what follows what — and the fourteen-character column
            // would clip the variant off anyway. The variant is one keystroke
            // away in the FX panel, which is where a slot gets edited properly.
            ChannelParam::FxSlot(index) => slot_of(ch, index).fx().kind.name().to_string(),
            ChannelParam::Pan => format_pan(ch.pan.get()),
        }
    }

    fn adjust(self, ch: &crate::synth::ChannelParams, delta: i32) {
        use crate::synth::range as r;
        match self {
            ChannelParam::Volume => ch
                .volume
                .set(step_to(ch.volume.get(), delta, 1.0, r::VOLUME)),
            ChannelParam::Waveform => ch.waveform.set(cycle(
                &Waveform::ALL,
                Waveform::from_f32(ch.waveform.get()),
                delta,
            ) as i32 as f32),
            ChannelParam::PulseWidth => {
                ch.pulse_width
                    .set(step_to(ch.pulse_width.get(), delta, 0.05, r::PULSE_WIDTH))
            }
            ChannelParam::NoiseLevel => {
                ch.noise_level
                    .set(step_to(ch.noise_level.get(), delta, 0.1, r::NOISE_LEVEL))
            }
            ChannelParam::Attack => {
                ch.attack
                    .set(step_ratio(ch.attack.get(), delta, 1.15, r::ATTACK))
            }
            ChannelParam::Decay => ch
                .decay
                .set(step_ratio(ch.decay.get(), delta, 1.15, r::DECAY)),
            ChannelParam::Sustain => {
                ch.sustain
                    .set(step_to(ch.sustain.get(), delta, 0.05, r::SUSTAIN))
            }
            ChannelParam::Release => {
                ch.release
                    .set(step_ratio(ch.release.get(), delta, 1.15, r::RELEASE))
            }
            ChannelParam::EnvCurve => {
                ch.env_curve
                    .set(step_to(ch.env_curve.get(), delta, 0.1, r::ENV_CURVE))
            }
            ChannelParam::Glide => ch.glide.set(step_to(ch.glide.get(), delta, 0.05, r::GLIDE)),
            ChannelParam::Cutoff => {
                ch.cutoff
                    .set(step_ratio(ch.cutoff.get(), delta, 1.15, r::CUTOFF))
            }
            ChannelParam::Resonance => {
                ch.resonance
                    .set(step_to(ch.resonance.get(), delta, 0.02, r::RESONANCE))
            }
            ChannelParam::FilterType => ch.filter_type.set(cycle(
                &FilterType::ALL,
                FilterType::from_f32(ch.filter_type.get()),
                delta,
            ) as i32 as f32),
            ChannelParam::FilterEnv => {
                ch.filter_env
                    .set(step_to(ch.filter_env.get(), delta, 0.1, r::FILTER_ENV))
            }
            ChannelParam::FilterAttack => ch.filter_attack.set(step_ratio(
                ch.filter_attack.get(),
                delta,
                1.15,
                r::FILTER_ATTACK,
            )),
            ChannelParam::FilterDecay => ch.filter_decay.set(step_ratio(
                ch.filter_decay.get(),
                delta,
                1.15,
                r::FILTER_DECAY,
            )),
            ChannelParam::KeyTrack => {
                ch.key_track
                    .set(step_to(ch.key_track.get(), delta, 0.05, r::KEY_TRACK))
            }
            ChannelParam::LfoPitch => {
                ch.lfo_pitch
                    .set(step_to(ch.lfo_pitch.get(), delta, 0.05, r::LFO_PITCH))
            }
            ChannelParam::LfoCutoff => {
                ch.lfo_cutoff
                    .set(step_to(ch.lfo_cutoff.get(), delta, 0.05, r::LFO_CUTOFF))
            }
            ChannelParam::LfoAmp => {
                ch.lfo_amp
                    .set(step_to(ch.lfo_amp.get(), delta, 0.05, r::LFO_AMP))
            }
            ChannelParam::LfoPwm => {
                ch.lfo_pwm
                    .set(step_to(ch.lfo_pwm.get(), delta, 0.05, r::LFO_PWM))
            }
            ChannelParam::Unison => {
                // Whole voices: the row is a count, and rounding a fractional
                // one away later would hide the press that caused it.
                let next = (ch.unison.get() + delta as f32).clamp(r::UNISON.0, r::UNISON.1);
                ch.unison.set(next);
            }
            ChannelParam::Detune => ch
                .detune
                .set(step_to(ch.detune.get(), delta, 2.0, r::DETUNE)),
            ChannelParam::Drive => ch.drive.set(step_to(ch.drive.get(), delta, 0.05, r::DRIVE)),
            ChannelParam::VelCutoff => {
                ch.vel_cutoff
                    .set(step_to(ch.vel_cutoff.get(), delta, 0.05, r::VEL_CUTOFF))
            }
            ChannelParam::VelPwm => {
                ch.vel_pwm
                    .set(step_to(ch.vel_pwm.get(), delta, 0.05, r::VEL_PWM))
            }
            ChannelParam::Position => {
                ch.position
                    .set(step_to(ch.position.get(), delta, 0.05, r::POSITION))
            }
            ChannelParam::PhaseDist => {
                ch.phase_dist
                    .set(step_to(ch.phase_dist.get(), delta, 0.05, r::PHASE_DIST))
            }
            // The short list, so a second oscillator cannot be set to a plucked
            // string: one voice has one string, and the row does not offer what
            // the voice cannot do.
            ChannelParam::Osc2Waveform => ch.osc2_waveform.set(cycle(
                &Waveform::OSC2_WAVEFORMS,
                Waveform::from_f32(ch.osc2_waveform.get()),
                delta,
            ) as i32 as f32),
            ChannelParam::Osc2Interval => ch.osc2_interval.set(step_to(
                ch.osc2_interval.get(),
                delta,
                1.0,
                r::OSC2_INTERVAL,
            )),
            ChannelParam::Osc2Level => {
                ch.osc2_level
                    .set(step_to(ch.osc2_level.get(), delta, 0.05, r::OSC2_LEVEL))
            }
            ChannelParam::Osc2Fm => {
                ch.osc2_fm
                    .set(step_to(ch.osc2_fm.get(), delta, 0.05, r::OSC2_FM))
            }
            ChannelParam::FmMode => {
                ch.fm_mode.set(
                    cycle(&FmMode::ALL, FmMode::from_f32(ch.fm_mode.get()), delta) as i32 as f32,
                )
            }
            ChannelParam::Feedback => {
                ch.feedback
                    .set(step_to(ch.feedback.get(), delta, 0.05, r::FEEDBACK))
            }
            ChannelParam::Osc2Ring => {
                ch.osc2_ring
                    .set(step_to(ch.osc2_ring.get(), delta, 0.05, r::OSC2_RING))
            }
            // Proportional, because the range is two and a half orders of
            // magnitude: a hundred milliseconds and four seconds are both
            // reachable in the same number of presses.
            ChannelParam::PluckDecay => ch.pluck_decay.set(step_ratio(
                ch.pluck_decay.get(),
                delta,
                1.15,
                r::PLUCK_DECAY,
            )),
            ChannelParam::PluckDamp => {
                ch.pluck_damp
                    .set(step_to(ch.pluck_damp.get(), delta, 0.05, r::PLUCK_DAMP))
            }
            ChannelParam::PluckBurst => {
                ch.pluck_burst
                    .set(step_to(ch.pluck_burst.get(), delta, 0.05, r::PLUCK_BURST))
            }
            ChannelParam::Transpose => {
                ch.transpose
                    .set(step_to(ch.transpose.get(), delta, 1.0, r::TRANSPOSE))
            }
            ChannelParam::ReverbSend => {
                ch.reverb_send
                    .set(step_to(ch.reverb_send.get(), delta, 0.05, r::REVERB_SEND))
            }
            ChannelParam::DelaySend => {
                ch.delay_send
                    .set(step_to(ch.delay_send.get(), delta, 0.05, r::REVERB_SEND))
            }
            // The whole kind in one press, variant and defaults included, so the
            // row auditions a family rather than dialling a number.
            ChannelParam::FxSlot(index) => {
                let slot = slot_of(ch, index);
                let next = FxKind::from_index(slot.fx().kind.index() + delta);
                slot.set_kind(next);
            }
            ChannelParam::Pan => ch.pan.set(step_to(ch.pan.get(), delta, 0.1, r::PAN)),
        }
    }

    /// The raw value behind this row, for an edit's `initial` snapshot.
    fn value(self, ch: &crate::synth::ChannelParams) -> f32 {
        match self {
            ChannelParam::Volume => ch.volume.get(),
            ChannelParam::Waveform => ch.waveform.get(),
            ChannelParam::PulseWidth => ch.pulse_width.get(),
            ChannelParam::NoiseLevel => ch.noise_level.get(),
            ChannelParam::Attack => ch.attack.get(),
            ChannelParam::Decay => ch.decay.get(),
            ChannelParam::Sustain => ch.sustain.get(),
            ChannelParam::Release => ch.release.get(),
            ChannelParam::EnvCurve => ch.env_curve.get(),
            ChannelParam::Glide => ch.glide.get(),
            ChannelParam::Cutoff => ch.cutoff.get(),
            ChannelParam::Resonance => ch.resonance.get(),
            ChannelParam::FilterType => ch.filter_type.get(),
            ChannelParam::FilterEnv => ch.filter_env.get(),
            ChannelParam::FilterAttack => ch.filter_attack.get(),
            ChannelParam::FilterDecay => ch.filter_decay.get(),
            ChannelParam::KeyTrack => ch.key_track.get(),
            ChannelParam::LfoPitch => ch.lfo_pitch.get(),
            ChannelParam::LfoCutoff => ch.lfo_cutoff.get(),
            ChannelParam::LfoAmp => ch.lfo_amp.get(),
            ChannelParam::LfoPwm => ch.lfo_pwm.get(),
            ChannelParam::Unison => ch.unison.get(),
            ChannelParam::Detune => ch.detune.get(),
            ChannelParam::Drive => ch.drive.get(),
            ChannelParam::VelCutoff => ch.vel_cutoff.get(),
            ChannelParam::VelPwm => ch.vel_pwm.get(),
            ChannelParam::Position => ch.position.get(),
            ChannelParam::PhaseDist => ch.phase_dist.get(),
            ChannelParam::Osc2Waveform => ch.osc2_waveform.get(),
            ChannelParam::Osc2Interval => ch.osc2_interval.get(),
            ChannelParam::Osc2Level => ch.osc2_level.get(),
            ChannelParam::Osc2Fm => ch.osc2_fm.get(),
            ChannelParam::FmMode => ch.fm_mode.get(),
            ChannelParam::Feedback => ch.feedback.get(),
            ChannelParam::Osc2Ring => ch.osc2_ring.get(),
            ChannelParam::PluckDecay => ch.pluck_decay.get(),
            ChannelParam::PluckDamp => ch.pluck_damp.get(),
            ChannelParam::PluckBurst => ch.pluck_burst.get(),
            ChannelParam::Transpose => ch.transpose.get(),
            ChannelParam::ReverbSend => ch.reverb_send.get(),
            ChannelParam::DelaySend => ch.delay_send.get(),
            // The row's value is a whole effect, and `Enter` on it opens the FX
            // panel rather than an edit, so this number is never used to undo
            // anything: it exists because the match is total, and it is the one
            // number the row has — which kind is loaded.
            ChannelParam::FxSlot(index) => slot_of(ch, index).fx().kind.index() as f32,
            ChannelParam::Pan => ch.pan.get(),
        }
    }

    /// Write a raw value back. Only `Esc` uses this, with a value it previously
    /// read, so it is already inside the row's range and needs no clamping.
    fn restore(self, ch: &crate::synth::ChannelParams, v: f32) {
        match self {
            ChannelParam::Volume => ch.volume.set(v),
            ChannelParam::Waveform => ch.waveform.set(v),
            ChannelParam::PulseWidth => ch.pulse_width.set(v),
            ChannelParam::NoiseLevel => ch.noise_level.set(v),
            ChannelParam::Attack => ch.attack.set(v),
            ChannelParam::Decay => ch.decay.set(v),
            ChannelParam::Sustain => ch.sustain.set(v),
            ChannelParam::Release => ch.release.set(v),
            ChannelParam::EnvCurve => ch.env_curve.set(v),
            ChannelParam::Glide => ch.glide.set(v),
            ChannelParam::Cutoff => ch.cutoff.set(v),
            ChannelParam::Resonance => ch.resonance.set(v),
            ChannelParam::FilterType => ch.filter_type.set(v),
            ChannelParam::FilterEnv => ch.filter_env.set(v),
            ChannelParam::FilterAttack => ch.filter_attack.set(v),
            ChannelParam::FilterDecay => ch.filter_decay.set(v),
            ChannelParam::KeyTrack => ch.key_track.set(v),
            ChannelParam::LfoPitch => ch.lfo_pitch.set(v),
            ChannelParam::LfoCutoff => ch.lfo_cutoff.set(v),
            ChannelParam::LfoAmp => ch.lfo_amp.set(v),
            ChannelParam::LfoPwm => ch.lfo_pwm.set(v),
            ChannelParam::Unison => ch.unison.set(v),
            ChannelParam::Detune => ch.detune.set(v),
            ChannelParam::Drive => ch.drive.set(v),
            ChannelParam::VelCutoff => ch.vel_cutoff.set(v),
            ChannelParam::VelPwm => ch.vel_pwm.set(v),
            ChannelParam::Position => ch.position.set(v),
            ChannelParam::PhaseDist => ch.phase_dist.set(v),
            ChannelParam::Osc2Waveform => ch.osc2_waveform.set(v),
            ChannelParam::Osc2Interval => ch.osc2_interval.set(v),
            ChannelParam::Osc2Level => ch.osc2_level.set(v),
            ChannelParam::Osc2Fm => ch.osc2_fm.set(v),
            ChannelParam::FmMode => ch.fm_mode.set(v),
            ChannelParam::Feedback => ch.feedback.set(v),
            ChannelParam::Osc2Ring => ch.osc2_ring.set(v),
            ChannelParam::PluckDecay => ch.pluck_decay.set(v),
            ChannelParam::PluckDamp => ch.pluck_damp.set(v),
            ChannelParam::PluckBurst => ch.pluck_burst.set(v),
            ChannelParam::Transpose => ch.transpose.set(v),
            ChannelParam::ReverbSend => ch.reverb_send.set(v),
            ChannelParam::DelaySend => ch.delay_send.set(v),
            // See `value`. Reachable only from a hand-built `Edit`, and it loses
            // the kind's parameters — which is exactly why the row's `Enter`
            // opens the panel that can undo a whole effect instead.
            ChannelParam::FxSlot(index) => {
                slot_of(ch, index).set_kind(FxKind::from_index(v as i32))
            }
            ChannelParam::Pan => ch.pan.set(v),
        }
    }
}

/// One insert slot of a register's rack, clamped to the rack that exists.
///
/// The index comes from a cursor, and a cursor is a number that may outlive the
/// row it was on — paging or a shorter rack must not be able to panic the draw.
fn slot_of(ch: &crate::synth::ChannelParams, index: usize) -> &FxSlotParams {
    &ch.chain.slots[index.min(CHAIN_SLOTS - 1)]
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
        /// The whole effect, for the one cell whose value is not a number: a
        /// `preset` row applies a stored effect, so `Esc` has to put the whole
        /// thing back rather than one float.
        before: Option<Fx>,
    },
}

enum Modal {
    AddRest,
    EnsembleNameInput { buffer: String },
    /// Filename to import, pre-filled with the newest export.
    ImportPathInput { buffer: String },
    /// Name for the pattern being built in the Sinko panel.
    RhythmNameInput { buffer: String },
    /// Naming the focused register so it can be kept as a new instrument.
    InstrumentNameInput {
        col: usize,
        buffer: String,
    },
    /// Browsing the instrument library for one register. `original` is the
    /// register's design before the picker opened, so `Esc` can put it back
    /// exactly — the same contract as every other editor here.
    InstrumentPicker {
        col: usize,
        index: usize,
        /// Boxed because it is by far the largest thing any modal holds, and a
        /// modal is moved around on every keystroke: the picker is the only
        /// variant that needs the whole channel, so it is the one that pays for
        /// the indirection.
        original: Box<ComposedChannel>,
    },
    /// Naming the curve on screen so it can be kept as a preset.
    EqPresetNameInput { buffer: String },
    /// Naming the effect on the FX panel's cursor so it can be kept as a preset.
    FxPresetNameInput { buffer: String },
}

impl Modal {
    fn pass_through_chords(&self) -> bool {
        !matches!(
            self,
            Modal::EnsembleNameInput { .. }
                | Modal::ImportPathInput { .. }
                | Modal::RhythmNameInput { .. }
                | Modal::InstrumentPicker { .. }
                | Modal::InstrumentNameInput { .. }
                | Modal::EqPresetNameInput { .. }
                | Modal::FxPresetNameInput { .. }
        )
    }
}

// -----------------------------------------------------------------------------
// Sinko panel
// -----------------------------------------------------------------------------

/// How long a chord has to be the sounding chord before it is written to the log.
///
/// The audition speaks the instant a chord changes, so pressing the left hand and
/// then the right hand of one shape sounds two chords: the triad, then the shape
/// you meant. A shape you only passed through is not a chord you played, so the
/// log waits this long before believing one — long enough for both hands of a
/// chord, short enough that a deliberate change is never missed.
const HISTORY_SETTLE: Duration = Duration::from_millis(150);

/// How long a movement-cued chord sounds.
///
/// Moving through the log has to be audible or the list is just text, and it has
/// to stop on its own or a step through twenty rows leaves twenty chords ringing.
const HISTORY_AUDITION: Duration = Duration::from_millis(200);

/// Rows of the log or the top list that fit, under the column header.
const HISTORY_ROWS: usize = 17;

/// Which of the three states `l` cycles: away, the log, the ranking.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum HistoryView {
    Off,
    Log,
    Top,
}

impl HistoryView {
    fn next(self) -> Self {
        match self {
            HistoryView::Off => HistoryView::Log,
            HistoryView::Log => HistoryView::Top,
            HistoryView::Top => HistoryView::Off,
        }
    }

    fn is_on(self) -> bool {
        self != HistoryView::Off
    }

    fn name(self) -> &'static str {
        match self {
            HistoryView::Off => "off",
            HistoryView::Log => "history",
            HistoryView::Top => "top",
        }
    }
}

/// One chord on its way into the log.
///
/// The audition speaks the moment a chord changes, so this is what the log is
/// waiting to be sure of: the chord, how long it has been the sounding one, and
/// whether it has been written down yet — which is what stops a chord held for a
/// minute being logged sixty times a second.
#[derive(Clone, Debug)]
struct PendingPlay {
    play: Play,
    since: Instant,
    logged: bool,
}

/// Rows of the Sinko panel. A fixed count, unlike the Progression panel: the
/// layer grid always draws every line, so the layout cannot jump as takes
/// arrive.
const SINKO_ROW_CHORD: usize = 0;
const SINKO_ROW_PATTERN: usize = 1;
const SINKO_ROW_OFFSET: usize = 2;
const SINKO_ROW_QUANT: usize = 3;
/// This pattern's own groove, or "follow transport".
const SINKO_ROW_SWING: usize = 4;
/// The cell cursor: walk the grid and toggle the hits on it.
const SINKO_ROW_HITS: usize = 5;
/// How long the hit under the cursor holds, overriding the pattern's default.
const SINKO_ROW_LENGTH: usize = 6;
/// How hard the hit under the cursor plays, `0..=100%`.
const SINKO_ROW_ACCENT: usize = 7;
/// How long a hit without its own length holds.
const SINKO_ROW_HOLD: usize = 8;
/// How much of the bar's tail is silent.
const SINKO_ROW_MUTE: usize = 9;
const SINKO_ROW_SMOOTH: usize = 10;
const SINKO_ROW_RECORD: usize = 11;
const SINKO_LAYER_ROWS: usize = crate::arrangement::RHYTHM_LAYERS;
const SINKO_ROW_NEW: usize = SINKO_ROW_RECORD + 1 + SINKO_LAYER_ROWS;
const SINKO_ROW_SAVE: usize = SINKO_ROW_NEW + 1;
/// Copy the selected chord's rhythm to the Sinko clipboard.
const SINKO_ROW_COPY: usize = SINKO_ROW_SAVE + 1;
/// Apply the clipboard to the selected chord, as its own copy.
const SINKO_ROW_PASTE: usize = SINKO_ROW_COPY + 1;
const SINKO_ROWS: usize = SINKO_ROW_PASTE + 1;

// -----------------------------------------------------------------------------
// Progression panel
// -----------------------------------------------------------------------------

/// One edit the Progression panel can make.
///
/// Copy, paste, delete and undo have keys because they are pressed constantly;
/// the rest are named in the panel's menu below, because a group action reads
/// better named than bound. Every one of them takes the whole selection.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ProgressionEdit {
    Copy,
    Paste,
    Delete,
    Undo,
    Redo,
    Replace,
    Reverse,
    Rotate,
    ClearRhythms,
}

/// The Progression panel's menu: the group actions that have no key of their own.
///
/// A menu rather than four more hotkeys, because these are the operations you
/// think about rather than reach for mid-performance — and because the chord
/// list is a two-dimensional surface either way once the menu is beside it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ProgressionMenu {
    Replace,
    Reverse,
    Rotate,
    ClearSinko,
}

/// The menu's items, in the order they are drawn. Labels are lowercase like the
/// panel's own rows, and short because the menu shares the chord list's width.
const PROGRESSION_MENU: [(&str, ProgressionMenu); 4] = [
    ("replace", ProgressionMenu::Replace),
    ("reverse", ProgressionMenu::Reverse),
    ("rotate", ProgressionMenu::Rotate),
    ("clear", ProgressionMenu::ClearSinko),
];

impl ProgressionMenu {
    fn edit(self) -> ProgressionEdit {
        match self {
            ProgressionMenu::Replace => ProgressionEdit::Replace,
            ProgressionMenu::Reverse => ProgressionEdit::Reverse,
            ProgressionMenu::Rotate => ProgressionEdit::Rotate,
            ProgressionMenu::ClearSinko => ProgressionEdit::ClearRhythms,
        }
    }
}

/// The width the menu's labels are right-aligned in, and the gaps around it.
const PROGRESSION_MENU_LABEL: usize = 7;
const PROGRESSION_MENU_GAP: usize = 3;

// -----------------------------------------------------------------------------
// Metronome panel
// -----------------------------------------------------------------------------

/// Rows of the metronome overlay, in order.
const METRONOME_ROW_CLICK: usize = 0;
const METRONOME_ROW_SOUND: usize = 1;
const METRONOME_ROW_VOLUME: usize = 2;
/// Clicks per beat: the beats, the "&", or the sixteenths.
const METRONOME_ROW_SUBDIVISION: usize = 3;
/// The groove every pattern without one of its own follows.
const METRONOME_ROW_SWING: usize = 4;
const METRONOME_ROWS: usize = 5;

/// The subdivisions the metronome offers, as clicks per beat.
const METRONOME_SUBDIVISIONS: [usize; 3] = [1, 2, 4];

/// How the click level reads: a percentage, and a bar so "quiet" is visible.
fn format_metronome_volume(volume: f32) -> String {
    let percent = (volume.clamp(0.0, 1.0) * 100.0).round() as i32;
    let filled = (percent as usize + 5) / 10;
    format!(
        "{:>3}%  [{}{}]",
        percent,
        "#".repeat(filled),
        "·".repeat(10 - filled)
    )
}

/// Taps closer together than this are key repeat, not a second tap.
const TAP_DEBOUNCE_MS: u128 = 30;

/// How long a chord change is left ringing after the keys come off.
///
/// The point of the audition is to try shapes quickly, so a chord must not be
/// cut the instant the hand moves — an organ with a long tail. The next computed
/// chord cancels the release rather than adding to it, which is what keeps a run
/// of changes legato instead of a smear.
const AUDITION_RELEASE: Duration = Duration::from_millis(500);

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
    /// Whether the metronome panel is covering the Transport slot, and which of
    /// its rows is under the cursor.
    ///
    /// An overlay rather than a sixth panel: it is a handful of settings for one
    /// row of the transport, so it borrows that panel's space instead of adding a
    /// `Tab` stop and three rows to every screen in the app.
    metronome_open: bool,
    metronome_row: usize,
    /// Whether the `[MIDI]` row is opened into its chooser, and which side of it
    /// is selected.
    ///
    /// A mode rather than a menu of its own: it borrows the row it belongs to, so
    /// opening it costs the transport panel nothing and there is no second place
    /// an export could be started from.
    midi_open: bool,
    midi_choice: usize,
    /// Synth table cursor: a row and, within it, a channel column (or one of
    /// the two master cells).
    synth_row: usize,
    synth_col: usize,
    /// Which page of the Synth table. Global to the panel rather than per
    /// channel: the three registers are compared column by column, so moving to
    /// a page has to move all three at once or there is nothing to compare.
    synth_page: SynthPage,
    /// The named single-channel designs a register can be swapped to.
    instrument_store: InstrumentStore,
    /// What each register was last loaded from, and the design it had at the
    /// time. A register is only *called* that instrument while it still matches,
    /// so the row cannot claim a sound it is no longer making.
    instrument_loaded: [Option<(String, VoicePatch)>; CHANNEL_COUNT],
    progression_row: usize,
    /// True when the Progression cursor sits in the gap *above* the first chord.
    ///
    /// That gap is the one insertion point "paste after the cursor" cannot
    /// reach, so it gets a cursor position of its own rather than a second
    /// paste command. `progression_row` stays 0 while it is set.
    progression_before_first: bool,
    /// The far end of a multi-chord selection, when `Shift` has extended one.
    ///
    /// `None` while a single row is selected, which is the ordinary case: the
    /// selection is then exactly the cursor, and nothing about the old
    /// single-row behaviour changes.
    progression_anchor: Option<usize>,
    /// Whether the Progression cursor is in the menu column rather than the
    /// chord list, and which item it is on.
    progression_menu: bool,
    progression_menu_row: usize,
    /// The computed chord the audition has already sounded, so a change is
    /// noticed once rather than every pass of the event loop.
    audition_sounding: Option<Vec<u8>>,
    /// When the sounding audition note is released, once the chord has gone.
    audition_release_at: Option<Instant>,
    ensemble_row: usize,
    /// EQ panel cursor: which of its four rows, which of the four equalisers it
    /// is pointed at, and which of the thirteen bands the curve display marks.
    ///
    /// Only the cursors live here. The curve itself lives in the audio layer's
    /// shared parameters, exactly like every other value the panels edit, so
    /// there is no second copy to drift out of step with what is sounding.
    eq_row: usize,
    /// Which equaliser the EQ panel *and* the Spectrum panel are pointed at.
    ///
    /// One cursor rather than two: the whole reason to look at the spectrum is
    /// to decide what to do on the EQ panel, so tabbing between them has to keep
    /// the part under the cursor.
    eq_target: usize,
    eq_band: usize,
    /// Spectrum panel cursor, the span it draws, whether it is holding peaks,
    /// and the peaks themselves.
    ///
    /// The peaks are held *here* rather than on the audio thread: the panel polls
    /// all fifty-two published levels every frame, so it sees what the callback
    /// measured without a second piece of shared state and without a reset the
    /// callback could miss.
    spectrum_row: usize,
    spectrum_range: usize,
    spectrum_hold: bool,
    spectrum_peaks: [[f32; crate::eq::EQ_BANDS]; ANALYZER_TAPS],
    presets: EqPresetStore,
    /// The effect library: the shipped presets with the user's own filed over
    /// them. `preset` on a master cell walks it, and the FX panel names, saves
    /// and loads through it.
    fx_presets: FxPresetStore,
    modal: Option<Modal>,
    ensemble_store: EnsembleStore,
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
    /// The rhythm clipboard: one entry per copied chord, `None` for a chord
    /// that had no rhythm. A list so a phrase's rhythms travel together.
    sinko_clipboard: Option<Vec<Option<RhythmPattern>>>,
    /// Feedback for the Sinko panel's own actions.
    rhythm_status: Option<ActionStatus>,
    /// Where the pattern library is written. A field rather than a call to
    /// `default_path()` at save time, so a test never writes into the working
    /// directory — the same reasoning as `export_dir`.
    /// Where the user's own rhythms are saved. The shipped defaults live in the
    /// tracked `rhythms.toml` and are read only.
    rhythm_user_path: PathBuf,
    flash_until: Option<Instant>,
    taps: TapTracker,
    /// Where MIDI exports are written. A field rather than a call to
    /// `current_dir()` at export time, so it is testable and can later become
    /// a setting.
    export_dir: PathBuf,
    export_status: Option<ActionStatus>,
    import_status: Option<ActionStatus>,
    /// Every chord played this run, in order and counted.
    history: History,
    /// FX panel cursor: which register's rack, which slot of it, which of that
    /// slot's parameters is on the `value` row, and which row the cursor is on.
    fx_col: usize,
    fx_slot: usize,
    fx_param: usize,
    fx_row: usize,
    /// Which view of it is up, and where its cursor is.
    history_view: HistoryView,
    history_row: usize,
    history_scroll: usize,
    /// The chord that is sounding and how long it has been sounding, so a shape
    /// the hands merely passed through is not written down.
    pending_play: Option<PendingPlay>,
    /// The notes the history wants sounded because a key is held down.
    history_held: Option<Vec<u8>>,
    /// The same, with the deadline a movement's 200 ms runs out at.
    history_timed: Option<(Vec<u8>, Instant)>,
    /// Where the key, tempo and master volume are written back. A field rather
    /// than a call to [`crate::settings::path`] at save time, so a test never
    /// writes into the working directory.
    settings_path: PathBuf,
    /// The settings as they were last seen, and when they last moved.
    ///
    /// The values are watched rather than hooked: every way of changing the
    /// tempo, the key or the volume already exists and several of them are not in
    /// this file, so comparing against what was last written is the only way to
    /// be sure nothing is missed — including an ensemble load, which sets the
    /// master volume from its mixer.
    settings_seen: Settings,
    settings_dirty: Option<Instant>,
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

    /// Take the levels the audio thread published and fold them into the peaks.
    ///
    /// Called once per frame, which is faster than the callback publishes, so
    /// nothing is missed. That is the whole reason the peaks can live here
    /// instead of on the audio thread.
    ///
    /// It is also where the audio thread is told whether the panel is on screen.
    /// The banks behind the levels are fifty-two biquads a sample and the levels
    /// reach nothing else, so when the Spectrum stop is not the one being drawn
    /// there is nothing to compute and nothing to publish — and the peaks are
    /// emptied here rather than left to be redrawn, so coming back to the panel
    /// opens on the sound that is playing rather than the one that was.
    fn poll_spectrum(&mut self, params: &SynthParams) {
        if self.focus != Focus::Spectrum {
            params.analyzer.enabled.set(0.0);
            self.clear_spectrum_peaks();
            return;
        }
        params.analyzer.enabled.set(1.0);
        for (tap, peaks) in self.spectrum_peaks.iter_mut().enumerate() {
            let published = params.analyzer.tap(tap);
            for (band, peak) in peaks.iter_mut().enumerate() {
                *peak = peak.max(published.level(band));
            }
        }
    }

    /// The chord the player is sounding right now, if any.
    ///
    /// The computed chord is the player's *intent*, but it only sounds while the
    /// transport is stopped or an in-place audition is armed — with the loop
    /// running and nothing armed, holding a chord is silent, and logging it would
    /// be a lie about what was played.
    fn audible_play(&self) -> Option<Play> {
        let playing = self.transport.playing.load(Ordering::Relaxed);
        if playing && self.transport.audition_slot().is_none() {
            return None;
        }
        let (degree, transformation) = resolved_chord(self)?;
        Some(Play::new(
            Chord::new(degree, transformation),
            self.registers.clone(),
        ))
    }

    /// Write a chord down once it has been the sounding chord long enough.
    ///
    /// Called once per frame. The rule is deliberately about *duration* rather
    /// than about a key press: what the log means by a play is a chord that was
    /// actually held, whichever hands put it there and whether or not it was ever
    /// committed to the progression.
    fn poll_history(&mut self) {
        let current = self.audible_play();
        let same = matches!(
            (&self.pending_play, &current),
            (Some(pending), Some(now)) if pending.play.chord == now.chord
        );
        if same {
            let pending = self.pending_play.as_mut().expect("just matched");
            // The registers may have moved under the same chord — a re-latch, a
            // hand swapping over — so the newest gesture is the one kept.
            pending.play.registers = current.expect("just matched").registers;
            if pending.logged || pending.since.elapsed() < HISTORY_SETTLE {
                return;
            }
            pending.logged = true;
            let play = pending.play.clone();
            self.history.record(play);
            return;
        }

        self.pending_play = current.map(|play| PendingPlay {
            play,
            since: Instant::now(),
            logged: false,
        });
    }

    /// How many rows the view on screen has.
    fn history_len(&self) -> usize {
        match self.history_view {
            HistoryView::Log => self.history.len(),
            HistoryView::Top => self.history.chords(),
            HistoryView::Off => 0,
        }
    }

    /// The play the cursor is on, if there is one.
    fn history_selected(&self) -> Option<&Play> {
        match self.history_view {
            HistoryView::Log => self.history.plays().get(self.history_row),
            HistoryView::Top => self
                .history
                .top()
                .get(self.history_row)
                .map(|tally| &tally.play),
            HistoryView::Off => None,
        }
    }

    /// Keep the cursor inside the window of rows the panel can draw.
    fn scroll_history_into_view(&mut self) {
        let rows = HISTORY_ROWS;
        if self.history_row < self.history_scroll {
            self.history_scroll = self.history_row;
        }
        let last = self.history_scroll + rows - 1;
        if self.history_row > last {
            self.history_scroll = self.history_row + 1 - rows;
        }
    }

    /// Whether a chord appears anywhere in the progression.
    ///
    /// By chord rather than by row: the same chord twice in the progression is
    /// still one chord you have in the progression.
    fn chord_is_in_the_progression(&self, chord: Chord) -> bool {
        let prog = self.progression.lock().unwrap();
        prog.slots.iter().any(|slot| match slot {
            Slot::Chord(entry) => {
                entry.degree == chord.degree && entry.transformation == chord.transformation
            }
            Slot::Rest => false,
        })
    }

    /// Notice a change to the persisted settings and write them back.
    ///
    /// Called once per frame. A change marks the settings dirty rather than
    /// writing them, and the write happens once they have been still for a
    /// moment: holding an arrow on `bpm` would otherwise rewrite a file thirty
    /// times a second, and every write is a chance to be interrupted halfway.
    fn poll_settings(&mut self, params: &SynthParams, logger: &Logger) {
        let now = Settings::capture(&self.transport, params.master_volume.get());
        if now != self.settings_seen {
            self.settings_seen = now;
            self.settings_dirty = Some(Instant::now());
            return;
        }
        let Some(since) = self.settings_dirty else {
            return;
        };
        if Instant::now().duration_since(since) < SETTINGS_SETTLE {
            return;
        }
        self.settings_dirty = None;
        if let Err(e) = self.settings_seen.save(&self.settings_path) {
            logger.input(&format!("SETTINGS could not be saved: {}", e));
        }
    }

    /// Publish the tempo the synced delay follows.
    ///
    /// The audio thread has no transport — the scheduler owns it — so the one
    /// number a tempo-synced effect needs is carried the way every other
    /// parameter is. Written every frame rather than hooked onto the four places
    /// the tempo can change from: watching one atomic is cheaper than finding
    /// them, and there is no fifth place to forget.
    fn poll_tempo(&self, params: &SynthParams) {
        params.tempo.set(self.transport.bpm() as f32);
    }

    /// Write the settings now, whatever the settle timer says.
    ///
    /// For the quit path: a tempo changed a tenth of a second before `Esc` twice
    /// is a tempo the player expects to find next time.
    fn flush_settings(&self, logger: &Logger) {
        if let Err(e) = self.settings_seen.save(&self.settings_path) {
            logger.input(&format!("SETTINGS could not be saved: {}", e));
        }
    }

    /// Drop every held peak.
    fn clear_spectrum_peaks(&mut self) {
        for peaks in self.spectrum_peaks.iter_mut() {
            *peaks = [0.0; crate::eq::EQ_BANDS];
        }
    }

    /// The cell the Synth cursor is on.
    fn synth_cell(&self) -> SynthCell {
        synth_cell(self.synth_page, self.synth_row, self.synth_col)
    }

    /// Everything a Synth or mixer cell needs, borrowed from the live state.
    ///
    /// One place to build it, so a cell that grows a need does not grow an
    /// argument in fifteen call sites.
    fn mixer_ctx<'a>(&'a self, params: &'a SynthParams) -> MixerCtx<'a> {
        MixerCtx::new(params, &self.transport, &self.fx_presets)
    }

    /// Rows on the Synth page now showing.
    fn synth_rows(&self) -> usize {
        synth_row_count(self.synth_page)
    }

    /// Load library instrument `index` into `col`, live.
    ///
    /// This is the whole feature: the parameters are lock-free and the audio
    /// callback reads them, so writing two dozen of them here is audible on the
    /// next buffer with the loop still running. Nothing is reallocated and no
    /// voice is retriggered, so a swap does not interrupt a held note.
    fn load_instrument(&mut self, col: usize, index: usize, params: &SynthParams) -> bool {
        let Some(instrument) = self.instrument_store.instruments.get(index).cloned() else {
            return false;
        };
        crate::synth::apply_voice(channel_at(params, col), &instrument.voice);
        if let Some(slot) = self.instrument_loaded.get_mut(col) {
            *slot = Some((instrument.name, instrument.voice));
        }
        true
    }

    /// Step the library in `col` by `delta` places, from wherever it is now.
    fn step_instrument(&mut self, col: usize, delta: i32, params: &SynthParams) -> Option<String> {
        let from = self
            .instrument_loaded
            .get(col)
            .and_then(|slot| slot.as_ref())
            .and_then(|(name, _)| self.instrument_store.index_of(name));
        let (index, _) = self.instrument_store.step(from, delta)?;
        self.load_instrument(col, index, params);
        self.instrument_loaded
            .get(col)
            .and_then(|slot| slot.as_ref())
            .map(|(name, _)| name.clone())
    }

    /// Whether `col` has been changed since it was loaded.
    ///
    /// `apply_channel` and `capture_channel` are exact inverses — a `SharedF32`
    /// holds the bit pattern it was given — so this is an equality test and not
    /// a tolerance.
    fn register_edited(&self, col: usize, params: &SynthParams) -> bool {
        match self.instrument_loaded.get(col).and_then(|s| s.as_ref()) {
            Some((_, loaded)) => crate::synth::capture_voice(channel_at(params, col)) != *loaded,
            None => false,
        }
    }

    /// What the instrument row shows for `col`.
    ///
    /// `custom` when the register was never loaded from the library, the
    /// instrument's name while the register still matches it, and a leading `*`
    /// once it has been changed — a row naming an instrument the register no
    /// longer is would be worse than a blank one.
    ///
    /// The marker goes *first* because the column is only wide enough for a
    /// dozen characters and instrument names are longer than that: trailing it
    /// would put it exactly where the clipping happens, so `Rhodes Dark (edited)`
    /// would read as `Rhodes Dark (e` and the one thing the marker exists to say
    /// would be the one thing cut off.
    fn instrument_label(&self, col: usize, params: &SynthParams) -> String {
        let Some((name, _)) = self.instrument_loaded.get(col).and_then(|s| s.as_ref()) else {
            return "custom".to_string();
        };
        if self.register_edited(col, params) {
            format!("* {}", name)
        } else {
            name.clone()
        }
    }

    fn row_count(&self) -> usize {
        match self.focus {
            Focus::Transport => TRANSPORT_ROWS,
            Focus::Progression => self.progression.lock().unwrap().len(),
            Focus::Sinko => SINKO_ROWS,
            Focus::Synth => self.synth_rows(),
            Focus::SynthEnsembles => self.ensemble_store.ensembles.len() + 1,
            Focus::Eq => EQ_ROWS,
            Focus::Spectrum => SPECTRUM_ROWS,
            Focus::Fx => FX_ROWS,
        }
    }

    fn current_row(&self) -> usize {
        match self.focus {
            Focus::Transport => self.transport_row.min(TRANSPORT_ROWS - 1),
            Focus::Progression => self.progression_row,
            Focus::Sinko => self.sinko_row.min(SINKO_ROWS - 1),
            Focus::Synth => self.synth_row.min(self.synth_rows() - 1),
            Focus::SynthEnsembles => self.ensemble_row,
            Focus::Eq => self.eq_row.min(EQ_ROWS - 1),
            Focus::Spectrum => self.spectrum_row.min(SPECTRUM_ROWS - 1),
            Focus::Fx => self.fx_row.min(FX_ROWS - 1),
        }
    }

    fn set_current_row(&mut self, row: usize) {
        let count = self.row_count();
        let clamped = if count == 0 { 0 } else { row.min(count - 1) };
        match self.focus {
            Focus::Transport => self.transport_row = clamped,
            Focus::Progression => {
                self.progression_row = clamped;
                // Naming a row is a plain single-row selection: the gap above the
                // list and any extended range are both left behind.
                self.progression_before_first = false;
                self.progression_anchor = None;
                self.progression_menu = false;
                self.transport.set_audition_slot(None);
            }
            Focus::Sinko => self.sinko_row = clamped,
            Focus::Synth => {
                let rows = synth_row_count(self.synth_page);
                self.synth_row = clamped.min(rows - 1);
                // A master row has two columns, a channel row three.
                self.synth_col = self
                    .synth_col
                    .min(synth_col_count(self.synth_page, self.synth_row) - 1);
            }
            Focus::SynthEnsembles => self.ensemble_row = clamped,
            Focus::Eq => self.eq_row = clamped,
            Focus::Spectrum => self.spectrum_row = clamped,
            Focus::Fx => self.fx_row = clamped,
        }
    }

    /// The rows the Progression selection covers, inclusive.
    ///
    /// `None` when there is nothing to act on: an empty progression, or the
    /// cursor in the gap above the first chord — which is a place to paste, not
    /// a row to copy.
    fn selection(&self) -> Option<(usize, usize)> {
        let len = self.progression.lock().unwrap().len();
        self.selection_in(len)
    }

    /// The same, for a caller that already holds the progression lock.
    ///
    /// A `Mutex` is not reentrant, so the renderer — which holds it for the whole
    /// chord list — cannot call [`Self::selection`].
    fn selection_in(&self, len: usize) -> Option<(usize, usize)> {
        if self.progression_before_first {
            return None;
        }
        if len == 0 || self.progression_row >= len {
            return None;
        }
        let row = self.progression_row;
        match self.progression_anchor {
            // A stale anchor — one a structural edit pushed past the end — is
            // dropped rather than widening the range over rows that are gone.
            Some(anchor) if anchor < len => Some((row.min(anchor), row.max(anchor))),
            _ => Some((row, row)),
        }
    }

    /// How many rows the selection covers, for the panel's own summary.
    fn selection_len(&self) -> usize {
        self.selection()
            .map(|(start, end)| end - start + 1)
            .unwrap_or(0)
    }

    /// Move the Progression cursor, extending the selection with `Shift`.
    ///
    /// Up from the first chord goes into the gap above it rather than wrapping,
    /// because that gap is a real place to paste. `Shift` never selects the gap:
    /// a range is made of chords.
    fn move_progression(&mut self, delta: i32, extend: bool) {
        let len = self.progression.lock().unwrap().len();
        if len == 0 {
            self.progression_before_first = true;
            self.progression_anchor = None;
            return;
        }
        let from = self.progression_row.min(len - 1);
        let moved = if delta < 0 {
            if self.progression_before_first {
                false
            } else if self.progression_row == 0 {
                self.progression_before_first = true;
                true
            } else {
                self.progression_row -= 1;
                true
            }
        } else if self.progression_before_first {
            self.progression_before_first = false;
            true
        } else if self.progression_row + 1 < len {
            self.progression_row += 1;
            true
        } else {
            false
        };
        if !moved {
            return;
        }
        // Moving the selection is what exits an in-place audition.
        self.transport.set_audition_slot(None);
        if self.progression_before_first {
            self.progression_anchor = None;
        } else if extend {
            // The anchor is where the range started, so it is set once and then
            // stays put while the cursor walks.
            self.progression_anchor.get_or_insert(from);
        } else {
            self.progression_anchor = None;
        }
        // After the range is settled, not before: collapsing it is what takes
        // the menu away, and the cursor has to come out of a column that has
        // gone.
        self.refresh_progression_menu();
    }

    /// Select the whole progression. Returns false when there is nothing to
    /// select, or when it is already selected.
    fn select_all_chords(&mut self) -> bool {
        let len = self.progression.lock().unwrap().len();
        if len == 0 {
            return false;
        }
        let already = self.selection() == Some((0, len - 1));
        self.transport.set_audition_slot(None);
        self.refresh_progression_menu();
        self.progression_before_first = false;
        self.progression_row = len - 1;
        self.progression_anchor = if len > 1 { Some(0) } else { None };
        !already
    }

    /// Whether the Progression panel is showing its menu.
    ///
    /// The menu is the *group* actions, so it exists only while a run is
    /// selected: with one chord every command in it would be a no-op, and the
    /// panel keeps its original one-column shape — which is also where plain
    /// `←/→` means the offset nudge rather than choosing a column.
    fn progression_menu_active(&self) -> bool {
        self.focus == Focus::Progression && self.selection_len() > 1
    }

    /// Move between the chord list and the menu beside it.
    fn move_progression_column(&mut self, delta: i32) {
        if !self.progression_menu_active() {
            return;
        }
        if delta < 0 {
            self.progression_menu = false;
        } else {
            self.progression_menu = true;
            self.progression_menu_row = self.progression_menu_row.min(PROGRESSION_MENU.len() - 1);
        }
    }

    /// Drop the menu cursor when the menu itself has gone.
    ///
    /// Collapsing a run back to one chord takes the column away, so a cursor
    /// left standing in it would be a cursor on nothing.
    fn refresh_progression_menu(&mut self) {
        if !self.progression_menu_active() {
            self.progression_menu = false;
        }
    }

    /// Move within the Synth table's columns, clamped to the row's width.
    fn move_synth_col(&mut self, delta: i32) {
        let count = synth_col_count(self.synth_page, self.synth_row) as i32;
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
        if len == 0 {
            // An emptied list leaves the gap above position 1 as the only place
            // the cursor can be, which is also where the next paste should land.
            self.progression_before_first = true;
            self.progression_anchor = None;
        } else if let Some(anchor) = self.progression_anchor {
            if anchor >= len {
                self.progression_anchor = None;
            }
        }
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
    let synth = {
        // Opening the device and building the engine: the voice pool, the
        // wavetables, the effect bank, the reverb tanks and the delay lines, all
        // before the stream starts.
        let _scope = crate::timing::Scope::new("startup.synth");
        Synth::new(Some(output_tap.peak()), output_tap.timing())
            .map_err(|e| io::Error::other(e.to_string()))?
    };

    let rhythm_store = Arc::new(Mutex::new(RhythmStore::load(
        &rhythm_store::default_path(),
        &rhythm_store::user_path(),
    )?));
    let settings_path = settings::path();
    let settings = match Settings::load(&settings_path) {
        Ok(settings) => settings.clamped(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Settings::default(),
        Err(e) => {
            // Reported rather than fatal: a settings file is a convenience, and
            // losing one should cost a tempo rather than the session.
            logger.input(&format!("SETTINGS unreadable ({}); using defaults", e));
            Settings::default()
        }
    };

    // Four libraries parsed out of TOML at start-up. Small, but it is the last
    // thing between pressing enter and hearing a note, and it is the one part of
    // start-up that grows with what the user has saved.
    let (instrument_store, ensemble_store, presets, fx_presets) = {
        let _scope = crate::timing::Scope::new("startup.library");
        (
            InstrumentStore::load(&crate::instrument::user_path())?,
            EnsembleStore::load(&ensemble::user_path())?,
            EqPresetStore::load(&crate::eq::user_path())?,
            FxPresetStore::load(&crate::fx::user_path())?,
        )
    };

    let initial_key = settings.key;

    let default_ensemble = ensemble_store.find("Default").cloned();
    if let Some(ref e) = default_ensemble {
        let (channels, _) = e.resolve(|name| instrument_store.voice_of(name));
        crate::synth::apply_channels(
            synth.params(),
            [&channels[0], &channels[1], &channels[2]],
            &e.mixer,
        );
    }

    let progression = Arc::new(Mutex::new(Progression::new()));
    let transport = Transport::new(initial_key);
    transport.set_bpm(settings.bpm);

    // Seed note_length from the default ensemble.
    if let Some(ref e) = default_ensemble {
        transport.set_note_length(e.mixer.note_length);
    }

    // The master volume is the one persisted setting the *synth* owns, so it is
    // written into the live parameters rather than into the transport — and it
    // is written *after* the default ensemble, which carries a volume of its
    // own, so the level you left the instrument at wins over the palette's.
    synth.params().master_volume.set(settings.master_volume);

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
        metronome_open: false,
        metronome_row: 0,
        midi_open: false,
        midi_choice: MIDI_EXPORT,
        history: History::default(),
        history_view: HistoryView::Off,
        history_row: 0,
        history_scroll: 0,
        pending_play: None,
        history_held: None,
        history_timed: None,
        synth_row: 0,
        synth_col: 0,
        synth_page: SynthPage::Tone,
        instrument_store,
        // The startup ensemble is applied above, not loaded from the library, so
        // every register starts as `custom` — which is true.
        instrument_loaded: Default::default(),
        progression_row: 0,
        progression_before_first: false,
        progression_anchor: None,
        progression_menu: false,
        progression_menu_row: 0,
        audition_sounding: None,
        audition_release_at: None,
        ensemble_row: 0,
        fx_col: 0,
        fx_slot: 0,
        fx_param: 0,
        fx_row: 0,
        eq_row: 0,
        eq_target: 0,
        eq_band: 0,
        spectrum_row: 0,
        spectrum_range: SPECTRUM_DEFAULT_RANGE,
        spectrum_hold: true,
        spectrum_peaks: [[0.0; crate::eq::EQ_BANDS]; ANALYZER_TAPS],
        presets,
        fx_presets,
        modal: None,
        ensemble_store,
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
        rhythm_user_path: rhythm_store::user_path(),
        flash_until: None,
        taps: TapTracker::default(),
        export_dir,
        export_status: None,
        import_status: None,
        settings_path,
        settings_seen: settings,
        settings_dirty: None,
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
        // One frame of interface work, dropped before the wait for input: a
        // scope that included `event::poll` would be a measurement of how long
        // nobody pressed a key.
        let frame = crate::timing::Scope::new("tui.frame");

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

        state.poll_spectrum(synth.params());
        state.poll_settings(synth.params(), logger);
        state.poll_tempo(synth.params());
        state.poll_history();
        render(stdout, synth.params(), state, Screen::read())?;

        while let Some(ev) = scheduler.try_recv() {
            match ev {
                SchedulerEvent::Stab {
                    group,
                    notes,
                    gain,
                    velocity,
                } => synth.play_stab(group, &notes, gain, velocity),
                SchedulerEvent::ReleaseStab { group } => synth.stop_stab(group),
                SchedulerEvent::Silence => synth.silence(),
                SchedulerEvent::Click {
                    strong,
                    sound,
                    volume,
                } => synth.play_click(strong, sound, volume),
            }
        }

        pump_audition(state, synth, logger);
        drop(frame);

        if !event::poll(Duration::from_millis(5))? {
            continue;
        }

        let Event::Key(ev) = event::read()? else {
            continue;
        };

        if let Some(Modal::InstrumentNameInput {
            col,
            ref mut buffer,
        }) = state.modal
        {
            if ev.kind == KeyEventKind::Press {
                match ev.code {
                    KeyCode::Esc => {
                        state.modal = None;
                        logger.input("MODAL cancel instrument name");
                    }
                    KeyCode::Enter => {
                        let name = if buffer.trim().is_empty() {
                            "Untitled Instrument".to_string()
                        } else {
                            buffer.trim().to_string()
                        };
                        save_instrument(
                            state,
                            synth.params(),
                            col,
                            &name,
                            &crate::instrument::user_path(),
                            logger,
                        );
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

        if let Some(Modal::EnsembleNameInput { ref mut buffer }) = state.modal {
            if ev.kind == KeyEventKind::Press {
                match ev.code {
                    KeyCode::Esc => {
                        state.modal = None;
                        logger.input("MODAL cancel ensemble name");
                    }
                    KeyCode::Enter => {
                        let name = if buffer.trim().is_empty() {
                            "Untitled".to_string()
                        } else {
                            buffer.trim().to_string()
                        };
                        save_ensemble(
                            state,
                            synth,
                            &name,
                            &ensemble::user_path(),
                            &crate::instrument::user_path(),
                            logger,
                        );
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

        if let Some(Modal::EqPresetNameInput { ref mut buffer }) = state.modal {
            if ev.kind == KeyEventKind::Press {
                match ev.code {
                    KeyCode::Esc => {
                        state.modal = None;
                        logger.input("MODAL cancel eq preset name");
                    }
                    KeyCode::Enter => {
                        let name = if buffer.trim().is_empty() {
                            "Untitled Curve".to_string()
                        } else {
                            buffer.trim().to_string()
                        };
                        save_eq_preset(state, synth.params(), &name, &crate::eq::user_path(), logger);
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

        if let Some(Modal::FxPresetNameInput { ref mut buffer }) = state.modal {
            if ev.kind == KeyEventKind::Press {
                match ev.code {
                    KeyCode::Esc => {
                        state.modal = None;
                        logger.input("MODAL cancel fx preset name");
                    }
                    KeyCode::Enter => {
                        let name = if buffer.trim().is_empty() {
                            "Untitled Effect".to_string()
                        } else {
                            buffer.trim().to_string()
                        };
                        save_fx_preset(
                            state,
                            synth.params(),
                            &name,
                            &crate::fx::user_path(),
                            logger,
                        );
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
                        // The picker auditions by *loading*, so cancelling has to
                        // undo the last thing it loaded. It never touched the
                        // row's marker, so restoring the channel restores the
                        // row as well.
                        if let Some(Modal::InstrumentPicker { col, original, .. }) = &state.modal {
                            crate::synth::apply_channel(channel_at(synth.params(), *col), original);
                            logger.input(&format!(
                                "MODAL instrument cancel on {}",
                                CHANNEL_COLUMN_LABELS[*col]
                            ));
                        } else {
                            logger.input("MODAL cancel");
                        }
                        state.modal = None;
                        continue;
                    }
                    // Saving what the register holds, from inside the picker,
                    // because that is where a player is thinking about
                    // instruments. `s` is free here: a modal that takes every
                    // key is the one place a bare letter is unambiguous.
                    KeyCode::Char('s') | KeyCode::Char('S')
                        if matches!(state.modal, Some(Modal::InstrumentPicker { .. })) =>
                    {
                        if let Some(Modal::InstrumentPicker { col, .. }) = state.modal {
                            let existing = state.instrument_label(col, synth.params());
                            state.modal = Some(Modal::InstrumentNameInput {
                                col,
                                buffer: default_instrument_name(&existing)
                                    .chars()
                                    .take(32)
                                    .collect(),
                            });
                        }
                        continue;
                    }
                    KeyCode::Up | KeyCode::Down
                        if matches!(state.modal, Some(Modal::InstrumentPicker { .. })) =>
                    {
                        let coarse = ev.modifiers.contains(KeyModifiers::SHIFT);
                        let step = if coarse { PATTERN_COARSE_STEP } else { 1 };
                        let delta = if matches!(ev.code, KeyCode::Up) {
                            step
                        } else {
                            -step
                        };
                        if let Some(Modal::InstrumentPicker { col, index, .. }) = &mut state.modal {
                            let col = *col;
                            let len = state.instrument_store.instruments.len();
                            if len > 0 {
                                let next = (*index as i32 + delta).rem_euclid(len as i32) as usize;
                                *index = next;
                                let voice = state.instrument_store.instruments[next].voice.clone();
                                crate::synth::apply_voice(channel_at(synth.params(), col), &voice);
                            }
                        }
                        continue;
                    }
                    KeyCode::Enter => match modal {
                        Modal::InstrumentPicker { col, index, .. } => {
                            let col = *col;
                            let picked = state
                                .instrument_store
                                .instruments
                                .get(*index)
                                .map(|i| (i.name.clone(), i.voice.clone()));
                            if let Some((name, voice)) = picked {
                                crate::synth::apply_voice(channel_at(synth.params(), col), &voice);
                                state.instrument_loaded[col] = Some((name.clone(), voice));
                                logger.input(&format!(
                                    "MODAL {} instrument = {}",
                                    CHANNEL_COLUMN_LABELS[col], name
                                ));
                            }
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
                        Modal::EnsembleNameInput { .. } => unreachable!(),
                        Modal::ImportPathInput { .. } => unreachable!(),
                        Modal::RhythmNameInput { .. } => unreachable!(),
                        Modal::InstrumentNameInput { .. } => unreachable!(),
                        Modal::EqPresetNameInput { .. } => unreachable!(),
                        Modal::FxPresetNameInput { .. } => unreachable!(),
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
                        // Whatever the settle timer was still holding.
                        state.flush_settings(logger);
                        return Ok(());
                    }
                    continue;
                }

                // The metronome panel covers the transport, so it takes every
                // key before anything else does — including `Esc`, which closes
                // it rather than stopping the transport.
                if state.metronome_open {
                    handle_metronome_key(state, &ev, logger);
                    continue;
                }

                // The `[MIDI]` chooser, on the same terms: it is a row opened
                // up, so `Esc` closes it and `Enter` runs the selected side.
                // Up and down are left to the row cursor, which closes it on the
                // way past.
                if state.midi_open {
                    match ev.code {
                        KeyCode::Esc => {
                            state.midi_open = false;
                            logger.input("TRANSPORT midi menu closed");
                            continue;
                        }
                        KeyCode::Enter => {
                            run_midi_action(state, logger);
                            continue;
                        }
                        KeyCode::Left => {
                            state.midi_choice = MIDI_EXPORT;
                            continue;
                        }
                        KeyCode::Right => {
                            state.midi_choice = MIDI_IMPORT;
                            continue;
                        }
                        _ => {}
                    }
                }

                // Cmd/Ctrl+A selects every chord. Checked before the chord
                // grammar, which would otherwise take `a` as a home-row key and
                // add it to the held set. `Super` is what macOS sends where a
                // terminal reports it; `Ctrl` is the fallback where it does not.
                if ev
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
                    && matches!(ev.code, KeyCode::Char('a') | KeyCode::Char('A'))
                {
                    select_all_chords(state, logger);
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
                        } else if pos.hotkey() == Some(Hotkey::HistoryPlay) {
                            // The one hotkey with a release half: `r` holds a
                            // chord, so letting it go is what ends it.
                            history_hold(state, false, logger);
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
        // The same list the row walks: every key, mode included. Up/down stay
        // semitone steps that keep the mode, which is the other question a player
        // asks, so both gestures are here.
        KeyCode::Left | KeyCode::Right => {
            let delta = if matches!(ev.code, KeyCode::Left) { -1 } else { 1 };
            let step = if ev.modifiers.contains(KeyModifiers::SHIFT) {
                6
            } else {
                1
            };
            current = key_at_choice(key_choice(current) + delta * step);
        }
        _ => {}
    }

    if keep_editing {
        state.edit = Edit::TrackKey { initial, current };
    }
}

/// Move the cursor or adjust a value, on whichever panel has focus.
///
/// Split out of [`handle_panel_key`] because it is the half that needs only the
/// mixer params — a `Synth` owns an audio device no test can build, so this is
/// the half the suite can drive.
fn handle_panel_arrow(
    state: &mut AppState,
    params: &SynthParams,
    ev: &event::KeyEvent,
    logger: &Logger,
) {
    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
    match ev.code {
        // Up/down is the row cursor everywhere, except in the Progression panel,
        // where `Shift` extends a selection, up from the first chord goes into
        // the gap above it, and the menu beside the list is a column of its own.
        KeyCode::Up | KeyCode::Down => {
            let delta = if matches!(ev.code, KeyCode::Up) {
                -1
            } else {
                1
            };
            // Stepping off the `[MIDI]` row folds its chooser back up, so the
            // button is a button again the next time the cursor lands on it.
            if state.midi_open {
                state.midi_open = false;
            }
            if state.focus == Focus::Progression && state.progression_menu {
                let row = state.progression_menu_row as i32 + delta;
                state.progression_menu_row =
                    row.clamp(0, PROGRESSION_MENU.len() as i32 - 1) as usize;
            } else if state.focus == Focus::Progression {
                state.move_progression(delta, shift);
            } else {
                let row = state.current_row();
                let next = if delta < 0 {
                    row.saturating_sub(1)
                } else {
                    row + 1
                };
                state.set_current_row(next);
            }
        }
        // The two panels with two columns take plain `←/→` as "pick the column"
        // and `Shift+←/→` as the value nudge. Every other panel keeps `←/→` as
        // "adjust", because it has only one column to be on.
        KeyCode::Left | KeyCode::Right => {
            let delta = if matches!(ev.code, KeyCode::Left) {
                -1
            } else {
                1
            };
            // The open chooser takes the arrows before the row cursor can: with
            // two options, left and right *are* the selection rather than a step
            // through a list.
            if state.midi_open {
                state.midi_choice = if matches!(ev.code, KeyCode::Left) {
                    MIDI_EXPORT
                } else {
                    MIDI_IMPORT
                };
                return;
            }
            let arrow = match state.focus {
                Focus::Synth | Focus::Progression => panel_arrow(ev.code, shift),
                _ => None,
            };
            match (state.focus, arrow) {
                (Focus::Synth, Some(PanelArrow::Column(c))) => state.move_synth_col(c),
                (Focus::Synth, Some(PanelArrow::Nudge(n))) => {
                    adjust_current(state, params, n, logger)
                }
                (Focus::Progression, Some(PanelArrow::Nudge(n))) => nudge_offset(state, n, logger),
                (Focus::Progression, Some(PanelArrow::Column(c))) => {
                    if state.progression_menu_active() {
                        state.move_progression_column(c)
                    } else {
                        // One column, so the arrows are the offset nudge they
                        // have always been on this panel.
                        nudge_offset(state, c, logger)
                    }
                }
                _ => adjust_current_with(state, params, delta, shift, logger),
            }
        }
        _ => {}
    }
}

/// What an arrow key means on a panel that has two columns.
///
/// The Synth table and the Progression panel are the two-dimensional surfaces:
/// plain `←/→` picks the column and `Shift+←/→` is the value nudge. Every other
/// panel keeps `←/→` as "adjust".
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum PanelArrow {
    Column(i32),
    Nudge(i32),
}

fn panel_arrow(code: KeyCode, shift: bool) -> Option<PanelArrow> {
    match code {
        KeyCode::Left if !shift => Some(PanelArrow::Column(-1)),
        KeyCode::Right if !shift => Some(PanelArrow::Column(1)),
        KeyCode::Left => Some(PanelArrow::Nudge(-1)),
        KeyCode::Right => Some(PanelArrow::Nudge(1)),
        _ => None,
    }
}

/// Open an edit on the Synth cursor.
///
/// Split out of `primary_action` because everything test-visible needs only
/// `SynthParams`; a `Synth` owns an audio device that no test can create.
fn begin_synth_edit(state: &mut AppState, params: &SynthParams) {
    let cell = state.synth_cell();
    let (initial, before) = {
        let ctx = state.mixer_ctx(params);
        (cell.value(ctx), cell.fx_before(ctx))
    };
    state.edit = Edit::SynthCell {
        cell,
        initial,
        before,
    };
}

/// Apply an ensemble: resolve its three placements, write them, and name the
/// registers they came from.
///
/// Loading an ensemble is the one time the instrument row knows all three
/// registers at once, because an ensemble says which instrument each one is.
fn apply_ensemble(state: &mut AppState, synth: &Synth, ensemble: &Ensemble, logger: &Logger) {
    let (channels, named, missing) = resolve_ensemble(&state.instrument_store, ensemble);

    crate::synth::apply_channels(
        synth.params(),
        [&channels[0], &channels[1], &channels[2]],
        &ensemble.mixer,
    );
    state.transport.set_note_length(ensemble.mixer.note_length);
    for (col, item) in named.into_iter().enumerate() {
        if let Some(slot) = state.instrument_loaded.get_mut(col) {
            *slot = Some(item);
        }
    }
    if !missing.is_empty() {
        // Reported rather than hidden: the registers still loaded, with a
        // neutral voice, because one bad name should not cost the whole palette.
        logger.input(&format!(
            "PRESET '{}' names instruments that are not in the library: {:?}",
            ensemble.name, missing
        ));
    }
    logger.input(&format!("ENSEMBLE load '{}'", ensemble.name));
}

/// Work out what an ensemble plays, and what each register now is.
///
/// Split out from applying it so the resolution — the interesting half, and the
/// half a user file can get wrong — can be tested without an audio device.
fn resolve_ensemble(
    instruments: &InstrumentStore,
    ensemble: &Ensemble,
) -> ([ComposedChannel; 3], [(String, VoicePatch); 3], Vec<String>) {
    let (channels, missing) = ensemble.resolve(|name| instruments.voice_of(name));
    let named: Vec<(String, VoicePatch)> = ensemble
        .placements()
        .iter()
        .zip(channels.iter())
        .map(|(placement, channel)| (placement.instrument.clone(), channel.voice.clone()))
        .collect();
    let named: [(String, VoicePatch); 3] = named
        .try_into()
        .unwrap_or_else(|_| unreachable!("an ensemble always has three registers"));
    (channels, named, missing)
}

/// Keep the live sound as a named ensemble.
///
/// Each register's voice is either already a library instrument or becomes one
/// named after the ensemble, because a placement has to name something and the
/// alternative is losing a sound the user dialled in.
fn save_ensemble(
    state: &mut AppState,
    synth: &Synth,
    name: &str,
    ensembles_path: &Path,
    instruments_path: &Path,
    logger: &Logger,
) {
    let (channels, mixer) = synth.capture_channels(state.transport.note_length());
    let mut placements = Vec::with_capacity(3);
    let mut created = false;
    for (col, channel) in channels.iter().enumerate() {
        let instrument = match state.instrument_store.name_for_voice(&channel.voice) {
            Some(existing) => existing,
            None => {
                let made = format!("{} {}", name, Ensemble::REGISTERS[col]);
                state.instrument_store.add(Instrument {
                    name: made.clone(),
                    voice: channel.voice.clone(),
                });
                created = true;
                made
            }
        };
        placements.push(channel.split(&instrument));
    }

    if created {
        if let Err(e) = state.instrument_store.save(instruments_path) {
            logger.input(&format!("SAVE ERROR: {}", e));
        }
    }

    let ensemble = Ensemble {
        name: name.to_string(),
        low: placements[0].clone(),
        mid: placements[1].clone(),
        high: placements[2].clone(),
        mixer,
    };
    state.ensemble_store.add(ensemble);
    match state.ensemble_store.save(ensembles_path) {
        Ok(()) => logger.input(&format!("SYNTH saved ensemble '{}'", name)),
        Err(e) => logger.input(&format!("SAVE ERROR: {}", e)),
    }
}

/// Keep `col`'s current design as a named instrument.
///
/// The path is a parameter rather than read from [`crate::instrument::user_path`]
/// here so a test can write to a temporary file instead of the checkout's.
fn save_instrument(
    state: &mut AppState,
    params: &SynthParams,
    col: usize,
    name: &str,
    path: &Path,
    logger: &Logger,
) {
    let voice = crate::synth::capture_voice(channel_at(params, col));
    state.instrument_store.add(Instrument {
        name: name.to_string(),
        voice: voice.clone(),
    });
    match state.instrument_store.save(path) {
        Ok(()) => logger.input(&format!(
            "SYNTH saved instrument '{}' from {}",
            name, CHANNEL_COLUMN_LABELS[col]
        )),
        Err(e) => logger.input(&format!("SAVE ERROR: {}", e)),
    }
    // The register *is* this instrument now, so the row says so rather than
    // falling back to `custom`.
    state.instrument_loaded[col] = Some((name.to_string(), voice));
}

/// What to pre-fill the name prompt with.
///
/// A register that is already an instrument offers that name with a `2` on it,
/// so saving a tweak keeps the original instead of silently replacing it. A
/// register that is `custom`, or that has drifted from what it was loaded from,
/// offers nothing — there is no name that would be true.
fn default_instrument_name(existing: &str) -> String {
    if existing == "custom" || existing.starts_with('*') {
        String::new()
    } else {
        format!("{} 2", existing)
    }
}

/// Open the instrument picker on the register the cursor is in.
///
/// It never touches the row's `(edited)` marker: the picker auditions by
/// loading, and only `Enter` names what it landed on. That is what makes `Esc`
/// exact — restoring the channel restores the row with it.
fn open_instrument_picker(state: &mut AppState, params: &SynthParams, logger: &Logger) {
    let Some(col) = state.synth_cell().col() else {
        return;
    };
    if state.instrument_store.instruments.is_empty() {
        state.flash(400);
        logger.input("SYNTH no instruments to load");
        return;
    }
    let current = state
        .instrument_loaded
        .get(col)
        .and_then(|slot| slot.as_ref())
        .and_then(|(name, _)| state.instrument_store.index_of(name));
    let original = crate::synth::capture_channel(channel_at(params, col));
    state.modal = Some(Modal::InstrumentPicker {
        col,
        index: current.unwrap_or(0),
        original: Box::new(original),
    });
    logger.input(&format!(
        "SYNTH instrument picker on {}",
        CHANNEL_COLUMN_LABELS[col]
    ));
}

/// Edit one Synth table cell.
///
/// Arrows adjust the *live* parameter, so the change is audible as it happens —
/// that is the point of editing here rather than typing a value. `Enter` keeps
/// the result; `Esc` writes the pre-edit value back, which makes a sweep you
/// did not like a no-op.
fn handle_synth_edit(state: &mut AppState, params: &SynthParams, ev: &event::KeyEvent, logger: &Logger) {
    let edit = std::mem::replace(&mut state.edit, Edit::None);
    let (cell, initial, before) = match edit {
        Edit::SynthCell {
            cell,
            initial,
            before,
        } => (cell, initial, before),
        other => {
            state.edit = other;
            return;
        }
    };

    if ev.kind != KeyEventKind::Press {
        state.edit = Edit::SynthCell {
            cell,
            initial,
            before,
        };
        return;
    }

    let transport = state.transport.clone();
    let presets = state.fx_presets.clone();
    let mut keep_editing = true;
    let ctx = MixerCtx::new(params, &transport, &presets);

    match ev.code {
        KeyCode::Esc => {
            cell.restore(ctx, initial, before);
            keep_editing = false;
            logger.input(&format!("SYNTH cancel {}", cell.label()));
        }
        KeyCode::Enter => {
            keep_editing = false;
            logger.input(&format!("SYNTH commit {}", cell.label()));
        }
        // Fine and coarse steps on the two axes, so one gesture covers both
        // "nudge it" and "sweep it" without leaving the cell.
        KeyCode::Left => cell.adjust(ctx, -1),
        KeyCode::Right => cell.adjust(ctx, 1),
        KeyCode::Down => cell.adjust(ctx, -5),
        KeyCode::Up => cell.adjust(ctx, 5),
        _ => {}
    }

    if keep_editing {
        state.edit = Edit::SynthCell {
            cell,
            initial,
            before,
        };
    }
}

/// Step the Synth table to the next or previous page.
///
/// `PageUp`/`PageDown` rather than a digit: every digit near the home row is
/// already a performance key — `1` toggles the metronome, `3` and `4` tap the
/// transport — and these two are the keys actually named for paging.
fn cycle_synth_page(state: &mut AppState, delta: i32, logger: &Logger) {
    let all = SynthPage::ALL;
    let index = all.iter().position(|&p| p == state.synth_page).unwrap_or(0) as i32;
    let page = all[(index + delta).rem_euclid(all.len() as i32) as usize];
    state.synth_page = page;
    // The cursor keeps its place where the new page is tall enough for it, so
    // stepping forward and back returns to the row you were on.
    let rows = synth_row_count(page);
    state.synth_row = state.synth_row.min(rows - 1);
    state.synth_col = state
        .synth_col
        .min(synth_col_count(page, state.synth_row) - 1);
    logger.input(&format!("SYNTH page = {}", page.name()));
}

fn handle_panel_key(
    state: &mut AppState,
    synth: &Synth,
    ev: &event::KeyEvent,
    logger: &Logger,
) {
    match ev.code {
        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {
            handle_panel_arrow(state, synth.params(), ev, logger)
        }
        // Not while an edit is open: the arrows belong to the value then, and a
        // page change under a live edit would move the cursor out from under it.
        KeyCode::PageDown if state.focus == Focus::Synth && matches!(state.edit, Edit::None) => {
            cycle_synth_page(state, 1, logger)
        }
        KeyCode::PageUp if state.focus == Focus::Synth && matches!(state.edit, Edit::None) => {
            cycle_synth_page(state, -1, logger)
        }
        KeyCode::Enter => match enter_intent(state, ev.modifiers.contains(KeyModifiers::CONTROL)) {
            EnterIntent::CommitChord { to_end } => add_current_chord(state, to_end, logger),
            EnterIntent::PanelAction => primary_action(state, synth, logger),
        },
        _ => {}
    }
}

/// The track key as one flat list of choices: C major, C minor, C# major, ...
///
/// Two knobs collapsed into one list. Stepping left/right walks every key there
/// is — which is the question a player actually asks ("a semitone up, same
/// mode") — rather than toggling the mode and leaving the root where it was.
pub const KEY_CHOICES: usize = 24;

/// The choice index of a key: its pitch class doubled, plus one for minor.
pub fn key_choice(key: Key) -> i32 {
    let pitch_class = (key.tonic % 12) as i32;
    let minor = if key.scale == Scale::Minor { 1 } else { 0 };
    (pitch_class * 2 + minor).rem_euclid(KEY_CHOICES as i32)
}

/// The key at a choice index, pinned to the octave starting at middle C — the
/// same range the editor has always used, so a key change never transposes the
/// progression by an octave underneath the player.
pub fn key_at_choice(index: i32) -> Key {
    let index = index.rem_euclid(KEY_CHOICES as i32);
    let pitch_class = (index / 2) as u8;
    let scale = if index % 2 == 1 {
        Scale::Minor
    } else {
        Scale::Major
    };
    Key::new(60 + pitch_class, scale)
}

/// Step the track key along [`KEY_CHOICES`], six at a time when `Shift` is held
/// — which lands a fourth above, the interval a modulation usually moves in.
fn move_track_key(state: &mut AppState, delta: i32, coarse: bool) {
    let step = if coarse { 6 } else { 1 };
    let next = key_choice(state.transport.key()) + delta * step;
    state.transport.set_key(key_at_choice(next));
}

fn adjust_current(state: &mut AppState, params: &SynthParams, delta: i32, logger: &Logger) {
    adjust_current_with(state, params, delta, false, logger);
}

/// The same, with `Shift` held.
///
/// `coarse` is the big step: five patterns at a time in the palette, ten BPM on
/// the transport. Everything else ignores it and adjusts one rung, because there
/// is nothing to coarsen — a toggle is a toggle.
fn adjust_current_with(
    state: &mut AppState,
    params: &SynthParams,
    delta: i32,
    coarse: bool,
    _logger: &Logger,
) {
    let row = state.current_row();
    match state.focus {
        Focus::Transport => match row {
            TRANSPORT_ROW_BPM => {
                let step = if coarse { BPM_COARSE_STEP } else { 1 };
                let v = state.transport.bpm() as i32 + delta * step;
                state.transport.set_bpm(v.clamp(BPM_MIN as i32, BPM_MAX as i32) as u16);
            }
            TRANSPORT_ROW_LOOP => {
                let cur = state.transport.looping.load(Ordering::Relaxed);
                state.transport.looping.store(!cur, Ordering::Relaxed);
            }
            TRANSPORT_ROW_METRONOME => toggle_metronome(state, _logger),
            TRANSPORT_ROW_PLAYING => toggle_playback(state, _logger),
            TRANSPORT_ROW_KEY => move_track_key(state, delta, coarse),
            // The same value the Synth panel's master block writes, through the
            // same function: one number with two places to reach it, not two
            // numbers that agree until they do not.
            TRANSPORT_ROW_VOLUME => {
                let ctx = state.mixer_ctx(params);
                MixerParam::MasterVolume.adjust(ctx, delta)
            }
            _ => {}
        },
        // The Progression panel's arrows nudge the selected entry's offset:
        // that is where the chord list is on screen, so that is where a chord
        // gets moved off its downbeat. The Sinko panel's offset row does the
        // same thing to the same value.
        Focus::Progression => nudge_offset(state, delta, _logger),
        // The log covers this panel while it is up, so its rows are not on
        // screen — and a key that acts on a row you cannot see is the one thing
        // the panel scoping exists to prevent.
        Focus::Sinko if state.history_view.is_on() => {}
        Focus::Sinko => match row {
            SINKO_ROW_CHORD => move_sinko_chord(state, delta, _logger),
            SINKO_ROW_PATTERN => {
                let step = if coarse { PATTERN_COARSE_STEP } else { 1 };
                cycle_assigned_pattern(state, delta * step, _logger);
            }
            SINKO_ROW_OFFSET => nudge_offset(state, delta * if coarse { 4 } else { 1 }, _logger),
            SINKO_ROW_QUANT => cycle_resolution(state, delta, _logger),
            SINKO_ROW_SWING => cycle_swing(state, delta, _logger),
            SINKO_ROW_HITS => move_cell_cursor(state, delta, _logger),
            SINKO_ROW_LENGTH => cycle_cell_length(state, delta, _logger),
            SINKO_ROW_ACCENT => cycle_cell_accent(state, delta, _logger),
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
        Focus::Synth => {
            let cell = state.synth_cell();
            if cell.is_instrument() {
                let col = cell.col().unwrap_or(0);
                let step = if coarse { PATTERN_COARSE_STEP } else { 1 };
                if let Some(name) = state.step_instrument(col, delta * step, params) {
                    _logger.input(&format!(
                        "SYNTH {} instrument = {}",
                        CHANNEL_COLUMN_LABELS[col], name
                    ));
                }
            } else {
                let ctx = state.mixer_ctx(params);
                cell.adjust(ctx, delta);
            }
        }
        Focus::SynthEnsembles => {}
        Focus::Eq => adjust_eq(state, params, delta, coarse, _logger),
        Focus::Spectrum => adjust_spectrum(state, params, delta, _logger),
        Focus::Fx => adjust_fx(state, params, delta, coarse, _logger),
    }
}

/// What `Enter` does on a Transport row.
///
/// Split out of [`primary_action`] because everything here needs only the mixer
/// params, and a `Synth` owns an audio device that no test can create — so this
/// half of the transport is testable and the rest is not.
fn transport_action(state: &mut AppState, params: &SynthParams, row: usize, logger: &Logger) {
    match row {
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
        // `Enter` opens the metronome panel rather than toggling: the toggle
        // is one arrow press away on this row, and the panel is where the
        // click's own settings live.
        TRANSPORT_ROW_METRONOME => open_metronome_panel(state, logger),
        TRANSPORT_ROW_PLAYING => toggle_playback(state, logger),
        TRANSPORT_ROW_KEY => {
            state.edit = Edit::TrackKey {
                initial: state.transport.key(),
                current: state.transport.key(),
            };
            logger.input("KEY edit begin");
        }
        TRANSPORT_ROW_VOLUME => {
            // The Synth table's own editor, on the cell that same value lives in:
            // arrows adjust it live and `Esc` puts the old value back, exactly as
            // editing it over there does.
            let (row, col) = master_row_of(MixerParam::MasterVolume);
            let cell = SynthCell::Master { row, col };
            let initial = {
                let ctx = state.mixer_ctx(params);
                MixerParam::MasterVolume.value(ctx)
            };
            state.edit = Edit::SynthCell {
                cell,
                initial,
                before: None,
            };
            logger.input("TRANSPORT master volume edit begin");
        }
        // Closed, the button opens its chooser; open, it runs the chosen side.
        // One row and two presses, which is what collapsing the pair of file
        // actions bought.
        TRANSPORT_ROW_MIDI => {
            if state.midi_open {
                run_midi_action(state, logger);
            } else {
                state.midi_open = true;
                state.midi_choice = MIDI_EXPORT;
                logger.input("TRANSPORT midi menu open");
            }
        }
        _ => {}
    }
}

/// Run whichever half of the `[MIDI]` chooser is selected, and fold it back up.
fn run_midi_action(state: &mut AppState, logger: &Logger) {
    state.midi_open = false;
    match state.midi_choice {
        MIDI_IMPORT => open_import_modal(state, logger),
        _ => export_midi(state, logger),
    }
}

/// Open the FX panel on one slot of the rack the table's cursor is in.
///
/// The cursor lands on the `type` row rather than the top: the reason to leave
/// the table for a slot is to change the effect, and starting on the row that
/// does that is one press less of navigation for the one thing the panel was
/// opened for. `Shift+←/→` on the table's own slot row already auditions kinds
/// without leaving it.
fn open_fx_panel(state: &mut AppState, slot: usize, logger: &Logger) {
    if let SynthCell::Channel { col, .. } = state.synth_cell() {
        state.fx_col = col.min(CHANNEL_COUNT - 1);
    }
    state.fx_slot = slot.min(CHAIN_SLOTS - 1);
    state.fx_param = 0;
    state.fx_row = FX_ROW_TYPE;
    state.focus = Focus::Fx;
    logger.input(&format!(
        "FX open {} slot {}",
        fx_rack_name(state),
        state.fx_slot + 1
    ));
}

fn primary_action(state: &mut AppState, synth: &Synth, logger: &Logger) {
    let row = state.current_row();
    match state.focus {
        Focus::Transport => transport_action(state, synth.params(), row, logger),
        Focus::Progression => {
            if state.progression_menu {
                run_progression_menu(state, logger);
            } else {
                add_current_chord(state, false, logger);
            }
        }
        Focus::Sinko if state.history_view.is_on() => {}
        Focus::Sinko => sinko_action(state, row, logger),
        Focus::Synth => {
            let cell = state.synth_cell();
            if cell.is_instrument() {
                open_instrument_picker(state, synth.params(), logger);
            } else if let Some(slot) = cell.fx_slot_index() {
                open_fx_panel(state, slot, logger);
            } else {
                begin_synth_edit(state, synth.params());
                logger.input(&format!("SYNTH edit {}", cell.label()));
            }
        }
        Focus::Eq => eq_action(state, synth.params(), logger),
        Focus::Spectrum => spectrum_action(state, logger),
        Focus::Fx => fx_action(state, synth.params(), row, logger),
        Focus::SynthEnsembles => {
            let preset_count = state.ensemble_store.ensembles.len();
            if row < preset_count {
                let name = state.ensemble_store.ensembles[row].name.clone();
                if let Some(e) = state.ensemble_store.find(&name).cloned() {
                    apply_ensemble(state, synth, &e, logger);
                }
            } else {
                state.modal = Some(Modal::EnsembleNameInput {
                    buffer: String::new(),
                });
                logger.input("ENSEMBLE save-as modal");
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
    // Rendering a whole progression to a file is the one thing the interface
    // does that scales with the length of the session rather than with the
    // frame, so it is worth knowing what it costs.
    let _scope = crate::timing::Scope::new("export");

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
            let swing = state.transport.swing();
            let rhythms = state.rhythm_store.lock().unwrap();
            project::encode(&prog, &rhythms, key, bpm, note_length).map(|document| {
                // The patterns travel inside the slots, so the score and the
                // document are built from the same entries.
                let score =
                    midi::render_progression_with_swing(&prog.slots, &key, bpm, note_length, swing);
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
    let _scope = crate::timing::Scope::new("import");

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
            state.transport.set_audition_slot(None);

            // Bring the file's patterns into the library, so the imported
            // progression resolves instead of falling back to whole bars.
            let imported = restored.rhythms.len();
            {
                let mut rhythms = state.rhythm_store.lock().unwrap();
                for pattern in restored.rhythms {
                    rhythms.add(pattern);
                }
                if imported > 0 {
                    if let Err(e) = rhythms.save(&state.rhythm_user_path) {
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
        } else if state.progression_before_first {
            // The cursor is in the gap above the first chord, so Enter adds
            // there: the same place a paste would land.
            prog.insert_at(0, Slot::Chord(entry))
        } else {
            prog.insert_at(state.progression_row + 1, Slot::Chord(entry))
        }
    };

    state.update_progression_len();
    state.progression_row = inserted_at;
    state.progression_before_first = false;
    state.progression_anchor = None;
    state.transport.set_audition_slot(None);
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
/// the Ensembles panel's `[Save As...]` row had).
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
        // The menu is a list of commands, so `Enter` runs one even while a chord
        // is held — the same promise the Sinko panel's buttons make.
        Focus::Progression => state.progression_menu,
        Focus::Transport => matches!(
            state.current_row(),
            // The play control is a button now that the space bar latches
            // registers instead: it is the only on-screen way to start playback.
            TRANSPORT_ROW_PLAYING | TRANSPORT_ROW_MIDI
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
        Focus::SynthEnsembles => state.current_row() >= state.ensemble_store.ensembles.len(),
        Focus::Eq => state.current_row() == EQ_ROW_SAVE,
        Focus::Spectrum => state.current_row() == SPECTRUM_ROW_RESET,
        Focus::Fx => fx_row_is_button(state.current_row()),
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
    let hotkey = hotkey.shifted(shift);

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
        Hotkey::HistoryView => cycle_history_view(state, logger),
        Hotkey::HistoryBack => move_history(state, -1, logger),
        Hotkey::HistoryForward => move_history(state, 1, logger),
        Hotkey::HistoryRecall => history_recall(state, logger),
        // The press half of `r`. Its release is handled where every other key
        // release is, because holding a chord and holding a history row are the
        // same gesture to the keyboard.
        Hotkey::HistoryPlay => history_hold(state, true, logger),
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
                let edit = match hotkey {
                    Hotkey::CopyChord => ProgressionEdit::Copy,
                    Hotkey::PasteChord => ProgressionEdit::Paste,
                    Hotkey::DeleteChord => ProgressionEdit::Delete,
                    Hotkey::Undo => ProgressionEdit::Undo,
                    _ => ProgressionEdit::Redo,
                };
                edit_progression(state, edit, logger);
            } else {
                state.flash(200);
                logger.input(&format!("{:?} outside progression -> flash", hotkey));
            }
        }
    }
}

/// Cmd/Ctrl+A: select the whole progression, so a group action can apply to it.
///
/// Scoped to the Progression panel for the same reason every other progression
/// action is: the selection lives there, and selecting chords in a panel that is
/// not on screen would be a keystroke with nothing to see.
fn select_all_chords(state: &mut AppState, logger: &Logger) {
    if state.focus != Focus::Progression {
        state.flash(200);
        logger.input("SELECT ALL outside progression -> flash");
        return;
    }
    if state.select_all_chords() {
        logger.input(&format!("PROG select all {}", state.selection_len()));
    } else {
        state.flash(200);
        logger.input("PROG select all: nothing to add");
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
    let Some((start, end)) = state.selection() else {
        state.flash(200);
        logger.input("PROG replace: nothing selected");
        return;
    };

    let changed = state.progression.lock().unwrap().replace_chord_range(
        start,
        end,
        degree,
        transformation,
        registers,
    );

    if changed {
        state.transport.set_audition_slot(None);
        state.update_progression_len();
        logger.input(&format!("PROG replace at {}..{}", start, end));
    } else {
        state.flash(200);
        logger.input(&format!("PROG replace no-op at {}..{}", start, end));
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
    // Recalling is what arms the in-place audition: from here until the
    // selection moves, the computed chord stands in for this slot while the
    // loop plays, so a change is heard against the rest of the progression.
    let slot = state.progression_row;
    set_audition_slot(state, Some(slot), logger);
    logger.input(&format!("RECALL chord at {}", slot));
}

/// Cycle the log away, on to the history, on to the top list, and away again.
///
/// Global rather than scoped: the point of the key is to flip the log up while
/// both hands are busy, read it, and flip it away, so it must not care which
/// panel has focus.
fn cycle_history_view(state: &mut AppState, logger: &Logger) {
    state.history_view = state.history_view.next();
    match state.history_view {
        HistoryView::Off => {
            // Anything the log was sounding stops with it: a key you are not
            // holding any more should not keep a note alive behind an
            // invisible panel.
            state.history_held = None;
            state.history_timed = None;
        }
        HistoryView::Log => {
            // Open on the newest play, which is the one you just made.
            state.history_row = state.history.len().saturating_sub(1);
            state.history_scroll = 0;
            state.scroll_history_into_view();
        }
        HistoryView::Top => {
            state.history_row = 0;
            state.history_scroll = 0;
        }
    }
    logger.input(&format!(
        "HISTORY view = {} ({} played, {} chords)",
        state.history_view.name(),
        state.history.len(),
        state.history.chords()
    ));
}

/// Step the cursor through whichever view is up, sounding each destination.
///
/// The chord sounds for [`HISTORY_AUDITION`] and the next movement takes the
/// voice over rather than stacking on it, so walking the list is one chord at a
/// time — the same contract the audition has everywhere else.
fn move_history(state: &mut AppState, delta: i32, logger: &Logger) {
    if !state.history_view.is_on() {
        state.flash(200);
        logger.input("HISTORY move with the log hidden -> flash");
        return;
    }
    let len = state.history_len();
    if len == 0 {
        state.flash(200);
        logger.input("HISTORY move with nothing logged -> flash");
        return;
    }
    let row = (state.history_row as i32 + delta).clamp(0, len as i32 - 1) as usize;
    state.history_row = row;
    state.scroll_history_into_view();

    let key = state.transport.key();
    let some = state
        .history_selected()
        .map(|play| (play.chord.label(&key), play.notes(&key)));
    if let Some((label, notes)) = some {
        state.history_timed = Some((notes, Instant::now() + HISTORY_AUDITION));
        logger.input(&format!(
            "HISTORY {} -> {}",
            state.history_view.name(),
            label
        ));
    }
}

/// Put the selected chord back in the registers.
///
/// The same gesture as recalling a progression row, on a chord that may not be in
/// the progression at all — which is how a chord you played by accident becomes
/// one you can commit with `Enter`.
fn history_recall(state: &mut AppState, logger: &Logger) {
    let Some(play) = state.history_selected().cloned() else {
        state.flash(200);
        logger.input("HISTORY recall with nothing selected -> flash");
        return;
    };
    state.registers = play.registers;
    state.update_live_chord();
    // No in-place audition is armed: that belongs to a progression slot, and a
    // history row has none. The ordinary audition sounds it, because the log is
    // browsed with the transport stopped.
    let label = play.chord.label(&state.transport.key());
    logger.input(&format!("HISTORY recall {}", label));
}

/// Sound the selected chord while `r` is held, and let it fade when it is let go.
///
/// The fade is the audition's own release, which is already half a second: a
/// history row held and released sounds exactly like a chord held and released on
/// the keyboard, because it is the same voice and the same release.
fn history_hold(state: &mut AppState, held: bool, logger: &Logger) {
    if !held {
        if state.history_held.take().is_some() {
            logger.input("HISTORY release");
        }
        return;
    }
    if !state.history_view.is_on() {
        state.flash(200);
        logger.input("HISTORY play with the log hidden -> flash");
        return;
    }
    let key = state.transport.key();
    let some = state
        .history_selected()
        .map(|play| (play.chord.label(&key), play.notes(&key)));
    let Some((label, notes)) = some else {
        state.flash(200);
        logger.input("HISTORY play with nothing selected -> flash");
        return;
    };
    state.history_held = Some(notes);
    logger.input(&format!("HISTORY play {}", label));
}

/// Apply a progression edit to the selection. Only called with the progression
/// panel focused, so the cursor and the selection are both live.
///
/// Every action here takes the whole selection: the single-row case is just a
/// range of one, so there is no second code path to keep in step with this one.
fn edit_progression(state: &mut AppState, edit: ProgressionEdit, logger: &Logger) {
    // Replace needs the registers, so it is the one edit that does not touch the
    // slot list itself.
    if edit == ProgressionEdit::Replace {
        replace_selected_chord(state, logger);
        return;
    }

    let before_first = state.progression_before_first;
    let selection = state.selection();

    // A paste is the one edit that works with nothing selected, because the gap
    // above the first chord is exactly "insert at position 1" — and that gap is
    // a place to paste *into*, not a row to copy, delete or reorder.
    if selection.is_none() && edit != ProgressionEdit::Paste {
        state.flash(200);
        logger.input(&format!("PROG {:?} refused: nothing selected", edit));
        return;
    }

    let (start, end) = selection.unwrap_or((0, 0));
    // Where a paste landed, so the cursor can follow it and the pasted phrase is
    // left selected — that is what makes paste-paste-paste chain.
    let mut pasted: Option<(usize, usize)> = None;

    let changed = {
        let mut prog = state.progression.lock().unwrap();
        match edit {
            ProgressionEdit::Copy => prog.copy_range(start, end),
            ProgressionEdit::Paste => {
                // After the block, into the gap for position 1, or at the end
                // when the cursor is somewhere the list no longer reaches.
                let at = match selection {
                    Some((_, end)) => end + 1,
                    None if before_first => 0,
                    None => prog.len(),
                };
                let count = prog.paste_at(at);
                if count > 0 {
                    pasted = Some((at, at + count - 1));
                    true
                } else {
                    false
                }
            }
            ProgressionEdit::Delete => prog.delete_range(start, end),
            ProgressionEdit::Reverse => prog.reverse_range(start, end),
            ProgressionEdit::Rotate => prog.rotate_range(start, end, 1),
            ProgressionEdit::ClearRhythms => prog.strip_rhythms(start, end),
            ProgressionEdit::Undo => prog.undo(),
            ProgressionEdit::Redo => prog.redo(),
            ProgressionEdit::Replace => unreachable!(),
        }
    };

    if !changed {
        state.flash(200);
        logger.input(&format!("PROG {:?} no-op at {}..{}", edit, start, end));
        return;
    }

    match edit {
        // The pasted run is now the selection, so the next action lands on it.
        ProgressionEdit::Paste => {
            if let Some((first, last)) = pasted {
                state.progression_before_first = false;
                state.progression_row = last;
                state.progression_anchor = if last > first { Some(first) } else { None };
            }
        }
        // A structural edit that is not a paste leaves the cursor where the list
        // can still hold it, with the range collapsed: the chords that were
        // selected are not all there any more.
        ProgressionEdit::Delete | ProgressionEdit::Undo | ProgressionEdit::Redo => {
            state.progression_anchor = None;
            state.progression_before_first = false;
            state.clamp_progression_row();
        }
        // Copy, reverse, rotate and clear all leave the list the same length, so
        // the selection survives them: rotating twice is how you get the other
        // direction, and clearing then re-copying is the same chords.
        _ => {}
    }

    // Any structural edit can move the slot an audition is armed for, so the
    // audition goes with it rather than pointing at whatever landed there.
    state.transport.set_audition_slot(None);
    state.update_progression_len();
    logger.input(&format!(
        "PROG {:?} at {}..{} ({} selected)",
        edit,
        start,
        end,
        end - start + 1
    ));
}

/// Run the menu item under the cursor.
fn run_progression_menu(state: &mut AppState, logger: &Logger) {
    let row = state.progression_menu_row.min(PROGRESSION_MENU.len() - 1);
    let (label, item) = PROGRESSION_MENU[row];
    logger.input(&format!("PROG menu -> {}", label));
    run_progression_menu_item(state, item, logger);
}

/// Run one menu item. Split out from the cursor so a test can name the action
/// rather than having to aim the cursor at it first.
fn run_progression_menu_item(state: &mut AppState, item: ProgressionMenu, logger: &Logger) {
    edit_progression(state, item.edit(), logger);
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
        return format!("(none){}", pattern_position_label(state, None));
    };
    let pristine = state
        .rhythm_store
        .lock()
        .unwrap()
        .find(&pattern.name)
        .cloned();
    let name = match pristine {
        Some(template) if template != pattern => format!("{}  (edited)", pattern.name),
        _ => pattern.name.clone(),
    };
    format!(
        "{}{}",
        name,
        pattern_position_label(state, Some(&pattern.name))
    )
}

/// Where the selected pattern sits in the palette, as `[8/23]`.
///
/// The `pattern` row cycles one press at a time through two dozen entries, so the
/// position is the only thing that says how far there is left to go, or whether a
/// `Shift` jump overshot. `(none)` is position zero: the palette starts after it.
fn pattern_position_label(state: &AppState, assigned: Option<&str>) -> String {
    let store = state.rhythm_store.lock().unwrap();
    let total = store.patterns.len();
    let index = match assigned {
        None => 0,
        Some(name) => store
            .patterns
            .iter()
            .position(|p| p.name == name)
            .map(|i| i + 1)
            .unwrap_or(0),
    };
    format!("   [{}/{}]", index, total)
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

/// Step the Progression selection from the Sinko panel's `chord` row.
///
/// The panel has no cursor of its own, so its `chord` row *is* the Progression
/// panel's selection: stepping it here is what re-targets every other row at a
/// different chord, exactly as the Progression panel's own up/down does. It
/// clamps rather than wraps, because that is what the list it is steering does.
///
/// The draft is reloaded in the same keypress. The event loop syncs the working
/// pattern before it handles a key, so without this the grid would still be
/// showing the chord just left until the *next* press — a row that looks one
/// selection behind.
fn move_sinko_chord(state: &mut AppState, delta: i32, logger: &Logger) {
    let len = state.progression.lock().unwrap().len();
    if len == 0 {
        state.flash(200);
        logger.input("SINKO chord: the progression is empty");
        return;
    }
    let next = (state.progression_row as i32 + delta).clamp(0, len as i32 - 1) as usize;
    if next == state.progression_row {
        return;
    }
    state.progression_row = next;
    sync_working(state);
    logger.input(&format!("SINKO chord -> progression row {}", next));
}

/// Every rung the `swing` row can land on: follow the transport, then straight
/// through to the triplet feel in tenths.
///
/// `None` first, so "follow" is reachable by walking the row left rather than
/// being a state you can only leave — and so a pattern with an override of its
/// own reads as deliberately different from the transport's.
const SWING_RUNGS: [Option<f32>; 12] = [
    None,
    Some(0.0),
    Some(0.1),
    Some(0.2),
    Some(0.3),
    Some(0.4),
    Some(0.5),
    Some(0.6),
    Some(0.7),
    Some(0.8),
    Some(0.9),
    Some(1.0),
];

/// Which swing rung a pattern is on, snapping an off-ladder value (a hand-edited
/// file) to the nearest.
fn swing_rung_index(swing: Option<f32>) -> i32 {
    match swing {
        None => 0,
        Some(v) => {
            let tenths = (v.clamp(0.0, 1.0) * 10.0).round() as i32;
            1 + tenths.clamp(0, 10)
        }
    }
}

/// The swing a pattern would take at a rung.
fn swing_at_rung(index: i32) -> Option<f32> {
    let index = index.clamp(0, SWING_RUNGS.len() as i32 - 1) as usize;
    SWING_RUNGS[index]
}

/// How the `swing` row reads, naming the transport's value when it follows it.
fn format_swing(pattern: Option<f32>, transport: f32) -> String {
    let percent = |v: f32| format!("{:.0}%", v.clamp(0.0, 1.0) * 100.0);
    match pattern {
        None => format!(
            "follow transport  ({}, {})",
            percent(transport),
            swing_feel(transport)
        ),
        Some(v) => format!("{}  ({})", percent(v), swing_feel(v)),
    }
}

/// The one-word feel for a swing amount, so a number is not the only cue.
fn swing_feel(swing: f32) -> &'static str {
    match swing {
        s if s <= 0.02 => "straight",
        s if s >= 0.98 => "triplet",
        s if s < 0.5 => "light",
        _ => "heavy",
    }
}

/// Step the pattern's own swing, `None` (follow the transport) included.
fn cycle_swing(state: &mut AppState, delta: i32, logger: &Logger) {
    if !working_is_assigned(state, logger, "swing") {
        return;
    }
    let current = swing_rung_index(state.working.swing);
    let next = (current + delta).clamp(0, SWING_RUNGS.len() as i32 - 1);
    if next == current {
        return;
    }
    state.working.swing = swing_at_rung(next);
    commit_working(state, logger);
    logger.input(&format!(
        "SINKO swing -> {:?}",
        state.working.swing.map(|v| (v * 100.0).round())
    ));
}

/// The rungs the `length` row walks: the pattern default, then the note ladder.
///
/// `0` is not a note length — it is "no override", which is why it is its own
/// rung rather than the bottom of the ladder.
fn length_rungs() -> Vec<i32> {
    let mut rungs = vec![0i32];
    rungs.extend(rhythm::NOTE_LADDER.iter().map(|t| *t as i32));
    rungs
}

/// How long the hit under the cursor holds, as the `length` row shows it.
fn cell_length_label(state: &AppState) -> String {
    let cell = cursor_cell(state);
    if !cell_is_on(state, cell) {
        return format!("cell {} has no hit", cell + 1);
    }
    if state.working.has_hold_override(cell) {
        format_ticks(state.working.cell_hold(cell) as u64)
    } else {
        format!("default  ({})", format_ticks(state.working.hold as u64))
    }
}

/// Step the hit under the cursor along the note ladder, or back to the default.
fn cycle_cell_length(state: &mut AppState, delta: i32, logger: &Logger) {
    if !working_is_assigned(state, logger, "length") {
        return;
    }
    let cell = cursor_cell(state);
    if !cell_is_on(state, cell) {
        state.flash(200);
        logger.input("SINKO length: no hit under the cursor");
        return;
    }
    let rungs = length_rungs();
    let current = if state.working.has_hold_override(cell) {
        state.working.cell_hold(cell) as i32
    } else {
        0
    };
    let next = rhythm::rung_step(&rungs, current, delta);
    state.working.set_cell_hold(cell, next as u32);
    commit_working(state, logger);
    logger.input(&format!("SINKO cell {} length -> {}", cell + 1, next));
}

/// The accent rungs, as percentages — coarse on purpose, because an accent is a
/// gesture and 5% is not a gesture.
const ACCENT_RUNGS: [i32; 6] = [20, 40, 60, 80, 90, 100];

/// How hard the hit under the cursor plays, as the `accent` row shows it.
fn cell_accent_label(state: &AppState) -> String {
    let cell = cursor_cell(state);
    if !cell_is_on(state, cell) {
        return format!("cell {} has no hit", cell + 1);
    }
    let percent = (state.working.cell_velocity(cell) * 100.0).round() as i32;
    if state.working.has_velocity_override(cell) {
        format!("{}%", percent)
    } else {
        "100%  (full)".to_string()
    }
}

/// Step the hit under the cursor's accent.
fn cycle_cell_accent(state: &mut AppState, delta: i32, logger: &Logger) {
    if !working_is_assigned(state, logger, "accent") {
        return;
    }
    let cell = cursor_cell(state);
    if !cell_is_on(state, cell) {
        state.flash(200);
        logger.input("SINKO accent: no hit under the cursor");
        return;
    }
    let current = (state.working.cell_velocity(cell) * 100.0).round() as i32;
    let next = rhythm::rung_step(&ACCENT_RUNGS, current, delta);
    state.working.set_cell_velocity(cell, next as f32 / 100.0);
    commit_working(state, logger);
    logger.input(&format!("SINKO cell {} accent -> {}%", cell + 1, next));
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
    state.modal.is_none()
        && matches!(state.edit, Edit::None)
        // The metronome panel and the `[MIDI]` chooser are both things Esc
        // *closes*: they cover or replace a row rather than being a prompt, so
        // they were the two cases where a free Esc used to stop the transport
        // out from under an open control.
        && !state.metronome_open
        && !state.midi_open
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

/// Open the metronome panel over the Transport slot.
fn open_metronome_panel(state: &mut AppState, logger: &Logger) {
    state.metronome_open = true;
    state.metronome_row = METRONOME_ROW_CLICK;
    logger.input("METRONOME panel opened");
}

/// Handle a key while the metronome panel is open.
///
/// Always consumes: the panel covers the transport, so an arrow that reached the
/// panel underneath would be editing something the player cannot see. `Esc` is
/// checked before the stop/quit gesture for the same reason it is in an edit —
/// here it means "close this", not "stop the transport".
fn handle_metronome_key(state: &mut AppState, ev: &event::KeyEvent, logger: &Logger) {
    if ev.kind != KeyEventKind::Press {
        return;
    }
    let transport = state.transport.clone();
    match ev.code {
        KeyCode::Esc => {
            state.metronome_open = false;
            logger.input("METRONOME panel closed");
        }
        KeyCode::Up => state.metronome_row = state.metronome_row.saturating_sub(1),
        KeyCode::Down => {
            state.metronome_row = (state.metronome_row + 1).min(METRONOME_ROWS - 1);
        }
        // `Enter` is the click switch, because that is the one row here that is a
        // toggle rather than a value — and because the row that opened this panel
        // is the one it is standing in for.
        KeyCode::Enter => {
            if state.metronome_row == METRONOME_ROW_CLICK {
                toggle_metronome(state, logger);
            }
        }
        KeyCode::Left | KeyCode::Right => {
            let delta = if matches!(ev.code, KeyCode::Left) {
                -1
            } else {
                1
            };
            match state.metronome_row {
                METRONOME_ROW_CLICK => toggle_metronome(state, logger),
                METRONOME_ROW_SOUND => {
                    let count = crate::synth::CLICK_SOUNDS.len() as i32;
                    let next = (transport.metronome_sound.load(Ordering::Relaxed) as i32 + delta)
                        .rem_euclid(count);
                    transport
                        .metronome_sound
                        .store(next as usize, Ordering::Relaxed);
                    logger.input(&format!(
                        "METRONOME sound -> {}",
                        crate::synth::CLICK_SOUNDS[next as usize].name
                    ));
                }
                METRONOME_ROW_VOLUME => {
                    let next = transport.metronome_volume() + delta as f32 * 0.05;
                    transport.set_metronome_volume(next);
                    logger.input(&format!(
                        "METRONOME volume -> {:.0}%",
                        transport.metronome_volume() * 100.0
                    ));
                }
                METRONOME_ROW_SUBDIVISION => {
                    let current = transport.metronome_subdivision();
                    let index = METRONOME_SUBDIVISIONS
                        .iter()
                        .position(|s| *s == current)
                        .unwrap_or(0) as i32;
                    let count = METRONOME_SUBDIVISIONS.len() as i32;
                    let next = METRONOME_SUBDIVISIONS[((index + delta).rem_euclid(count)) as usize];
                    transport.set_metronome_subdivision(next);
                    logger.input(&format!("METRONOME subdivision -> {} per beat", next));
                }
                METRONOME_ROW_SWING => {
                    let next = (transport.swing() * 100.0).round() as i32 + delta * 5;
                    transport.set_swing(next.clamp(0, 100) as f32 / 100.0);
                    logger.input(&format!(
                        "METRONOME swing -> {:.0}%",
                        transport.swing() * 100.0
                    ));
                }
                _ => {}
            }
        }
        _ => {}
    }
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
    let swing = state.working.swing;
    let name = state.working.name.clone();
    match RhythmPattern::blank(name, next) {
        Ok(mut pattern) => {
            // The hold is ticks, so it survives a grid change — the whole point
            // of measuring it in ticks rather than in cells.
            pattern.hold = hold;
            pattern.mute_ticks = mute;
            pattern.swing = swing;
            // Per-cell lengths and accents are indexed by cell, so a new grid
            // invalidates them: they are dropped with the draft rather than
            // silently re-pointed at different beats.
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
        store.save(&state.rhythm_user_path)
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

/// Copy the selection's rhythms, so they can be pasted onto others.
///
/// The rhythm only — the grid, the hits, the hold, the accents and the muted
/// tail. The offset stays with each chord, because where a chord sits in the bar
/// is placement rather than rhythm, and copying it would move chords nobody asked
/// to move. A chord with no rhythm copies as "no rhythm", so pasting a phrase
/// reproduces which of its chords were plain as well as which were not.
fn copy_sinko(state: &mut AppState, logger: &Logger) {
    let rows = sinko_rows(state);
    if rows.is_empty() {
        state.rhythm_status = Some(ActionStatus::refused("select a chord first".to_string()));
        state.flash(300);
        logger.input("SINKO copy refused: no chord selected");
        return;
    }

    let patterns: Vec<Option<RhythmPattern>> = {
        let prog = state.progression.lock().unwrap();
        rows.iter()
            .map(|row| match prog.slots.get(*row) {
                Some(Slot::Chord(entry)) => entry.pattern.clone(),
                _ => None,
            })
            .collect()
    };

    if patterns.iter().all(|p| p.is_none()) {
        state.rhythm_status = Some(ActionStatus::refused("nothing to copy".to_string()));
        state.flash(300);
        logger.input("SINKO copy refused: none of these chords has a pattern");
        return;
    }

    let named = patterns.iter().filter_map(|p| p.as_ref()).count();
    state.sinko_clipboard = Some(patterns);
    state.rhythm_status = Some(ActionStatus::ok(format!(
        "copied {} rhythm{}",
        named,
        if named == 1 { "" } else { "s" }
    )));
    state.flash(400);
    logger.input(&format!("SINKO copied {} rhythm(s)", named));
}

/// Give the selection a copy of the clipboard's rhythms.
///
/// The clipboard is laid across the selection in order and repeats if it is
/// shorter — which is what makes a single copied rhythm land on *every* selected
/// chord, while a phrase of four lands one per chord. One undoable edit, and safe
/// to repeat: each paste is an independent copy, so editing a chord afterwards
/// cannot reach any of the others.
fn paste_sinko(state: &mut AppState, logger: &Logger) {
    let Some(patterns) = state.sinko_clipboard.clone() else {
        state.rhythm_status = Some(ActionStatus::refused("nothing copied yet".to_string()));
        state.flash(300);
        logger.input("SINKO paste refused: the clipboard is empty");
        return;
    };
    let rows = sinko_rows(state);
    if rows.is_empty() {
        state.rhythm_status = Some(ActionStatus::refused("select a chord first".to_string()));
        state.flash(300);
        logger.input("SINKO paste refused: no chord selected");
        return;
    }

    let (start, end) = (rows[0], rows[rows.len() - 1]);
    let changed = state
        .progression
        .lock()
        .unwrap()
        .assign_patterns(start, end, &patterns);

    // Show the chord the Sinko panel is describing straight away rather than
    // waiting for the next key.
    state.working = match patterns.first().cloned().flatten() {
        Some(pattern) => pattern,
        None => RhythmPattern::draft(),
    };
    state.working_slot = sinko_target(state);
    state.sinko_cell = 0;
    state.takes.clear();
    state.current_take.clear();
    state.take_started_at = None;

    if !changed {
        state.flash(200);
        return;
    }

    state.rhythm_status = Some(ActionStatus::ok(format!(
        "pasted {} rhythm{} onto {}",
        patterns.len(),
        if patterns.len() == 1 { "" } else { "s" },
        rows.len()
    )));
    state.flash(600);
    logger.input(&format!(
        "SINKO pasted {} rhythm(s) onto rows {}..{}",
        patterns.len(),
        start,
        end
    ));
}

/// What the audition should do on this pass.
///
/// The decision is split from the audio call so it can be tested: a `Synth` owns
/// an audio device no test can build, and the decision is where the behaviour
/// lives.
#[derive(Debug, PartialEq)]
enum AuditionStep {
    /// Nothing to do.
    Idle,
    /// Speak this chord on the audition voice.
    Play(Vec<u8>),
    /// Let the audition voice go.
    Release,
}

/// Sound the computed chord as it changes, for as long as the transport is
/// stopped.
///
/// This is the audition, and it is deliberately not bar-quantised: a change
/// speaks the moment it happens, so the keyboard plays the instrument rather
/// than waiting for a bar line at whatever tempo is set. When the chord goes away
/// the note is left to ring for [`AUDITION_RELEASE`]; a new chord arriving in
/// that window takes the note over instead of stacking on top of it.
///
/// While the transport *is* playing there is nothing to speak here: a change is
/// heard where the loop reaches the auditioned slot, which is the whole point of
/// auditioning in place.
fn pump_audition(state: &mut AppState, synth: &Synth, logger: &Logger) {
    match audition_step(state, Instant::now()) {
        AuditionStep::Idle => {}
        AuditionStep::Play(notes) => {
            // Auditioning is a hand played chord, so it is hit at full velocity:
            // there is no accent to hear and nothing to be darker than.
            synth.play_stab(crate::synth::AUDITION_GROUP, &notes, 1.0, 1.0);
            logger.input(&format!("AUDITION {} note(s)", notes.len()));
        }
        AuditionStep::Release => synth.stop_stab(crate::synth::AUDITION_GROUP),
    }
}

/// Decide the audition's next move.
fn audition_step(state: &mut AppState, now: Instant) -> AuditionStep {
    // A movement's 200 ms runs out here, and it stops dead rather than ringing
    // on: the tail the audition leaves a chord that was *released* would smear
    // one row of the log into the next. The next frame plays whatever else wants
    // the voice, so nothing is lost by letting go of it.
    if matches!(&state.history_timed, Some((_, until)) if now >= *until) {
        state.history_timed = None;
        let was_sounding = state.audition_sounding.take().is_some();
        state.audition_release_at = None;
        return if was_sounding {
            AuditionStep::Release
        } else {
            AuditionStep::Idle
        };
    }

    // The history's own requests outrank the live chord. They are deliberate —
    // a key is down, or a movement just happened — and while the log is being
    // read the hands are usually off the keyboard entirely.
    let history_wants = state
        .history_held
        .clone()
        .or_else(|| state.history_timed.as_ref().map(|(notes, _)| notes.clone()));

    if state.transport.playing.load(Ordering::Relaxed) && history_wants.is_none() {
        // Hand the voice back rather than leaving a tail over the loop.
        let was_ringing =
            state.audition_sounding.take().is_some() || state.audition_release_at.take().is_some();
        return if was_ringing {
            AuditionStep::Release
        } else {
            AuditionStep::Idle
        };
    }

    let computed = match history_wants {
        Some(notes) => Some(notes),
        None => state.transport.live_chord.lock().unwrap().clone(),
    };
    if computed != state.audition_sounding {
        match computed {
            Some(notes) if !notes.is_empty() => {
                state.audition_sounding = Some(notes.clone());
                // The release the old chord was owed is cancelled, not stacked:
                // the new chord takes the note over.
                state.audition_release_at = None;
                return AuditionStep::Play(notes);
            }
            _ => {
                // Nothing is being played any more: keep the last chord ringing
                // and let the deadline below end it, unless a new chord arrives
                // first.
                if state.audition_sounding.is_some() {
                    state.audition_release_at = Some(now + AUDITION_RELEASE);
                }
                state.audition_sounding = None;
            }
        }
    }

    if let Some(at) = state.audition_release_at {
        if now >= at {
            state.audition_release_at = None;
            return AuditionStep::Release;
        }
    }
    AuditionStep::Idle
}

/// Arm the in-place audition for the recalled slot, or drop it.
///
/// Armed by a recall and dropped by anything that moves the selection, which is
/// what makes "changing the selected chord exits the audition" true without a
/// mode to enter or leave.
fn set_audition_slot(state: &AppState, slot: Option<usize>, logger: &Logger) {
    state.transport.set_audition_slot(slot);
    match slot {
        Some(slot) => logger.input(&format!("AUDITION armed for slot {}", slot)),
        None => logger.input("AUDITION disarmed"),
    }
}

/// The rows the Sinko panel's actions apply to.
///
/// A Progression selection outranks the cursor, in either panel: the selection is
/// state rather than a highlight, so tabbing to Sinko to press `[Paste Sinko]`
/// does not quietly narrow "all of these" down to one chord. With nothing
/// selected it is the single chord the panel is describing.
fn sinko_rows(state: &AppState) -> Vec<usize> {
    if let Some((start, end)) = state.selection() {
        return (start..=end).collect();
    }
    sinko_target(state).into_iter().collect()
}

/// How the sinko clipboard reads on the `[Paste Sinko]` row.
///
/// One entry names itself; several say how many and whether any of them is a
/// chord with no rhythm, so a phrase paste is legible before it happens.
fn clipboard_label(patterns: &[Option<RhythmPattern>]) -> String {
    match patterns {
        [] => "nothing".to_string(),
        [Some(pattern)] => pattern.name.clone(),
        [None] => "(no rhythm)".to_string(),
        many => {
            let named = many.iter().filter(|p| p.is_some()).count();
            format!("{} rhythms, {} plain", many.len(), many.len() - named)
        }
    }
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
    // The chord list on the left, the transport on the right: the list is what
    // you read and edit, the transport is a row of settings at the far edge, and
    // the panels below take the full width. Each block has its header centred
    // over its own body on the way out.
    draw_columns(
        stdout,
        &centre_block_header(&panel_block(|out| render_progression_panel(out, state))),
        &centre_block_header(&panel_block(|out| {
            if state.metronome_open {
                render_metronome_panel(out, state)
            } else {
                render_transport_panel(out, params, state)
            }
        })),
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
        render_modal(stdout, modal, screen.width, &state.instrument_store)?;
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
    let ensembles_focused = state.focus == Focus::SynthEnsembles;
    let eq_focused = state.focus == Focus::Eq;
    let spectrum_focused = state.focus == Focus::Spectrum;
    let fx_focused = state.focus == Focus::Fx;

    // The page is in the title, because a paged table whose page you cannot see
    // is a table with settings you will never find.
    let page_no = SynthPage::ALL
        .iter()
        .position(|&p| p == state.synth_page)
        .unwrap_or(0)
        + 1;
    let header = if focused && matches!(state.edit, Edit::SynthCell { .. }) {
        format!(
            " Synth [{} {}/{} · editing] ",
            state.synth_page.name(),
            page_no,
            SynthPage::ALL.len()
        )
    } else if focused {
        format!(
            " Synth [{} {}/{}] ",
            state.synth_page.name(),
            page_no,
            SynthPage::ALL.len()
        )
    } else if ensembles_focused {
        " Synth [Ensembles] ".to_string()
    } else if eq_focused {
        format!(" EQ [{}] ", EqTarget::from_index(state.eq_target).name())
    } else if spectrum_focused {
        format!(
            " Spectrum [{}] ",
            EqTarget::from_index(state.eq_target).name()
        )
    } else if fx_focused {
        // The whole effect in the title: the panel shows the kind and the variant
        // on their own rows, and the one thing a rack wants at a glance is the
        // name of what is loaded.
        format!(
            " FX [{} {}/{}  {}] ",
            fx_rack_name(state),
            state.fx_slot + 1,
            CHAIN_SLOTS,
            fx_slot_at(state, params).fx().label()
        )
    } else {
        " Synth ".to_string()
    };
    let header = header.as_str();
    draw_panel_header(
        stdout,
        header,
        focused || ensembles_focused || eq_focused || spectrum_focused || fx_focused,
    )?;

    if focused {
        render_synth_table(stdout, params, state)?;
    } else if ensembles_focused {
        render_ensembles_body(stdout, state)?;
    } else if eq_focused {
        render_eq_body(stdout, params, state)?;
    } else if spectrum_focused {
        render_spectrum_body(stdout, params, state)?;
    } else if fx_focused {
        render_fx_body(stdout, params, state)?;
    } else {
        render_synth_summary(stdout, params, state)?;
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// The FX panel
// -----------------------------------------------------------------------------

/// The editable rows of the FX panel, in display order.
///
/// A fixed count whatever the slot holds, which is why the six parameters are
/// reached through a `param` row and a `value` row rather than as six rows of
/// their own: the number of parameters belongs to the kind, and a row list that
/// grew and shrank as the kind changed would move every row below it out from
/// under the cursor. It is the same pair the EQ panel uses for its thirteen
/// bands — one row to choose, one row to move — and for the same reason.
const FX_ROWS: usize = 11;
const FX_ROW_RACK: usize = 0;
const FX_ROW_SLOT: usize = 1;
const FX_ROW_TYPE: usize = 2;
const FX_ROW_SUBTYPE: usize = 3;
const FX_ROW_PRESET: usize = 4;
const FX_ROW_PARAM: usize = 5;
const FX_ROW_VALUE: usize = 6;
const FX_ROW_EARLIER: usize = 7;
const FX_ROW_LATER: usize = 8;
const FX_ROW_CLEAR: usize = 9;
const FX_ROW_SAVE: usize = 10;

const FX_LABEL_WIDTH: usize = 14;
/// A row of the panel that runs a command rather than naming a value.
fn fx_row_is_button(row: usize) -> bool {
    matches!(
        row,
        FX_ROW_EARLIER | FX_ROW_LATER | FX_ROW_CLEAR | FX_ROW_SAVE
    )
}

/// The insert slot the FX panel is on.
///
/// The index comes from a cursor, so it is clamped to the rack that exists: a
/// stale index must not be able to panic a draw.
fn fx_slot_at<'a>(state: &AppState, params: &'a SynthParams) -> &'a FxSlotParams {
    slot_of(channel_at(params, state.fx_col), state.fx_slot)
}

/// Which of the slot's parameters the `value` row is on, clamped to the kind.
fn fx_param_of(state: &AppState, fx: &Fx) -> usize {
    state.fx_param.min(fx.kind.params().len().saturating_sub(1))
}

/// The rack's name, for the title and the log.
fn fx_rack_name(state: &AppState) -> &'static str {
    CHANNEL_COLUMN_LABELS[state.fx_col.min(CHANNEL_COUNT - 1)]
}

/// Where the panel's cursor is, as `low slot 3`.
fn fx_where(state: &AppState) -> String {
    format!("{} slot {}", fx_rack_name(state), state.fx_slot + 1)
}

fn adjust_fx(
    state: &mut AppState,
    params: &SynthParams,
    delta: i32,
    coarse: bool,
    logger: &Logger,
) {
    let slot = fx_slot_at(state, params);
    let fx = slot.fx();
    match state.fx_row {
        FX_ROW_RACK => {
            let next = (state.fx_col as i32 + delta).rem_euclid(CHANNEL_COUNT as i32);
            state.fx_col = next as usize;
            logger.input(&format!("FX rack = {}", fx_rack_name(state)));
        }
        FX_ROW_SLOT => {
            let next = (state.fx_slot as i32 + delta).rem_euclid(CHAIN_SLOTS as i32);
            state.fx_slot = next as usize;
            // The parameter cursor belongs to the slot that was on screen, so it
            // starts over rather than pointing into a slot it never saw.
            state.fx_param = 0;
            logger.input(&format!("FX slot = {}", state.fx_slot + 1));
        }
        // A whole family in one press, defaults and all: the point of auditioning
        // kinds is to hear them, and a kind that arrived with the last kind's
        // numbers would be a rename.
        FX_ROW_TYPE => {
            slot.set_kind(FxKind::from_index(fx.kind.index() + delta));
            state.fx_param = 0;
            logger.input(&format!(
                "FX {} = {}",
                fx_where(state),
                slot.fx().kind.name()
            ));
        }
        FX_ROW_SUBTYPE => {
            let len = fx.kind.subtypes().len() as i32;
            let next = (slot.subtype_index() as i32 + delta).rem_euclid(len);
            slot.set_subtype_index(next as usize);
            logger.input(&format!(
                "FX {} = {}",
                fx_where(state),
                slot.fx().subtype.name()
            ));
        }
        // The library for the kind and variant on screen. `Shift` is unused
        // here: a preset is one thing, not a ladder.
        FX_ROW_PRESET => {
            if let Some(preset) = state.fx_presets.step(&fx, delta) {
                slot.set(&preset.fx);
                logger.input(&format!("FX {} preset = {}", fx_where(state), preset.name));
            } else {
                logger.input(&format!("FX no presets for {}", fx.kind.name()));
                state.flash(300);
            }
        }
        FX_ROW_PARAM => {
            let len = fx.kind.params().len().max(1) as i32;
            state.fx_param = (state.fx_param as i32 + delta).rem_euclid(len) as usize;
        }
        FX_ROW_VALUE => {
            let index = fx_param_of(state, &fx);
            if let Some(spec) = fx.kind.params().get(index) {
                slot.set_param(index, fx.stepped(index, delta, coarse));
                // Read back through the slot rather than from the value just
                // computed: a delay's `time` is written down together with the
                // note value it lands on when `sync` is on, and only the pair
                // knows the second.
                let now = slot.fx().display(index, state.transport.bpm() as f32);
                logger.input(&format!("FX {} {} = {}", fx_where(state), spec.label, now));
            }
        }
        _ => {}
    }
}

/// Slide the slot's effect one place along the rack.
///
/// The cursor follows the *effect* rather than the position: the reason to move a
/// distortion is to put it in front of the chorus, and afterwards it is the same
/// distortion you are still editing.
fn move_fx_slot(state: &mut AppState, params: &SynthParams, delta: i32, logger: &Logger) {
    let to = state.fx_slot as i32 + delta;
    if to < 0 || to >= CHAIN_SLOTS as i32 {
        logger.input(&format!(
            "FX {} is already at the end of the rack",
            fx_where(state)
        ));
        state.flash(300);
        return;
    }
    let to = to as usize;
    let from = state.fx_slot;
    let chain = &channel_at(params, state.fx_col).chain;
    // Written as whole effects, because a slot is a set of atomics with no value
    // to borrow and nothing that can be left half-swapped.
    let moving = chain.slots[from].fx();
    let displaced = chain.slots[to].fx();
    chain.slots[to].set(&moving);
    chain.slots[from].set(&displaced);
    state.fx_slot = to;
    logger.input(&format!(
        "FX {} slot {} <-> {}",
        fx_rack_name(state),
        from + 1,
        to + 1
    ));
}

/// `Enter` on an FX row. Everything else on this panel is an arrow press.
fn fx_action(state: &mut AppState, params: &SynthParams, row: usize, logger: &Logger) {
    let slot = fx_slot_at(state, params);
    match row {
        FX_ROW_EARLIER => move_fx_slot(state, params, -1, logger),
        FX_ROW_LATER => move_fx_slot(state, params, 1, logger),
        FX_ROW_CLEAR => {
            if slot.fx().is_none() {
                logger.input(&format!("FX {} is already empty", fx_where(state)));
                state.flash(300);
                return;
            }
            slot.set_kind(FxKind::None);
            logger.input(&format!("FX {} emptied", fx_where(state)));
        }
        FX_ROW_SAVE => {
            if slot.fx().is_none() {
                logger.input("FX nothing to save: the slot is empty");
                state.flash(300);
                return;
            }
            state.modal = Some(Modal::FxPresetNameInput {
                buffer: String::new(),
            });
            logger.input("FX preset name prompt");
        }
        _ => {}
    }
}

/// Keep the effect under the cursor as a named preset.
///
/// The slot is taken as it stands, variant and all, because that is what is on
/// screen and therefore what "save this" means.
fn save_fx_preset(
    state: &mut AppState,
    params: &SynthParams,
    name: &str,
    path: &Path,
    logger: &Logger,
) {
    let fx = fx_slot_at(state, params).fx();
    if fx.is_none() {
        logger.input("FX nothing to save: the slot is empty");
        state.flash(400);
        return;
    }
    state.fx_presets.add(FxPreset {
        name: name.to_string(),
        fx,
    });
    match state.fx_presets.save(path) {
        Ok(()) => logger.input(&format!(
            "FX saved preset '{}' from {}",
            name,
            fx_where(state)
        )),
        Err(e) => logger.input(&format!("SAVE ERROR: {}", e)),
    }
}

/// The rack, opened out: one slot at a time, in full.
fn render_fx_body<W: io::Write>(
    stdout: &mut W,
    params: &SynthParams,
    state: &AppState,
) -> io::Result<()> {
    let focused = state.focus == Focus::Fx;
    let slot = fx_slot_at(state, params);
    let fx = slot.fx();
    let empty = fx.is_none();
    let index = fx_param_of(state, &fx);
    let spec = fx.kind.params().get(index);
    let row = |r: usize| focused && state.fx_row == r;
    let marker = |r: usize| if row(r) { "▸" } else { " " };
    // One label column and one value column for every row, so nothing can wrap
    // the panel and push the rows below it down.
    let field = |r: usize, label: &str, value: &str| {
        format!(
            "  {} {:<FX_LABEL_WIDTH$}{}",
            marker(r),
            label,
            clip_to(value, FX_VALUE_WIDTH)
        )
    };
    // An empty slot has no variant, no preset and no numbers, so those rows read
    // as a dash rather than as `none` and `custom` — which would be two words
    // that mean "nothing here" dressed up as values.
    let dash = || "—".to_string();
    let dashes = |shown: bool, text: &str| if shown { text.to_string() } else { dash() };

    let param_text = match spec {
        Some(spec) => format!(
            "{} of {}  {}",
            index + 1,
            fx.kind.params().len(),
            spec.label
        ),
        None => dash(),
    };
    let value_text = match spec {
        Some(_) => fx.display(index, state.transport.bpm() as f32),
        None => dash(),
    };
    let preset_text = dashes(!empty, state.fx_presets.name_for(&fx).unwrap_or("custom"));

    let rows: [(usize, &str, String); 7] = [
        (FX_ROW_RACK, "rack", fx_rack_name(state).to_string()),
        (
            FX_ROW_SLOT,
            "slot",
            format!("{} of {}", state.fx_slot + 1, CHAIN_SLOTS),
        ),
        (FX_ROW_TYPE, "type", fx.kind.name().to_string()),
        (FX_ROW_SUBTYPE, "subtype", dashes(!empty, fx.subtype.name())),
        (FX_ROW_PRESET, "preset", preset_text),
        (FX_ROW_PARAM, "param", param_text),
        (FX_ROW_VALUE, "value", value_text),
    ];
    for (r, label, value) in rows {
        draw_row(stdout, &field(r, label, &value), row(r))?;
    }

    for (r, text) in [
        (FX_ROW_EARLIER, "[Move Earlier]"),
        (FX_ROW_LATER, "[Move Later]"),
        (FX_ROW_CLEAR, "[Empty This Slot]"),
        (FX_ROW_SAVE, "[Save Effect As...]"),
    ] {
        draw_row(stdout, &format!("  {} {}", marker(r), text), row(r))?;
    }
    Ok(())
}

/// Wide enough for the longest variant name in the palette plus air. A value is
/// clipped rather than allowed to reflow the panel.
const FX_VALUE_WIDTH: usize = 34;

/// Widths for the Synth table.
///
/// Every field is padded to a fixed width and clipped, so no value — a long
/// waveform name, say — can reflow the grid and shove the panels below it down
/// the screen. The same reasoning as `single_line_status` on the Transport.
const SYNTH_LABEL_WIDTH: usize = 16;
/// Wide enough for the longest waveform name in the palette plus the two
/// brackets the cursor puts around a selected cell.
///
/// Every value in the table shares this width, so one long name widens the whole
/// grid — which is the point. A clipped waveform name is a setting that reads as
/// one thing and is another, and the selected cell is exactly the one the user
/// is about to change. A test derives the longest name from `Waveform::ALL` and
/// fails if this stops accommodating it.
const SYNTH_VALUE_WIDTH: usize = 14;

fn clip_to(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
}

// -----------------------------------------------------------------------------
// The EQ panel
// -----------------------------------------------------------------------------

/// The editable rows of the EQ panel, in display order.
///
/// The thirteen bands are drawn under them but are not rows: they are the curve
/// the four rows make, and the band cursor walks them from the `band` row.
const EQ_ROWS: usize = 5;
const EQ_ROW_TARGET: usize = 0;
const EQ_ROW_PRESET: usize = 1;
const EQ_ROW_BAND: usize = 2;
const EQ_ROW_GAIN: usize = 3;
/// Keep the curve on screen as a named preset. A button rather than a value,
/// like `[Save As...]` at the foot of the ensemble list.
const EQ_ROW_SAVE: usize = 4;

/// How far `Shift` jumps the preset row.
///
/// Twenty-odd curves one press at a time is a lot of presses; five is one
/// screenful of a library that is mostly a shortlist.
const EQ_PRESET_COARSE_STEP: i32 = 5;

/// The label column, wide enough for the longest of the value rows' names.
const EQ_LABEL_WIDTH: usize = 9;

/// How many decibels one cell of the curve display stands for.
///
/// One, which is the coarsest a thirteen-band shape can be read at and the finest
/// that fits alongside the frequency and the value. The exact number is on the
/// `gain` row; the bar is for seeing the shape.
const EQ_DB_PER_CELL: f32 = 1.0;

/// How many cells the display gives each half of the centre rule.
const EQ_CELLS: usize = (crate::eq::GAIN_RANGE.1 / EQ_DB_PER_CELL) as usize;

/// Where the bars start, so the scale line under the controls lines up with
/// them: two spaces, the cursor marker, a space, the label, a gap.
const EQ_BAR_INDENT: usize = 2 + 1 + 1 + 5 + 2;

/// The heading of the EQ panel, which names the register and its instrument as
/// well as the target: with the Synth table hidden, this is the only place that
/// says what the curve is being applied *to*.
fn eq_header(state: &AppState, params: &SynthParams) -> String {
    let target = EqTarget::from_index(state.eq_target);
    match target.register() {
        Some(col) => format!(
            "{} — {}",
            target.name(),
            state.instrument_label(col, params)
        ),
        None => target.name().to_string(),
    }
}

/// One band's bar: twelve cells either side of a centre rule — the ±12 dB range,
/// not the band count.
///
/// A boost grows to the right of the rule and a cut to the left, which is the
/// arrangement a hardware graphic equaliser has, so the shape of a familiar
/// curve is recognisable at a glance.
fn eq_bar(db: f32) -> String {
    let filled = ((db.abs() / EQ_DB_PER_CELL).round() as usize).min(EQ_CELLS);
    let slack = "─".repeat(EQ_CELLS - filled);
    let mut line = String::with_capacity(EQ_CELLS * 2 + 1);
    // The cut side first, then the rule, then the boost side: the blocks always
    // start at the rule and grow outwards, so the height of the bar and which
    // side of zero it is on say everything the shape needs to.
    if db < 0.0 {
        line.push_str(&slack);
        line.push_str(&"█".repeat(filled));
    } else {
        line.push_str(&"─".repeat(EQ_CELLS));
    }
    line.push('┼');
    if db > 0.0 {
        line.push_str(&"█".repeat(filled));
        line.push_str(&slack);
    } else {
        line.push_str(&"─".repeat(EQ_CELLS));
    }
    line
}

/// One band of the curve display, with the cursor marking the band the arrows
/// will move.
///
/// The frequency is right-aligned: a frequency axis that lines up on its right
/// edge is one you can read down, and `12.5k` next to `31.5` left-aligned would
/// put the decades wherever the name happens to end.
fn eq_band_line(index: usize, db: f32, cursor: bool, highlight: bool) -> (String, bool) {
    let marker = if cursor { "▸" } else { " " };
    let value = if db == 0.0 {
        "0.0".to_string()
    } else {
        format!("{:+.1}", db)
    };
    (
        format!(
            "  {} {:>5}  {}  {:>6}",
            marker,
            crate::eq::BAND_LABELS[index],
            eq_bar(db),
            value
        ),
        highlight,
    )
}

/// The scale under the controls, aligned with the bars it labels.
fn eq_scale_line() -> String {
    let mut line = " ".repeat(EQ_BAR_INDENT);
    line.push_str("-12 dB");
    line.push_str(&"─".repeat(6));
    line.push('0');
    line.push_str(&"─".repeat(6));
    line.push_str("+12 dB");
    line
}

/// The four value rows, the button, and the curve they make.
fn render_eq_body<W: io::Write>(
    stdout: &mut W,
    params: &SynthParams,
    state: &AppState,
) -> io::Result<()> {
    let focused = state.focus == Focus::Eq;
    let target = EqTarget::from_index(state.eq_target);
    let eq = params.eq_at(target);
    let curve = eq.curve();
    let band = state.eq_band.min(EQ_BANDS - 1);
    let row = |index: usize| focused && state.eq_row == index;
    let marker = |index: usize| if row(index) { "▸" } else { " " };

    let preset = state
        .presets
        .name_for_curve(&curve)
        .unwrap_or("custom")
        .to_string();
    let rows = [
        ("target", eq_header(state, params)),
        ("preset", preset),
        (
            "band",
            format!(
                "{}/{}  {}",
                band + 1,
                EQ_BANDS,
                crate::eq::BAND_LABELS[band]
            ),
        ),
        ("gain", format!("{:+.1} dB", curve.band(band))),
    ];
    for (index, (label, value)) in rows.iter().enumerate() {
        draw_row(
            stdout,
            &format!("  {} {:<EQ_LABEL_WIDTH$}{}", marker(index), label, value),
            row(index),
        )?;
    }
    draw_row(
        stdout,
        &format!("  {} [Save Curve As...]", marker(EQ_ROW_SAVE)),
        row(EQ_ROW_SAVE),
    )?;

    execute!(stdout, Print("\r\n"))?;
    execute!(stdout, Print(format!("{}\r\n", eq_scale_line())))?;
    for index in 0..EQ_BANDS {
        let (line, highlight) = eq_band_line(
            index,
            curve.band(index),
            index == band,
            focused && state.eq_row == EQ_ROW_BAND,
        );
        draw_row(stdout, &line, highlight)?;
    }
    Ok(())
}

/// What an arrow key does on each EQ row.
///
/// The band and preset rows have long ladders, so `Shift` jumps them; the target
/// only has four places to be. The gain is a half decibel, or three with
/// `Shift`, which is the same fine/coarse pair the rest of the app uses.
fn adjust_eq(
    state: &mut AppState,
    params: &SynthParams,
    delta: i32,
    coarse: bool,
    logger: &Logger,
) {
    let target = EqTarget::from_index(state.eq_target);
    let eq = params.eq_at(target);
    match state.eq_row {
        EQ_ROW_TARGET => cycle_eq_target(state, delta, logger),
        EQ_ROW_PRESET => {
            let step = if coarse { EQ_PRESET_COARSE_STEP } else { 1 };
            apply_eq_preset(state, params, delta * step, logger);
        }
        EQ_ROW_BAND => {
            let step = if coarse { 4 } else { 1 };
            let next = (state.eq_band as i32 + delta * step).rem_euclid(EQ_BANDS as i32);
            state.eq_band = next as usize;
        }
        EQ_ROW_GAIN => {
            let step = if coarse { 3.0 } else { 0.5 };
            let band = state.eq_band.min(EQ_BANDS - 1);
            let next = step_to(eq.band(band), delta, step, crate::synth::range::EQ_GAIN);
            eq.set_band(band, next);
            logger.input(&format!(
                "EQ {} {} = {:+.1} dB",
                target.name(),
                crate::eq::BAND_LABELS[band],
                next
            ));
        }
        // The save row is a button: `Enter` opens its prompt and the arrows
        // have nothing to move.
        _ => {}
    }
}

/// Point the EQ and Spectrum panels at the next register, or at the mix.
///
/// The band cursor is per target in spirit but one index in fact, so it is left
/// where it is: switching register to compare the same band on the other two is
/// the whole reason the row exists.
fn cycle_eq_target(state: &mut AppState, delta: i32, logger: &Logger) {
    let next = (state.eq_target as i32 + delta).rem_euclid(EqTarget::ALL.len() as i32);
    state.eq_target = next as usize;
    logger.input(&format!(
        "TARGET = {}",
        EqTarget::from_index(state.eq_target).name()
    ));
}

/// Step the `preset` row through the library and write the curve it lands on.
///
/// The row's value is *derived* from the thirteen gains rather than remembered, so
/// a curve that is not in the library has no place in the walk to step from.
/// Pressing the arrow on `custom` therefore starts at the beginning of the
/// library — `Flat`, the one curve that is a reset — rather than at an arbitrary
/// entry with no name.
fn apply_eq_preset(state: &mut AppState, params: &SynthParams, delta: i32, logger: &Logger) {
    let target = EqTarget::from_index(state.eq_target);
    let current = params.eq_at(target).curve();
    // `None` when the curve is not in the library, which `step` reads as "just
    // off the start" — so the first press lands on the first entry, `Flat`.
    let from = state
        .presets
        .name_for_curve(&current)
        .and_then(|name| state.presets.index_of(name));
    let Some((_, preset)) = state.presets.step(from, delta) else {
        state.flash(400);
        return;
    };
    let (name, gains) = (preset.name.clone(), preset.gains);
    params.eq_at(target).set(&gains);
    logger.input(&format!("EQ {} preset = {}", target.name(), name));
}

/// What `Enter` does on an EQ row.
///
/// One action, on the one row where "put it back" is a thing you want mid-sweep:
/// `Enter` on `gain` returns the band under the cursor to zero. Everything else
/// is a value the arrows already move.
fn eq_action(state: &mut AppState, params: &SynthParams, logger: &Logger) {
    match state.eq_row {
        EQ_ROW_GAIN => {
            let target = EqTarget::from_index(state.eq_target);
            let band = state.eq_band.min(EQ_BANDS - 1);
            params.eq_at(target).set_band(band, 0.0);
            logger.input(&format!(
                "EQ {} {} = 0.0 dB",
                target.name(),
                crate::eq::BAND_LABELS[band]
            ));
        }
        EQ_ROW_SAVE => {
            state.modal = Some(Modal::EqPresetNameInput {
                buffer: String::new(),
            });
            logger.input("EQ preset name begin");
        }
        _ => {}
    }
}

/// Keep the target's current curve as a named preset.
///
/// The path is a parameter rather than read from [`crate::eq::user_path`] here
/// so a test writes to a temporary file instead of the checkout's, the same
/// arrangement the instrument and ensemble savers use.
fn save_eq_preset(
    state: &mut AppState,
    params: &SynthParams,
    name: &str,
    path: &Path,
    logger: &Logger,
) {
    // `Flat` is the name of the bypassed curve. A preset allowed to take it
    // would make the `preset` row say "Flat" about a curve that is not, which is
    // the one lie this panel is built not to tell.
    if name.eq_ignore_ascii_case(crate::eq::FLAT_LABEL) {
        logger.input("EQ a curve named Flat would mean bypass: not saved");
        state.flash(400);
        return;
    }
    let target = EqTarget::from_index(state.eq_target);
    let gains = params.eq_at(target).curve();
    state.presets.add(EqPreset {
        name: name.to_string(),
        gains,
    });
    match state.presets.save(path) {
        Ok(()) => logger.input(&format!(
            "EQ saved preset '{}' from {}",
            name,
            target.name()
        )),
        Err(e) => logger.input(&format!("SAVE ERROR: {}", e)),
    }
}

// -----------------------------------------------------------------------------
// The Spectrum panel
// -----------------------------------------------------------------------------

/// The editable rows of the Spectrum panel, in display order.
///
/// Everything except `target` is about how the picture is drawn rather than what
/// it contains; nothing here changes a sound.
const SPECTRUM_ROWS: usize = 5;
const SPECTRUM_ROW_TARGET: usize = 0;
const SPECTRUM_ROW_RANGE: usize = 1;
const SPECTRUM_ROW_SPEED: usize = 2;
const SPECTRUM_ROW_HOLD: usize = 3;
const SPECTRUM_ROW_RESET: usize = 4;

/// The label column, wide enough for the longest row name.
const SPECTRUM_LABEL_WIDTH: usize = 9;

/// How tall the chart is, in character rows.
///
/// Each row can be half full, so this is **twice** this many levels of
/// resolution — twenty-four down a sixty decibel span is two and a half decibels
/// a step, which is fine enough to see a shape move and coarse enough to read at
/// a glance.
const SPECTRUM_CHART_ROWS: usize = 12;

/// The spans the `range` row offers, in decibels from the top of the chart to the
/// bottom, and what to call them.
///
/// All three are multiples of the row count, so every row is a whole number of
/// decibels and every printed scale mark lands on an integer. A span that did not
/// divide would put "−17.3" on the axis, which is a worse picture than a
/// differently sized one.
const SPECTRUM_RANGES: [f32; 3] = [48.0, 60.0, 72.0];
const SPECTRUM_RANGE_LABELS: [&str; 3] = ["narrow", "medium", "wide"];
const SPECTRUM_DEFAULT_RANGE: usize = 1;

/// The scale marks down the left edge: four for the number, then the rule.
const SPECTRUM_SCALE_WIDTH: usize = 6;

/// Characters per band column.
///
/// Five, which is what `12.5k` needs. A narrower column would force the labels to
/// be abbreviated or printed under every other band, and an axis you cannot read
/// is worse than a chart that is five characters narrower.
const SPECTRUM_CELL: usize = 5;

/// Where the levels come from, named the way the target row names them.
fn spectrum_tap(target: EqTarget) -> usize {
    target.index()
}

/// How many of the chart's half-rows a level fills.
///
/// Full scale is `1.0` at the top of the chart: the master tap is taken after the
/// soft clip, so nothing there can exceed it. A register tap is taken *before*
/// the clip, so a part driven past full scale pegs at the top rather than being
/// hidden — which is the honest thing for a meter to do.
fn spectrum_levels(level: f32, span: f32) -> usize {
    let db = 20.0 * level.max(1.0e-9).log10();
    let filled = ((db + span) / span * (SPECTRUM_CHART_ROWS * 2) as f32).round();
    filled.clamp(0.0, (SPECTRUM_CHART_ROWS * 2) as f32) as usize
}

/// One band's character on one row.
///
/// `row` counts from the bottom, because a bar grows up. The peak marker is drawn
/// only where it is clear of the bar's own top, so a held peak reads as a tick
/// floating above the level rather than as part of it.
fn spectrum_cell(level: f32, peak: f32, span: f32, row: usize) -> char {
    let levels = spectrum_levels(level, span);
    let peaks = spectrum_levels(peak, span);
    let full = 2 * (row + 1);
    if levels >= full {
        '█'
    } else if levels + 1 == full {
        '▄'
    } else if peaks > levels && peaks > 2 * row && peaks <= full {
        '▀'
    } else {
        ' '
    }
}

/// The decibel mark on the left of chart row `row`, counting from the top.
///
/// A mark on every third row, plus the bottom one: five numbers down a twelve row
/// chart is enough to read a level off and few enough to read past.
fn spectrum_scale_mark(row: usize, span: f32) -> String {
    let step = span / SPECTRUM_CHART_ROWS as f32;
    let marked = row.is_multiple_of(3) || row == SPECTRUM_CHART_ROWS - 1;
    if !marked {
        return " ".repeat(SPECTRUM_SCALE_WIDTH);
    }
    let db = if row == SPECTRUM_CHART_ROWS - 1 {
        -span
    } else {
        -(row as f32 * step)
    };
    // Negating a zero gives `-0.0`, which formats as "-0" — a strange thing to
    // print where the full-scale mark belongs.
    let db = if db == 0.0 { 0.0 } else { db };
    format!("{:>4} ┤", format!("{:.0}", db))
}

/// The live spectrum: thirteen band levels for one register or the mix.
fn render_spectrum_body<W: io::Write>(
    stdout: &mut W,
    params: &SynthParams,
    state: &AppState,
) -> io::Result<()> {
    let focused = state.focus == Focus::Spectrum;
    let target = EqTarget::from_index(state.eq_target);
    let tap = spectrum_tap(target);
    let span = SPECTRUM_RANGES[state.spectrum_range.min(SPECTRUM_RANGES.len() - 1)];
    let row = |index: usize| focused && state.spectrum_row == index;
    let marker = |index: usize| if row(index) { "▸" } else { " " };

    let speed = params.analyzer.release.get();
    let speed_label = ANALYZER_SPEEDS
        .iter()
        .min_by(|a, b| {
            (a.0 - speed)
                .abs()
                .partial_cmp(&(b.0 - speed).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, name)| *name)
        .unwrap_or("medium");

    let rows = [
        ("target", eq_header(state, params)),
        (
            "range",
            format!(
                "{}  {:.0} dB",
                SPECTRUM_RANGE_LABELS[state.spectrum_range.min(SPECTRUM_RANGE_LABELS.len() - 1)],
                span
            ),
        ),
        ("speed", format!("{}  {:.0} dB/s", speed_label, speed)),
        (
            "hold",
            if state.spectrum_hold { "on" } else { "off" }.to_string(),
        ),
    ];
    for (index, (label, value)) in rows.iter().enumerate() {
        draw_row(
            stdout,
            &format!(
                "  {} {:<SPECTRUM_LABEL_WIDTH$}{}",
                marker(index),
                label,
                value
            ),
            row(index),
        )?;
    }
    draw_row(
        stdout,
        &format!("  {} [Reset Peaks]", marker(SPECTRUM_ROW_RESET)),
        row(SPECTRUM_ROW_RESET),
    )?;

    execute!(stdout, Print("\r\n"))?;

    let published = params.analyzer.tap(tap);
    let peaks = state
        .spectrum_peaks
        .get(tap)
        .copied()
        .unwrap_or([0.0; crate::eq::EQ_BANDS]);
    for chart_row in 0..SPECTRUM_CHART_ROWS {
        // The chart is drawn top down and the bars grow up, so the rows are
        // walked in display order and converted once.
        let from_bottom = SPECTRUM_CHART_ROWS - 1 - chart_row;
        let mut line = spectrum_scale_mark(chart_row, span);
        for (band, peak) in peaks.iter().enumerate() {
            let level = published.level(band);
            let peak = if state.spectrum_hold { *peak } else { 0.0 };
            let cell = spectrum_cell(level, peak, span, from_bottom);
            for _ in 0..SPECTRUM_CELL {
                line.push(cell);
            }
        }
        execute!(stdout, Print(format!("{}\r\n", line.trim_end())))?;
    }

    // The axis, in the same five character columns the bars are drawn in.
    //
    // Centred rather than right-aligned, unlike the EQ panel's own band labels:
    // these sit next to *each other* rather than at the end of a row, and the
    // ladder's one five-character name — `12.5k`, beside `8k` — would otherwise
    // print as `8k12.5k` and read as one label.
    let mut axis = " ".repeat(SPECTRUM_SCALE_WIDTH);
    for label in crate::eq::BAND_LABELS {
        axis.push_str(&format!("{:^SPECTRUM_CELL$}", label));
    }
    execute!(stdout, Print(format!("{}\r\n", axis.trim_end())))?;
    Ok(())
}

/// What an arrow key does on each Spectrum row.
///
/// Nothing here changes a sound, so `Shift` has nothing to coarsen: every row is
/// a short cycle or a toggle.
fn adjust_spectrum(state: &mut AppState, params: &SynthParams, delta: i32, logger: &Logger) {
    match state.spectrum_row {
        SPECTRUM_ROW_TARGET => cycle_eq_target(state, delta, logger),
        SPECTRUM_ROW_RANGE => {
            let next =
                (state.spectrum_range as i32 + delta).rem_euclid(SPECTRUM_RANGES.len() as i32);
            state.spectrum_range = next as usize;
            logger.input(&format!(
                "SPECTRUM range = {} ({:.0} dB)",
                SPECTRUM_RANGE_LABELS[state.spectrum_range], SPECTRUM_RANGES[state.spectrum_range]
            ));
        }
        SPECTRUM_ROW_SPEED => {
            let speed = params.analyzer.release.get();
            let index = ANALYZER_SPEEDS
                .iter()
                .position(|(rate, _)| *rate == speed)
                .unwrap_or(crate::analyzer::DEFAULT_SPEED);
            let next = (index as i32 + delta).rem_euclid(ANALYZER_SPEEDS.len() as i32);
            let (rate, name) = ANALYZER_SPEEDS[next as usize];
            params.analyzer.release.set(rate);
            logger.input(&format!("SPECTRUM speed = {} ({:.0} dB/s)", name, rate));
        }
        SPECTRUM_ROW_HOLD => {
            state.spectrum_hold = !state.spectrum_hold;
            logger.input(&format!(
                "SPECTRUM hold = {}",
                if state.spectrum_hold { "on" } else { "off" }
            ));
        }
        // `[Reset Peaks]` is a button: `Enter` runs it and the arrows have
        // nothing to move.
        _ => {}
    }
}

/// What `Enter` does on a Spectrum row: the one button.
fn spectrum_action(state: &mut AppState, logger: &Logger) {
    if state.spectrum_row != SPECTRUM_ROW_RESET {
        return;
    }
    state.clear_spectrum_peaks();
    logger.input("SPECTRUM peaks cleared");
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

/// One page of the sound design: three channel columns plus the master block,
/// which is on every page because it belongs to no page in particular.
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
    let page = state.synth_page;
    let rows = page.rows();

    let mut header = format!("    {:<SYNTH_LABEL_WIDTH$}", "param");
    for name in CHANNEL_COLUMN_LABELS {
        header.push_str(&format!("{:<SYNTH_VALUE_WIDTH$}", name));
    }
    execute!(stdout, Print(format!("{}\r\n", header.trim_end())))?;

    // The instrument row, above the page's own settings and on every page: what
    // you do *to* a register rather than one of its parameters, and the one row
    // worth having while the loop plays.
    {
        let marker = if focused && state.synth_row == SYNTH_ROW_INSTRUMENT {
            "▸"
        } else {
            " "
        };
        let mut line = format!("  {} {:<SYNTH_LABEL_WIDTH$}", marker, "instrument");
        for col in 0..CHANNEL_COUNT {
            let cell = SynthCell::Instrument { col };
            line.push_str(&synth_field(
                &state.instrument_label(col, params),
                SYNTH_VALUE_WIDTH,
                selected == Some(cell),
            ));
        }
        draw_row(
            stdout,
            line.trim_end(),
            focused && state.synth_row == SYNTH_ROW_INSTRUMENT,
        )?;
    }

    for (row, param) in rows.iter().enumerate() {
        let row = row + channel_row_start();
        let marker = if focused && state.synth_row == row {
            "▸"
        } else {
            " "
        };
        let mut line = format!("  {} {:<SYNTH_LABEL_WIDTH$}", marker, param.label());
        for col in 0..CHANNEL_COUNT {
            let cell = SynthCell::Channel { param: *param, col };
            line.push_str(&synth_field(
                &param.display(channel_at(params, col)),
                SYNTH_VALUE_WIDTH,
                selected == Some(cell),
            ));
        }
        draw_row(stdout, line.trim_end(), focused && state.synth_row == row)?;
    }

    // Every page is padded to the tallest, so the panels below this one do not
    // move when the page does. A table that resizes the whole layout on a
    // keystroke is worse than one that leaves a little air in it.
    for _ in rows.len()..page_row_capacity() {
        execute!(stdout, Print("\r\n"))?;
    }

    for (index, cells) in MASTER_ROWS.iter().enumerate() {
        let row = channel_row_start() + rows.len() + index;
        let marker = if focused && state.synth_row == row {
            "▸"
        } else {
            " "
        };
        let mut line = format!("  {} ", marker);
        for (col, param) in cells.iter().enumerate() {
            // One column per cell, so a short row simply ends earlier rather
            // than leaving a ragged half.
            line.push_str(&format!("{:<SYNTH_LABEL_WIDTH$}", param.label()));
            line.push_str(&synth_field(
                &param.display(state.mixer_ctx(params)),
                SYNTH_VALUE_WIDTH,
                selected == Some(SynthCell::Master { row: index, col }),
            ));
        }
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
        "| master {:.0}  reverb {:.0}%  delay {:.0}%  {}",
        params.master_volume.get(),
        params.reverb_mix.get() * 100.0,
        params.delay_mix.get() * 100.0,
        format_note_length(state.transport.note_length())
    ));
    execute!(stdout, Print(format!("  {}\r\n", line)))?;
    Ok(())
}

fn render_ensembles_body<W: io::Write>(stdout: &mut W, state: &AppState) -> io::Result<()> {
    let focused = state.focus == Focus::SynthEnsembles;
    for (i, ensemble) in state.ensemble_store.ensembles.iter().enumerate() {
        let marker = if focused && i == state.ensemble_row {
            "▸"
        } else {
            " "
        };
        draw_row(
            stdout,
            &format!("  {} {}", marker, ensemble.name),
            focused && i == state.ensemble_row,
        )?;
    }
    let save_row = state.ensemble_store.ensembles.len();
    let marker = if focused && state.ensemble_row == save_row {
        "▸"
    } else {
        " "
    };
    draw_row(
        stdout,
        &format!("  {} [Save As...]", marker),
        focused && state.ensemble_row == save_row,
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
/// One row of the log or the ranking, before it is a string.
struct HistoryLine {
    /// `#12` in the log — which play it was — or the count in the ranking.
    left: String,
    play: Play,
    selected: bool,
    in_progression: bool,
}

/// The rows the panel can show, oldest first in the log and best first in the
/// ranking.
fn history_lines(state: &AppState) -> Vec<HistoryLine> {
    let mut lines: Vec<HistoryLine> = match state.history_view {
        HistoryView::Log => state
            .history
            .plays()
            .iter()
            .enumerate()
            .map(|(index, play)| HistoryLine {
                // Numbered from one, in the order they were played, so a row can
                // be pointed at out loud.
                left: format!("#{}", index + 1),
                play: play.clone(),
                selected: false,
                in_progression: false,
            })
            .collect(),
        HistoryView::Top => state
            .history
            .top()
            .iter()
            .map(|tally| HistoryLine {
                left: format!("{}", tally.count),
                play: tally.play.clone(),
                selected: false,
                in_progression: false,
            })
            .collect(),
        HistoryView::Off => Vec::new(),
    };
    for (index, line) in lines.iter_mut().enumerate() {
        line.selected = index == state.history_row;
        line.in_progression = state.chord_is_in_the_progression(line.play.chord);
    }
    lines
}

/// One row: what it is on the left, then the chord, and where it sits in the
/// progression on the right.
fn history_row_text(line: &HistoryLine, key: &Key) -> String {
    let notes: Vec<String> = line.play.notes(key).iter().map(|n| note_name(*n)).collect();
    format!(
        "  {} {:>4}  {:<10} {:<5} {:<22}{}",
        if line.selected { "▸" } else { " " },
        clip_cell(&line.left, 4),
        clip_cell(&line.play.chord.label(key), 10),
        format!("({})", line.play.chord.degree_label()),
        clip_cell(&notes.join(" "), 22),
        if line.in_progression { "●" } else { " " },
    )
}

/// The log, in whichever of its two views is up.
///
/// It borrows the rhythm panel's slot, which is the widest and tallest thing on
/// screen, so the chord list and the readout above it stay visible while you read
/// back what you played.
fn render_history<W: io::Write>(stdout: &mut W, state: &AppState) -> io::Result<()> {
    let key = state.transport.key();
    let top = state.history_view == HistoryView::Top;
    // The heavy rule, because the log is what the history keys act on while it is
    // up, wherever the Tab cursor happens to be.
    let header = if top {
        format!(
            " Top [{} chords · {} played] ",
            state.history.chords(),
            state.history.len()
        )
    } else {
        format!(
            " History [{} played · {} chords] ",
            state.history.len(),
            state.history.chords()
        )
    };
    draw_panel_header(stdout, &header, true)?;

    draw_row(
        stdout,
        &format!(
            "    {:>4}  {:<10} {:<5} {:<22}",
            if top { "n" } else { "#" },
            "chord",
            "deg",
            "notes"
        ),
        false,
    )?;

    let lines = history_lines(state);
    if lines.is_empty() {
        draw_row(
            stdout,
            "      (nothing played yet — hold a chord, or press Enter)",
            false,
        )?;
        for _ in 1..HISTORY_ROWS {
            execute!(stdout, Print("\r\n"))?;
        }
    } else {
        let start = state.history_scroll.min(lines.len().saturating_sub(1));
        let window = lines.iter().skip(start).take(HISTORY_ROWS);
        let mut drawn = 0;
        for line in window {
            let text = history_row_text(line, &key);
            if line.selected || !line.in_progression {
                draw_row(stdout, &text, line.selected)?;
            } else {
                // In the progression, and not under the cursor: the one thing on
                // this panel that colour is carrying. The cursor row keeps its
                // band, which is a stronger cue than a colour.
                execute!(
                    stdout,
                    SetForegroundColor(Color::Green),
                    Print(format!("{}\r\n", text)),
                    SetForegroundColor(Color::Reset),
                )?;
            }
            drawn += 1;
        }
        for _ in drawn..HISTORY_ROWS {
            execute!(stdout, Print("\r\n"))?;
        }
    }

    draw_row(
        stdout,
        "  f put in the registers   g/c move   r play while held   l hide   ● in the progression",
        false,
    )?;
    Ok(())
}

fn render_sinko_panel<W: io::Write>(
    stdout: &mut W,
    state: &AppState,
    budget: usize,
) -> io::Result<()> {
    // The log borrows this slot, and it is drawn whether or not the panel has
    // focus: `l` flips it up from wherever the hands are, so it cannot depend on
    // a cursor that is somewhere else.
    if state.history_view.is_on() {
        return render_history(stdout, state);
    }

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
            row(SINKO_ROW_SWING),
            "swing",
            format_swing(state.working.swing, state.transport.swing())
        ),
        state.sinko_row == SINKO_ROW_SWING,
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
            row(SINKO_ROW_LENGTH),
            "length",
            cell_length_label(state)
        ),
        state.sinko_row == SINKO_ROW_LENGTH,
    )?;

    draw_row(
        stdout,
        &format!(
            "  {} {:<9}{}",
            row(SINKO_ROW_ACCENT),
            "accent",
            cell_accent_label(state)
        ),
        state.sinko_row == SINKO_ROW_ACCENT,
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
    // The cursor is drawn on the newest take's line, because that is the take a
    // new hit goes into — and on the two rows that edit the cell it points at,
    // so `length` and `accent` say which hit they are about without a second
    // cursor to keep in step.
    let cursor = if state.focus == Focus::Sinko
        && matches!(
            state.sinko_row,
            SINKO_ROW_HITS | SINKO_ROW_LENGTH | SINKO_ROW_ACCENT
        ) {
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
        Some(patterns) => format!("[Paste Sinko: {}]", clipboard_label(patterns)),
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
    // A range is worth saying out loud even when the highlight makes it visible,
    // because it is the count that tells you whether a group action will hit four
    // chords or three.
    let selected = state.selection_len();
    let range = if selected > 1 {
        format!("  {} selected", selected)
    } else {
        String::new()
    };
    // The header line is the rule and nothing else; the clipboard status — which
    // the rhythm keys report into, because this panel is where they are pressed —
    // is a row of its own below it. A row that comes and goes costs nothing here:
    // the chord list is the shorter of the two columns, so the transport's
    // padding absorbs it.
    draw_panel_header(
        stdout,
        &format!(" Progression{}{} ", history, range),
        focused,
    )?;
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
    let selection = state.selection_in(prog_len);

    // The gap above the first chord: the cursor can sit here, and it is where a
    // paste would land. Drawn as a rule rather than another row, because it is a
    // *place between* chords, not a chord.
    if focused && state.progression_before_first {
        execute!(
            stdout,
            SetForegroundColor(INSERT_FG),
            Print("  ────────────────────\r\n"),
            ResetColor,
        )?;
    }

    // The chord rows are built first and measured, because the menu beside them
    // is a right-hand column: it has to start where the widest chord row ends,
    // and the menu has to know that before the first row is drawn.
    struct Row {
        text: String,
        colour: Option<Color>,
        accent: bool,
    }
    let rows: Vec<Row> = prog
        .slots
        .iter()
        .enumerate()
        .map(|(i, slot)| {
            let is_play = Some(i) == playing_bar && i < prog_len;
            let is_cursor =
                focused && !state.progression_before_first && i == state.progression_row;
            let is_sel = focused
                && selection
                    .map(|(start, end)| i >= start && i <= end)
                    .unwrap_or(false);
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
                        format!(
                            "   {}   {}",
                            if pattern.is_empty() { "—" } else { &pattern },
                            format_offset(entry.offset_ticks)
                        )
                    }
                }
                Slot::Rest => String::new(),
            };
            // Four states on one row: sounding (red and bold) outranks
            // everything, then the row the cursor is on (yellow and bold), then
            // the rest of the selection (yellow), then nothing. A range has to
            // read as a range at a glance, and the cursor inside it has to stay
            // findable.
            let (colour, accent) = if is_play {
                (Some(Color::Red), true)
            } else if is_cursor {
                (Some(Color::Yellow), true)
            } else if is_sel {
                (Some(Color::Yellow), false)
            } else {
                (None, false)
            };
            Row {
                text: format!(
                    "  {} {} {}{}",
                    if is_cursor { "▸" } else { " " },
                    if is_play { "▶" } else { " " },
                    slot.label(&key),
                    rhythm
                ),
                colour,
                accent,
            }
        })
        .collect();

    let chord_width = rows
        .iter()
        .map(|row| visible_width(&row.text))
        .max()
        .unwrap_or(0);
    // The menu is the group actions, so it appears with a group: a single chord
    // would get a column of commands that all do nothing.
    let menu_shown = focused && selection.map(|(start, end)| end > start).unwrap_or(false);
    let menu_focused = menu_shown && state.progression_menu;

    for (i, row) in rows.iter().enumerate() {
        match row.colour {
            Some(colour) => execute!(
                stdout,
                SetForegroundColor(colour),
                SetAttribute(Attribute::Bold),
                Print(&row.text),
                SetAttribute(Attribute::Reset),
            )?,
            None => execute!(stdout, Print(&row.text))?,
        };
        let _ = row.accent;

        // The menu: the first few chord rows carry it, right-aligned in its own
        // column, so the commands sit beside the chords they act on without
        // stealing the rows the list needs for a long progression.
        if let Some((label, _)) = PROGRESSION_MENU.get(i).filter(|_| menu_shown) {
            let text = format!(
                "{}{:>width$}",
                if menu_focused && i == state.progression_menu_row {
                    "▸ "
                } else {
                    "  "
                },
                label,
                width = PROGRESSION_MENU_LABEL
            );
            execute!(
                stdout,
                Print(" ".repeat(
                    PROGRESSION_MENU_GAP + chord_width.saturating_sub(visible_width(&row.text))
                )),
            )?;
            if menu_focused && i == state.progression_menu_row {
                execute!(
                    stdout,
                    SetAttribute(Attribute::Bold),
                    SetForegroundColor(Color::White),
                    SetBackgroundColor(FOCUS_ROW_BG),
                    Print(text),
                    SetAttribute(Attribute::Reset),
                )?;
            } else {
                execute!(stdout, Print(text))?;
            }
        }
        execute!(stdout, Print("\r\n"))?;
    }
    Ok(())
}

fn render_transport_panel<W: io::Write>(
    stdout: &mut W,
    params: &SynthParams,
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
    // The loop is exactly the progression: the live chord is no longer appended
    // as an extra bar, so the count is the chord count and nothing else.
    let playing_display = if state.transport.playing.load(Ordering::Relaxed) {
        let bar = state.transport.current_bar.load(Ordering::Relaxed);
        let auditioning = state.transport.audition_slot().is_some();
        format!(
            "▶ bar {}/{}{}",
            bar + 1,
            len.max(1),
            if auditioning { " *" } else { "" }
        )
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

    // The mixer's master volume, reached from here as well as from the Synth
    // panel: the same value, not a copy of it. A player setting a level while
    // playing should not have to Tab three panels away to do it.
    draw_transport_row(
        stdout,
        focused && row == TRANSPORT_ROW_VOLUME,
        "master volume",
        &format!("{:.0}", params.master_volume.get()),
    )?;

    // The one action button. Selected like a value row, but
    // `row_is_action_button` makes Enter activate it even while a chord is held,
    // and it shows the outcome of its last run, since flashing is invisible on
    // this panel.
    draw_midi_row(stdout, state, focused && row == TRANSPORT_ROW_MIDI)?;

    Ok(())
}

/// The `[MIDI]` row: a button, or a chooser once it is open.
///
/// Two rows of file actions were the least-used thing in the panel and the only
/// two rows that did the same kind of thing, so they are one button that opens
/// sideways: `[MIDI] ─ [EXPORT] import`, with `←`/`→` picking a side and the
/// bracketed one being what `Enter` will run.
fn draw_midi_row<W: io::Write>(stdout: &mut W, state: &AppState, selected: bool) -> io::Result<()> {
    let marker = if selected { "▸" } else { " " };

    // Closed, it is a button like any other: the label column, then the value
    // column, which is where whichever of the two actions ran last reports
    // itself. One row now, so one outcome.
    if !state.midi_open {
        let head = transport_row_head(marker, "[MIDI]");
        let status = match (&state.export_status, &state.import_status) {
            (Some(export), Some(import)) => Some(if export.shown_at >= import.shown_at {
                export
            } else {
                import
            }),
            (Some(export), None) => Some(export),
            (None, Some(import)) => Some(import),
            (None, None) => None,
        };
        return match status.and_then(|status| status.appearance_at(Instant::now())) {
            Some((colour, text)) => draw_row_with(
                stdout,
                &head,
                selected,
                Some((colour, format!(" {}", single_line_status(text)))),
            ),
            None => draw_row_with(
                stdout,
                &head,
                selected,
                Some((Color::Reset, " ".repeat(TRANSPORT_VALUE_WIDTH))),
            ),
        };
    }

    // Open, the chooser is one sentence rather than a key and a value, so it
    // spans both columns — and still reaches the panel's right edge, because
    // every row on this panel does.
    let mut chooser = String::from("[MIDI] ─ ");
    for (index, action) in MIDI_ACTIONS.iter().enumerate() {
        if index > 0 {
            chooser.push(' ');
        }
        if index == state.midi_choice {
            chooser.push_str(&format!("[{}]", action));
        } else {
            chooser.push_str(action);
        }
    }
    let width = TRANSPORT_LABEL_WIDTH + TRANSPORT_VALUE_WIDTH;
    let line = format!(
        "  {} {:<width$}",
        marker,
        clip_cell(&chooser, width),
        width = width
    );
    draw_row(stdout, &line, selected)
}

/// The metronome panel: the click's own settings, drawn in the Transport slot.
///
/// It reuses the transport's row shape deliberately — same marker, same padding —
/// because it *is* the transport's metronome row opened up, and `Esc` folds it
/// back down.
fn render_metronome_panel<W: io::Write>(stdout: &mut W, state: &AppState) -> io::Result<()> {
    let transport = &state.transport;
    let row = state.metronome_row.min(METRONOME_ROWS - 1);
    draw_panel_header(stdout, " Metronome ", true)?;

    let click = if state.metronome_on { "on" } else { "off" };
    let click = if state.recording {
        "on (recording)".to_string()
    } else {
        click.to_string()
    };
    draw_transport_row(stdout, row == METRONOME_ROW_CLICK, "click", &click)?;

    let sound = transport.metronome_sound.load(Ordering::Relaxed);
    let index = sound.min(crate::synth::CLICK_SOUNDS.len() - 1);
    let preset = &crate::synth::CLICK_SOUNDS[index];
    draw_transport_row(
        stdout,
        row == METRONOME_ROW_SOUND,
        "sound",
        &format!(
            "{}  [{}/{}]",
            preset.name,
            index + 1,
            crate::synth::CLICK_SOUNDS.len()
        ),
    )?;

    draw_transport_row(
        stdout,
        row == METRONOME_ROW_VOLUME,
        "volume",
        &format_metronome_volume(transport.metronome_volume()),
    )?;

    let subdivision = transport.metronome_subdivision();
    draw_transport_row(
        stdout,
        row == METRONOME_ROW_SUBDIVISION,
        "subdivision",
        &format!(
            "{}  ({} per beat)",
            format_resolution(BEATS_PER_BAR as usize * subdivision),
            subdivision
        ),
    )?;

    let swing = transport.swing();
    draw_transport_row(
        stdout,
        row == METRONOME_ROW_SWING,
        "swing",
        &format!("{:.0}%  ({})", swing * 100.0, swing_feel(swing)),
    )?;

    // Padded to the panel's width, so opening the metronome cannot change the
    // width of the column it borrows — the rows above already end there.
    for line in [
        String::new(),
        "  swing is the default for patterns".to_string(),
        "  [Esc] close [↑/↓] row [←/→] adjust".to_string(),
    ] {
        draw_row(
            stdout,
            &format!("{:<width$}", line, width = TRANSPORT_PANEL_WIDTH),
            false,
        )?;
    }
    Ok(())
}

fn draw_transport_row<W: io::Write>(
    stdout: &mut W,
    selected: bool,
    label: &str,
    value: &str,
) -> io::Result<()> {
    let marker = if selected { "▸" } else { " " };
    draw_row(stdout, &transport_row_text(marker, label, value), selected)
}

/// The transport panel's fixed shape.
///
/// The panel is the right-hand column and sits against the screen edge, so a
/// *fixed* width is what makes every value end in the same place: a value column
/// is only right-aligned if the rows agree on where the right is, and a width
/// that followed the longest row would move it every time a tempo grew a digit.
const TRANSPORT_LABEL_WIDTH: usize = 14;
const TRANSPORT_VALUE_WIDTH: usize = 18;
const TRANSPORT_PANEL_WIDTH: usize = 4 + TRANSPORT_LABEL_WIDTH + TRANSPORT_VALUE_WIDTH;

/// One `key → value` row of the transport: the key left, the value right.
fn transport_row_text(marker: &str, label: &str, value: &str) -> String {
    format!(
        "{}{:>width$}",
        transport_row_head(marker, label),
        clip_cell(value, TRANSPORT_VALUE_WIDTH),
        width = TRANSPORT_VALUE_WIDTH
    )
}

/// Everything up to the value column, padded so a value can be appended at the
/// right place — for rows whose value is coloured and so cannot be one string.
fn transport_row_head(marker: &str, label: &str) -> String {
    format!(
        "  {} {:<width$}",
        marker,
        clip_cell(label, TRANSPORT_LABEL_WIDTH),
        width = TRANSPORT_LABEL_WIDTH
    )
}

fn render_modal<W: io::Write>(
    stdout: &mut W,
    modal: &Modal,
    width: usize,
    instruments: &crate::instrument::InstrumentStore,
) -> io::Result<()> {
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
        Modal::AddRest => "  Add a rest (silent bar)?  [Enter] yes  [Esc] no  ".to_string(),
        Modal::EnsembleNameInput { buffer } => format!(
            "  Save ensemble as: {}   [Enter] save  [Esc] cancel  ",
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
        Modal::InstrumentNameInput { col, buffer } => format!(
            "  Keep {} as instrument: {}   [Enter] save  [Esc] cancel  ",
            CHANNEL_COLUMN_LABELS[*col],
            shown(buffer)
        ),
        Modal::EqPresetNameInput { buffer } => format!(
            "  Save curve as preset: {}   [Enter] save  [Esc] cancel  ",
            shown(buffer)
        ),
        Modal::FxPresetNameInput { buffer } => format!(
            "  Save effect as preset: {}   [Enter] save  [Esc] cancel  ",
            shown(buffer)
        ),
        // One row rather than a scrolling list: what auditioning needs is to
        // hear the next instrument and know its name, and the position tells you
        // where you are in the library without a screenful of names to read.
        Modal::InstrumentPicker { col, index, .. } => {
            let len = instruments.instruments.len();
            let name = instruments
                .instruments
                .get(*index)
                .map(|i| i.name.as_str())
                .unwrap_or("none");
            format!(
                "  {} ← {}  [{}/{}]   ↑/↓ try  Enter keep  Esc cancel  s save  ",
                CHANNEL_COLUMN_LABELS[*col],
                name,
                index + 1,
                len
            )
        }
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
/// Enough rows for the default view (15) with a line to spare. Anything shorter
/// and the panels below the fold simply are not drawn.
const MIN_SCREEN_HEIGHT: usize = 16;
/// Where a window wider than [`MIN_SCREEN_WIDTH`] puts the extra columns: into
/// the bar grid, which is the one thing on screen that is a drawing.
const GRID_BUDGET_MAX: usize = 120;
/// Assumed only when the terminal will not say (a redirected stdout). Exactly the
/// width the layout is designed for, and the height every always-drawn panel
/// fits in — which `panel_view_heights_are_tracked` is what keeps true.
///
/// One slot is exempt, deliberately: the Synth panel's, whose four views are the
/// synth table (28 rows), the equaliser (34), the spectrum (33) and the ensemble
/// list — and the last of those is as long as the library is, so it scrolls off
/// the bottom of any terminal rather than being cut. So the panels that are on
/// screen *whatever* the focus cost this many rows, and the views of that one
/// slot cost what their content costs.
const DEFAULT_SCREEN: Screen = Screen {
    width: MIN_SCREEN_WIDTH,
    height: 35,
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
         \x20 Panels always on screen:  {} × {}\r\n",
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
    let width = block.lines().map(visible_width).max().unwrap_or(0);
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
/// The rule that stands for the gap above the first chord: where a paste into
/// position 1 would land. Orange rather than the selection's yellow, because it
/// is not a selected chord — it is a place.
const INSERT_FG: Color = Color::Rgb {
    r: 0xFF,
    g: 0xA5,
    b: 0x00,
};

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
    use crate::eq::EqCurve;

    fn logger() -> Arc<Logger> {
        let mut path = std::env::temp_dir();
        path.push(format!("chord-tool-tui-{}.log", std::process::id()));
        Logger::create(path.to_str().unwrap()).unwrap()
    }

    fn state(focus: Focus) -> AppState {
        AppState {
            instrument_store: InstrumentStore::with_builtins(),
            instrument_loaded: Default::default(),
            held: PositionSet::new(),
            registers: Registers::default(),
            focus,
            progression: Arc::new(Mutex::new(Progression::new())),
            transport: Transport::new(Key::new(60, Scale::Major)),
            edit: Edit::None,
            transport_row: 0,
            metronome_open: false,
            metronome_row: 0,
            midi_open: false,
            midi_choice: MIDI_EXPORT,
            history: History::default(),
            history_view: HistoryView::Off,
            history_row: 0,
            history_scroll: 0,
            pending_play: None,
            history_held: None,
            history_timed: None,
            synth_row: 0,
            synth_col: 0,
            synth_page: SynthPage::Tone,
            progression_row: 0,
            progression_before_first: false,
            progression_anchor: None,
            progression_menu: false,
            progression_menu_row: 0,
            audition_sounding: None,
            audition_release_at: None,
            ensemble_row: 0,
            fx_col: 0,
            fx_slot: 0,
            fx_param: 0,
            fx_row: 0,
            eq_row: 0,
            eq_target: 0,
            eq_band: 0,
            spectrum_row: 0,
            spectrum_range: SPECTRUM_DEFAULT_RANGE,
            spectrum_hold: true,
            spectrum_peaks: [[0.0; crate::eq::EQ_BANDS]; ANALYZER_TAPS],
            presets: EqPresetStore::with_builtins(),
            fx_presets: FxPresetStore::with_builtins(),
            modal: None,
            ensemble_store: EnsembleStore::with_builtins(),
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
            rhythm_user_path: std::env::temp_dir().join("chord-tool-unused-rhythms.toml"),
            flash_until: None,
            taps: TapTracker::default(),
            export_dir: std::env::temp_dir(),
            export_status: None,
        import_status: None,
            settings_path: std::env::temp_dir().join("chord-tool-unused-settings.toml"),
            settings_seen: Settings::default(),
            settings_dirty: None,
        }
    }

    /// The shipped effect library, built once.
    ///
    /// A `MixerCtx` borrows the library, and a test that only wants to move a
    /// volume should not have to build and hold one.
    use crate::fx::P3;

    fn preset_library() -> &'static FxPresetStore {
        static LIBRARY: std::sync::OnceLock<FxPresetStore> = std::sync::OnceLock::new();
        LIBRARY.get_or_init(FxPresetStore::with_builtins)
    }

    /// A mixer context over a bare parameter set.
    fn ctx<'a>(p: &'a SynthParams, t: &'a Transport) -> MixerCtx<'a> {
        MixerCtx::new(p, t, preset_library())
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
            Focus::SynthEnsembles,
            Focus::Eq,
            Focus::Spectrum,
        ] {
            let mut s = state(focus);
            s.held.insert(KeyPosition::LeftIndex);
            assert!(enter_commits_chord(&s), "focus {:?}", focus);
        }
    }

    #[test]
    fn enter_defers_to_the_panel_when_no_chord_resolves() {
        for focus in [Focus::Transport, Focus::SynthEnsembles] {
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

        edit_progression(&mut s, ProgressionEdit::Delete, &log);
        assert_eq!(degrees(&s), vec![ScaleDegree::I, ScaleDegree::V]);
        assert_eq!(s.transport.progression_len.load(Ordering::Relaxed), 2);

        edit_progression(&mut s, ProgressionEdit::Undo, &log);
        assert_eq!(
            degrees(&s),
            vec![ScaleDegree::I, ScaleDegree::IV, ScaleDegree::V]
        );
        assert_eq!(s.transport.progression_len.load(Ordering::Relaxed), 3);

        edit_progression(&mut s, ProgressionEdit::Redo, &log);
        assert_eq!(degrees(&s), vec![ScaleDegree::I, ScaleDegree::V]);
    }

    #[test]
    fn copy_then_paste_duplicates_after_the_selection() {
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        s.progression_row = 0;

        edit_progression(&mut s, ProgressionEdit::Copy, &log);
        edit_progression(&mut s, ProgressionEdit::Paste, &log);

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
        edit_progression(&mut s, ProgressionEdit::Paste, &log);
        assert_eq!(degrees(&s), vec![ScaleDegree::I]);
        assert!(s.is_flashing());
    }

    #[test]
    fn undo_and_redo_are_noops_when_history_is_empty() {
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I]);
        edit_progression(&mut s, ProgressionEdit::Undo, &log);
        assert_eq!(degrees(&s), vec![ScaleDegree::I]);
        assert!(s.is_flashing());
    }

    #[test]
    fn delete_past_the_end_of_the_list_flashes() {
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I]);
        s.progression_row = 5;
        edit_progression(&mut s, ProgressionEdit::Delete, &log);
        assert_eq!(degrees(&s), vec![ScaleDegree::I]);
        assert!(s.is_flashing());
    }

    #[test]
    fn deleting_the_last_chord_leaves_the_cursor_valid() {
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        s.progression_row = 1;
        edit_progression(&mut s, ProgressionEdit::Delete, &log);
        assert_eq!(s.progression_row, 0);
    }

    // ---- the gap above the first chord, and the selection ----

    #[test]
    fn up_from_the_first_chord_lands_in_the_gap_above_it() {
        let mut s = state(Focus::Progression);
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        s.progression_row = 0;

        s.move_progression(-1, false);
        assert!(s.progression_before_first, "up from the first chord");
        assert_eq!(s.progression_row, 0, "with the row index left where it was");

        // Up again stays put: there is nothing above the gap.
        s.move_progression(-1, false);
        assert!(s.progression_before_first);

        s.move_progression(1, false);
        assert!(
            !s.progression_before_first,
            "down comes back onto the first"
        );
        assert_eq!(s.progression_row, 0);
    }

    #[test]
    fn a_chord_can_be_pasted_into_position_one() {
        // The gap is the one insertion point "paste after the cursor" cannot
        // reach, so this is the whole reason it exists.
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V, ScaleDegree::VI]);

        s.progression_row = 2;
        edit_progression(&mut s, ProgressionEdit::Copy, &log);

        s.progression_row = 0;
        s.move_progression(-1, false);
        assert!(s.progression_before_first);
        edit_progression(&mut s, ProgressionEdit::Paste, &log);

        assert_eq!(
            degrees(&s),
            vec![
                ScaleDegree::VI,
                ScaleDegree::I,
                ScaleDegree::V,
                ScaleDegree::VI
            ],
            "the copy landed at the front"
        );
        assert!(!s.progression_before_first, "and the cursor followed it");
        assert_eq!(s.progression_row, 0);
    }

    #[test]
    fn nothing_but_a_paste_acts_in_the_gap() {
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        s.progression_row = 0;
        s.move_progression(-1, false);

        for edit in [
            ProgressionEdit::Copy,
            ProgressionEdit::Delete,
            ProgressionEdit::Reverse,
            ProgressionEdit::Rotate,
            ProgressionEdit::ClearRhythms,
        ] {
            edit_progression(&mut s, edit, &log);
            assert_eq!(degrees(&s).len(), 2, "{:?} must not touch the list", edit);
            assert!(s.is_flashing(), "{:?} should say why", edit);
        }
    }

    #[test]
    fn enter_in_the_gap_adds_the_chord_at_the_front() {
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I]);
        s.held.insert(KeyPosition::LeftIndex);
        s.held.insert(KeyPosition::RightInner);

        s.progression_row = 0;
        s.move_progression(-1, false);
        add_current_chord(&mut s, false, &log);

        assert_eq!(s.progression_row, 0, "the new chord is at the front");
        assert!(!s.progression_before_first);
        assert_eq!(degrees(&s).len(), 2);
    }

    // ---- auditioning ----

    #[test]
    fn a_stopped_transport_speaks_the_computed_chord_at_once() {
        // The whole point of the audition: a change speaks when it happens, not
        // when the next bar line arrives at whatever tempo is set.
        let mut s = state(Focus::Transport);
        let now = Instant::now();
        assert_eq!(
            audition_step(&mut s, now),
            AuditionStep::Idle,
            "silent to start"
        );

        s.transport.set_live_chord(Some(vec![60, 64, 67]));
        assert_eq!(
            audition_step(&mut s, now),
            AuditionStep::Play(vec![60, 64, 67]),
            "a chord speaks the moment it is computed"
        );
        assert_eq!(
            audition_step(&mut s, now),
            AuditionStep::Idle,
            "and is not repeated every pass of the event loop"
        );

        // A different chord is a new note, immediately.
        s.transport.set_live_chord(Some(vec![62, 65, 69]));
        assert_eq!(
            audition_step(&mut s, now),
            AuditionStep::Play(vec![62, 65, 69])
        );
    }

    #[test]
    fn the_audition_release_is_generous_and_the_next_chord_cancels_it() {
        // Long tail, taken over rather than stacked: a run of changes stays
        // legato instead of smearing into every chord before it.
        let mut s = state(Focus::Transport);
        let start = Instant::now();

        s.transport.set_live_chord(Some(vec![60, 64, 67]));
        assert!(matches!(
            audition_step(&mut s, start),
            AuditionStep::Play(_)
        ));

        // The keys come off: the note keeps ringing rather than being cut.
        s.transport.set_live_chord(None);
        assert_eq!(
            audition_step(&mut s, start),
            AuditionStep::Idle,
            "still ringing"
        );
        assert_eq!(
            audition_step(&mut s, start + AUDITION_RELEASE / 2),
            AuditionStep::Idle,
            "and still ringing half way through the tail"
        );
        assert_eq!(
            audition_step(&mut s, start + AUDITION_RELEASE),
            AuditionStep::Release,
            "and released at the deadline"
        );

        // The next chord inside the window takes the note over instead.
        s.transport.set_live_chord(Some(vec![60, 64, 67]));
        assert!(matches!(
            audition_step(&mut s, start),
            AuditionStep::Play(_)
        ));
        s.transport.set_live_chord(None);
        assert_eq!(audition_step(&mut s, start), AuditionStep::Idle);
        s.transport.set_live_chord(Some(vec![62, 65, 69]));
        assert_eq!(
            audition_step(&mut s, start + AUDITION_RELEASE / 2),
            AuditionStep::Play(vec![62, 65, 69]),
            "the new chord cancels the pending release"
        );
        assert_eq!(
            audition_step(&mut s, start + AUDITION_RELEASE),
            AuditionStep::Idle,
            "and nothing is left to release"
        );
    }

    #[test]
    fn starting_the_transport_hands_the_audition_voice_back() {
        let mut s = state(Focus::Transport);
        let now = Instant::now();
        s.transport.set_live_chord(Some(vec![60, 64, 67]));
        assert!(matches!(audition_step(&mut s, now), AuditionStep::Play(_)));

        // The loop takes over: the audition must not ring over it, and must not
        // leave a release pending for when the transport stops again.
        s.transport.playing.store(true, Ordering::Relaxed);
        assert_eq!(audition_step(&mut s, now), AuditionStep::Release);
        assert_eq!(audition_step(&mut s, now), AuditionStep::Idle);
        assert!(s.audition_sounding.is_none());
        assert!(s.audition_release_at.is_none());
    }

    #[test]
    fn recalling_a_chord_arms_the_in_place_audition() {
        let log = logger();
        let mut s = sinko_state();
        s.focus = Focus::Progression;
        s.progression_row = 2;
        s.held.insert(KeyPosition::LeftIndex);
        s.registers.lock_right(&s.held);
        s.held.clear();

        handle_hotkey(&mut s, Hotkey::LoadSelectedChord, false, &log);
        assert_eq!(s.transport.audition_slot(), Some(2));
    }

    #[test]
    fn moving_the_selection_exits_the_audition() {
        let log = logger();
        let mut s = sinko_state();
        s.focus = Focus::Progression;
        s.progression_row = 2;
        set_audition_slot(&s, Some(2), &log);
        assert_eq!(s.transport.audition_slot(), Some(2));

        s.move_progression(1, false);
        assert_eq!(s.transport.audition_slot(), None, "a plain move exits it");

        set_audition_slot(&s, Some(2), &log);
        s.move_progression(-1, true);
        assert_eq!(s.transport.audition_slot(), None, "and so does extending");

        set_audition_slot(&s, Some(2), &log);
        s.set_current_row(0);
        assert_eq!(s.transport.audition_slot(), None, "and naming a row");

        set_audition_slot(&s, Some(2), &log);
        select_all_chords(&mut s, &log);
        assert_eq!(s.transport.audition_slot(), None, "and selecting all");

        set_audition_slot(&s, Some(2), &log);
        edit_progression(&mut s, ProgressionEdit::Reverse, &log);
        assert_eq!(s.transport.audition_slot(), None, "and a structural edit");
    }

    #[test]
    fn the_auditioned_chord_stands_in_for_its_slot_and_nothing_else() {
        // In place: the slot keeps its rhythm, its offset and its place in the
        // loop, so the change is heard against the rest of the progression.
        use crate::arrangement::{self, Audition};
        use crate::progression::ProgressionEntry;
        let key = Key::new(60, Scale::Major);
        let mut first = ProgressionEntry::new(ScaleDegree::I, None);
        first.pattern =
            Some(crate::rhythm::RhythmPattern::from_step_string("Q", 1.0, "xxxx").unwrap());
        let slots = vec![
            Slot::Chord(first),
            Slot::Chord(ProgressionEntry::new(ScaleDegree::V, None)),
        ];

        let plain = arrangement::arrangement(&slots, &key, 1.0);
        assert_eq!(plain[0].notes, vec![60, 64, 67], "the stored I");

        let swapped = vec![72u8, 76, 79];
        let auditioned = arrangement::arrangement_auditioning(
            &slots,
            &key,
            1.0,
            0.0,
            Some(Audition {
                slot: 0,
                notes: &swapped,
            }),
        );
        let in_first_bar = |plan: &[crate::arrangement::Stab]| -> Vec<Vec<u8>> {
            plan.iter()
                .filter(|s| s.start < crate::music::BAR_TICKS)
                .map(|s| s.notes.clone())
                .collect()
        };
        assert_eq!(
            in_first_bar(&auditioned),
            vec![swapped.clone(); 4],
            "slot 0 plays the stand-in, on the slot's own rhythm"
        );
        assert_eq!(
            auditioned.len(),
            plain.len(),
            "and nothing else about the plan moved"
        );

        // The slot next door is untouched.
        let only_second = arrangement::arrangement_auditioning(
            &slots,
            &key,
            1.0,
            0.0,
            Some(Audition {
                slot: 1,
                notes: &swapped,
            }),
        );
        assert!(
            in_first_bar(&only_second)
                .iter()
                .all(|n| *n == vec![60, 64, 67]),
            "auditioning slot 1 leaves slot 0 alone"
        );
    }

    #[test]
    fn the_loop_is_the_progression_and_nothing_more() {
        // The live bar used to be appended after the loop, which is what the
        // audition replaced.
        let mut s = state(Focus::Transport);
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V, ScaleDegree::VI]);
        s.transport.progression_len.store(3, Ordering::Relaxed);
        s.transport.playing.store(true, Ordering::Relaxed);
        s.transport.set_live_chord(Some(vec![60, 64, 67]));

        let text = render_transport(&s);
        assert!(
            strip_ansi(&text).contains("bar 1/3"),
            "the loop is three bars:\n{}",
            strip_ansi(&text)
        );
    }

    // ---- the menu column ----

    #[test]
    fn the_menu_is_a_column_beside_the_chords() {
        let log = logger();
        let mut s = sinko_state();
        s.focus = Focus::Progression;
        s.transport.set_bpm(240);

        // With one chord the panel has one column: the arrows are the offset
        // nudge, and there is no menu to go to.
        assert!(!s.progression_menu_active());
        state_arrow(&mut s, &log, KeyCode::Right, false);
        assert!(!s.progression_menu, "no menu without a run");
        assert_ne!(offset_at(&s, 2), 0, "the arrows nudged the offset instead");

        // A run brings the menu out, and then left/right picks the column.
        s.progression_anchor = Some(1);
        assert!(s.progression_menu_active());
        assert!(!s.progression_menu);
        state_arrow(&mut s, &log, KeyCode::Right, false);
        assert!(s.progression_menu, "right goes into the menu");
        assert_eq!(s.progression_menu_row, 0, "on its first item");
        state_arrow(&mut s, &log, KeyCode::Down, false);
        assert_eq!(s.progression_menu_row, 1, "and the menu walks with up/down");
        state_arrow(&mut s, &log, KeyCode::Left, false);
        assert!(!s.progression_menu, "left comes back to the chords");
        assert_eq!(s.progression_menu_row, 1, "remembering where it was");
        assert_eq!(s.progression_row, 2, "and leaving the chord cursor alone");
    }

    #[test]
    fn shift_arrow_still_nudges_the_offset_from_the_chord_column() {
        // The nudge had to survive the menu taking over plain `←/→`: it is the
        // same rule the Synth table has used all along.
        let log = logger();
        let mut s = sinko_state();
        s.focus = Focus::Progression;
        s.progression_row = 1;
        assert_eq!(offset_at(&s, 1), 0);

        state_arrow(&mut s, &log, KeyCode::Right, true);
        assert_ne!(offset_at(&s, 1), 0, "Shift+right nudged it");
        let nudged = offset_at(&s, 1);
        state_arrow(&mut s, &log, KeyCode::Left, true);
        assert_ne!(offset_at(&s, 1), nudged, "and back");
        assert!(!s.progression_menu, "a nudge does not move the column");
    }

    #[test]
    fn the_menu_walks_its_items_and_clamps() {
        let log = logger();
        let mut s = sinko_state();
        s.focus = Focus::Progression;
        s.progression_anchor = Some(1);
        s.progression_menu = true;
        s.progression_menu_row = 0;

        state_arrow(&mut s, &log, KeyCode::Up, false);
        assert_eq!(s.progression_menu_row, 0, "clamped at the top");
        for _ in 0..10 {
            state_arrow(&mut s, &log, KeyCode::Down, false);
        }
        assert_eq!(
            s.progression_menu_row,
            PROGRESSION_MENU.len() - 1,
            "and the bottom"
        );
        assert_eq!(s.progression_row, 2, "the chord cursor did not move");
    }

    #[test]
    fn enter_in_the_menu_runs_the_item_even_with_a_chord_held() {
        // Otherwise `Enter` would add the held chord instead of running the
        // command, which is the same trap the Sinko panel's buttons have.
        let log = logger();
        let mut s = sinko_state();
        s.focus = Focus::Progression;
        s.progression_anchor = Some(1);
        s.progression_menu = true;
        s.progression_menu_row = 1; // reverse
        s.held.insert(KeyPosition::LeftIndex);

        assert!(row_is_action_button(&s));
        assert!(
            enter_commits_chord(&s),
            "a chord is held, so it could commit"
        );
        assert_eq!(
            enter_intent(&s, false),
            EnterIntent::PanelAction,
            "but the menu outranks it"
        );
        run_progression_menu(&mut s, &log);
        assert!(!s.is_flashing(), "reversing two chords is a real edit");
    }

    #[test]
    fn the_menu_items_run_the_actions_they_name() {
        let log = logger();
        let mut s = sinko_state();
        s.focus = Focus::Progression;
        s.progression_row = 3;
        s.progression_anchor = Some(0);

        run_progression_menu_item(&mut s, ProgressionMenu::Reverse, &log);
        assert_eq!(
            degrees(&s),
            vec![
                ScaleDegree::IV,
                ScaleDegree::VI,
                ScaleDegree::V,
                ScaleDegree::I
            ]
        );

        run_progression_menu_item(&mut s, ProgressionMenu::Rotate, &log);
        assert_eq!(degrees(&s)[0], ScaleDegree::I, "the last came round");

        assign(&mut s, 1, "Quarters");
        run_progression_menu_item(&mut s, ProgressionMenu::ClearSinko, &log);
        assert_eq!(sinko_pattern_at(&s, 1), None, "the rhythms came off");
        assert_eq!(degrees(&s).len(), 4, "and the chords stayed");
    }

    #[test]
    fn the_menu_appears_with_a_run_and_not_before() {
        // The column is the group actions, so it is only there when there is a
        // group: one chord would get a menu of commands that all do nothing, and
        // the width it costs would buy nothing.
        let mut s = sinko_state();
        s.focus = Focus::Progression;
        let render = |s: &AppState| {
            let mut out: Vec<u8> = Vec::new();
            render_progression_panel(&mut out, s).unwrap();
            strip_ansi(&String::from_utf8(out).unwrap())
        };

        let width = |text: &str| text.lines().map(|l| l.chars().count()).max().unwrap_or(0);

        let alone = render(&s);
        assert!(!alone.contains("replace"), "{}", alone);

        s.progression_anchor = Some(1);
        let run = render(&s);
        assert!(run.contains("replace"), "{}", run);
        assert!(
            width(&run) > width(&alone),
            "the menu costs width, which is why it is not always there: {} vs {}",
            width(&run),
            width(&alone)
        );

        // And it goes away again when the run collapses, taking its width with
        // it.
        s.progression_anchor = None;
        let alone_again = render(&s);
        assert!(!alone_again.contains("replace"), "{}", alone_again);
        assert_eq!(width(&alone_again), width(&alone), "the panel is as it was");
    }

    #[test]
    fn collapsing_a_run_takes_the_menu_cursor_with_it() {
        let mut s = sinko_state();
        s.focus = Focus::Progression;
        s.progression_anchor = Some(1);
        s.progression_menu = true;
        s.progression_menu_row = 2;

        // A plain arrow drops the range, so the column it was standing in is
        // gone and the cursor comes back to the chords.
        s.move_progression(1, false);
        assert!(!s.progression_menu, "the cursor came back to the chords");
        assert!(!s.progression_menu_active());
    }

    #[test]
    fn the_menu_is_drawn_beside_the_chord_list_and_shares_its_edge() {
        colour_on();
        let mut s = sinko_state();
        s.focus = Focus::Progression;
        s.progression_anchor = Some(1);
        s.progression_menu = true;
        s.progression_menu_row = 2;

        let raw = {
            let mut out: Vec<u8> = Vec::new();
            render_progression_panel(&mut out, &s).unwrap();
            String::from_utf8(out).unwrap()
        };
        let text = strip_ansi(&raw);
        assert!(text.contains("replace"), "{}", text);
        assert!(text.contains("rotate"), "{}", text);

        // Right-aligned: the labels all end on the same column, and every menu
        // row is on a chord row rather than a row of its own.
        let ends: Vec<usize> = text
            .lines()
            .filter(|line| {
                PROGRESSION_MENU
                    .iter()
                    .any(|(label, _)| line.trim_end().ends_with(label))
            })
            .map(|line| line.trim_end().chars().count())
            .collect();
        assert_eq!(ends.len(), PROGRESSION_MENU.len(), "{}", text);
        assert!(
            ends.windows(2).all(|w| w[0] == w[1]),
            "{:?}\n{}",
            ends,
            text
        );

        // The cursor's item is banded, like every other focused row in the app.
        let cursor_line = raw
            .lines()
            .find(|line| line.contains("rotate"))
            .expect("the rotate row");
        assert!(cursor_line.contains("48;5;"), "{:?}", cursor_line);

        // And the chords column is still there, with its own cursor.
        assert!(text.contains("▸"), "{}", text);
    }

    #[test]
    fn the_menu_does_not_squeeze_the_transport_at_the_design_width() {
        // The menu makes the left column wider, and the right column is
        // squeezed first — so the design width is where this has to still fit.
        let mut s = sinko_state();
        s.focus = Focus::Progression;
        s.progression_menu = true;
        let raw = {
            let mut out: Vec<u8> = Vec::new();
            render(&mut out, &SynthParams::defaults(), &s, DEFAULT_SCREEN).unwrap();
            String::from_utf8(out).unwrap()
        };
        for line in raw.lines() {
            let visible = strip_ansi(line);
            assert!(
                visible.chars().count() <= DEFAULT_SCREEN.width,
                "row is {} wide at {} columns: {:?}",
                visible.chars().count(),
                DEFAULT_SCREEN.width,
                visible
            );
        }
        // The transport's own rows are intact: every value still ends on the
        // panel's edge rather than being clipped away.
        let bpm = raw
            .lines()
            .find(|line| line.contains("bpm"))
            .expect("the bpm row");
        assert!(
            strip_ansi(bpm).trim_end().ends_with("120"),
            "{:?}",
            strip_ansi(bpm)
        );
    }

    #[test]
    fn the_gap_is_drawn_above_the_first_chord() {
        colour_on();
        let mut s = state(Focus::Progression);
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);

        let raw_with = |s: &AppState| {
            let mut out: Vec<u8> = Vec::new();
            render_progression_panel(&mut out, s).unwrap();
            String::from_utf8(out).unwrap()
        };
        let orange = "\x1b[38;2;255;165;0m";
        assert!(
            !raw_with(&s).contains(orange),
            "no gap rule when the cursor is on a chord"
        );

        s.progression_row = 0;
        s.move_progression(-1, false);
        let raw = raw_with(&s);
        assert!(raw.contains(orange), "the gap is drawn, in orange");
        let lines: Vec<&str> = raw.lines().collect();
        let at = lines
            .iter()
            .position(|line| line.contains(orange))
            .expect("the gap line");
        assert!(
            lines[at].contains('─'),
            "and it reads as a rule: {:?}",
            lines[at]
        );
        // It sits directly above the first chord rather than among the chords.
        let next = strip_ansi(lines[at + 1]);
        assert!(
            next.contains("C") && !next.contains("G"),
            "the first chord follows the rule: {:?} in\n{:?}",
            next,
            lines
        );
        assert_eq!(
            lines.iter().filter(|line| line.contains(orange)).count(),
            1,
            "one rule, not one per row"
        );
    }

    #[test]
    fn shift_down_extends_the_selection_and_a_plain_arrow_collapses_it() {
        let mut s = state(Focus::Progression);
        seed(
            &mut s,
            &[
                ScaleDegree::I,
                ScaleDegree::V,
                ScaleDegree::VI,
                ScaleDegree::IV,
            ],
        );
        s.progression_row = 0;
        assert_eq!(s.selection(), Some((0, 0)));

        // Four chords from the first, the way it was asked for.
        s.move_progression(1, true);
        s.move_progression(1, true);
        s.move_progression(1, true);
        assert_eq!(s.selection(), Some((0, 3)));
        assert_eq!(s.selection_len(), 4);
        assert_eq!(s.progression_anchor, Some(0));
        assert_eq!(s.progression_row, 3, "the cursor is the moving end");

        // And it works upwards too, from wherever the cursor is.
        s.move_progression(-1, true);
        assert_eq!(s.selection(), Some((0, 2)));

        s.move_progression(1, false);
        assert_eq!(s.selection(), Some((3, 3)), "a plain arrow drops the range");
        assert_eq!(s.progression_anchor, None);
    }

    #[test]
    fn the_selection_never_runs_off_either_end() {
        let mut s = state(Focus::Progression);
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        s.progression_row = 0;
        for _ in 0..5 {
            s.move_progression(-1, true);
        }
        // Up from the first row goes into the gap, and the gap is not a range: a
        // selection is made of chords.
        assert!(s.progression_before_first);
        assert_eq!(s.selection(), None);
        assert_eq!(s.progression_anchor, None);
    }

    #[test]
    fn cmd_a_selects_every_chord() {
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(
            &mut s,
            &[
                ScaleDegree::I,
                ScaleDegree::V,
                ScaleDegree::VI,
                ScaleDegree::IV,
            ],
        );

        select_all_chords(&mut s, &log);
        assert_eq!(s.selection(), Some((0, 3)));
        assert_eq!(s.selection_len(), 4);

        // From anywhere in the list, and from the gap above it.
        s.progression_row = 2;
        s.progression_anchor = None;
        select_all_chords(&mut s, &log);
        assert_eq!(s.selection(), Some((0, 3)));

        s.move_progression(-1, false);
        s.progression_row = 0;
        s.move_progression(-1, false);
        assert!(s.progression_before_first);
        select_all_chords(&mut s, &log);
        assert_eq!(s.selection(), Some((0, 3)), "the gap is left behind");
    }

    #[test]
    fn select_all_outside_the_progression_panel_flashes() {
        let mut s = state(Focus::Sinko);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        select_all_chords(&mut s, &log);
        assert_eq!(s.selection(), Some((0, 0)), "the cursor did not move");
        assert!(s.is_flashing());
    }

    #[test]
    fn a_selection_copies_and_pastes_as_one_phrase() {
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(
            &mut s,
            &[
                ScaleDegree::I,
                ScaleDegree::V,
                ScaleDegree::VI,
                ScaleDegree::IV,
            ],
        );
        s.progression_row = 0;
        s.progression_anchor = Some(1);
        edit_progression(&mut s, ProgressionEdit::Copy, &log);

        // Paste it at the end, from a single-row cursor.
        s.progression_row = 3;
        s.progression_anchor = None;
        edit_progression(&mut s, ProgressionEdit::Paste, &log);

        assert_eq!(
            degrees(&s),
            vec![
                ScaleDegree::I,
                ScaleDegree::V,
                ScaleDegree::VI,
                ScaleDegree::IV,
                ScaleDegree::I,
                ScaleDegree::V,
            ]
        );
        assert_eq!(s.selection(), Some((4, 5)), "the pasted phrase is selected");
    }

    #[test]
    fn a_selection_deletes_as_one_edit() {
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(
            &mut s,
            &[
                ScaleDegree::I,
                ScaleDegree::V,
                ScaleDegree::VI,
                ScaleDegree::IV,
            ],
        );
        s.progression_row = 2;
        s.progression_anchor = Some(1);
        edit_progression(&mut s, ProgressionEdit::Delete, &log);

        assert_eq!(degrees(&s), vec![ScaleDegree::I, ScaleDegree::IV]);
        assert_eq!(
            s.progression_anchor, None,
            "the range is gone with the chords"
        );
        assert!(s.progression.lock().unwrap().undo());
        assert_eq!(degrees(&s).len(), 4, "one undo brings both back");
    }

    #[test]
    fn reordering_a_selection_keeps_it_selected() {
        // Reversing and rotating do not change the list's length, so the range
        // survives them — which is what makes pressing rotate a few times a way
        // to walk a phrase round.
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(
            &mut s,
            &[
                ScaleDegree::I,
                ScaleDegree::V,
                ScaleDegree::VI,
                ScaleDegree::IV,
            ],
        );
        s.progression_row = 3;
        s.progression_anchor = Some(0);

        edit_progression(&mut s, ProgressionEdit::Reverse, &log);
        assert_eq!(
            degrees(&s),
            vec![
                ScaleDegree::IV,
                ScaleDegree::VI,
                ScaleDegree::V,
                ScaleDegree::I
            ]
        );
        assert_eq!(s.selection(), Some((0, 3)), "still all four");

        edit_progression(&mut s, ProgressionEdit::Rotate, &log);
        assert_eq!(
            degrees(&s),
            vec![
                ScaleDegree::I,
                ScaleDegree::IV,
                ScaleDegree::VI,
                ScaleDegree::V
            ],
            "the last chord comes round to the front"
        );
        assert_eq!(s.selection_len(), 4);
    }

    #[test]
    fn clearing_the_selection_leaves_the_chords_and_the_loop_alone() {
        let mut s = sinko_state();
        let log = logger();
        assign(&mut s, 1, "Quarters");
        assign(&mut s, 2, "Offbeat Eighths");
        s.progression.lock().unwrap().set_offset(2, 480);
        s.focus = Focus::Progression;
        s.progression_row = 1;
        s.progression_anchor = Some(2);

        edit_progression(&mut s, ProgressionEdit::ClearRhythms, &log);

        assert_eq!(degrees(&s).len(), 4, "the chords are all still there");
        assert_eq!(sinko_pattern_at(&s, 1), None);
        assert_eq!(sinko_pattern_at(&s, 2), None);
        assert_eq!(offset_at(&s, 2), 0, "and the offsets went with them");
    }

    #[test]
    fn one_copied_rhythm_lands_on_every_selected_chord() {
        let mut s = sinko_state();
        let log = logger();
        assign(&mut s, 1, "Quarters");
        s.focus = Focus::Progression;

        // Copy from row 1, then select rows 0..2 and paste.
        s.progression_row = 1;
        s.progression_anchor = None;
        handle_hotkey(&mut s, Hotkey::CopySinko, false, &log);
        assert!(s.sinko_clipboard.is_some());

        s.progression_row = 2;
        s.progression_anchor = Some(0);
        handle_hotkey(&mut s, Hotkey::PasteSinko, false, &log);

        for row in 0..=2 {
            assert_eq!(
                sinko_pattern_at(&s, row).map(|p| p.name),
                Some("Quarters".to_string()),
                "row {} took the single copied rhythm",
                row
            );
        }
        assert_eq!(
            sinko_pattern_at(&s, 3),
            None,
            "and the chord outside the selection was left alone"
        );
    }

    #[test]
    fn a_phrase_of_rhythms_pastes_in_order_over_a_selection() {
        let mut s = sinko_state();
        let log = logger();
        assign(&mut s, 0, "Quarters");
        assign(&mut s, 1, "Offbeat Eighths");
        s.focus = Focus::Progression;

        s.progression_row = 0;
        s.progression_anchor = Some(1);
        handle_hotkey(&mut s, Hotkey::CopySinko, false, &log);

        s.progression_row = 3;
        s.progression_anchor = Some(2);
        handle_hotkey(&mut s, Hotkey::PasteSinko, false, &log);

        assert_eq!(
            sinko_pattern_at(&s, 2).map(|p| p.name),
            Some("Quarters".to_string())
        );
        assert_eq!(
            sinko_pattern_at(&s, 3).map(|p| p.name),
            Some("Offbeat Eighths".to_string())
        );
    }

    #[test]
    fn the_pattern_row_says_how_many_rhythms_are_waiting() {
        let mut s = sinko_state();
        let log = logger();
        assign(&mut s, 0, "Quarters");
        assign(&mut s, 1, "Offbeat Eighths");
        s.focus = Focus::Progression;
        s.progression_row = 0;
        s.progression_anchor = Some(1);
        handle_hotkey(&mut s, Hotkey::CopySinko, false, &log);

        // The paste row lives in the Sinko panel, so that is where it is read —
        // and the selection survives the Tab.
        s.focus = Focus::Sinko;
        let text = render_sinko(&s);
        assert!(
            text.contains("2 rhythms"),
            "the paste row names the phrase:\n{}",
            text
        );
    }

    #[test]
    fn the_selection_is_banded_with_the_cursor_inside_it() {
        colour_on();
        let mut s = state(Focus::Progression);
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V, ScaleDegree::VI]);
        s.progression_row = 1;
        s.progression_anchor = Some(0);

        let raw = {
            let mut out: Vec<u8> = Vec::new();
            render_progression_panel(&mut out, &s).unwrap();
            String::from_utf8(out).unwrap()
        };
        // The chord rows are the ones carrying the yellow foreground; the header
        // and the transport beside them are not.
        let selected: Vec<&str> = raw
            .lines()
            .filter(|line| line.contains("\x1b[38;5;11m") || line.contains("\x1b[33m"))
            .collect();
        assert_eq!(
            selected.len(),
            2,
            "two chords are highlighted:\n{:?}",
            raw.lines().collect::<Vec<_>>()
        );

        // The cursor row is bold as well, so the moving end is findable inside
        // the range.
        let cursor_line = raw
            .lines()
            .find(|line| line.contains("▸"))
            .expect("the cursor row");
        assert!(
            cursor_line.contains("\x1b[1m"),
            "the cursor is bold: {:?}",
            cursor_line
        );

        // And the header counts them, because a count is what says whether the
        // next group action hits two chords or three.
        assert!(
            strip_ansi(&raw).contains("2 selected"),
            "{}",
            strip_ansi(&raw)
        );
    }

    #[test]
    fn shift_turns_undo_into_redo() {
        // `handle_hotkey` resolves the Shift modifier, keeping
        // `KeyPosition::hotkey` a pure function of the physical key.
        let mut s = state(Focus::Progression);
        let log = logger();
        seed(&mut s, &[ScaleDegree::I, ScaleDegree::V]);
        s.progression_row = 1;
        edit_progression(&mut s, ProgressionEdit::Delete, &log);
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
        edit_progression(&mut s, ProgressionEdit::Delete, &log);
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
    fn transport_panel_exposes_the_midi_button_and_the_linked_volume() {
        let mut s = state(Focus::Transport);
        assert_eq!(s.row_count(), TRANSPORT_ROWS);
        s.set_current_row(TRANSPORT_ROW_MIDI);
        assert_eq!(s.current_row(), TRANSPORT_ROW_MIDI);
        assert!(row_is_action_button(&s));

        // The volume row is a value row, not a button, and the master block
        // really does contain the cell it edits.
        s.set_current_row(TRANSPORT_ROW_VOLUME);
        assert!(!row_is_action_button(&s));
        assert_eq!(MASTER_ROWS[master_row_of(MixerParam::MasterVolume).0]
            [master_row_of(MixerParam::MasterVolume).1], MixerParam::MasterVolume);
    }

    #[test]
    fn a_deep_synth_cursor_does_not_touch_the_transport_selection() {
        // These panels used to share one row index, so leaving the cursor deep
        // in the mixer pushed the Transport selection off the end of its own
        // (shorter) row list. They are separate fields now; assert that.
        let mut s = state(Focus::Synth);
        // The `fx` page is the deepest one, so its last row is the deepest row
        // this panel has.
        s.synth_page = SynthPage::Fx;
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
        // The whole point of the buttons-win rule: selecting the MIDI button and
        // pressing Enter must open its chooser, not add the chord under the
        // hands.
        let mut s = state(Focus::Transport);
        s.held.insert(KeyPosition::LeftIndex);
        s.set_current_row(TRANSPORT_ROW_MIDI);
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
        s.set_current_row(TRANSPORT_ROW_MIDI);
        // No chord resolves, but Ctrl+Enter appends regardless.
        assert_eq!(
            enter_intent(&s, true),
            EnterIntent::CommitChord { to_end: true }
        );
    }

    #[test]
    fn the_ensembles_save_row_is_a_button_too() {
        // The same latent trap existed here before the rule was introduced.
        let mut s = state(Focus::SynthEnsembles);
        s.ensemble_store.ensembles.clear();
        s.set_current_row(0);
        assert!(row_is_action_button(&s));

        s.held.insert(KeyPosition::LeftIndex);
        assert_eq!(enter_intent(&s, false), EnterIntent::PanelAction);
    }

    #[test]
    fn the_eq_save_row_is_a_button_too() {
        // The same latent trap: a held chord must not swallow the button press
        // and add a chord instead of opening the prompt.
        let mut s = state(Focus::Eq);
        s.set_current_row(EQ_ROW_SAVE);
        assert!(row_is_action_button(&s));

        s.held.insert(KeyPosition::LeftIndex);
        assert_eq!(enter_intent(&s, false), EnterIntent::PanelAction);

        // And a value row on the same panel still commits the chord.
        s.set_current_row(EQ_ROW_GAIN);
        assert_eq!(
            enter_intent(&s, false),
            EnterIntent::CommitChord { to_end: true }
        );
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
    fn the_midi_chooser_picks_a_side_and_runs_it() {
        // Collapsed from two rows into one: `Enter` opens the chooser, `←`/`→`
        // pick a side, and a second `Enter` runs the bracketed one.
        let log = logger();
        let mut s = state(Focus::Transport);
        s.set_current_row(TRANSPORT_ROW_MIDI);

        transport_action(&mut s, &SynthParams::defaults(), TRANSPORT_ROW_MIDI, &log);
        assert!(s.midi_open, "Enter opens the chooser");
        assert_eq!(s.midi_choice, MIDI_EXPORT, "it opens on export");

        // The chooser takes the arrows for its own selection.
        handle_panel_arrow(&mut s, &SynthParams::defaults(), &key(KeyCode::Right), &log);
        assert_eq!(s.midi_choice, MIDI_IMPORT);
        handle_panel_arrow(&mut s, &SynthParams::defaults(), &key(KeyCode::Left), &log);
        assert_eq!(s.midi_choice, MIDI_EXPORT);

        // Choosing import and running it opens the filename prompt.
        s.midi_choice = MIDI_IMPORT;
        transport_action(&mut s, &SynthParams::defaults(), TRANSPORT_ROW_MIDI, &log);
        assert!(!s.midi_open, "running a side folds the chooser back up");
        assert_eq!(modal_name(s.modal.as_ref()), "import path");

        // And export writes a file, with no prompt in the way.
        s.modal = None;
        s.export_dir = unique_export_dir("midi-chooser");
        seed(&mut s, &[ScaleDegree::I]);
        s.midi_open = true;
        s.midi_choice = MIDI_EXPORT;
        transport_action(&mut s, &SynthParams::defaults(), TRANSPORT_ROW_MIDI, &log);
        assert_eq!(modal_name(s.modal.as_ref()), "none");
        assert_eq!(std::fs::read_dir(&s.export_dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&s.export_dir).unwrap();
    }

    #[test]
    fn the_midi_chooser_folds_up_when_the_cursor_leaves_or_esc_is_pressed() {
        let log = logger();
        let mut s = state(Focus::Transport);
        s.set_current_row(TRANSPORT_ROW_MIDI);
        s.midi_open = true;

        // Esc closes the chooser rather than stopping the transport: it is a row
        // opened up, not a prompt.
        assert!(!esc_is_free(&s), "the chooser owns Esc");
        s.midi_open = false;
        assert!(esc_is_free(&s), "and gives it back");

        // Stepping off the row folds it up too, however it is left.
        s.midi_open = true;
        handle_panel_arrow(&mut s, &SynthParams::defaults(), &key(KeyCode::Up), &log);
        assert!(!s.midi_open);
        assert_eq!(s.current_row(), TRANSPORT_ROW_VOLUME);
    }

    #[test]
    fn the_open_metronome_panel_also_owns_esc() {
        // It always did in the documentation, and did not in the dispatch: a
        // "free" Esc stopped the transport out from under an open panel.
        let mut s = state(Focus::Transport);
        s.metronome_open = true;
        assert!(!esc_is_free(&s));
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
            let last = prog.len().saturating_sub(1);
            prog.delete_range(0, last);
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
        s.rhythm_user_path = unique_export_dir("rhythm-failure")
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
    fn replace_refuses_with_nothing_selected() {
        // The menu item acts on the selection, so it needs one — the same
        // promise copy, delete and the reordering items make.
        let log = logger();
        let mut s = sinko_state();
        s.focus = Focus::Progression;
        s.held.insert(KeyPosition::LeftMiddle);
        s.registers.lock_both(&s.held);
        s.held.clear();
        s.progression_row = 9;

        let before = degrees(&s);
        run_progression_menu_item(&mut s, ProgressionMenu::Replace, &log);
        assert_eq!(degrees(&s), before, "nothing was replaced");
        assert!(s.is_flashing(), "it flashes instead of guessing a row");
    }

    #[test]
    fn replace_runs_from_the_menu() {
        let log = logger();
        let mut s = sinko_state();
        s.focus = Focus::Progression;
        s.progression_row = 0;
        s.held.insert(KeyPosition::LeftMiddle);
        s.registers.lock_both(&s.held);
        s.held.clear();

        run_progression_menu_item(&mut s, ProgressionMenu::Replace, &log);
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
            Focus::SynthEnsembles,
            Focus::Eq,
            Focus::Spectrum,
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
        // A 16-cell grid with the cursor near the end, then down the ladder: the
        // cursor has to come back inside at every rung.
        let log = logger();
        let mut s = hits_state(16, &"x".repeat(16));
        s.sinko_cell = 15;
        assert_eq!(cursor_cell(&s), 15);

        cycle_resolution(&mut s, -1, &log);
        assert_eq!(
            s.working.steps_per_bar(),
            12,
            "the triplet grid is the next rung down"
        );
        assert_eq!(cursor_cell(&s), 11, "clamped onto the triplet grid");

        cycle_resolution(&mut s, -1, &log);
        assert_eq!(s.working.steps_per_bar(), 8);
        assert_eq!(cursor_cell(&s), 7, "clamped to the new grid");
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
        let mut s = sinko_state();
        // Assigning a held pattern loads it into the draft, so the row shows it.
        let row = s.progression_row;
        assign(&mut s, row, "Held Half");
        sync_working(&mut s);

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
            s.current_take = vec![jitter, 960 + jitter];
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

        // The first two entries of whatever palette this build ships, so the
        // test is about the cycle and not about which pattern happens to be
        // first.
        let (first, second) = {
            let store = s.rhythm_store.lock().unwrap();
            (
                store.patterns[0].name.clone(),
                store.patterns[1].name.clone(),
            )
        };

        cycle_assigned_pattern(&mut s, 1, &log);
        assert_eq!(assigned_in(&s).as_deref(), Some(first.as_str()));
        cycle_assigned_pattern(&mut s, 1, &log);
        assert_eq!(assigned_in(&s).as_deref(), Some(second.as_str()));

        // Backwards from the first entry lands on "none", so clearing an
        // assignment needs no row of its own.
        cycle_assigned_pattern(&mut s, -1, &log);
        cycle_assigned_pattern(&mut s, -1, &log);
        assert_eq!(assigned_in(&s), None);

        // And each cycle is one undoable edit.
        assert!(s.progression.lock().unwrap().undo());
        assert!(assigned_in(&s).is_some(), "undo brings the pattern back");
    }

    // ---- the Sinko panel's coarse step, counter and per-cell shape ----

    #[test]
    fn shift_arrow_crosses_the_palette_five_at_a_time() {
        let log = logger();
        let mut s = sinko_state();
        s.sinko_row = SINKO_ROW_PATTERN;
        assert_eq!(assigned_in(&s), None);

        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        let first = assigned_in(&s).expect("the first pattern");

        adjust_current_with(&mut s, &SynthParams::defaults(), 1, true, &log);
        let fifth = assigned_in(&s).expect("five on");
        let names: Vec<String> = s
            .rhythm_store
            .lock()
            .unwrap()
            .patterns
            .iter()
            .map(|p| p.name.clone())
            .collect();
        assert_eq!(names[0], first);
        assert_eq!(names[5], fifth, "one Shift press is five patterns");

        // And it wraps, so a long walk does not dead-end at the last entry.
        let last = names.len() - 1;
        let row = s.progression_row;
        assign(&mut s, row, &names[last]);
        adjust_current_with(&mut s, &SynthParams::defaults(), 1, true, &log);
        let wrapped = assigned_in(&s).expect("still on a pattern");
        let index = names
            .iter()
            .position(|n| *n == wrapped)
            .unwrap_or(usize::MAX);
        assert!(index < 5, "five past the end wraps to the start: {}", index);
    }

    #[test]
    fn the_pattern_row_reports_its_place_in_the_palette() {
        let log = logger();
        let mut s = sinko_state();
        s.sinko_row = SINKO_ROW_PATTERN;
        let total = s.rhythm_store.lock().unwrap().patterns.len();

        assert!(
            render_sinko(&s).contains(&format!("[0/{}]", total)),
            "nothing assigned is position zero:\n{}",
            render_sinko(&s)
        );

        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert!(
            render_sinko(&s).contains(&format!("[1/{}]", total)),
            "the first pattern is 1:\n{}",
            render_sinko(&s)
        );

        adjust_current_with(&mut s, &SynthParams::defaults(), 1, true, &log);
        assert!(
            render_sinko(&s).contains(&format!("[6/{}]", total)),
            "and a Shift press moves it five:\n{}",
            render_sinko(&s)
        );
    }

    #[test]
    fn the_swing_row_walks_from_follow_to_an_override() {
        let log = logger();
        let mut s = sinko_state();
        own(
            &mut s,
            RhythmPattern::from_step_string("S", 1.0, "xxxx").unwrap(),
        );
        s.sinko_row = SINKO_ROW_SWING;
        assert_eq!(s.working.swing, None, "a new pattern follows the transport");

        // Left from "follow" clamps there: there is nothing below it.
        adjust_current(&mut s, &SynthParams::defaults(), -1, &log);
        assert_eq!(s.working.swing, None);

        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert_eq!(s.working.swing, Some(0.0), "the first rung is straight");
        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert_eq!(s.working.swing, Some(0.1));

        // It is written through to the entry like every other row.
        assert_eq!(
            sinko_pattern(&s).expect("assigned").swing,
            Some(0.1),
            "the override is on the chord's own rhythm"
        );
    }

    #[test]
    fn the_swing_row_names_the_transport_when_it_follows_it() {
        let mut s = sinko_state();
        own(
            &mut s,
            RhythmPattern::from_step_string("S", 1.0, "xxxx").unwrap(),
        );
        s.transport.set_swing(0.6);

        let text = render_sinko(&s);
        assert!(text.contains("follow transport"), "{}", text);
        assert!(text.contains("60%"), "the transport's amount: {}", text);
        assert!(text.contains("heavy"), "and its feel: {}", text);

        s.working.swing = Some(0.0);
        let text = render_sinko(&s);
        assert!(!text.contains("follow transport"), "{}", text);
        assert!(text.contains("straight"), "{}", text);
    }

    #[test]
    fn the_length_row_edits_the_hit_under_the_cursor() {
        let log = logger();
        let mut s = sinko_state();
        own(
            &mut s,
            RhythmPattern::from_step_string("L", 1.0, "x-x-").unwrap(),
        );
        s.sinko_row = SINKO_ROW_HITS;
        s.sinko_cell = 0;

        s.sinko_row = SINKO_ROW_LENGTH;
        // The first rung is "no override", so one press right lands on the
        // shortest note rather than leaving the default in place.
        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert_eq!(s.working.cell_hold(0), rhythm::NOTE_LADDER[0]);
        assert!(s.working.has_hold_override(0));

        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert_eq!(s.working.cell_hold(0), rhythm::NOTE_LADDER[1]);
        adjust_current(&mut s, &SynthParams::defaults(), -1, &log);
        adjust_current(&mut s, &SynthParams::defaults(), -1, &log);
        assert!(!s.working.has_hold_override(0), "back to the default");
        assert_eq!(s.working.cell_hold(0), s.working.hold);

        // And it is written through, so the chord is what changes.
        assert_eq!(
            sinko_pattern(&s).expect("assigned").cell_hold(0),
            s.working.hold
        );
    }

    #[test]
    fn the_shape_rows_refuse_a_cell_with_no_hit() {
        let log = logger();
        let mut s = sinko_state();
        own(
            &mut s,
            RhythmPattern::from_step_string("L", 1.0, "x---").unwrap(),
        );
        s.sinko_cell = 2;
        s.sinko_row = SINKO_ROW_LENGTH;
        assert!(!cell_is_on(&s, 2));

        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert!(!s.working.has_hold_override(2), "nothing to lengthen");
        assert!(s.is_flashing(), "and it says so");

        s.sinko_row = SINKO_ROW_ACCENT;
        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert!(!s.working.has_velocity_override(2), "nothing to accent");
    }

    #[test]
    fn the_accent_row_edits_the_hit_under_the_cursor() {
        let log = logger();
        let mut s = sinko_state();
        own(
            &mut s,
            RhythmPattern::from_step_string("A", 1.0, "x-x-").unwrap(),
        );
        s.sinko_row = SINKO_ROW_HITS;
        s.sinko_cell = 2;

        s.sinko_row = SINKO_ROW_ACCENT;
        assert_eq!(s.working.cell_velocity(2), 1.0);
        adjust_current(&mut s, &SynthParams::defaults(), -1, &log);
        assert_eq!(s.working.cell_velocity(2), 0.9, "the top rung below full");
        assert!(s.working.has_velocity_override(2));
        for _ in 0..10 {
            adjust_current(&mut s, &SynthParams::defaults(), -1, &log);
        }
        assert_eq!(s.working.cell_velocity(2), 0.2, "clamped at the bottom");
        for _ in 0..10 {
            adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        }
        assert_eq!(s.working.cell_velocity(2), 1.0, "and at full");
        assert!(!s.working.has_velocity_override(2));
    }

    #[test]
    fn the_shape_rows_keep_the_cell_cursor_visible() {
        // `length` and `accent` edit the hit under the cursor, so the cursor has
        // to be drawn on those rows too — otherwise they are editing a cell the
        // player cannot pick out of the bar.
        let mut s = hits_state(4, "x---");
        s.sinko_cell = 2;
        for row in [SINKO_ROW_HITS, SINKO_ROW_LENGTH, SINKO_ROW_ACCENT] {
            s.sinko_row = row;
            let raw = {
                let mut out: Vec<u8> = Vec::new();
                render_sinko_panel(&mut out, &s, DEFAULT_SCREEN.grid_budget()).unwrap();
                String::from_utf8(out).unwrap()
            };
            assert!(raw.contains("\x1b[7m"), "row {} must show the cursor", row);
        }

        s.sinko_row = SINKO_ROW_HOLD;
        let raw = {
            let mut out: Vec<u8> = Vec::new();
            render_sinko_panel(&mut out, &s, DEFAULT_SCREEN.grid_budget()).unwrap();
            String::from_utf8(out).unwrap()
        };
        assert!(!raw.contains("\x1b[7m"), "no cursor on a value row");
    }

    #[test]
    fn the_shape_rows_name_the_cell_they_are_about() {
        let log = logger();
        let mut s = sinko_state();
        own(
            &mut s,
            RhythmPattern::from_step_string("A", 1.0, "x-x-").unwrap(),
        );

        s.sinko_cell = 1;
        s.sinko_row = SINKO_ROW_ACCENT;
        assert!(
            render_sinko(&s).contains("cell 2 has no hit"),
            "a rest cannot be accented:\n{}",
            render_sinko(&s)
        );

        s.sinko_cell = 2;
        assert!(
            render_sinko(&s).contains("100%  (full)"),
            "a hit starts at full level:\n{}",
            render_sinko(&s)
        );
        adjust_current(&mut s, &SynthParams::defaults(), -1, &log);
        assert!(
            render_sinko(&s).contains("90%"),
            "and the row shows the override:\n{}",
            render_sinko(&s)
        );
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
        s.rhythm_user_path = unique_export_dir("rhythm-save").join("rhythms.toml");
        s.current_take = vec![0, 480, 960];
        close_take(&mut s, &log);

        save_working_pattern(&mut s, "Tapped", &log);

        assert_eq!(assigned_in(&s).as_deref(), Some("Tapped"));
        assert!(s.rhythm_user_path.exists(), "the library must be written");

        // Read it back: the file is the durable half of the feature.
        let store = RhythmStore::load(&rhythm_store::default_path(), &s.rhythm_user_path).unwrap();
        let saved = store.find("Tapped").expect("the saved pattern");
        assert!(saved.any_hit());
        assert_eq!(saved.steps_per_bar(), rhythm::DEFAULT_STEPS);
    }

    #[test]
    fn saving_needs_a_chord_to_assign_to() {
        let log = logger();
        let mut s = sinko_state();
        s.rhythm_user_path = unique_export_dir("rhythm-orphan").join("rhythms.toml");
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
        s.rhythm_user_path = unique_export_dir("rhythm-unique").join("rhythms.toml");
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
        s.rhythm_user_path = unique_export_dir("rhythm-silent").join("rhythms.toml");

        save_working_pattern(&mut s, "Nothing", &log);
        assert_eq!(assigned_in(&s), None);
        assert!(
            !s.rhythm_user_path.exists(),
            "nothing should have been written"
        );
        assert!(s.is_flashing());
    }

    #[test]
    fn a_muted_tail_reaches_the_sound_without_saving() {
        // End to end: mute the last quarter in the panel and check the
        // arrangement that playback and export read actually drops the tail.
        let log = logger();
        let mut s = sinko_state();
        s.rhythm_user_path = unique_export_dir("rhythm-mute").join("rhythms.toml");
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
        assert_eq!(format_resolution(12), "1/8T", "eighth-note triplets");
        assert_eq!(format_resolution(16), "1/16");
        assert_eq!(format_resolution(24), "1/16T");
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
        assert_eq!(
            s.working.steps_per_bar(),
            12,
            "the triplet rung comes first"
        );
        adjust_current(&mut s, &SynthParams::defaults(), -1, &log);
        assert_eq!(
            s.working.steps_per_bar(),
            8,
            "then the straight eighth grid"
        );
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
    fn the_chord_row_steps_the_progression_selection() {
        let log = logger();
        let mut s = sinko_state();
        assign(&mut s, 2, "Offbeat Eighths");
        assign(&mut s, 3, "Quarters");
        s.sinko_row = SINKO_ROW_CHORD;
        sync_working(&mut s);
        assert_eq!(s.working.name, "Offbeat Eighths", "the selected chord");

        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert_eq!(s.progression_row, 3, "right selects the next chord");
        assert_eq!(
            s.working.name, "Quarters",
            "and the field's contents follow in the same press"
        );

        adjust_current(&mut s, &SynthParams::defaults(), -1, &log);
        assert_eq!(s.progression_row, 2, "left selects the previous chord");
        assert_eq!(s.working.name, "Offbeat Eighths");
    }

    #[test]
    fn the_chord_row_moves_the_rendered_panel_with_it() {
        let log = logger();
        let mut s = sinko_state();
        assign(&mut s, 3, "Quarters");
        s.sinko_row = SINKO_ROW_CHORD;
        assert!(render_sinko(&s).contains("#3"), "starts on row 2");

        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        let text = render_sinko(&s);
        assert!(text.contains("#4"), "the chord row renames the entry");
        assert!(
            text.contains("Quarters"),
            "and the pattern row follows it:\n{}",
            text
        );
    }

    #[test]
    fn the_chord_row_clamps_at_the_ends_of_the_progression() {
        let log = logger();
        let mut s = sinko_state();
        s.sinko_row = SINKO_ROW_CHORD;

        s.progression_row = 0;
        adjust_current(&mut s, &SynthParams::defaults(), -1, &log);
        assert_eq!(s.progression_row, 0, "left at the first entry stays put");

        let last = s.progression.lock().unwrap().len() - 1;
        s.progression_row = last;
        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert_eq!(s.progression_row, last, "right at the last stays put");
    }

    #[test]
    fn a_saved_pattern_is_what_was_tapped() {
        // End to end: tap a take, save it, and check the arrangement the
        // scheduler and the exporter read plays it back.
        let log = logger();
        let mut s = sinko_state();
        s.rhythm_user_path = unique_export_dir("rhythm-end-to-end").join("rhythms.toml");
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
        s.rhythm_user_path = unique_export_dir("rhythm-live").join("rhythms.toml");

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
            !s.rhythm_user_path.exists(),
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
        draw_key(&mut out, KeyPosition::LeftPinky, false).unwrap();
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
                Focus::SynthEnsembles,
                Focus::Eq,
                Focus::Spectrum,
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
                Focus::SynthEnsembles,
                Focus::Eq,
                Focus::Spectrum,
                Focus::Fx,
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
            Focus::SynthEnsembles,
            // Every view of the Synth slot, because they share the panel and do
            // not share its shape: the equaliser and the rack are both grids of
            // their own.
            Focus::Eq,
            Focus::Spectrum,
            Focus::Fx,
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
            Focus::SynthEnsembles,
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
    fn the_transport_rows_share_one_right_edge() {
        // The key is left and the value is right, in fixed fields: a value
        // column that followed the longest row would move every time a tempo
        // grew a digit, which is not an alignment at all.
        let s = {
            let s = state(Focus::Transport);
            s.transport.set_bpm(BPM_MAX);
            s
        };
        let text = strip_ansi(&render_transport(&s));
        let rows: Vec<&str> = text.lines().filter(|line| line.starts_with("  ")).collect();
        assert_eq!(rows.len(), TRANSPORT_ROWS, "one line per row:\n{}", text);

        // Every row is the panel's width, so the right edge is a real edge.
        for row in &rows {
            assert_eq!(
                row.chars().count(),
                TRANSPORT_PANEL_WIDTH,
                "row is not the panel's width: {:?}",
                row
            );
        }

        // The value rows all end on that edge, which is the alignment.
        for row in rows.iter().filter(|row| !row.contains('[')) {
            assert_eq!(
                row.trim_end().chars().count(),
                TRANSPORT_PANEL_WIDTH,
                "value does not reach the edge: {:?}",
                row
            );
        }

        let bpm = rows
            .iter()
            .find(|row| row.contains("bpm"))
            .expect("the bpm row");
        assert!(bpm.trim_end().ends_with("240"), "{:?}", bpm);
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
        s.set_current_row(TRANSPORT_ROW_MIDI);
        assert!(row_is_action_button(&s));
        s.set_current_row(TRANSPORT_ROW_VOLUME);
        assert!(!row_is_action_button(&s));
        s.set_current_row(TRANSPORT_ROW_METRONOME);
        assert!(!row_is_action_button(&s));
        assert_eq!(s.row_count(), TRANSPORT_ROWS);
    }

    // ---- the transport's own coarse steps and key cycling ----

    #[test]
    fn shift_arrow_moves_the_tempo_by_ten() {
        let log = logger();
        let mut s = state(Focus::Transport);
        s.set_current_row(TRANSPORT_ROW_BPM);
        s.transport.set_bpm(120);

        adjust_current_with(&mut s, &SynthParams::defaults(), 1, false, &log);
        assert_eq!(s.transport.bpm(), 121, "a plain arrow is one bpm");

        adjust_current_with(&mut s, &SynthParams::defaults(), 1, true, &log);
        assert_eq!(s.transport.bpm(), 131, "Shift is ten");

        adjust_current_with(&mut s, &SynthParams::defaults(), -1, true, &log);
        assert_eq!(s.transport.bpm(), 121, "and back");

        // Still clamped, however big the step.
        s.transport.set_bpm(BPM_MAX);
        adjust_current_with(&mut s, &SynthParams::defaults(), 1, true, &log);
        assert_eq!(s.transport.bpm(), BPM_MAX);
    }

    #[test]
    fn the_track_key_walks_every_key_and_mode() {
        // C major -> C minor -> C# major -> C# minor -> D major, which is the
        // order a player thinks in when they ask for "up a semitone".
        let log = logger();
        let mut s = state(Focus::Transport);
        s.set_current_row(TRANSPORT_ROW_KEY);
        s.transport.set_key(Key::new(60, Scale::Major));

        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert_eq!(s.transport.key(), Key::new(60, Scale::Minor));
        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert_eq!(s.transport.key(), Key::new(61, Scale::Major));
        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert_eq!(s.transport.key(), Key::new(61, Scale::Minor));
        adjust_current(&mut s, &SynthParams::defaults(), 1, &log);
        assert_eq!(s.transport.key(), Key::new(62, Scale::Major));

        adjust_current(&mut s, &SynthParams::defaults(), -1, &log);
        assert_eq!(s.transport.key(), Key::new(61, Scale::Minor), "and back");
    }

    #[test]
    fn shift_arrow_moves_the_key_six_choices() {
        let log = logger();
        let mut s = state(Focus::Transport);
        s.set_current_row(TRANSPORT_ROW_KEY);
        s.transport.set_key(Key::new(60, Scale::Major));

        // Six choices is three semitones, mode unchanged: C major -> D# major.
        adjust_current_with(&mut s, &SynthParams::defaults(), 1, true, &log);
        assert_eq!(s.transport.key(), Key::new(63, Scale::Major));

        // And it wraps rather than sticking at the top of the octave.
        s.transport.set_key(Key::new(70, Scale::Minor));
        adjust_current_with(&mut s, &SynthParams::defaults(), 1, true, &log);
        assert_eq!(
            s.transport.key(),
            Key::new(61, Scale::Minor),
            "70 + 3 = 73 -> 61"
        );
    }

    #[test]
    fn the_key_editor_walks_the_same_list() {
        // The row and the editor must agree, or Enter would move the key to
        // somewhere the row had just left.
        let log = logger();
        let mut s = state(Focus::Transport);
        s.set_current_row(TRANSPORT_ROW_KEY);
        s.transport.set_key(Key::new(60, Scale::Major));
        s.edit = Edit::TrackKey {
            initial: s.transport.key(),
            current: s.transport.key(),
        };

        handle_track_key_edit(&mut s, &key(KeyCode::Right), &log);
        assert_eq!(
            match s.edit {
                Edit::TrackKey { current, .. } => current,
                _ => panic!("still editing"),
            },
            Key::new(60, Scale::Minor)
        );

        let mut shifted = key(KeyCode::Right);
        shifted.modifiers = KeyModifiers::SHIFT;
        handle_track_key_edit(&mut s, &shifted, &log);
        assert_eq!(
            match s.edit {
                Edit::TrackKey { current, .. } => current,
                _ => panic!("still editing"),
            },
            Key::new(63, Scale::Minor),
            "six choices on from C minor"
        );
    }

    // ---- the metronome panel ----

    #[test]
    fn enter_on_the_metronome_row_opens_the_panel_and_esc_closes_it() {
        let log = logger();
        let mut s = state(Focus::Transport);
        s.set_current_row(TRANSPORT_ROW_METRONOME);
        assert!(!s.metronome_open);

        transport_action(
            &mut s,
            &SynthParams::defaults(),
            TRANSPORT_ROW_METRONOME,
            &log,
        );
        assert!(s.metronome_open, "Enter opens the panel");
        assert_eq!(s.metronome_row, METRONOME_ROW_CLICK, "on its first row");
        assert!(!s.metronome_on, "and does not toggle the click on the way");

        handle_metronome_key(&mut s, &key(KeyCode::Esc), &log);
        assert!(!s.metronome_open, "Esc closes it");
    }

    #[test]
    fn the_metronome_panel_walks_its_rows() {
        let log = logger();
        let mut s = state(Focus::Transport);
        s.metronome_open = true;

        handle_metronome_key(&mut s, &key(KeyCode::Down), &log);
        assert_eq!(s.metronome_row, METRONOME_ROW_SOUND);
        for _ in 0..10 {
            handle_metronome_key(&mut s, &key(KeyCode::Down), &log);
        }
        assert_eq!(s.metronome_row, METRONOME_ROWS - 1, "clamped at the bottom");
        for _ in 0..10 {
            handle_metronome_key(&mut s, &key(KeyCode::Up), &log);
        }
        assert_eq!(s.metronome_row, 0, "and at the top");
    }

    #[test]
    fn the_metronome_panel_sets_sound_volume_subdivision_and_swing() {
        let log = logger();
        let mut s = state(Focus::Transport);
        s.metronome_open = true;
        let sounds = crate::synth::CLICK_SOUNDS.len();

        s.metronome_row = METRONOME_ROW_SOUND;
        let first = s.transport.metronome_sound.load(Ordering::Relaxed);
        handle_metronome_key(&mut s, &key(KeyCode::Right), &log);
        assert_eq!(
            s.transport.metronome_sound.load(Ordering::Relaxed),
            (first + 1) % sounds
        );
        handle_metronome_key(&mut s, &key(KeyCode::Left), &log);
        assert_eq!(s.transport.metronome_sound.load(Ordering::Relaxed), first);

        s.metronome_row = METRONOME_ROW_VOLUME;
        let volume = s.transport.metronome_volume();
        handle_metronome_key(&mut s, &key(KeyCode::Right), &log);
        assert!(s.transport.metronome_volume() > volume, "louder");
        for _ in 0..40 {
            handle_metronome_key(&mut s, &key(KeyCode::Right), &log);
        }
        assert_eq!(s.transport.metronome_volume(), 1.0, "clamped");

        s.metronome_row = METRONOME_ROW_SUBDIVISION;
        assert_eq!(s.transport.metronome_subdivision(), 1);
        handle_metronome_key(&mut s, &key(KeyCode::Right), &log);
        assert_eq!(s.transport.metronome_subdivision(), 2, "the \"&\"");
        handle_metronome_key(&mut s, &key(KeyCode::Right), &log);
        assert_eq!(s.transport.metronome_subdivision(), 4, "the sixteenths");
        handle_metronome_key(&mut s, &key(KeyCode::Right), &log);
        assert_eq!(s.transport.metronome_subdivision(), 1, "and around");

        s.metronome_row = METRONOME_ROW_SWING;
        assert_eq!(s.transport.swing(), 0.0);
        handle_metronome_key(&mut s, &key(KeyCode::Right), &log);
        assert_eq!(s.transport.swing(), 0.05);
        for _ in 0..40 {
            handle_metronome_key(&mut s, &key(KeyCode::Left), &log);
        }
        assert_eq!(s.transport.swing(), 0.0, "straight is the floor");
    }

    #[test]
    fn the_metronome_panel_switches_the_click_with_enter_and_the_arrows() {
        let log = logger();
        let mut s = state(Focus::Transport);
        s.metronome_open = true;
        s.metronome_row = METRONOME_ROW_CLICK;

        handle_metronome_key(&mut s, &key(KeyCode::Enter), &log);
        assert!(s.metronome_on);
        assert!(s.transport.metronome.load(Ordering::Relaxed));

        handle_metronome_key(&mut s, &key(KeyCode::Left), &log);
        assert!(!s.metronome_on, "the arrows toggle it too");
    }

    #[test]
    fn the_metronome_panel_says_what_it_holds() {
        let log = logger();
        let mut s = state(Focus::Transport);
        s.metronome_open = true;
        let text = render_metronome(&s);
        assert!(text.contains("Metronome"), "{}", text);
        assert!(text.contains("click"), "{}", text);
        assert!(text.contains("Blip"), "the first sound preset: {}", text);
        assert!(text.contains("subdivision"), "{}", text);
        assert!(text.contains("swing"), "{}", text);
        assert!(text.contains("[Esc] close"), "{}", text);

        // The swing row reports the transport's own amount, which is the groove
        // every pattern without an override follows.
        s.transport.set_swing(1.0);
        let text = render_metronome(&s);
        assert!(text.contains("100%"), "{}", text);
        assert!(text.contains("triplet"), "{}", text);

        // The panel shares the transport's shape, so opening it does not jump
        // the column's width or misalign its own rows.
        for row in text.lines().filter(|line| line.starts_with("  ")) {
            assert!(
                row.chars().count() <= TRANSPORT_PANEL_WIDTH,
                "row overflows the panel ({}) : {:?}",
                row.chars().count(),
                row
            );
        }
        let _ = log;
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
        render_transport_panel(&mut out, &SynthParams::defaults(), state).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn render_metronome(state: &AppState) -> String {
        let mut out: Vec<u8> = Vec::new();
        render_metronome_panel(&mut out, state).unwrap();
        strip_ansi(&String::from_utf8(out).unwrap())
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
            // Anything but `[` after the introducer is not a CSI (`ESC c`, say):
            // the introducer is all there was, and the character is dropped with
            // it, which is what the old `_ => {}` arm did.
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

    /// The whole frame with the parameters the caller owns, so a test can assert
    /// on what a keystroke wrote rather than only on what was drawn.
    fn render_frame_with(params: &SynthParams, state: &AppState) -> String {
        let mut out: Vec<u8> = Vec::new();
        render(&mut out, params, state, DEFAULT_SCREEN).unwrap();
        strip_ansi(&String::from_utf8(out).unwrap())
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

    /// One arrow key through the real handler, so a test exercises the same
    /// routing a player gets.
    fn state_arrow(s: &mut AppState, log: &Logger, code: KeyCode, shift: bool) {
        let mut ev = key(code);
        if shift {
            ev.modifiers = KeyModifiers::SHIFT;
        }
        handle_panel_arrow(s, &SynthParams::defaults(), &ev, log);
    }

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
    fn zz_pages() {
        let p = SynthParams::defaults();
        for page in SynthPage::ALL {
            let mut s = state(Focus::Synth);
            s.synth_page = page;
            println!("--- {} ({} rows)", page.name(), page.rows().len());
            for line in render_synth_with(&p, &s).lines().take(page.rows().len() + 2) {
                println!("|{}", line.trim_end());
            }
        }
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
            34,
            "the aux units' settings ride on every Synth page, so this view is \
             now nearly as tall as the Sinko panel: re-check the README budget"
        );
        // Padded pages all render at the same height, so measuring one is
        // measuring all of them; `every_page_is_the_same_height_as_every_other` proves it.
        assert_eq!(
            SynthPage::ALL
                .iter()
                .map(|page| synth_row_count(*page))
                .max()
                .unwrap(),
            SYNTH_ROWS,
            "the tallest page is what the layout is budgeted for"
        );
        assert_eq!(
            ui_height(Focus::Sinko, &chords),
            35,
            "the Sinko panel is the tallest view"
        );
        assert_eq!(
            ui_height(Focus::Transport, &chords),
            15,
            "the default view should stay comfortably short"
        );
        // The equaliser costs a row per band, so it is the one view that could
        // quietly outgrow the rest. It borrows the Synth panel's slot, so it
        // costs the other views nothing — and it stays under the ceiling the
        // README advertises.
        // The FX panel is the shortest view of the Synth slot: eleven rows, which
        // is what a fixed row list buys — the parameters are reached through a
        // selector rather than as six rows of their own. It is drawn last, so a
        // shorter panel here costs the layout nothing.
        assert_eq!(
            ui_height(Focus::Fx, &chords),
            25,
            "the rack panel: eleven rows and the fourteen the frame costs"
        );
        assert_eq!(
            ui_height(Focus::Eq, &chords),
            34,
            "the EQ panel must stay under the tallest always-drawn view"
        );
        assert!(
            ui_height(Focus::Eq, &chords) <= DEFAULT_SCREEN.height,
            "a view taller than the advertised height has to change the advert"
        );
        // The spectrum is the fourth view of that slot, and the shortest of the
        // four: twelve rows of chart, five of settings.
        assert_eq!(
            ui_height(Focus::Spectrum, &chords),
            33,
            "the Spectrum panel"
        );
        assert!(ui_height(Focus::Spectrum, &chords) <= DEFAULT_SCREEN.height);
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

    /// The rhythm on a specific row, whoever is selected.
    fn sinko_pattern_at(s: &AppState, row: usize) -> Option<RhythmPattern> {
        match s.progression.lock().unwrap().slots.get(row) {
            Some(Slot::Chord(entry)) => entry.pattern.clone(),
            _ => None,
        }
    }

    /// The offset on a specific row.
    fn offset_at(s: &AppState, row: usize) -> i32 {
        match s.progression.lock().unwrap().slots.get(row) {
            Some(Slot::Chord(entry)) => entry.offset_ticks,
            _ => panic!("expected a chord at {}", row),
        }
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
        assert_eq!(
            Some(pasted.clone()),
            s.sinko_clipboard
                .clone()
                .unwrap()
                .into_iter()
                .next()
                .flatten(),
            "the only copied rhythm is the one that landed"
        );
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
            s.sinko_clipboard.as_ref().unwrap().len(),
            1,
            "one chord was copied, so one rhythm"
        );
        assert_eq!(
            s.sinko_clipboard
                .as_ref()
                .unwrap()
                .first()
                .and_then(|p| p.as_ref())
                .map(|p| p.name.as_str()),
            Some("Quarters"),
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
        assert!(text.contains("copied 1 rhythm"), "rendered:\n{}", text);

        s.progression_row = 3;
        handle_hotkey(&mut s, Hotkey::PasteChord, true, &log);
        let text = render_frame(&s);
        assert!(text.contains("pasted 1 rhythm"), "rendered:\n{}", text);
    }

    #[test]
    fn the_rhythm_clipboard_hotkeys_are_scoped_to_the_two_chord_lists() {
        let log = logger();
        let mut s = sinko_state();
        assign(&mut s, 2, "Quarters");
        handle_hotkey(&mut s, Hotkey::CopyChord, true, &log);
        assert!(s.sinko_clipboard.is_some());

        // A panel with no chord cursor cannot tell which chord is meant.
        for focus in [Focus::Transport, Focus::Synth, Focus::SynthEnsembles] {
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
        s.rhythm_user_path = unique_export_dir("sinko-new").join("rhythms.toml");
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
        assert!(
            !s.rhythm_user_path.exists(),
            "and nothing was written to disk"
        );
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

    /// Whether some line of `text` is the row for `label`.
    ///
    /// Starting-with rather than containing, because the labels contain each
    /// other: `attack` is a suffix of `filter attack`, so a plain substring
    /// search would report the filter page as drawing the amp attack row that
    /// lives on the envelope page.
    fn draws_row(text: &str, label: &str) -> bool {
        text.lines()
            .any(|line| line.trim_start_matches([' ', '▸']).starts_with(label))
    }

    #[test]
    fn the_synth_table_shows_every_setting_somewhere() {
        // This panel outgrew one screen, so the invariant is no longer "each
        // setting is visible at once" but "each setting is visible somewhere,
        // and the mixer is visible always". A setting that is addressable but
        // drawn on no page would leave a reachable row that renders nothing.
        let mut everything = String::new();
        for page in SynthPage::ALL {
            let mut s = state(Focus::Synth);
            s.synth_page = page;
            let text = render_synth(&s);
            for param in page.rows() {
                assert!(
                    draws_row(&text, param.label()),
                    "the {} page does not draw {:?}",
                    page.name(),
                    param.label()
                );
            }
            // The master block is pinned on every page, which is what keeps the
            // reverb and the master volume reachable from any of them. A plain
            // search rather than `draws_row`, because half of these labels sit
            // in the middle of a two-cell line.
            for param in MIXER_PARAMS {
                assert!(
                    text.contains(param.label()),
                    "the {} page is missing the master row {:?}",
                    page.name(),
                    param.label()
                );
            }
            // Every page, not just the one the width test happens to render:
            // the `osc` page carries the longest labels in the table and it is
            // not the default page, so nothing else would look at it.
            for line in text.lines() {
                assert!(
                    line.chars().count() <= 80,
                    "the {} page wraps at {} columns: {:?}",
                    page.name(),
                    line.chars().count(),
                    line
                );
            }
            everything.push_str(&text);
        }
        for param in CHANNEL_PARAMS {
            assert!(
                draws_row(&everything, param.label()),
                "no page draws {:?}",
                param.label()
            );
        }
    }

    #[test]
    fn the_old_shape_of_this_test_is_gone() {
        let text = render_synth(&state(Focus::Synth));
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
            .flat_map(|row| row.iter().copied())
            .collect();
        assert_eq!(seen.len(), MIXER_PARAMS.len());
        seen.sort_by_key(|p| p.label());
        let mut expected = MIXER_PARAMS.to_vec();
        expected.sort_by_key(|p| p.label());
        assert_eq!(seen, expected);
    }

    #[test]
    fn every_waveform_name_fits_the_panel_column_even_when_selected() {
        // Selection wraps a value in brackets, so the usable width is two less
        // than the column — and the selected cell is the one being changed, so
        // clipping *it* is the worst case rather than an edge case.
        let longest = Waveform::ALL
            .iter()
            .map(|w| w.name().chars().count())
            .max()
            .unwrap();
        assert!(
            SYNTH_VALUE_WIDTH >= longest + 2,
            "the widest waveform name is {} characters, the column is {}",
            longest,
            SYNTH_VALUE_WIDTH
        );
        for waveform in Waveform::ALL {
            for selected in [false, true] {
                let field = synth_field(waveform.name(), SYNTH_VALUE_WIDTH, selected);
                assert_eq!(field.chars().count(), SYNTH_VALUE_WIDTH);
                assert!(
                    field.contains(waveform.name()),
                    "{:?} renders as {:?}{}",
                    waveform,
                    field,
                    if selected { " while selected" } else { "" }
                );
            }
        }
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
        aim(&mut s, ChannelParam::Volume);
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
        let (row, _) = master_row_of(MixerParam::ReverbSize);
        s.synth_row = channel_row_start() + SynthPage::Tone.rows().len() + row;
        s.synth_col = 1;
        let p = SynthParams::defaults();
        p.aux_reverb.set_param(P0, 0.80); // size
        p.aux_reverb.set_param(P1, 0.10); // damp

        let row = render_synth_with(&p, &s)
            .lines()
            .find(|l| l.contains("size"))
            .expect("reverb size row")
            .to_string();
        assert!(
            row.contains("[10%]"),
            "the second cell must be selected: {row:?}"
        );
        assert!(
            row.contains("80%"),
            "the first cell must still show: {row:?}"
        );
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
        s.set_current_row(channel_row_start() + SynthPage::Tone.rows().len());
        assert_eq!(s.synth_col, 1, "master rows hold two cells");
    }

    #[test]
    fn shift_selects_the_nudge_and_plain_arrows_select_the_column() {
        assert_eq!(
            panel_arrow(KeyCode::Left, false),
            Some(PanelArrow::Column(-1))
        );
        assert_eq!(
            panel_arrow(KeyCode::Right, false),
            Some(PanelArrow::Column(1))
        );
        assert_eq!(
            panel_arrow(KeyCode::Left, true),
            Some(PanelArrow::Nudge(-1))
        );
        assert_eq!(
            panel_arrow(KeyCode::Right, true),
            Some(PanelArrow::Nudge(1))
        );
        assert_eq!(panel_arrow(KeyCode::Up, false), None);
        assert_eq!(panel_arrow(KeyCode::Enter, false), None);
    }

    #[test]
    fn one_tab_reaches_the_synth_and_one_tab_leaves_it() {
        assert_eq!(Focus::Sinko.next(), Focus::Synth);
        assert_eq!(Focus::Synth.next(), Focus::SynthEnsembles);
        assert_eq!(Focus::Synth.prev(), Focus::Sinko);
    }

    // ---- the FX panel ----

    /// A state on the FX panel, on one register's rack.
    fn fx_state(col: usize, slot: usize) -> AppState {
        let mut s = state(Focus::Fx);
        s.fx_col = col;
        s.fx_slot = slot;
        s.fx_row = FX_ROW_TYPE;
        s
    }

    /// One arrow press through the panel's own handler, so a test exercises the
    /// same routing a player gets.
    fn fx_arrow(s: &mut AppState, p: &SynthParams, code: KeyCode, shift: bool, log: &Logger) {
        let mut ev = key(code);
        if shift {
            ev.modifiers = KeyModifiers::SHIFT;
        }
        handle_panel_arrow(s, p, &ev, log);
    }

    /// A rack with something in every slot, so a move has something to displace.
    fn loaded_rack(p: &SynthParams, col: usize) {
        let chain = &channel_at(p, col).chain;
        for (index, kind) in [
            FxKind::Distortion,
            FxKind::Chorus,
            FxKind::Delay,
            FxKind::Phaser,
            FxKind::Tremolo,
            FxKind::Reverb,
        ]
        .into_iter()
        .enumerate()
        {
            chain.slots[index].set_kind(kind);
        }
    }

    #[test]
    fn the_fx_cursor_walks_every_row_of_the_panel() {
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = fx_state(0, 0);
        s.fx_row = 0;
        assert_eq!(s.row_count(), FX_ROWS);
        // Down walks the rows in order, as it does on every other panel.
        for expected in 0..FX_ROWS {
            assert_eq!(s.current_row(), expected);
            fx_arrow(&mut s, &p, KeyCode::Down, false, &log);
        }
        // And stops at the last one rather than wrapping: a row cursor that
        // wrapped would turn "one more press" into "back to the top".
        assert_eq!(s.current_row(), FX_ROWS - 1);
        for _ in 0..FX_ROWS {
            fx_arrow(&mut s, &p, KeyCode::Up, false, &log);
        }
        assert_eq!(s.current_row(), 0);
    }

    #[test]
    fn every_fx_row_is_either_a_value_or_a_button() {
        // The panel is eleven rows: seven that an arrow moves and four that
        // `Enter` runs. Asserting the split both ways is what stops a row being
        // added to neither half — a row the cursor can reach and nothing can
        // change.
        let buttons: Vec<usize> = (0..FX_ROWS).filter(|r| fx_row_is_button(*r)).collect();
        assert_eq!(
            buttons,
            vec![FX_ROW_EARLIER, FX_ROW_LATER, FX_ROW_CLEAR, FX_ROW_SAVE]
        );
        // `Enter` on a button does something, and on a value row it does not.
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = fx_state(0, 0);
        channel_at(&p, 0).chain.slots[0].set_kind(FxKind::Chorus);
        let value_row = FX_ROW_TYPE;
        let before = channel_at(&p, 0).chain.slots[0].fx();
        fx_action(&mut s, &p, value_row, &log);
        assert_eq!(
            channel_at(&p, 0).chain.slots[0].fx(),
            before,
            "a value row is the arrows' business"
        );
    }

    #[test]
    fn the_type_row_swaps_the_whole_family_defaults_and_all() {
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = fx_state(1, 0);
        s.fx_row = FX_ROW_TYPE;
        assert!(channel_at(&p, 1).chain.slots[0].fx().is_none());

        fx_arrow(&mut s, &p, KeyCode::Right, false, &log);
        let first = channel_at(&p, 1).chain.slots[0].fx();
        assert_eq!(first.kind, FxKind::Reverb, "none is first, reverb second");
        assert_eq!(
            first.params,
            FxKind::Reverb.defaults(first.subtype),
            "the new family's own defaults came with it"
        );
        // One more press is the next family, not a variant of this one.
        fx_arrow(&mut s, &p, KeyCode::Right, false, &log);
        assert_eq!(channel_at(&p, 1).chain.slots[0].fx().kind, FxKind::Delay);
        // And back past the first lands on the last: delay, reverb, none, gate.
        for _ in 0..3 {
            fx_arrow(&mut s, &p, KeyCode::Left, false, &log);
        }
        assert_eq!(channel_at(&p, 1).chain.slots[0].fx().kind, FxKind::Gate);
    }

    #[test]
    fn the_rack_and_slot_rows_choose_which_slot_is_on_screen() {
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = fx_state(0, 0);
        s.fx_row = FX_ROW_RACK;
        fx_arrow(&mut s, &p, KeyCode::Right, false, &log);
        assert_eq!(s.fx_col, 1);
        // The rack wraps rather than sticking at the last register.
        for _ in 0..CHANNEL_COUNT {
            fx_arrow(&mut s, &p, KeyCode::Right, false, &log);
        }
        assert_eq!(s.fx_col, 1);

        s.fx_row = FX_ROW_SLOT;
        fx_arrow(&mut s, &p, KeyCode::Left, false, &log);
        assert_eq!(s.fx_slot, CHAIN_SLOTS - 1, "the rack wraps at the front");
        // Choosing a slot resets the parameter cursor, which belonged to the
        // slot that was on screen.
        s.fx_param = 3;
        fx_arrow(&mut s, &p, KeyCode::Right, false, &log);
        assert_eq!(s.fx_slot, 0);
        assert_eq!(s.fx_param, 0);
    }

    #[test]
    fn the_param_and_value_rows_move_one_parameter_of_the_slot() {
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = fx_state(0, 0);
        channel_at(&p, 0).chain.slots[0].set_kind(FxKind::Distortion);

        // `param` chooses; `value` moves. Neither does the other's job.
        s.fx_row = FX_ROW_PARAM;
        let drive = channel_at(&p, 0).chain.slots[0].fx().param(P0);
        fx_arrow(&mut s, &p, KeyCode::Right, false, &log);
        assert_eq!(s.fx_param, 1, "the selector moved");
        assert_eq!(
            channel_at(&p, 0).chain.slots[0].fx().param(P0),
            drive,
            "and the value did not"
        );

        s.fx_row = FX_ROW_VALUE;
        let tone = channel_at(&p, 0).chain.slots[0].fx().param(P1);
        fx_arrow(&mut s, &p, KeyCode::Right, false, &log);
        assert!(
            channel_at(&p, 0).chain.slots[0].fx().param(P1) > tone,
            "right must raise the parameter the selector is on"
        );
        assert_eq!(
            channel_at(&p, 0).chain.slots[0].fx().param(P0),
            drive,
            "and leave the other one alone"
        );
    }

    #[test]
    fn the_parameter_cursor_is_clamped_to_the_kind_on_screen() {
        // The selector outlives the kind: a slot on its fourth parameter, set to
        // a kind with two of them, must not index past the end of the table.
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = fx_state(0, 0);
        s.fx_param = 5;
        let slot = &channel_at(&p, 0).chain.slots[0];
        // Tremolo declares three, so the panel's cursor reads the third.
        slot.set_kind(FxKind::Tremolo);
        assert_eq!(slot.fx().kind.params().len(), 3);
        s.fx_row = FX_ROW_VALUE;
        // The third is the mix, which ships at its ceiling — so the arrow that
        // proves the row is addressing it is the one that lowers it.
        let before = slot.fx().param(2);
        fx_arrow(&mut s, &p, KeyCode::Left, false, &log);
        assert!(
            slot.fx().param(2) < before,
            "the last parameter is the one addressed"
        );
        assert_eq!(s.fx_param, 5, "the field itself is left alone");
        // A kind with no parameters at all has nothing for either row to do.
        slot.set_kind(FxKind::None);
        fx_arrow(&mut s, &p, KeyCode::Right, false, &log);
        assert!(slot.fx().is_none(), "and nothing was invented");
    }

    #[test]
    fn the_subtype_and_preset_rows_walk_the_library_for_this_slot() {
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = fx_state(2, 1);
        let slot = &channel_at(&p, 2).chain.slots[1];
        slot.set_kind(FxKind::Reverb);

        s.fx_row = FX_ROW_SUBTYPE;
        fx_arrow(&mut s, &p, KeyCode::Right, false, &log);
        assert_eq!(slot.fx().subtype, crate::fx::FxSubtype::Room);
        assert_ne!(
            slot.fx().params,
            FxKind::Reverb.defaults(crate::fx::FxSubtype::Hall),
            "the variant's own numbers came with it"
        );

        // The preset row walks this kind and variant only, and a chain slot uses
        // all of its parameters — `mix` included, unlike an aux unit.
        s.fx_row = FX_ROW_PRESET;
        let at = state_preset_name(&s, &p);
        fx_arrow(&mut s, &p, KeyCode::Right, false, &log);
        assert_ne!(state_preset_name(&s, &p), at, "the walk moved");
        let named = s.fx_presets.matching(&slot.fx());
        assert!(
            named
                .iter()
                .any(|preset| preset.name == state_preset_name(&s, &p)),
            "and landed on a preset of this kind and variant"
        );
    }

    /// The preset name the FX panel would draw for the slot under its cursor.
    fn state_preset_name(s: &AppState, p: &SynthParams) -> String {
        s.fx_presets
            .name_for(&fx_slot_at(s, p).fx())
            .unwrap_or("custom")
            .to_string()
    }

    #[test]
    fn moving_a_slot_swaps_the_two_effects_and_the_cursor_follows_the_effect() {
        let log = logger();
        let p = SynthParams::defaults();
        loaded_rack(&p, 0);
        let mut s = fx_state(0, 1);
        let moving = channel_at(&p, 0).chain.slots[1].fx();
        let displaced = channel_at(&p, 0).chain.slots[2].fx();

        s.fx_row = FX_ROW_LATER;
        fx_action(&mut s, &p, FX_ROW_LATER, &log);
        assert_eq!(channel_at(&p, 0).chain.slots[2].fx(), moving);
        assert_eq!(channel_at(&p, 0).chain.slots[1].fx(), displaced);
        assert_eq!(s.fx_slot, 2, "the cursor stayed on the effect it moved");
        // Nothing else in the rack was touched.
        assert_eq!(channel_at(&p, 0).chain.slots[3].fx().kind, FxKind::Phaser);

        fx_action(&mut s, &p, FX_ROW_EARLIER, &log);
        assert_eq!(channel_at(&p, 0).chain.slots[1].fx(), moving);
        assert_eq!(s.fx_slot, 1);
    }

    #[test]
    fn moving_past_either_end_is_refused_rather_than_wrapping() {
        // A rack is an order, not a wheel: wrapping would silently move the last
        // effect in front of the first, which is a different edit entirely.
        let log = logger();
        let p = SynthParams::defaults();
        loaded_rack(&p, 1);
        let mut s = fx_state(1, 0);
        let before: Vec<_> = (0..CHAIN_SLOTS)
            .map(|i| channel_at(&p, 1).chain.slots[i].fx())
            .collect();
        fx_action(&mut s, &p, FX_ROW_EARLIER, &log);
        assert_eq!(s.fx_slot, 0);
        assert_eq!(
            (0..CHAIN_SLOTS)
                .map(|i| channel_at(&p, 1).chain.slots[i].fx())
                .collect::<Vec<_>>(),
            before,
            "the rack is where it was"
        );
        assert!(s.is_flashing(), "and the refusal was reported");

        s.fx_slot = CHAIN_SLOTS - 1;
        fx_action(&mut s, &p, FX_ROW_LATER, &log);
        assert_eq!(s.fx_slot, CHAIN_SLOTS - 1);
    }

    #[test]
    fn emptying_a_slot_leaves_none_and_touches_nothing_else() {
        let log = logger();
        let p = SynthParams::defaults();
        loaded_rack(&p, 2);
        let mut s = fx_state(2, 3);
        let neighbour = channel_at(&p, 2).chain.slots[4].fx();
        fx_action(&mut s, &p, FX_ROW_CLEAR, &log);
        assert!(channel_at(&p, 2).chain.slots[3].fx().is_none());
        assert_eq!(channel_at(&p, 2).chain.slots[4].fx(), neighbour);
        // The rack still knows it has something in it.
        assert!(channel_at(&p, 2).chain.any());
    }

    #[test]
    fn an_arrow_on_a_slot_row_swaps_the_family_in_place() {
        // The table's `fx` page auditions kinds without leaving the table: the
        // whole family arrives at once, defaults and all, so the row is a way to
        // find out whether a rack wants a phaser at all.
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        s.synth_page = SynthPage::Fx;
        s.synth_row = channel_row_start() + 2; // slot 1
        s.synth_col = 1;
        let slot = &channel_at(&p, 1).chain.slots[0];
        slot.set_kind(FxKind::Fuzz);
        assert_eq!(slot.fx().kind, FxKind::Fuzz);

        fx_arrow(&mut s, &p, KeyCode::Right, true, &log);
        let now = channel_at(&p, 1).chain.slots[0].fx();
        assert_eq!(now.kind, FxKind::Bitcrusher, "the next family in the list");
        assert_eq!(
            now.params,
            FxKind::Bitcrusher.defaults(now.subtype),
            "and it arrived with its own defaults"
        );
        assert!(
            matches!(s.edit, Edit::None),
            "an arrow nudge never opens an edit"
        );
    }

    #[test]
    fn enter_on_a_slot_row_opens_the_panel_that_edits_it() {
        let log = logger();
        let mut s = state(Focus::Synth);
        s.synth_page = SynthPage::Fx;
        s.synth_row = channel_row_start() + 4; // slot 3
        s.synth_col = 2;
        let cell = s.synth_cell();
        assert_eq!(cell.fx_slot_index(), Some(2));

        open_fx_panel(&mut s, 2, &log);
        assert_eq!(s.focus, Focus::Fx);
        assert_eq!((s.fx_col, s.fx_slot), (2, 2));
        assert_eq!(s.fx_row, FX_ROW_TYPE, "landing where the edit happens");
    }

    #[test]
    fn saving_a_slot_files_it_under_its_name() {
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = fx_state(0, 0);
        let slot = &channel_at(&p, 0).chain.slots[0];
        slot.set_kind(FxKind::Phaser);
        slot.set_subtype_index(1);
        slot.set_param(P1, 0.9);
        let saved = slot.fx();

        let path = std::env::temp_dir().join("chord-tool-fx-preset-save.toml");
        let _ = std::fs::remove_file(&path);
        save_fx_preset(&mut s, &p, "Test Vibe", &path, &log);
        let stored = FxPresetStore::load(&path).unwrap();
        assert_eq!(
            stored.find("Test Vibe").map(|preset| preset.fx),
            Some(saved)
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_empty_slot_has_nothing_to_save_and_files_nothing() {
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = fx_state(0, 0);
        let before = s.fx_presets.presets.len();
        let path = std::env::temp_dir().join("chord-tool-fx-preset-empty.toml");
        let _ = std::fs::remove_file(&path);
        save_fx_preset(&mut s, &p, "Nothing", &path, &log);
        assert_eq!(s.fx_presets.presets.len(), before, "nothing was filed");
        assert!(!path.exists(), "and nothing was written");
    }

    #[test]
    fn the_fx_panel_draws_every_row_and_names_the_slot_it_is_on() {
        let p = SynthParams::defaults();
        let s = fx_state(1, 2);
        channel_at(&p, 1).chain.slots[2].set_kind(FxKind::Bitcrusher);
        let mut out: Vec<u8> = Vec::new();
        render_fx_body(&mut out, &p, &s).unwrap();
        let text = strip_ansi(&String::from_utf8(out).unwrap());
        for label in [
            "rack", "slot", "type", "subtype", "preset", "param", "value",
        ] {
            assert!(
                draws_row(&text, label),
                "the {label} row is missing: {text:?}"
            );
        }
        assert!(text.contains("bitcrusher"), "the kind is on screen");
        assert!(text.contains("mid"), "and so is the rack");
        assert!(text.contains("3 of 6"), "and the slot's place in it");
        for button in [
            "[Move Earlier]",
            "[Move Later]",
            "[Empty This Slot]",
            "[Save Effect As...]",
        ] {
            assert!(text.contains(button), "{button} is missing");
        }
    }

    #[test]
    fn the_fx_title_names_the_rack_the_slot_and_the_effect() {
        // The header is the only place the panel says where in the rig you are:
        // the rows say *what* is loaded, and the title says which of the
        // eighteen places it is.
        let p = SynthParams::defaults();
        let s = fx_state(2, 3);
        channel_at(&p, 2).chain.slots[3].set_kind(FxKind::Bitcrusher);
        let mut out: Vec<u8> = Vec::new();
        render_synth_panel(&mut out, &p, &s).unwrap();
        let text = strip_ansi(&String::from_utf8(out).unwrap());
        assert!(
            text.contains("FX [high 4/6"),
            "the place in the rig: {text:?}"
        );
        assert!(
            text.contains("bitcrusher"),
            "and the effect, whole: {text:?}"
        );
    }

    #[test]
    fn an_empty_slot_reads_as_an_empty_slot() {
        // `none` and `custom` are words that mean "nothing here" dressed up as
        // values. An empty slot is a row of dashes.
        let p = SynthParams::defaults();
        let s = fx_state(0, 0);
        let mut out: Vec<u8> = Vec::new();
        render_fx_body(&mut out, &p, &s).unwrap();
        let text = strip_ansi(&String::from_utf8(out).unwrap());
        assert!(text.contains("none"), "the type row says what it is");
        assert!(
            !text.contains("custom"),
            "and no row pretends to be a preset: {text:?}"
        );
        assert_eq!(
            text.lines().filter(|line| line.contains('—')).count(),
            4,
            "subtype, preset, param and value are all dashes: {text:?}"
        );
    }

    #[test]
    fn the_focus_cycle_is_the_layout_order() {
        // Left to right along the shared row, then down the stack. `Tab` and
        // `Shift+Tab` are inverses of each other, and the cycle closes.
        assert_eq!(Focus::Progression.next(), Focus::Transport);
        assert_eq!(Focus::Transport.next(), Focus::Sinko);
        assert_eq!(Focus::Sinko.next(), Focus::Synth);
        assert_eq!(Focus::Synth.next(), Focus::SynthEnsembles);
        assert_eq!(Focus::SynthEnsembles.next(), Focus::Eq);
        assert_eq!(Focus::Eq.next(), Focus::Spectrum);
        assert_eq!(Focus::Spectrum.next(), Focus::Fx);
        assert_eq!(Focus::Fx.next(), Focus::Progression);

        for focus in [
            Focus::Progression,
            Focus::Transport,
            Focus::Sinko,
            Focus::Synth,
            Focus::SynthEnsembles,
            Focus::Eq,
            Focus::Spectrum,
            Focus::Fx,
        ] {
            assert_eq!(focus.next().prev(), focus, "{:?}", focus);
            assert_eq!(focus.prev().next(), focus, "{:?}", focus);
        }
    }

    #[test]
    fn every_panel_is_reachable_by_tabbing() {
        let mut seen = Vec::new();
        let mut focus = Focus::Transport;
        for _ in 0..10 {
            if seen.contains(&focus) {
                break;
            }
            seen.push(focus);
            focus = focus.next();
        }
        assert_eq!(seen.len(), 8, "panels: {:?}", seen);
        assert_eq!(focus, Focus::Transport, "the cycle must close");
        assert_eq!(Focus::Transport.prev(), Focus::Progression);
    }

    /// Every channel setting, on exactly one page.
    ///
    /// The tests' own expectation list, kept here rather than beside the panel
    /// so that it is an independent statement of what the panel is supposed to
    /// show: a parameter can never be added to `ChannelParams` and forgotten by
    /// every page, which with two dozen of them is the mistake that would be
    /// easiest to make and hardest to notice.
    const CHANNEL_PARAMS: [ChannelParam; 48] = [
        ChannelParam::Volume,
        ChannelParam::Waveform,
        ChannelParam::PulseWidth,
        ChannelParam::NoiseLevel,
        ChannelParam::Attack,
        ChannelParam::Decay,
        ChannelParam::Sustain,
        ChannelParam::Release,
        ChannelParam::EnvCurve,
        ChannelParam::Glide,
        ChannelParam::Cutoff,
        ChannelParam::Resonance,
        ChannelParam::FilterType,
        ChannelParam::FilterEnv,
        ChannelParam::FilterAttack,
        ChannelParam::FilterDecay,
        ChannelParam::KeyTrack,
        ChannelParam::LfoPitch,
        ChannelParam::LfoCutoff,
        ChannelParam::LfoAmp,
        ChannelParam::LfoPwm,
        ChannelParam::Unison,
        ChannelParam::Detune,
        ChannelParam::Drive,
        ChannelParam::VelCutoff,
        ChannelParam::VelPwm,
        ChannelParam::Position,
        ChannelParam::PhaseDist,
        ChannelParam::Osc2Waveform,
        ChannelParam::Osc2Interval,
        ChannelParam::Osc2Level,
        ChannelParam::Osc2Fm,
        ChannelParam::FmMode,
        ChannelParam::Feedback,
        ChannelParam::Osc2Ring,
        ChannelParam::PluckDecay,
        ChannelParam::PluckDamp,
        ChannelParam::PluckBurst,
        ChannelParam::Transpose,
        ChannelParam::ReverbSend,
        ChannelParam::DelaySend,
        ChannelParam::FxSlot(0),
        ChannelParam::FxSlot(1),
        ChannelParam::FxSlot(2),
        ChannelParam::FxSlot(3),
        ChannelParam::FxSlot(4),
        ChannelParam::FxSlot(5),
        ChannelParam::Pan,
    ];

    #[test]
    fn every_synth_cell_addresses_a_labelled_parameter() {
        for page in SynthPage::ALL {
            for row in 0..synth_row_count(page) {
                for col in 0..synth_col_count(page, row) {
                    let cell = synth_cell(page, row, col);
                    assert!(!cell.label().is_empty(), "cell {cell:?} has no label");
                }
            }
        }
        assert_eq!(
            CHANNEL_PARAMS.len(),
            SynthPage::ALL.iter().map(|p| p.rows().len()).sum::<usize>(),
            "the palette and the pages must describe the same rows"
        );
        assert!(
            SynthPage::ALL
                .iter()
                .all(|p| synth_row_count(*p) <= SYNTH_ROWS),
            "SYNTH_ROWS is the worst-case height the layout is budgeted for"
        );
    }

    /// Every setting is reachable, and reachable exactly once.
    ///
    /// With two dozen parameters spread over four pages, the mistake that would
    /// be easiest to make is also the one hardest to notice: a parameter added
    /// to `ChannelParams` and to no page at all, which simply never appears in
    /// the panel. This is the guard for that.
    #[test]
    fn every_channel_parameter_is_on_exactly_one_page() {
        for param in CHANNEL_PARAMS {
            let pages: Vec<SynthPage> = SynthPage::ALL
                .into_iter()
                .filter(|p| p.rows().contains(&param))
                .collect();
            assert_eq!(
                pages.len(),
                1,
                "{} is on {:?}",
                param.label(),
                pages.iter().map(|p| p.name()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn every_page_is_the_same_height_as_every_other() {
        // A page that resized the panel would move every panel drawn under it,
        // on a keystroke. The rows are padded out to the tallest page precisely
        // so that cannot happen, and this is what keeps the padding honest.
        let heights: Vec<usize> = SynthPage::ALL
            .into_iter()
            .map(|page| {
                let mut s = state(Focus::Synth);
                s.synth_page = page;
                render_synth(&s).lines().count()
            })
            .collect();
        assert!(
            heights.windows(2).all(|w| w[0] == w[1]),
            "the pages render at different heights: {:?}",
            heights
        );
        assert_eq!(
            SYNTH_ROWS,
            channel_row_start()
                + SynthPage::ALL.iter().map(|p| p.rows().len()).max().unwrap()
                + MASTER_ROWS.len(),
            "SYNTH_ROWS must be the instrument row, the tallest page and the master block"
        );
    }

    /// Put the Synth cursor on a parameter, whichever page it lives on.
    fn aim(s: &mut AppState, param: ChannelParam) {
        let page = SynthPage::ALL
            .into_iter()
            .find(|p| p.rows().contains(&param))
            .expect("parameter is on no page");
        s.synth_page = page;
        s.synth_row = channel_row_start() + page.rows().iter().position(|&p| p == param).unwrap();
    }

    // ---- the instrument library, in the panel ----

    /// What kind of modal this is, for an assertion message.
    fn modal_name(modal: Option<&Modal>) -> &'static str {
        match modal {
            None => "none",
            Some(Modal::AddRest) => "add rest",
            Some(Modal::EnsembleNameInput { .. }) => "ensemble name",
            Some(Modal::ImportPathInput { .. }) => "import path",
            Some(Modal::RhythmNameInput { .. }) => "rhythm name",
            Some(Modal::InstrumentPicker { .. }) => "instrument picker",
            Some(Modal::InstrumentNameInput { .. }) => "instrument name",
            Some(Modal::EqPresetNameInput { .. }) => "eq preset name",
            Some(Modal::FxPresetNameInput { .. }) => "fx preset name",
        }
    }

    /// The rendered instrument row.
    fn instrument_row(text: &str) -> String {
        text.lines()
            .find(|l| l.contains("instrument"))
            .expect("the instrument row")
            .to_string()
    }

    #[test]
    fn the_instrument_row_has_a_cell_for_every_register() {
        let text = render_synth(&state(Focus::Synth));
        let row = instrument_row(&text);
        // Nothing is loaded at rest, and the row says so rather than inventing a
        // name for a sound that came from nowhere in the library.
        assert_eq!(row.matches("custom").count(), CHANNEL_COUNT, "{row:?}");
    }

    #[test]
    fn loading_an_instrument_names_the_register_it_went_into() {
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        let name = s.instrument_store.instruments[0].name.clone();

        assert!(s.load_instrument(1, 0, &p));
        assert_eq!(s.instrument_label(1, &p), name);
        assert!(!s.register_edited(1, &p));
        // Only that register.
        assert_eq!(s.instrument_label(0, &p), "custom");
        assert_eq!(s.instrument_label(2, &p), "custom");
        assert!(instrument_row(&render_synth_with(&p, &s)).contains(&name));
    }

    #[test]
    fn changing_a_loaded_register_marks_it_edited() {
        // The row must never name an instrument the register no longer is — the
        // same contract as the `(edited)` marker on a rhythm.
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        let name = s.instrument_store.instruments[3].name.clone();
        s.load_instrument(0, 3, &p);
        assert_eq!(s.instrument_label(0, &p), name);

        p.low.cutoff.set(p.low.cutoff.get() * 1.4);
        assert!(s.register_edited(0, &p));
        let label = s.instrument_label(0, &p);
        assert!(label.starts_with('*'), "{label:?}");
        assert!(label.ends_with(&name), "{label:?}");
        // And the marker survives the column, which is why it leads.
        let row = instrument_row(&render_synth_with(&p, &s));
        assert!(row.contains(&format!("* {}", name)), "{row:?}");

        // Putting the value back clears it again.
        let original = s.instrument_store.instruments[3].voice.cutoff;
        p.low.cutoff.set(original);
        assert_eq!(s.instrument_label(0, &p), name);
    }

    #[test]
    fn a_register_can_be_put_back_exactly_as_it_was() {
        // What the picker's `Esc` does. The register is not a fresh one: it is
        // something a player dialled in by hand, which is exactly when losing it
        // would matter.
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        p.low.cutoff.set(1234.0);
        p.low.attack.set(0.333);
        p.low.unison.set(3.0);
        p.low.lfo_pwm.set(0.4);
        let original = crate::synth::capture_channel(channel_at(&p, 0));

        s.load_instrument(0, 9, &p);
        assert_ne!(crate::synth::capture_channel(channel_at(&p, 0)), original);

        crate::synth::apply_channel(channel_at(&p, 0), &original);
        assert_eq!(crate::synth::capture_channel(channel_at(&p, 0)), original);
    }

    #[test]
    fn stepping_walks_the_library_and_touches_one_register() {
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        let before_mid = crate::synth::capture_channel(channel_at(&p, 1));
        let before_high = crate::synth::capture_channel(channel_at(&p, 2));

        let first = s.step_instrument(1, 1, &p).expect("a first instrument");
        assert_eq!(s.instrument_label(1, &p), first);

        let second = s.step_instrument(1, 1, &p).expect("a second");
        assert_ne!(first, second, "stepping must move");
        assert_eq!(s.instrument_label(1, &p), second);

        // And back again, which lands exactly where it started.
        let back = s.step_instrument(1, -1, &p).expect("back");
        assert_eq!(back, first);

        // The other registers never moved.
        assert_eq!(
            crate::synth::capture_channel(channel_at(&p, 1)),
            before_high_or(&p, &before_mid)
        );
        assert_eq!(
            crate::synth::capture_channel(channel_at(&p, 2)),
            before_high
        );
    }

    /// The mid register after `first` was loaded — a helper so the assertion
    /// above reads as "the other registers did not move".
    fn before_high_or(p: &SynthParams, _unused: &ComposedChannel) -> ComposedChannel {
        crate::synth::capture_channel(channel_at(p, 1))
    }

    #[test]
    fn stepping_from_nothing_starts_at_the_top_of_the_library() {
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        let first = s.instrument_store.instruments[0].name.clone();
        assert_eq!(s.step_instrument(2, 1, &p), Some(first));
        assert_eq!(
            s.step_instrument(2, -1, &p),
            Some(s.instrument_store.instruments.last().unwrap().name.clone())
        );
    }

    #[test]
    fn the_picker_opens_on_the_instrument_already_in_the_register() {
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        s.synth_row = SYNTH_ROW_INSTRUMENT;
        s.synth_col = 1;
        s.load_instrument(1, 5, &p);

        open_instrument_picker(&mut s, &p, &log);
        match &s.modal {
            Some(Modal::InstrumentPicker { col, index, .. }) => {
                assert_eq!(*col, 1);
                assert_eq!(*index, 5, "the cursor should start where the register is");
            }
            other => panic!("expected the picker, got {}", modal_name(other.as_ref())),
        }
    }

    #[test]
    fn the_picker_opens_at_the_top_when_the_register_is_custom() {
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        s.synth_row = SYNTH_ROW_INSTRUMENT;
        s.synth_col = 0;
        open_instrument_picker(&mut s, &p, &log);
        match &s.modal {
            Some(Modal::InstrumentPicker { index, .. }) => assert_eq!(*index, 0),
            other => panic!("expected the picker, got {}", modal_name(other.as_ref())),
        }
    }

    #[test]
    fn the_picker_prompt_names_the_register_and_the_position_in_the_library() {
        let s = state(Focus::Synth);
        let modal = Modal::InstrumentPicker {
            col: 2,
            index: 4,
            original: Box::new(ComposedChannel::neutral(4.0, 4000.0)),
        };
        let mut out: Vec<u8> = Vec::new();
        render_modal(&mut out, &modal, DEFAULT_SCREEN.width, &s.instrument_store).unwrap();
        let text = strip_ansi(&String::from_utf8(out).unwrap());
        let name = &s.instrument_store.instruments[4].name;
        assert!(text.contains(name), "{text:?}");
        assert!(
            text.contains("high"),
            "the register belongs in the prompt: {text:?}"
        );
        assert!(
            text.contains(&format!("[5/{}]", s.instrument_store.instruments.len())),
            "{text:?}"
        );
        assert!(
            text.trim_end().chars().count() <= DEFAULT_SCREEN.width,
            "the prompt is {} wide: {text:?}",
            text.trim_end().chars().count()
        );
    }

    #[test]
    fn saving_a_register_keeps_it_as_a_named_instrument() {
        // The half of the feature that has to reach the disk: a player dials in
        // something, keeps it, and finds it in the library next time.
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        p.mid.cutoff.set(1234.0);
        p.mid.attack.set(0.222);
        p.mid.waveform.set(Waveform::Reed as i32 as f32);

        let mut path = std::env::temp_dir();
        path.push(format!("chord-tool-save-inst-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);

        save_instrument(&mut s, &p, 1, "My Reed", &path, &log);

        assert!(path.exists(), "nothing was written");
        assert!(s.instrument_store.index_of("My Reed").is_some());
        assert_eq!(s.instrument_label(1, &p), "My Reed");
        assert!(!s.register_edited(1, &p));

        // And it comes back from the file, layered over the defaults.
        let reloaded = crate::instrument::InstrumentStore::load(&path).unwrap();
        let mine = reloaded
            .instruments
            .iter()
            .find(|i| i.name == "My Reed")
            .expect("the saved instrument");
        assert_eq!(mine.voice.cutoff, 1234.0);
        assert_eq!(mine.voice.waveform, Waveform::Reed);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn saving_a_variant_does_not_silently_replace_the_instrument_it_came_from() {
        // The prompt defaults to `Name 2` rather than `Name`, so a tweak kept
        // from a loaded instrument adds to the library instead of overwriting
        // the thing that was tweaked.
        assert_eq!(default_instrument_name("Rhodes Dark"), "Rhodes Dark 2");
        assert_eq!(default_instrument_name("custom"), "");
        assert_eq!(default_instrument_name("* Rhodes Dark"), "");
    }

    #[test]
    fn saving_a_variant_leaves_the_original_in_the_library() {
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        let mut path = std::env::temp_dir();
        path.push(format!("chord-tool-variant-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let original = s.instrument_store.instruments[4].clone();
        s.load_instrument(0, 4, &p);
        let before = s.instrument_store.instruments.len();

        // Tweak it and keep the tweak under the offered name.
        p.low.cutoff.set(p.low.cutoff.get() * 0.5);
        let offered = default_instrument_name(&s.instrument_label(0, &p));
        save_instrument(&mut s, &p, 0, &offered, &path, &log);

        assert_eq!(
            s.instrument_store.instruments.len(),
            before + 1,
            "the variant should be added, not swapped in"
        );
        let still_there = s
            .instrument_store
            .index_of(&original.name)
            .map(|i| &s.instrument_store.instruments[i])
            .expect("the original");
        assert_eq!(still_there.voice, original.voice, "the original moved");
        assert_eq!(s.instrument_label(0, &p), offered);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_name_prompt_prompt_is_readable_and_fits_the_screen() {
        let s = state(Focus::Synth);
        let modal = Modal::InstrumentNameInput {
            col: 1,
            buffer: "My Reed".to_string(),
        };
        let mut out: Vec<u8> = Vec::new();
        render_modal(&mut out, &modal, DEFAULT_SCREEN.width, &s.instrument_store).unwrap();
        let text = strip_ansi(&String::from_utf8(out).unwrap());
        assert!(text.contains("My Reed"), "{text:?}");
        assert!(
            text.contains("mid"),
            "the register belongs in the prompt: {text:?}"
        );
        assert!(text.trim_end().chars().count() <= DEFAULT_SCREEN.width);
    }

    #[test]
    fn loading_an_ensemble_names_all_three_registers() {
        // The gain from the split: an ensemble says which instrument each
        // register is, so the row can name all three instead of reporting
        // `custom` for a sound it did not load from the library.
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        let ensemble = s.ensemble_store.ensembles[0].clone();

        let (channels, named, missing) = resolve_ensemble(&s.instrument_store, &ensemble);
        assert!(missing.is_empty(), "{:?}", missing);
        for col in 0..CHANNEL_COUNT {
            crate::synth::apply_channel(channel_at(&p, col), &channels[col]);
            s.instrument_loaded[col] = Some(named[col].clone());
        }

        for col in 0..CHANNEL_COUNT {
            let label = s.instrument_label(col, &p);
            assert_ne!(label, "custom", "register {} has no name", col);
            assert!(!label.starts_with('*'), "register {} reads as edited", col);
            assert!(!s.register_edited(col, &p));
        }
    }

    #[test]
    fn auditioning_an_instrument_leaves_the_mix_alone() {
        // The workflow payoff of splitting the voice from the placement: trying
        // instruments must not rebalance the register it is being tried in.
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        p.mid.volume.set(2.5);
        p.mid.pan.set(-0.6);
        p.mid.transpose.set(7.0);
        p.mid.reverb_send.set(0.8);

        let before = crate::synth::capture_channel(channel_at(&p, 1));
        s.load_instrument(1, 0, &p);
        let after = crate::synth::capture_channel(channel_at(&p, 1));

        assert_ne!(after.voice, before.voice, "the voice should have changed");
        assert_eq!(after.volume, before.volume, "the level moved");
        assert_eq!(after.pan, before.pan, "the pan moved");
        assert_eq!(after.transpose, before.transpose, "the transpose moved");
        assert_eq!(
            after.reverb_send, before.reverb_send,
            "the reverb send moved"
        );
    }

    #[test]
    fn stepping_through_the_library_leaves_the_mix_alone_too() {
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        p.low.volume.set(6.0);
        p.low.pan.set(0.75);
        for _ in 0..5 {
            s.step_instrument(0, 1, &p);
        }
        assert_eq!(p.low.volume.get(), 6.0);
        assert_eq!(p.low.pan.get(), 0.75);
    }

    #[test]
    fn paging_moves_between_pages_and_wraps() {
        let log = logger();
        let mut s = state(Focus::Synth);
        assert_eq!(s.synth_page, SynthPage::Tone);
        for _ in 0..SynthPage::ALL.len() {
            cycle_synth_page(&mut s, 1, &log);
        }
        assert_eq!(s.synth_page, SynthPage::Tone, "the cycle must close");
        cycle_synth_page(&mut s, -1, &log);
        assert_eq!(s.synth_page, SynthPage::Fx, "and go backwards too");
    }

    #[test]
    fn paging_keeps_the_cursor_on_a_row_that_exists() {
        let log = logger();
        let mut s = state(Focus::Synth);
        // Row 6 exists on the tone and filter pages and not on env or mod.
        s.synth_row = 6;
        s.synth_col = 2;
        // Tone (7 rows), osc (8), pluck (3), env (5): three pages on from tone
        // is env, which is shorter than the row the cursor was on.
        cycle_synth_page(&mut s, 3, &log);
        assert_eq!(s.synth_page, SynthPage::Env);
        // Back two, through the three-row pluck page, to osc and its eight rows.
        cycle_synth_page(&mut s, -2, &log);
        assert_eq!(s.synth_page, SynthPage::Osc);
        assert_eq!(s.synth_row, 6, "a row that exists on the new page is kept");
        // Forward one lands on the pluck page, which is shorter than row six.
        cycle_synth_page(&mut s, 1, &log);
        assert_eq!(s.synth_page, SynthPage::Pluck);
        assert!(
            s.synth_row < synth_row_count(SynthPage::Pluck),
            "the cursor must land on a row that exists"
        );
        assert!(
            s.synth_row < synth_row_count(SynthPage::Mod),
            "the cursor must land on a row that exists"
        );
        assert!(s.synth_col < CHANNEL_COUNT);
    }

    #[test]
    fn the_title_names_the_page_and_where_it_sits() {
        let mut s = state(Focus::Synth);
        s.synth_page = SynthPage::Filter;
        let text = render_synth_with(&SynthParams::defaults(), &s);
        assert!(
            text.contains("filter 5/7"),
            "the page belongs in the title: {text:?}"
        );
    }

    #[test]
    fn every_channel_cell_adjusts_only_its_own_column() {
        let p = SynthParams::defaults();
        let t = Transport::new(Key::new(60, Scale::Major));
        let before = (p.low.volume.get(), p.mid.volume.get(), p.high.volume.get());

        synth_cell(SynthPage::Tone, channel_row_start(), 1).adjust(ctx(&p, &t), 1);
        assert_eq!(p.low.volume.get(), before.0, "low must be untouched");
        assert_eq!(p.mid.volume.get(), before.1 + 1.0);
        assert_eq!(p.high.volume.get(), before.2, "high must be untouched");
    }

    #[test]
    fn a_synth_nudge_uses_the_same_clamp_as_the_old_mixer() {
        let p = SynthParams::defaults();
        let t = Transport::new(Key::new(60, Scale::Major));
        // Master volume saturates at 7.0 rather than running away.
        let (row, col) = master_row_of(MixerParam::MasterVolume);
        for _ in 0..20 {
            synth_cell(
                SynthPage::Tone,
                channel_row_start() + SynthPage::Tone.rows().len() + row,
                col,
            )
            .adjust(ctx(&p, &t), 1);
        }
        assert_eq!(p.master_volume.get(), 7.0);
    }

    // ---- the two aux units in the master block ----

    /// The master cell at `param`, aimed at through the real cursor.
    fn master_cell(s: &mut AppState, param: MixerParam) {
        let (row, col) = master_row_of(param);
        s.synth_row = channel_row_start() + SynthPage::Tone.rows().len() + row;
        s.synth_col = col;
        assert_eq!(s.synth_cell().master(), Some(param));
    }

    #[test]
    fn every_master_cell_has_a_label_and_a_real_setting_behind_it() {
        // `label` and `display` end in catch-all arms so their matches stay
        // exhaustive; this is what stops one of them quietly swallowing a row.
        for param in MIXER_PARAMS {
            assert!(!param.label().is_empty(), "{param:?} has no label");
            if let Some((unit, _)) = param.aux_param() {
                assert_eq!(
                    param.aux_unit(),
                    Some(unit),
                    "{param:?} disagrees with itself about which unit it is"
                );
            }
            if param.is_preset() {
                assert!(param.aux_unit().is_some(), "{param:?} has no unit");
            }
        }
    }

    #[test]
    fn the_master_block_covers_exactly_the_slots_the_aux_units_read() {
        // A row for every slot that counts and no row for a slot that does not,
        // which is what keeps the units' unused `mix` off the panel. The grid and
        // the parameter tables are two separate lists, so this is the check that
        // they have not drifted apart.
        let mut rows: Vec<(AuxUnit, usize)> =
            MIXER_PARAMS.iter().filter_map(|p| p.aux_param()).collect();
        rows.sort_unstable();
        let mut deduped = rows.clone();
        deduped.dedup();
        assert_eq!(rows, deduped, "two rows edit the same slot");

        let mut expected: Vec<(AuxUnit, usize)> = Vec::new();
        for unit in [AuxUnit::Reverb, AuxUnit::Delay] {
            for sync in [0.0, 1.0] {
                let mut fx = Fx::new(unit.kind());
                fx.set_param(P4, sync);
                expected.extend(unit.params(&fx).iter().map(|index| (unit, *index)));
            }
        }
        expected.sort_unstable();
        expected.dedup();
        assert_eq!(rows, expected);
    }

    #[test]
    fn the_subtype_row_loads_the_variant_it_lands_on() {
        // A variant is a starting point rather than a label: `plate` and `hall`
        // are the same slots with different numbers, so choosing one has to move
        // them, or the choice would be inaudible.
        let p = SynthParams::defaults();
        let t = Transport::new(Key::new(60, Scale::Major));
        let c = ctx(&p, &t);
        let mut s = state(Focus::Synth);
        master_cell(&mut s, MixerParam::ReverbSubtype);
        let cell = s.synth_cell();

        let hall = p.aux_reverb.fx();
        assert_eq!(hall.subtype.name(), "hall");
        cell.adjust(c, 1);
        let room = p.aux_reverb.fx();
        assert_eq!(room.subtype.name(), "room");
        assert_ne!(room.params, hall.params, "the numbers came with it");

        // Forward past the end wraps to the first, and back from the first lands
        // on the last.
        let len = FxKind::Reverb.subtypes().len() as i32;
        for _ in 0..len - 1 {
            cell.adjust(c, 1);
        }
        assert_eq!(p.aux_reverb.fx().subtype.name(), "hall");
        cell.adjust(c, -1);
        assert_eq!(p.aux_reverb.fx().subtype.name(), "ambience");
    }

    #[test]
    fn the_preset_row_walks_the_library_and_names_what_it_lands_on() {
        let p = SynthParams::defaults();
        let t = Transport::new(Key::new(60, Scale::Major));
        let c = ctx(&p, &t);
        let mut s = state(Focus::Synth);
        master_cell(&mut s, MixerParam::ReverbPreset);
        let cell = s.synth_cell();

        // The shipped hall reads as the preset it ships as, although the stored
        // preset carries a `mix` this position never uses.
        assert_eq!(MixerParam::ReverbPreset.display(c), "Hall");
        cell.adjust(c, 1);
        assert_eq!(MixerParam::ReverbPreset.display(c), "Big Hall");
        assert_eq!(p.aux_reverb.fx().param(P0), 0.78, "the size arrived");
        // Past the end it wraps, and back from the first lands on the last.
        cell.adjust(c, 1);
        assert_eq!(MixerParam::ReverbPreset.display(c), "Hall");
        cell.adjust(c, -1);
        assert_eq!(MixerParam::ReverbPreset.display(c), "Big Hall");
    }

    #[test]
    fn a_delays_division_only_counts_once_sync_is_on() {
        // The division names a note value only the sync switch reads, so an
        // unsynced delay parked on the quarter note is still the shipped preset.
        let p = SynthParams::defaults();
        let t = Transport::new(Key::new(60, Scale::Major));
        let c = ctx(&p, &t);
        let mut s = state(Focus::Synth);
        master_cell(&mut s, MixerParam::DelayPreset);
        let cell = s.synth_cell();
        assert_eq!(MixerParam::DelayPreset.display(c), "Digital Delay");

        // Switch sync on and the note value starts counting, so the unit is no
        // longer literally any preset — the time it plays is now the division.
        p.aux_delay.set_param(P4, 1.0);
        assert_eq!(MixerParam::DelayPreset.display(c), "custom");
        // Stepping from a custom effect starts at the beginning of the list and
        // walks from there, sync and all.
        cell.adjust(c, 1);
        assert_eq!(MixerParam::DelayPreset.display(c), "Digital Delay");
        cell.adjust(c, 1);
        assert_eq!(MixerParam::DelayPreset.display(c), "Quarter Note");
        assert_eq!(p.aux_delay.fx().param(P4), 1.0, "and sync is still on");
        cell.adjust(c, 1);
        assert_eq!(MixerParam::DelayPreset.display(c), "Dotted Eighth");
    }

    #[test]
    fn esc_puts_back_the_whole_effect_a_preset_row_replaced() {
        // A preset is applied rather than dialled, so the edit's snapshot cannot
        // be one float: `Esc` after walking the library has to give back the
        // effect you had.
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        master_cell(&mut s, MixerParam::ReverbPreset);
        let before = p.aux_reverb.fx();

        begin_synth_edit(&mut s, &p);
        assert!(matches!(
            s.edit,
            Edit::SynthCell {
                before: Some(_),
                ..
            }
        ));
        handle_synth_edit(&mut s, &p, &key(KeyCode::Right), &log);
        assert_ne!(p.aux_reverb.fx(), before, "the preset was applied");

        handle_synth_edit(&mut s, &p, &key(KeyCode::Esc), &log);
        assert_eq!(p.aux_reverb.fx(), before, "the effect came back whole");
        assert!(matches!(s.edit, Edit::None));

        // And `Enter` keeps what the walk landed on.
        begin_synth_edit(&mut s, &p);
        handle_synth_edit(&mut s, &p, &key(KeyCode::Right), &log);
        let chosen = p.aux_reverb.fx();
        handle_synth_edit(&mut s, &p, &key(KeyCode::Enter), &log);
        assert_eq!(p.aux_reverb.fx(), chosen);
    }

    #[test]
    fn a_parameter_row_carries_no_effect_snapshot() {
        // The two halves of an edit are exclusive: a cell is a number or a
        // preset, and a number's undo is the number.
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        master_cell(&mut s, MixerParam::ReverbSize);
        begin_synth_edit(&mut s, &p);
        assert!(matches!(s.edit, Edit::SynthCell { before: None, .. }));
    }

    #[test]
    fn the_return_level_is_the_mixers_number_not_the_units_dry_wet() {
        // How much reverb you hear is one mixer number, so the unit stays fully
        // wet and its own dry/wet has no row at all — turning the return up must
        // not disturb the effect the preset row names.
        let p = SynthParams::defaults();
        let t = Transport::new(Key::new(60, Scale::Major));
        let c = ctx(&p, &t);
        let mut s = state(Focus::Synth);
        master_cell(&mut s, MixerParam::ReverbMix);

        let unit = p.aux_reverb.fx();
        assert_eq!(MixerParam::ReverbPreset.display(c), "Hall");
        MixerParam::ReverbMix.adjust(c, 4);
        assert_eq!(p.reverb_mix.get(), 0.20);
        assert_eq!(MixerParam::ReverbMix.display(c), "20%");
        assert_eq!(p.aux_reverb.fx(), unit, "the unit itself did not move");
        assert_eq!(MixerParam::ReverbPreset.display(c), "Hall");

        // The slot either unit's dry/wet lives in is addressed by no row.
        for unit in [AuxUnit::Reverb, AuxUnit::Delay] {
            assert!(
                MIXER_PARAMS
                    .iter()
                    .all(|p| p.aux_param() != Some((unit, P3))),
                "an aux's dry/wet must not be a master row"
            );
        }
    }

    #[test]
    fn esc_reverts_a_synth_edit_and_enter_keeps_it() {
        let log = logger();
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        aim(&mut s, ChannelParam::Cutoff);
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
        aim(&mut s, ChannelParam::Cutoff);
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
        aim(&mut s, ChannelParam::Waveform);
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
        aim(&mut s, ChannelParam::Volume);
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

    // ---- the EQ panel ----

    /// One arrow key on the EQ panel, through the real handler.
    fn eq_arrow(s: &mut AppState, p: &SynthParams, log: &Logger, code: KeyCode, shift: bool) {
        let mut ev = key(code);
        if shift {
            ev.modifiers = KeyModifiers::SHIFT;
        }
        handle_panel_arrow(s, p, &ev, log);
    }

    fn eq_state(row: usize) -> AppState {
        let mut s = state(Focus::Eq);
        s.eq_row = row;
        s
    }

    #[test]
    fn the_eq_panel_draws_every_band_and_the_scale() {
        let p = SynthParams::defaults();
        let s = eq_state(EQ_ROW_GAIN);
        let text = render_frame_with(&p, &s);
        for label in crate::eq::BAND_LABELS {
            assert!(text.contains(label), "no {} in:\n{text}", label);
        }
        // Every row name, the scale that says which side is which, and the
        // target the curve on screen belongs to.
        for needle in ["target", "preset", "band", "gain", "Save Curve"] {
            assert!(text.contains(needle), "no {:?} in:\n{text}", needle);
        }
        assert!(text.contains("low"), "the target's own name:\n{text}");
        assert!(text.contains("-12 dB"), "rendered:\n{text}");
        assert!(text.contains("+12 dB"), "rendered:\n{text}");
        // The curve is one bar per band, each with a centre rule.
        assert_eq!(text.matches('┼').count(), EQ_BANDS, "rendered:\n{text}");
        // And it takes the Synth slot, so neither the table nor the one-line
        // stand-in is drawn: the panel is the equaliser and nothing else.
        assert!(text.contains("━━ EQ [low] ━━"), "rendered:\n{text}");
        assert!(!text.contains("waveform"), "rendered:\n{text}");
        assert!(!text.contains("note length"), "rendered:\n{text}");
        assert!(!text.contains("reverb"), "rendered:\n{text}");
    }

    #[test]
    fn the_eq_panel_says_which_register_and_which_instrument_it_is_on() {
        let p = SynthParams::defaults();
        let mut s = eq_state(EQ_ROW_TARGET);
        s.eq_target = EqTarget::Mid as usize;
        s.instrument_loaded[1] = Some(("Rhodes Dark".to_string(), VoicePatch::neutral(2000.0)));
        let text = render_frame_with(&p, &s);
        assert!(text.contains("Rhodes Dark"), "rendered:\n{text}");
        assert!(text.contains(" EQ [mid] "), "rendered:\n{text}");
    }

    #[test]
    fn the_eq_target_row_cycles_the_three_registers_and_the_mix() {
        let p = SynthParams::defaults();
        let mut s = eq_state(EQ_ROW_TARGET);
        let log = logger();
        let mut seen = Vec::new();
        for _ in 0..EqTarget::ALL.len() {
            seen.push(EqTarget::from_index(s.eq_target).name());
            eq_arrow(&mut s, &p, &log, KeyCode::Right, false);
        }
        assert_eq!(seen, ["low", "mid", "high", "master"]);
        assert_eq!(s.eq_target, 0, "the row wraps back to where it started");
        eq_arrow(&mut s, &p, &log, KeyCode::Left, false);
        assert_eq!(EqTarget::from_index(s.eq_target).name(), "master");
    }

    #[test]
    fn the_eq_gain_row_writes_through_to_the_live_parameters() {
        let p = SynthParams::defaults();
        let mut s = eq_state(EQ_ROW_GAIN);
        let log = logger();

        eq_arrow(&mut s, &p, &log, KeyCode::Right, false);
        assert_eq!(p.low.eq.band(0), 0.5);
        eq_arrow(&mut s, &p, &log, KeyCode::Right, true);
        assert_eq!(p.low.eq.band(0), 3.5, "shift is three decibels");
        eq_arrow(&mut s, &p, &log, KeyCode::Left, true);
        assert_eq!(p.low.eq.band(0), 0.5);

        // The other three equalisers never moved, and neither did the other
        // eleven bands.
        assert_eq!(p.mid.eq.curve(), EqCurve::flat());
        assert_eq!(p.high.eq.curve(), EqCurve::flat());
        assert_eq!(p.master_eq.curve(), EqCurve::flat());
        assert_eq!(
            p.low.eq.curve().gains.iter().filter(|g| **g != 0.0).count(),
            1
        );
    }

    #[test]
    fn the_eq_gain_row_stops_at_the_ends_of_its_range() {
        let p = SynthParams::defaults();
        let mut s = eq_state(EQ_ROW_GAIN);
        let log = logger();
        for _ in 0..40 {
            eq_arrow(&mut s, &p, &log, KeyCode::Right, true);
        }
        assert_eq!(p.low.eq.band(0), crate::synth::range::EQ_GAIN.1);
        for _ in 0..40 {
            eq_arrow(&mut s, &p, &log, KeyCode::Left, true);
        }
        assert_eq!(p.low.eq.band(0), crate::synth::range::EQ_GAIN.0);
    }

    #[test]
    fn the_eq_band_cursor_walks_every_band_and_wraps() {
        let p = SynthParams::defaults();
        let mut s = eq_state(EQ_ROW_BAND);
        let log = logger();
        let mut visited = Vec::new();
        for _ in 0..EQ_BANDS {
            visited.push(s.eq_band);
            eq_arrow(&mut s, &p, &log, KeyCode::Right, false);
        }
        assert_eq!(visited, (0..EQ_BANDS).collect::<Vec<_>>());
        assert_eq!(s.eq_band, 0, "the band cursor wraps");
        eq_arrow(&mut s, &p, &log, KeyCode::Left, false);
        assert_eq!(s.eq_band, EQ_BANDS - 1);
        // Shift jumps four at a time, and wrapping never leaves the range.
        for _ in 0..10 {
            eq_arrow(&mut s, &p, &log, KeyCode::Right, true);
            assert!(s.eq_band < EQ_BANDS);
        }
    }

    #[test]
    fn the_eq_gain_row_edits_the_band_the_cursor_is_on() {
        // The two rows have to agree about which band is being moved, or the
        // number on screen belongs to a different band from the one the bar
        // marks.
        let p = SynthParams::defaults();
        let mut s = eq_state(EQ_ROW_BAND);
        let log = logger();
        // Five steps lands on 250 Hz, the middle of the ladder.
        for _ in 0..5 {
            eq_arrow(&mut s, &p, &log, KeyCode::Right, false);
        }
        assert_eq!(crate::eq::BAND_LABELS[s.eq_band], "250");
        s.eq_row = EQ_ROW_GAIN;
        eq_arrow(&mut s, &p, &log, KeyCode::Right, false);
        assert_eq!(p.low.eq.band(5), 0.5);
        assert_eq!(
            p.low.eq.curve().gains.iter().filter(|g| **g != 0.0).count(),
            1
        );
        let text = render_frame_with(&p, &s);
        // The cursor marker is on the fifth band's row.
        let marked = text
            .lines()
            .find(|l| l.contains('▸') && l.contains("250"))
            .unwrap_or_else(|| panic!("no cursor on 250 Hz:\n{text}"));
        assert!(
            marked.contains('█'),
            "the bar should show the boost: {:?}",
            marked
        );
    }

    #[test]
    fn the_eq_preset_row_applies_a_whole_curve_to_one_target() {
        let p = SynthParams::defaults();
        let mut s = eq_state(EQ_ROW_PRESET);
        let log = logger();
        eq_arrow(&mut s, &p, &log, KeyCode::Right, false);
        assert_eq!(p.low.eq.curve(), s.presets.presets[1].gains);
        // The register's own curve moved and nothing else did.
        assert_eq!(p.mid.eq.curve(), EqCurve::flat());
        assert_eq!(p.high.eq.curve(), EqCurve::flat());
        assert_eq!(p.master_eq.curve(), EqCurve::flat());

        // The walk wraps at both ends rather than sticking.
        let len = s.presets.presets.len();
        eq_arrow(&mut s, &p, &log, KeyCode::Left, false);
        assert_eq!(p.low.eq.curve(), s.presets.presets[0].gains, "back to Flat");
        eq_arrow(&mut s, &p, &log, KeyCode::Left, false);
        assert_eq!(
            p.low.eq.curve(),
            s.presets.presets[len - 1].gains,
            "past the start is the end"
        );
    }

    #[test]
    fn the_eq_preset_row_starts_a_custom_curve_from_flat() {
        // A curve that is not in the library has no position to step from, so
        // the first press gives the first preset rather than something with no
        // name. Without this, `custom` would make the arrows do nothing.
        let p = SynthParams::defaults();
        let mut s = eq_state(EQ_ROW_PRESET);
        let log = logger();
        p.low.eq.set_band(3, 1.5);
        assert_eq!(s.presets.name_for_curve(&p.low.eq.curve()), None, "custom");

        eq_arrow(&mut s, &p, &log, KeyCode::Right, false);
        assert_eq!(p.low.eq.curve(), s.presets.presets[0].gains);
        assert!(p.low.eq.curve().is_flat(), "which is Flat");
    }

    #[test]
    fn the_eq_preset_row_has_its_own_coarse_step() {
        let p = SynthParams::defaults();
        let mut s = eq_state(EQ_ROW_PRESET);
        let log = logger();
        eq_arrow(&mut s, &p, &log, KeyCode::Right, false);
        let first = p.low.eq.curve();
        eq_arrow(&mut s, &p, &log, KeyCode::Right, true);
        assert_ne!(p.low.eq.curve(), first);
        assert_eq!(
            p.low.eq.curve(),
            s.presets.presets[(1 + EQ_PRESET_COARSE_STEP) as usize].gains
        );
    }

    #[test]
    fn enter_on_the_gain_row_returns_that_band_to_zero() {
        let p = SynthParams::defaults();
        let mut s = eq_state(EQ_ROW_GAIN);
        let log = logger();
        eq_arrow(&mut s, &p, &log, KeyCode::Right, true);
        assert_eq!(p.low.eq.band(0), 3.0);
        eq_action(&mut s, &p, &log);
        assert_eq!(p.low.eq.band(0), 0.0);
        assert!(p.low.eq.curve().is_flat(), "a reset band is an exact zero");
    }

    #[test]
    fn enter_on_the_other_rows_leaves_the_curve_alone() {
        for row in [EQ_ROW_TARGET, EQ_ROW_PRESET, EQ_ROW_BAND] {
            let p = SynthParams::defaults();
            let mut s = eq_state(row);
            let log = logger();
            eq_arrow(&mut s, &p, &log, KeyCode::Right, true);
            let before = p.low.eq.curve();
            eq_action(&mut s, &p, &log);
            assert_eq!(p.low.eq.curve(), before, "row {}", row);
        }
    }

    #[test]
    fn enter_on_the_save_row_opens_the_preset_prompt() {
        let p = SynthParams::defaults();
        let mut s = eq_state(EQ_ROW_SAVE);
        let log = logger();
        eq_action(&mut s, &p, &log);
        assert_eq!(modal_name(s.modal.as_ref()), "eq preset name");
        // And it is a button, so Enter runs it rather than committing a chord.
        assert!(row_is_action_button(&s));
        s.eq_row = EQ_ROW_GAIN;
        assert!(!row_is_action_button(&s));
    }

    #[test]
    fn saving_an_eq_preset_writes_a_user_file_that_reloads() {
        let p = SynthParams::defaults();
        let mut s = eq_state(EQ_ROW_TARGET);
        s.eq_target = EqTarget::High as usize;
        let log = logger();
        p.high.eq.set(&s.presets.presets[4].gains);

        let mut path = std::env::temp_dir();
        path.push(format!("chord-tool-eq-preset-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);

        save_eq_preset(&mut s, &p, "My Curve", &path, &log);
        assert!(path.exists(), "the preset file should be written");

        let reloaded = EqPresetStore::load(&path).unwrap();
        assert_eq!(reloaded.find("My Curve").unwrap().gains, p.high.eq.curve());
        // The shipped library is untouched: only the user's own entry is written.
        assert_eq!(reloaded.presets.len(), s.presets.presets.len());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_preset_named_flat_is_refused() {
        // `Flat` means bypassed. A preset allowed to take the name would make
        // the row say "Flat" about a curve that is doing something.
        let p = SynthParams::defaults();
        let mut s = eq_state(EQ_ROW_SAVE);
        let log = logger();
        p.low.eq.set_band(0, 6.0);

        let mut path = std::env::temp_dir();
        path.push(format!("chord-tool-eq-flat-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let before = s.presets.presets.len();
        save_eq_preset(&mut s, &p, "flat", &path, &log);
        assert_eq!(s.presets.presets.len(), before, "nothing was added");
        assert!(!path.exists(), "and nothing was written");
    }

    #[test]
    fn changing_the_eq_does_not_mark_a_register_as_an_edited_instrument() {
        // The equaliser is part of where a sound sits, not what it is: moving it
        // must not make the `instrument` row claim the sound has changed.
        let p = SynthParams::defaults();
        let mut s = state(Focus::Synth);
        let voice = s.instrument_store.instruments[0].voice.clone();
        // The register has to actually *be* that instrument, or the row is
        // right to call it edited.
        crate::synth::apply_voice(channel_at(&p, 0), &voice);
        s.instrument_loaded[0] = Some(("Some Instrument".to_string(), voice));
        assert_eq!(s.instrument_label(0, &p), "Some Instrument");

        p.low.eq.set(&s.presets.presets[3].gains);
        assert_eq!(
            s.instrument_label(0, &p),
            "Some Instrument",
            "the EQ is not part of the voice"
        );

        // And swapping the instrument leaves the curve exactly where it was.
        let before = p.low.eq.curve();
        crate::synth::apply_voice(
            channel_at(&p, 0),
            &s.instrument_store.instruments[5].voice.clone(),
        );
        assert_eq!(p.low.eq.curve(), before);
    }

    #[test]
    fn the_eq_travels_with_a_placement_through_composing_and_splitting() {
        // The whole round trip: an ensemble says where the curve is, applying it
        // writes the live parameters, and capturing reads them back the same.
        let mut ensemble = crate::ensemble::builtin_ensembles()
            .into_iter()
            .find(|e| e.name == "Default")
            .expect("the default ensemble");
        ensemble.low.eq.gains[2] = -6.0;
        ensemble.high.eq.gains[9] = 4.5;
        ensemble.mixer.master_eq.gains[0] = 7.5;

        let library = InstrumentStore::with_builtins();
        let (channels, missing) = ensemble.resolve(|name| library.voice_of(name));
        assert!(missing.is_empty());
        assert_eq!(channels[0].eq, ensemble.low.eq);
        assert_eq!(channels[2].eq, ensemble.high.eq);

        let captured = channels[0].split("Whatever");
        assert_eq!(captured.eq, ensemble.low.eq);
        assert_eq!(
            ComposedChannel::compose(channels[0].voice.clone(), &captured).eq,
            ensemble.low.eq
        );
    }

    #[test]
    fn the_eq_panel_keeps_the_frame_inside_the_window() {
        let p = SynthParams::defaults();
        let mut s = eq_state(EQ_ROW_GAIN);
        for target in 0..EqTarget::ALL.len() {
            s.eq_target = target;
            let text = render_frame_with(&p, &s);
            for line in text.lines() {
                assert!(
                    line.chars().count() <= DEFAULT_SCREEN.width,
                    "{} columns: {:?}",
                    line.chars().count(),
                    line
                );
            }
        }
    }
    // ---- the Spectrum panel ----

    fn spectrum_state(target: EqTarget, row: usize) -> AppState {
        let mut s = state(Focus::Spectrum);
        s.eq_target = target as usize;
        s.spectrum_row = row;
        s
    }

    #[test]
    fn a_level_maps_to_the_row_it_should() {
        // The chart's arithmetic, on its own: full scale at the top, silence at
        // the bottom, and a half-scale tone a sixth of the way down.
        assert_eq!(spectrum_levels(0.0, 60.0), 0);
        assert_eq!(spectrum_levels(1.0, 60.0), SPECTRUM_CHART_ROWS * 2);
        assert_eq!(spectrum_levels(0.001, 60.0), 0, "−60 dB is the floor");
        let half = spectrum_levels(0.5, 60.0);
        assert!(
            (21..=22).contains(&half),
            "half scale should be about 22 of 24, got {}",
            half
        );
        // A narrower span puts the same level lower, which is the point of the
        // row. Measured well down the scale: near the top all three spans round
        // to the same row, which is a resolution fact and not a bug.
        let quiet = spectrum_levels(0.05, 60.0);
        assert!(
            spectrum_levels(0.05, 48.0) < quiet && quiet < spectrum_levels(0.05, 72.0),
            "48/60/72 dB gave {} / {} / {}",
            spectrum_levels(0.05, 48.0),
            quiet,
            spectrum_levels(0.05, 72.0)
        );
        // And nothing can ask for more chart than there is.
        assert_eq!(spectrum_levels(100.0, 48.0), SPECTRUM_CHART_ROWS * 2);
    }

    #[test]
    fn a_bar_is_solid_below_its_top_and_half_at_it() {
        let span = 60.0;
        let full = 1.0f32;
        let top = SPECTRUM_CHART_ROWS - 1;
        assert_eq!(spectrum_cell(full, 0.0, span, top), '█');
        assert_eq!(spectrum_cell(full, 0.0, span, top - 1), '█');
        assert_eq!(spectrum_cell(full, 0.0, span, 0), '█');
        // Silence draws nothing anywhere.
        for row in 0..SPECTRUM_CHART_ROWS {
            assert_eq!(spectrum_cell(0.0, 0.0, span, row), ' ');
        }
        // A held peak above the bar is a tick, and the bar is drawn where the
        // two share a row.
        assert_eq!(spectrum_cell(0.5, 1.0, span, top), '▀');
        assert_eq!(spectrum_cell(0.5, 1.0, span, top - 1), '█');
        assert_eq!(spectrum_cell(0.5, 0.5, span, top), ' ');
    }

    #[test]
    fn the_spectrum_draws_its_bands_its_scale_and_its_axis() {
        let p = SynthParams::defaults();
        let s = spectrum_state(EqTarget::Master, SPECTRUM_ROW_TARGET);
        let text = render_frame_with(&p, &s);
        for label in crate::eq::BAND_LABELS {
            assert!(text.contains(label), "no {} in:\n{text}", label);
        }
        // The scale runs from full scale to the bottom of a sixty decibel span.
        for mark in ["0 ┤", "-15", "-30", "-45", "-60"] {
            assert!(text.contains(mark), "no {:?} in:\n{text}", mark);
        }
        assert!(
            text.contains("━━ Spectrum [master] ━━"),
            "rendered:\n{text}"
        );
        for row in ["target", "range", "speed", "hold", "Reset Peaks"] {
            assert!(text.contains(row), "no {:?} in:\n{text}", row);
        }
        // The axis labels sit next to each other, so the one five character name
        // must not run into its neighbour.
        assert!(!text.contains("8k12.5k"), "the axis ran together:\n{text}");
        assert!(text.contains("12.5k"), "rendered:\n{text}");
        // It takes the Synth slot, so neither the table nor the summary is drawn.
        assert!(!text.contains("waveform"), "rendered:\n{text}");
        assert!(!text.contains("note length"), "rendered:\n{text}");
        assert!(!text.contains("reverb"), "rendered:\n{text}");
        // And no EQ bars: this panel is a readout, not an editor.
        assert!(!text.contains('┼'), "rendered:\n{text}");
    }

    #[test]
    fn the_spectrum_draws_the_level_the_audio_thread_published() {
        // Nothing is computed here: the panel draws what the callback stored.
        let p = SynthParams::defaults();
        let s = spectrum_state(EqTarget::Mid, SPECTRUM_ROW_TARGET);
        p.analyzer.taps[1].levels[7].set(1.0);
        let quiet = p.analyzer.taps[1].levels[9].get();
        assert_eq!(quiet, 0.0);

        let text = render_frame_with(&p, &s);
        // A full-scale band fills its whole column: five cells on each of the
        // twelve rows, and nothing anywhere else.
        assert_eq!(
            text.matches('█').count(),
            SPECTRUM_CHART_ROWS * SPECTRUM_CELL,
            "one column, filled:\n{text}"
        );
        let bar = text
            .lines()
            .find(|l| l.contains('█'))
            .unwrap_or_else(|| panic!("no bar at all:\n{text}"));
        assert!(
            bar.contains("   0 ┤"),
            "full scale belongs on the top row: {:?}",
            bar
        );
        // And in the 1 kHz column: the scale, then seven bands before it.
        let column = bar.chars().position(|c| c == '█').unwrap();
        assert_eq!(
            column,
            SPECTRUM_SCALE_WIDTH + 7 * SPECTRUM_CELL,
            "the bar is in the wrong column: {:?}",
            bar
        );
    }

    #[test]
    fn the_spectrum_only_reads_its_own_tap() {
        let p = SynthParams::defaults();
        p.analyzer.taps[3].levels[0].set(1.0);
        let low = render_frame_with(&p, &spectrum_state(EqTarget::Low, SPECTRUM_ROW_TARGET));
        assert_eq!(
            low.matches('█').count(),
            0,
            "the master tap must not draw on the low register's chart:\n{low}"
        );
        let master = render_frame_with(&p, &spectrum_state(EqTarget::Master, SPECTRUM_ROW_TARGET));
        assert_eq!(
            master.matches('█').count(),
            SPECTRUM_CHART_ROWS * SPECTRUM_CELL
        );
    }

    #[test]
    fn the_spectrum_target_is_the_eq_panels_target() {
        // One cursor, so tabbing from the EQ panel to the spectrum keeps the
        // part you were working on.
        let p = SynthParams::defaults();
        let mut s = state(Focus::Eq);
        s.eq_row = EQ_ROW_TARGET;
        let log = logger();
        eq_arrow(&mut s, &p, &log, KeyCode::Right, false);
        assert_eq!(EqTarget::from_index(s.eq_target).name(), "mid");

        s.focus = Focus::Spectrum;
        s.spectrum_row = SPECTRUM_ROW_TARGET;
        let text = render_frame_with(&p, &s);
        assert!(text.contains("━━ Spectrum [mid] ━━"), "rendered:\n{text}");

        // And the spectrum's own target row moves it back for the EQ panel too.
        adjust_spectrum(&mut s, &p, 1, &log);
        assert_eq!(EqTarget::from_index(s.eq_target).name(), "high");
    }

    #[test]
    fn the_spectrum_rows_cycle_what_they_say_they_cycle() {
        let p = SynthParams::defaults();
        let mut s = spectrum_state(EqTarget::Low, SPECTRUM_ROW_RANGE);
        s.spectrum_range = 0;
        let log = logger();

        let mut spans = Vec::new();
        for _ in 0..SPECTRUM_RANGES.len() {
            spans.push(SPECTRUM_RANGES[s.spectrum_range]);
            adjust_spectrum(&mut s, &p, 1, &log);
        }
        assert_eq!(spans, SPECTRUM_RANGES.to_vec());
        assert_eq!(s.spectrum_range, 0, "it wraps");
        adjust_spectrum(&mut s, &p, -1, &log);
        assert_eq!(SPECTRUM_RANGES[s.spectrum_range], 72.0);

        s.spectrum_row = SPECTRUM_ROW_SPEED;
        p.analyzer.release.set(ANALYZER_SPEEDS[0].0);
        let mut rates = Vec::new();
        for _ in 0..ANALYZER_SPEEDS.len() {
            rates.push(p.analyzer.release.get());
            adjust_spectrum(&mut s, &p, 1, &log);
        }
        assert_eq!(
            rates,
            ANALYZER_SPEEDS.iter().map(|(r, _)| *r).collect::<Vec<_>>()
        );
        assert_eq!(
            p.analyzer.release.get(),
            ANALYZER_SPEEDS[0].0,
            "three steps of a three entry list is back where it started"
        );

        s.spectrum_row = SPECTRUM_ROW_HOLD;
        let before = s.spectrum_hold;
        adjust_spectrum(&mut s, &p, 1, &log);
        assert_ne!(s.spectrum_hold, before);
        adjust_spectrum(&mut s, &p, -1, &log);
        assert_eq!(s.spectrum_hold, before, "a toggle is a toggle");
    }

    #[test]
    fn the_spectrum_speed_reaches_the_shared_parameter() {
        // The audio thread reads this, so a row that only moved a local would be
        // a control that does nothing.
        let p = SynthParams::defaults();
        let mut s = spectrum_state(EqTarget::Low, SPECTRUM_ROW_SPEED);
        let log = logger();
        adjust_spectrum(&mut s, &p, 1, &log);
        assert_eq!(p.analyzer.release.get(), ANALYZER_SPEEDS[2].0);
        assert!(p.analyzer.release.get() < 24.0, "it should be slower");
    }

    #[test]
    fn polling_folds_the_published_levels_into_peaks() {
        let p = SynthParams::defaults();
        let mut s = spectrum_state(EqTarget::Mid, SPECTRUM_ROW_HOLD);

        p.analyzer.taps[1].levels[4].set(0.5);
        s.poll_spectrum(&p);
        assert_eq!(s.spectrum_peaks[1][4], 0.5);

        // A quiet frame does not lower the peak: that is the whole point of it.
        p.analyzer.taps[1].levels[4].set(0.1);
        s.poll_spectrum(&p);
        assert_eq!(s.spectrum_peaks[1][4], 0.5);

        // A louder one raises it.
        p.analyzer.taps[1].levels[4].set(0.9);
        s.poll_spectrum(&p);
        assert_eq!(s.spectrum_peaks[1][4], 0.9);

        // And only the tap that published.
        assert_eq!(s.spectrum_peaks[0][4], 0.0);
        assert_eq!(s.spectrum_peaks[3][4], 0.0);
    }

    #[test]
    fn enter_on_the_reset_row_clears_the_peaks() {
        let mut s = spectrum_state(EqTarget::Low, SPECTRUM_ROW_RESET);
        let log = logger();
        for peaks in s.spectrum_peaks.iter_mut() {
            peaks[2] = 0.8;
        }
        assert!(row_is_action_button(&s), "the reset row is a button");
        spectrum_action(&mut s, &log);
        for peaks in s.spectrum_peaks.iter() {
            assert_eq!(peaks[2], 0.0);
        }

        // And the other rows are not buttons, so `Enter` still commits a chord.
        for row in [
            SPECTRUM_ROW_TARGET,
            SPECTRUM_ROW_RANGE,
            SPECTRUM_ROW_SPEED,
            SPECTRUM_ROW_HOLD,
        ] {
            let s = spectrum_state(EqTarget::Low, row);
            assert!(!row_is_action_button(&s), "row {}", row);
        }
        s.spectrum_row = SPECTRUM_ROW_RANGE;
        s.held.insert(KeyPosition::LeftIndex);
        assert_eq!(
            enter_intent(&s, false),
            EnterIntent::CommitChord { to_end: true }
        );
    }

    #[test]
    fn enter_on_the_other_spectrum_rows_changes_nothing() {
        let log = logger();
        for row in [
            SPECTRUM_ROW_TARGET,
            SPECTRUM_ROW_RANGE,
            SPECTRUM_ROW_SPEED,
            SPECTRUM_ROW_HOLD,
        ] {
            let mut s = spectrum_state(EqTarget::Low, row);
            for peaks in s.spectrum_peaks.iter_mut() {
                peaks[1] = 0.7;
            }
            let before = (s.eq_target, s.spectrum_range, s.spectrum_hold);
            spectrum_action(&mut s, &log);
            assert_eq!((s.eq_target, s.spectrum_range, s.spectrum_hold), before);
            assert_eq!(s.spectrum_peaks[0][1], 0.7, "row {}", row);
        }
    }

    #[test]
    fn the_spectrum_hold_row_decides_whether_a_peak_is_drawn() {
        let p = SynthParams::defaults();
        let mut s = spectrum_state(EqTarget::Low, SPECTRUM_ROW_HOLD);
        p.analyzer.taps[0].levels[0].set(0.05);
        s.poll_spectrum(&p);
        s.spectrum_peaks[0][0] = 1.0;

        s.spectrum_hold = true;
        let with = render_frame_with(&p, &s);
        assert!(with.contains('▀'), "the peak should be drawn:\n{with}");

        s.spectrum_hold = false;
        let without = render_frame_with(&p, &s);
        assert!(!without.contains('▀'), "no tick with hold off:\n{without}");
    }
    // ---- the settings that survive a restart ----

    /// A state whose settings are written to a file of its own.
    fn settings_state() -> (AppState, PathBuf) {
        // Per *thread*, not merely per process: three tests share this path, they
        // run in parallel, and one removing the file while another reads it is a
        // race that shows up as a flake rather than as a failure. It happened
        // under load, which is exactly when a flaky test is most expensive —
        // while something else is being measured.
        let mut path = std::env::temp_dir();
        path.push(format!(
            "chord-tool-tui-settings-{}-{:?}.toml",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut s = state(Focus::Transport);
        s.settings_path = path.clone();
        (s, path)
    }

    #[test]
    fn a_changed_setting_is_written_once_it_settles() {
        let (mut s, path) = settings_state();
        let p = SynthParams::defaults();
        let log = logger();

        // Nothing has moved, so nothing is written.
        s.poll_settings(&p, &log);
        assert!(!path.exists(), "an unchanged run writes nothing");

        // A tempo change marks them dirty rather than writing on the spot.
        s.transport.set_bpm(143);
        s.poll_settings(&p, &log);
        assert!(!path.exists(), "it waits for the arrows to stop");
        assert!(s.settings_dirty.is_some());

        // Once they have been still for the settle window, they go out.
        s.settings_dirty = Some(Instant::now() - SETTINGS_SETTLE - Duration::from_millis(10));
        s.poll_settings(&p, &log);
        assert!(path.exists(), "settled settings are written");
        assert!(s.settings_dirty.is_none(), "and the timer is cleared");

        let written = Settings::load(&path).unwrap();
        assert_eq!(written.bpm, 143);
        assert_eq!(written.key, s.transport.key());
        assert_eq!(written.master_volume, p.master_volume.get());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_settings_watch_notices_a_change_made_anywhere() {
        let (mut s, path) = settings_state();
        let p = SynthParams::defaults();
        let log = logger();

        // Through the Synth panel's master block, which is the *other* place the
        // same volume lives.
        MixerParam::MasterVolume.adjust(ctx(&p, &s.transport), 1);
        s.poll_settings(&p, &log);
        assert!(s.settings_dirty.is_some(), "a volume change counts");

        // And through the transport's own row, which is the linked one.
        s.settings_dirty = None;
        s.settings_seen = Settings::capture(&s.transport, p.master_volume.get());
        s.set_current_row(TRANSPORT_ROW_VOLUME);
        let before = p.master_volume.get();
        adjust_current_with(&mut s, &p, 1, false, &log);
        assert_eq!(p.master_volume.get(), before + 1.0, "the same value moved");
        s.poll_settings(&p, &log);
        assert!(s.settings_dirty.is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn flushing_writes_now_whatever_the_timer_says() {
        // The quit path: a tempo changed a tenth of a second before the second
        // Esc is a tempo the player expects to find next time.
        let (mut s, path) = settings_state();
        let p = SynthParams::defaults();
        let log = logger();
        s.transport.set_bpm(88);
        MixerParam::MasterVolume.adjust(ctx(&p, &s.transport), -2);
        s.transport.set_key(Key::new(63, Scale::Minor));

        s.poll_settings(&p, &log);
        assert!(!path.exists(), "still inside the settle window");
        s.flush_settings(&log);
        assert!(path.exists());

        let written = Settings::load(&path).unwrap();
        assert_eq!(written.bpm, 88);
        assert_eq!(written.key, Key::new(63, Scale::Minor));
        assert_eq!(written.master_volume, p.master_volume.get());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_settings_file_round_trips_through_the_watch() {
        // Write, reload, and put the values back where they came from — which is
        // what the next launch does with them.
        let (mut s, path) = settings_state();
        let p = SynthParams::defaults();
        let log = logger();
        s.transport.set_bpm(101);
        s.transport.set_key(Key::new(58, Scale::Minor));
        p.master_volume.set(6.5);
        s.poll_settings(&p, &log);
        s.flush_settings(&log);

        let loaded = Settings::load(&path).unwrap().clamped();
        let fresh = Transport::new(loaded.key);
        fresh.set_bpm(loaded.bpm);
        assert_eq!(fresh.bpm(), 101);
        assert_eq!(fresh.key(), Key::new(58, Scale::Minor));
        assert_eq!(loaded.master_volume, 6.5);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_transport_and_the_mixer_share_one_master_volume() {
        // Not a copy that agrees until it does not: one atomic, two rows.
        let (mut s, path) = settings_state();
        let p = SynthParams::defaults();
        let log = logger();
        s.set_current_row(TRANSPORT_ROW_VOLUME);
        assert_eq!(s.current_row(), TRANSPORT_ROW_VOLUME);

        let start = p.master_volume.get();
        adjust_current_with(&mut s, &p, 1, false, &log);
        assert_eq!(p.master_volume.get(), start + 1.0);
        // The Synth panel's own row reads the same number back.
        assert_eq!(
            MixerParam::MasterVolume.display(ctx(&p, &s.transport)),
            format!("{:.0}", start + 1.0)
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn enter_on_the_transport_volume_opens_the_same_editor_the_synth_panel_does() {
        let (mut s, path) = settings_state();
        let p = SynthParams::defaults();
        let log = logger();
        s.set_current_row(TRANSPORT_ROW_VOLUME);
        transport_action(&mut s, &p, TRANSPORT_ROW_VOLUME, &log);
        match s.edit {
            Edit::SynthCell {
                cell,
                initial,
                before,
            } => {
                assert_eq!(
                    cell,
                    SynthCell::Master {
                        row: master_row_of(MixerParam::MasterVolume).0,
                        col: master_row_of(MixerParam::MasterVolume).1,
                    }
                );
                assert_eq!(initial, p.master_volume.get());
                assert!(before.is_none(), "a level is a number, not a preset");
            }
            Edit::Bpm { .. } | Edit::TrackKey { .. } | Edit::None => {
                panic!("expected a volume edit, got something else")
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn every_master_setting_sits_in_exactly_one_cell() {
        // `master_row_of` names a cell by position, so the block must not list a
        // setting twice or leave one out.
        for param in MIXER_PARAMS {
            let hits = MASTER_ROWS
                .iter()
                .flat_map(|cells| cells.iter())
                .filter(|cell| **cell == param)
                .count();
            assert_eq!(hits, 1, "{:?} appears {} times", param, hits);
        }
        assert_eq!(
            MASTER_ROWS.iter().map(|cells| cells.len()).sum::<usize>(),
            MIXER_PARAMS.len()
        );
    }
    // ---- the chord log ----

    /// A state with a progression and a log, on the Sinko slot.
    fn history_state() -> AppState {
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
        s
    }

    /// The left-hand position that produces a degree, per `left_hand_degree`.
    fn key_for(degree: ScaleDegree) -> KeyPosition {
        match degree {
            ScaleDegree::I => KeyPosition::LeftIndex,
            ScaleDegree::II => KeyPosition::LeftPinky,
            ScaleDegree::III => KeyPosition::LeftRing,
            ScaleDegree::IV => KeyPosition::LeftMiddle,
            ScaleDegree::V => KeyPosition::LeftMiddle,
            ScaleDegree::VI => KeyPosition::LeftRing,
            ScaleDegree::VII => KeyPosition::LeftPinky,
        }
    }

    /// Let go, which is what ends a play: the chord stops sounding and the log's
    /// pending entry is dropped, so the next hold is a new play.
    fn release(s: &mut AppState) {
        s.held.clear();
        s.update_live_chord();
        s.poll_history();
    }

    /// Hold a chord the way the keyboard does, and let the log see it.
    fn sounding(s: &mut AppState, degree: ScaleDegree) {
        s.held.clear();
        s.held.insert(key_for(degree));
        s.registers.left = Some(Default::default());
        s.registers.right = Some(Default::default());
        s.update_live_chord();
        // The log's rule is about duration, so a test that wants a chord written
        // down has to say the chord has been held. That is the whole mechanism.
        s.poll_history();
        if let Some(pending) = s.pending_play.as_mut() {
            pending.since = Instant::now() - HISTORY_SETTLE;
        }
        s.poll_history();
    }

    #[test]
    fn a_chord_is_logged_once_it_has_settled_and_only_once() {
        let mut s = history_state();
        assert!(s.history.is_empty());
        sounding(&mut s, ScaleDegree::I);
        assert_eq!(s.history.len(), 1, "the hold wrote it down");
        assert_eq!(s.history.plays()[0].chord.degree, ScaleDegree::I);

        // Holding it longer does not write it down again: the play is the hold,
        // not the frame.
        for _ in 0..5 {
            s.poll_history();
        }
        assert_eq!(s.history.len(), 1);

        // Letting go and playing it again does, which is what "played twice"
        // means. The release is what makes it two plays rather than one long one.
        release(&mut s);
        sounding(&mut s, ScaleDegree::I);
        assert_eq!(s.history.len(), 2);
        assert_eq!(s.history.top()[0].count, 2, "and the ranking counted both");
    }

    #[test]
    fn a_shape_the_hands_only_passed_through_is_not_logged() {
        // Pressing the left hand and then the right hand of one shape sounds two
        // chords; only the one that was held is a chord that was played.
        let mut s = history_state();
        s.held.insert(key_for(ScaleDegree::I));
        s.registers.left = Some(Default::default());
        s.registers.right = Some(Default::default());
        s.update_live_chord();
        s.poll_history(); // the triad, not yet believed
        assert!(s.pending_play.is_some());
        assert!(s.history.is_empty(), "nothing written yet");

        // The right hand arrives, changing the chord, before the window closes.
        s.held.insert(KeyPosition::RightIndex);
        s.update_live_chord();
        s.poll_history();
        assert!(s.history.is_empty(), "the triad was never held");

        // Now the shape that was meant is held, and it is the one written down.
        if let Some(pending) = s.pending_play.as_mut() {
            pending.since = Instant::now() - HISTORY_SETTLE;
        }
        s.poll_history();
        assert_eq!(s.history.len(), 1);
        assert_eq!(s.history.plays()[0].chord.degree, ScaleDegree::I);
        assert!(
            s.history.plays()[0].chord.transformation.is_some(),
            "the transformation the right hand added"
        );
    }

    #[test]
    fn a_chord_nothing_can_hear_is_not_logged() {
        // With the loop running and no in-place audition armed, holding a chord
        // is silent — so calling it a play would be a lie about what was played.
        let mut s = history_state();
        s.transport.playing.store(true, Ordering::Relaxed);
        s.held.insert(KeyPosition::LeftPinky);
        s.update_live_chord();
        s.poll_history();
        if let Some(pending) = s.pending_play.as_mut() {
            pending.since = Instant::now() - HISTORY_SETTLE;
        }
        s.poll_history();
        assert!(s.history.is_empty(), "silent, so not played");

        // Arm the in-place audition and the same hold is audible, and logged.
        s.transport.set_audition_slot(Some(0));
        s.poll_history();
        if let Some(pending) = s.pending_play.as_mut() {
            pending.since = Instant::now() - HISTORY_SETTLE;
        }
        s.poll_history();
        assert_eq!(s.history.len(), 1, "auditioning in place is playing");
    }

    #[test]
    fn l_cycles_away_history_top_and_away_again() {
        let log = logger();
        let mut s = history_state();
        assert_eq!(s.history_view, HistoryView::Off);
        for expected in [HistoryView::Log, HistoryView::Top, HistoryView::Off] {
            handle_hotkey(&mut s, Hotkey::HistoryView, false, &log);
            assert_eq!(s.history_view, expected);
        }
        // Global: it works from any panel, because the point is to flip the log
        // up without moving the Tab cursor.
        s.focus = Focus::Transport;
        handle_hotkey(&mut s, Hotkey::HistoryView, false, &log);
        assert_eq!(s.history_view, HistoryView::Log);
    }

    #[test]
    fn opening_the_log_lands_on_the_newest_play() {
        let log = logger();
        let mut s = history_state();
        for degree in [ScaleDegree::I, ScaleDegree::V, ScaleDegree::VI] {
            s.history
                .record(Play::new(Chord::new(degree, None), Registers::default()));
        }
        handle_hotkey(&mut s, Hotkey::HistoryView, false, &log);
        assert_eq!(
            s.history_row, 2,
            "the newest is the one you just played, so it is where the cursor starts"
        );
        assert_eq!(s.history_selected().unwrap().chord.degree, ScaleDegree::VI);
    }

    #[test]
    fn moving_through_the_log_sounds_each_destination_for_its_moment() {
        let log = logger();
        let mut s = history_state();
        for degree in [ScaleDegree::I, ScaleDegree::V, ScaleDegree::VI] {
            s.history
                .record(Play::new(Chord::new(degree, None), Registers::default()));
        }
        s.history_view = HistoryView::Log;
        s.history_row = 2;

        // Back one: the sixth, sounded, with a deadline 200 ms out.
        handle_hotkey(&mut s, Hotkey::HistoryBack, false, &log);
        assert_eq!(s.history_row, 1);
        let (notes, until) = s.history_timed.as_ref().expect("a movement sounds");
        assert_eq!(*notes, vec![67, 71, 74], "the fifth, in C major");
        let window = until.duration_since(Instant::now());
        assert!(
            window <= HISTORY_AUDITION && window > HISTORY_AUDITION / 2,
            "the deadline should be about 200 ms out, got {:?}",
            window
        );

        // Forward again: the new chord replaces the old one rather than stacking
        // on it, which is the movement taking the voice over.
        handle_hotkey(&mut s, Hotkey::HistoryForward, false, &log);
        assert_eq!(s.history_row, 2);
        assert_eq!(
            s.history_timed.as_ref().unwrap().0,
            vec![69, 72, 76],
            "the sixth"
        );

        // And the ends hold rather than wrapping.
        for _ in 0..5 {
            handle_hotkey(&mut s, Hotkey::HistoryForward, false, &log);
        }
        assert_eq!(s.history_row, 2);
        for _ in 0..5 {
            handle_hotkey(&mut s, Hotkey::HistoryBack, false, &log);
        }
        assert_eq!(s.history_row, 0);
    }

    #[test]
    fn moving_or_playing_with_the_log_hidden_is_refused_rather_than_silent() {
        let log = logger();
        let mut s = history_state();
        s.history.record(Play::new(
            Chord::new(ScaleDegree::I, None),
            Registers::default(),
        ));
        for hotkey in [
            Hotkey::HistoryBack,
            Hotkey::HistoryForward,
            Hotkey::HistoryRecall,
            Hotkey::HistoryPlay,
        ] {
            assert_eq!(s.history_view, HistoryView::Off);
            handle_hotkey(&mut s, hotkey, false, &log);
            assert!(s.history_timed.is_none() && s.history_held.is_none());
            assert!(s.is_flashing(), "{:?} should say it could not act", hotkey);
            s.flash_until = None;
        }
    }

    #[test]
    fn f_puts_the_selected_row_back_in_the_registers() {
        let log = logger();
        let mut s = history_state();
        // A real gesture: the left-hand key the grammar reads as the fifth, and
        // an empty right hand, which is `Some(empty)` rather than `None` and so
        // resolves to the plain triad.
        let wanted = Registers {
            left: Some([key_for(ScaleDegree::V)].into_iter().collect()),
            right: Some(Default::default()),
        };
        s.history
            .record(Play::new(Chord::new(ScaleDegree::V, None), wanted.clone()));
        s.registers = Registers::default();
        s.history_view = HistoryView::Log;
        s.history_row = 0;

        handle_hotkey(&mut s, Hotkey::HistoryRecall, false, &log);
        assert_eq!(s.registers, wanted, "the gesture that produced it");
        // And the live chord follows, so the recall is audible.
        assert!(!s.transport.live_chord.lock().unwrap().clone().unwrap().is_empty());
    }

    #[test]
    fn r_holds_the_selected_chord_and_lets_it_go() {
        let log = logger();
        let mut s = history_state();
        s.history.record(Play::new(
            Chord::new(ScaleDegree::I, None),
            Registers::default(),
        ));
        s.history_view = HistoryView::Log;
        s.history_row = 0;

        handle_hotkey(&mut s, Hotkey::HistoryPlay, false, &log);
        assert_eq!(s.history_held.as_deref(), Some(&[60u8, 64, 67][..]));
        // Let go: nothing is held any more, and the audition's own half-second
        // release is what fades it.
        history_hold(&mut s, false, &log);
        assert!(s.history_held.is_none());
        assert_eq!(AUDITION_RELEASE, Duration::from_millis(500));
    }

    #[test]
    fn the_history_borrows_the_sinko_slot_and_keeps_its_height() {
        let mut s = history_state();
        s.history.record(Play::new(
            Chord::new(ScaleDegree::I, None),
            Registers::default(),
        ));
        let without = render_frame(&s).lines().count();
        s.history_view = HistoryView::Log;
        let with = render_frame(&s).lines().count();
        assert!(
            with <= without.max(35),
            "the log must not make the layout taller: {} against {}",
            with,
            without
        );
        assert!(with <= DEFAULT_SCREEN.height, "{} rows", with);

        let text = render_frame(&s);
        assert!(
            text.contains(" History [1 played · 1 chords]"),
            "rendered:\n{text}"
        );
        assert!(
            !text.contains("quant"),
            "the rhythm grid is covered:\n{text}"
        );

        s.history_view = HistoryView::Top;
        let text = render_frame(&s);
        assert!(text.contains(" Top [1 chords · 1 played]"), "rendered:\n{text}");
        assert!(!text.contains("quant"), "rendered:\n{text}");
    }

    #[test]
    fn the_top_view_ranks_and_marks_what_is_in_the_progression() {
        let mut s = history_state();
        for degree in [
            ScaleDegree::I,
            ScaleDegree::V,
            ScaleDegree::VI,
            ScaleDegree::I,
            ScaleDegree::III,
        ] {
            s.history
                .record(Play::new(Chord::new(degree, None), Registers::default()));
        }
        s.history_view = HistoryView::Top;
        let text = render_frame(&s);
        let rows: Vec<&str> = text.lines().filter(|l| l.contains('(')).collect();
        assert!(rows[0].contains("4") && rows[0].contains('C'), "most played first: {:?}", rows[0]);
        assert!(rows[0].contains('●'), "C is in the progression: {:?}", rows[0]);
        // The mediant is not in the seeded progression, so it is the row without
        // a marker — and the only one.
        let third = rows
            .iter()
            .find(|row| row.contains("(III)"))
            .unwrap_or_else(|| panic!("no mediant row:\n{text}"));
        assert!(!third.contains('●'), "the mediant is not in it: {:?}", third);
        assert_eq!(
            rows.iter().filter(|row| !row.contains('●')).count(),
            1,
            "only the mediant is unmarked:\n{text}"
        );
    }

    #[test]
    fn an_empty_log_says_so_rather_than_drawing_nothing() {
        let mut s = history_state();
        s.history_view = HistoryView::Log;
        let text = render_frame(&s);
        assert!(text.contains("nothing played yet"), "rendered:\n{text}");
        assert!(
            text.contains(" History [0 played · 0 chords]"),
            "rendered:\n{text}"
        );
    }

    #[test]
    fn the_panel_the_log_covers_does_not_answer_its_own_keys() {
        // The rhythm panel is behind the log, so its rows are not on screen:
        // `Enter` and the arrows must not act on what you cannot see. The log is
        // `l` away, and then they work again.
        let log = logger();
        let mut s = history_state();
        s.sinko_row = SINKO_ROW_SMOOTH;
        let before = s.sinko_smooth;

        s.history_view = HistoryView::Log;
        adjust_current_with(&mut s, &SynthParams::defaults(), 1, false, &log);
        primary_action_without_synth(&mut s, &log);
        assert_eq!(s.sinko_smooth, before, "not while it is covered");

        s.history_view = HistoryView::Off;
        adjust_current_with(&mut s, &SynthParams::defaults(), 1, false, &log);
        assert_ne!(s.sinko_smooth, before, "and again once it is back");
    }

    /// `primary_action` needs a `Synth`, which no test can build. The Sinko branch
    /// is what is being checked, so call the half that does not.
    fn primary_action_without_synth(state: &mut AppState, logger: &Logger) {
        if state.history_view.is_on() {
            return;
        }
        sinko_action(state, state.current_row(), logger);
    }

    #[test]
    fn the_hotkey_row_above_home_is_the_one_the_document_names() {
        // The five keys the log owns, by the character they produce on the active
        // layout, which is what the reference table promises.
        let cases = [
            ('f', Hotkey::HistoryRecall),
            ('g', Hotkey::HistoryBack),
            ('c', Hotkey::HistoryForward),
            ('r', Hotkey::HistoryPlay),
            ('l', Hotkey::HistoryView),
        ];
        for (typed, expected) in cases {
            let pos = ACTIVE_LAYOUT.position(typed).expect("the key is mapped");
            assert_eq!(pos.hotkey(), Some(expected), "{}", typed);
            // And none of them is a chord key, so they are usable with both hands
            // holding a chord.
            assert!(!pos.is_home_row(), "{} must not be in the grammar", typed);
        }
    }
}
