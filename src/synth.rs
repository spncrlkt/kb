//! Polyphonic synth: cpal stream driven by lock-free shared atomics.
//!
//! Three channels (low, mid, high), each with its own sound-design params
//! and its own pair of voice pools: one for the progression, one for the
//! preview chord. Voices support linear portamento (glide)
//! and auto-release after a fixed hold time.

use std::error::Error;
use std::f32::consts::{PI, TAU};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::{Deserialize, Serialize};

use crate::analyzer::{Analyzer, TAPS as ANALYZER_TAPS};
use crate::eq::{Eq, EqCurve, EqState};
use crate::fx::{Fx, FxKind, FxSubtype, CHAIN_SLOTS, FX_PARAMS};
use crate::fx_dsp::FxBank;
use crate::voice::{ComposedChannel, MixerSettings, VoicePatch};

// -----------------------------------------------------------------------------
// Voice pool sizes
// -----------------------------------------------------------------------------

/// Most unison voices one note can stack.
///
/// This is a pool-size multiplier, not a limit on the sound: it is how many
/// audio voices *each* allocated note owns, whether or not a voice uses them.
/// Four is the widest a detuned stack gets before it stops reading as one
/// instrument and starts reading as a chord.
pub const UNISON_MAX: usize = 4;

/// Notes in one stab group: the low channel takes one, the mid up to four and
/// the high one. The widest voicing this tool can produce is six notes, so that
/// covers it.
const LOW_NOTES: usize = 1;
const MID_NOTES: usize = 4;
const HIGH_NOTES: usize = 1;

/// Voices in one stab group.
///
/// Each note owns [`UNISON_MAX`] consecutive voices, so a note's stack is a
/// contiguous slice and a trigger walks it in order.
const GROUP_LOW: usize = LOW_NOTES * UNISON_MAX;
const GROUP_MID: usize = MID_NOTES * UNISON_MAX;
const GROUP_HIGH: usize = HIGH_NOTES * UNISON_MAX;

/// Every parameter's adjustable range, in one place.
///
/// The Synth panel clamps to these values, and a test asserts every shipped
/// sound already sits inside them — because a built-in the arrows would *snap*
/// is one that changes the first time it is touched. Keeping both sides of
/// that contract reading the same constants is the only way it can hold.
///
/// Pairs are `(min, max)`, inclusive.
pub mod range {
    pub const VOLUME: (f32, f32) = (0.0, 7.0);
    /// White noise mixed alongside the oscillator.
    pub const NOISE_LEVEL: (f32, f32) = (0.0, 1.0);
    /// As long as a decay: a pad swell and a slow filter contour both want
    /// seconds, and capping the amp attack at half a second is what stopped
    /// `String Pad` from ever being a swell.
    pub const ATTACK: (f32, f32) = (0.001, 2.0);
    /// Eight seconds, not two. Two was enough for every sound the voice could
    /// make before it could make a bell or a plucked string: a tube has to ring
    /// for longer than a pad, and a struck string's own decay runs to eight.
    pub const DECAY: (f32, f32) = (0.001, 8.0);
    pub const SUSTAIN: (f32, f32) = (0.0, 1.0);
    pub const RELEASE: (f32, f32) = (0.001, 8.0);
    pub const ENV_CURVE: (f32, f32) = (0.0, 1.0);
    pub const GLIDE: (f32, f32) = (0.0, 2.0);
    pub const CUTOFF: (f32, f32) = (200.0, 8000.0);
    /// Just short of self-oscillation, which the linear SVF cannot survive.
    pub const RESONANCE: (f32, f32) = (0.0, 0.99);
    /// Signed: negative closes the filter as the contour falls.
    pub const FILTER_ENV: (f32, f32) = (-1.0, 1.0);
    pub const FILTER_ATTACK: (f32, f32) = (0.001, 2.0);
    pub const FILTER_DECAY: (f32, f32) = (0.001, 2.0);
    pub const KEY_TRACK: (f32, f32) = (0.0, 1.0);
    pub const LFO_PITCH: (f32, f32) = (0.0, 1.0);
    pub const LFO_CUTOFF: (f32, f32) = (0.0, 1.0);
    pub const LFO_AMP: (f32, f32) = (0.0, 1.0);
    pub const LFO_PWM: (f32, f32) = (0.0, 1.0);
    /// Never fully open or fully shut: at either end the square is a DC offset
    /// rather than a sound.
    pub const PULSE_WIDTH: (f32, f32) = (0.05, 0.95);
    /// Whole voices only.
    pub const UNISON: (f32, f32) = (1.0, 4.0);
    pub const DETUNE: (f32, f32) = (0.0, 50.0);
    /// Filter drive: a pre-gain into a saturator on the way into the filter.
    /// One number rather than a pre-gain and a make-up gain, because the knob it
    /// imitates has one.
    pub const DRIVE: (f32, f32) = (0.0, 1.0);
    /// Velocity to cutoff, 0..1 = 0..4 octaves down at no velocity.
    ///
    /// Zero at full velocity, so a patch that never accents anything is not
    /// darkened by this row merely being in the file.
    pub const VEL_CUTOFF: (f32, f32) = (0.0, 1.0);
    /// Velocity to pulse width: 0..1 = a little over 40 % of the cycle either
    /// side of full velocity.
    pub const VEL_PWM: (f32, f32) = (0.0, 1.0);
    /// Wavetable position, 0..1: 0 is the chosen waveform alone, 1 is the next
    /// one in its octave group.
    pub const POSITION: (f32, f32) = (0.0, 1.0);
    /// Phase distortion, 0..1. Zero is the unwarped phase, exactly.
    pub const PHASE_DIST: (f32, f32) = (0.0, 1.0);
    /// The second oscillator's interval, in semitones.
    pub const OSC2_INTERVAL: (f32, f32) = (-24.0, 24.0);
    pub const OSC2_LEVEL: (f32, f32) = (0.0, 1.0);
    /// How hard the second oscillator bends the first one.
    ///
    /// One depth for all three domains of [`FmMode`]: what the row means is
    /// whichever of phase, hertz or octaves the mode row has chosen.
    pub const OSC2_FM: (f32, f32) = (0.0, 1.0);
    /// The oscillator bending its own phase with its own last sample.
    ///
    /// The top of the range is deliberately past the point where it settles: a
    /// sine folded back on itself becomes a saw, and a saw folded back on itself
    /// becomes noise, which is a usable percussion and breath source rather than
    /// a fault.
    pub const FEEDBACK: (f32, f32) = (0.0, 1.0);
    /// The two oscillators multiplied together, mixed in alongside them.
    pub const OSC2_RING: (f32, f32) = (0.0, 1.0);
    /// How long a plucked string rings, in seconds to -60 dB.
    pub const PLUCK_DECAY: (f32, f32) = (0.05, 8.0);
    /// How fast the string's upper partials die, 0..1.
    pub const PLUCK_DAMP: (f32, f32) = (0.0, 1.0);
    /// How long the string is excited, as a fraction of one period.
    pub const PLUCK_BURST: (f32, f32) = (0.05, 1.0);
    pub const TRANSPOSE: (f32, f32) = (-24.0, 24.0);
    pub const REVERB_SEND: (f32, f32) = (0.0, 1.0);
    pub const PAN: (f32, f32) = (-1.0, 1.0);
    /// Slow enough to hear as movement, fast enough to hear as vibrato.
    pub const LFO_RATE: (f32, f32) = (0.05, 20.0);
    pub const REVERB_MIX: (f32, f32) = (0.0, 1.0);
    pub const MASTER_VOLUME: (f32, f32) = (0.0, 7.0);
    /// One band of the equaliser. The range lives in `eq` beside the band
    /// layout it belongs to, and is re-exported here so the panel and the
    /// library are checked against the same numbers.
    pub const EQ_GAIN: (f32, f32) = crate::eq::GAIN_RANGE;
}

/// Peak detune spread, in cents either side of centre.
const MAX_DETUNE_CENTS: f32 = range::DETUNE.1;

/// Longest portamento time, in seconds.
const MAX_GLIDE_SECS: f32 = range::GLIDE.1;

/// Stab groups, one per overlapping take.
///
/// Shared with the arrangement planner, which never puts two simultaneous hits
/// in the same group while a free one exists — so the number here is the deepest
/// a stack of takes can sound before something has to be cut.
pub const STAB_GROUPS: usize = crate::arrangement::RHYTHM_LAYERS;

/// The voice the stopped-transport audition speaks on.
///
/// One past the scheduler's groups, so a chord being tried out can never
/// retrigger or cut a chord the loop is playing — and so stopping and starting
/// the transport cannot leave a half-released audition note in a group the
/// scheduler is about to reuse.
pub const AUDITION_GROUP: usize = STAB_GROUPS;

/// The click pool: one voice, used by the metronome.
///
/// Its own voice rather than a stab group, because a click is wanted while a
/// take is being recorded — the very moment the groups are busy with the
/// progression — and it must not retrigger anything the player is hearing.
const CLICK_VOICES: usize = 1;

/// One metronome timbre: the pitch of the strong and weak click, how long the
/// pitch sweep in takes, and how long the blip rings.
///
/// Generated rather than sampled, like everything else here: a click is a very
/// short voice on the mid channel, and "which timbre" is really "how high, how
/// sharp, how long".
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ClickSound {
    pub name: &'static str,
    pub strong_note: u8,
    pub weak_note: u8,
    /// The pitch-sweep length: the click's edge. Near zero is a tick.
    pub attack_secs: f32,
    /// How long the blip rings before it auto-releases.
    pub hold_secs: f32,
}

/// The timbres the metronome panel offers, coarsest first.
pub const CLICK_SOUNDS: [ClickSound; 5] = [
    ClickSound {
        name: "Blip",
        strong_note: 84,
        weak_note: 79,
        attack_secs: 0.002,
        hold_secs: 0.030,
    },
    ClickSound {
        name: "Tick",
        strong_note: 96,
        weak_note: 91,
        attack_secs: 0.001,
        hold_secs: 0.012,
    },
    ClickSound {
        name: "Wood",
        strong_note: 72,
        weak_note: 67,
        attack_secs: 0.004,
        hold_secs: 0.018,
    },
    ClickSound {
        name: "Beep",
        strong_note: 65,
        weak_note: 60,
        attack_secs: 0.006,
        hold_secs: 0.060,
    },
    ClickSound {
        name: "Two Tone",
        strong_note: 90,
        weak_note: 74,
        attack_secs: 0.002,
        hold_secs: 0.035,
    },
];

// -----------------------------------------------------------------------------
// SharedF32
// -----------------------------------------------------------------------------

#[derive(Clone)]
pub struct SharedF32(Arc<AtomicU32>);

impl SharedF32 {
    pub fn new(v: f32) -> Self {
        SharedF32(Arc::new(AtomicU32::new(v.to_bits())))
    }
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
    pub fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
}

// -----------------------------------------------------------------------------
// Waveform
// -----------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Waveform {
    #[default]
    Sine = 0,
    Saw = 1,
    Square = 2,
    Triangle = 3,
    /// White noise instead of a tone.
    ///
    /// The one source that is not pitch-dependent, which is what makes hats,
    /// wind and breath possible. A channel with this waveform ignores its own
    /// frequency entirely, and a chord voiced across several registers stacks
    /// several independent noise streams rather than one.
    Noise = 4,
    /// A plucked string: a delay line with a damping filter in its loop, excited
    /// by a burst of noise.
    ///
    /// Not a shape at a phase — it is a *model*, and the only waveform here that
    /// remembers what it played a moment ago. That is what makes it read as a
    /// string rather than as a filtered saw: the partials of a real string are
    /// already inharmonic by a few cents and die at different rates, and a
    /// feedback loop gets both for free.
    ///
    /// It sits *before* the table-backed tail on purpose. The tail's index into
    /// [`crate::wavetable::TIMBRES`] is its discriminant minus
    /// [`WAVETABLE_FIRST`], and a variant appended after it would shift every
    /// table by one.
    Pluck = 5,

    // ---- the table-backed timbres ----
    //
    // A stored single cycle rather than a formula, which is the only way to get
    // an additive spectrum out of one oscillator: nine harmonics summed per
    // sample per voice would cost more than the rest of the callback put
    // together, and the same nine summed once into a table cost one lookup.
    // `wavetable` owns the recipes; the names below are indices into it, and a
    // test holds the two lists together.
    OrganFull = 6,
    OrganJazz = 7,
    OrganBright = 8,
    OrganHollow = 9,
    Metallic = 10,
    Vox = 11,
    GlassTone = 12,
    Mellow = 13,
    Buzz = 14,
    Principal = 15,
    Clarinet = 16,
    Reed = 17,
    Piano = 18,
    Vibes = 19,
    Nylon = 20,
    VoxAah = 21,
    VoxOoh = 22,
    Gedeckt = 23,
}

/// Where [`Waveform::stable_id`] numbers the table-backed waveforms from.
///
/// High enough that no plausible number of computed shapes reaches it, so a
/// shape added to that group cannot collide with a table in a golden master.
#[cfg(test)]
const TABLE_ID_BASE: i32 = 1000;

/// The first `Waveform` that reads a table rather than computing a shape.
///
/// Everything before it is a formula, and `from_f32` and the panel both depend
/// on the variants being in discriminant order from here.
const WAVETABLE_FIRST: usize = Waveform::OrganFull as usize;

impl Waveform {
    /// Every waveform, **in discriminant order**.
    ///
    /// The order is load-bearing: the panel cycles this list, `from_f32` indexes
    /// it, and the table-backed tail of it has to line up with
    /// `wavetable::TIMBRES`. Tests pin all three.
    pub const ALL: [Waveform; 24] = [
        Waveform::Sine,
        Waveform::Saw,
        Waveform::Square,
        Waveform::Triangle,
        Waveform::Noise,
        Waveform::Pluck,
        Waveform::OrganFull,
        Waveform::OrganJazz,
        Waveform::OrganBright,
        Waveform::OrganHollow,
        Waveform::Metallic,
        Waveform::Vox,
        Waveform::GlassTone,
        Waveform::Mellow,
        Waveform::Buzz,
        Waveform::Principal,
        Waveform::Clarinet,
        Waveform::Reed,
        Waveform::Piano,
        Waveform::Vibes,
        Waveform::Nylon,
        Waveform::VoxAah,
        Waveform::VoxOoh,
        Waveform::Gedeckt,
    ];

    /// What the panel's `waveform` row shows.
    pub fn name(self) -> &'static str {
        match self {
            Waveform::Sine => "sine",
            Waveform::Saw => "saw",
            Waveform::Square => "square",
            Waveform::Triangle => "triangle",
            Waveform::Noise => "noise",
            Waveform::Pluck => "pluck",
            other => {
                crate::wavetable::TIMBRES[other
                    .table()
                    .expect("every remaining variant is table-backed")]
                .label
            }
        }
    }

    /// The waveforms a *second* oscillator may be set to.
    ///
    /// Everything but `pluck`. A plucked string is not a shape at a phase — it is
    /// a delay line with a burst in it — so a voice would need a second string
    /// buffer to have two of them, and the row simply does not offer it. The list
    /// is a separate constant rather than a filter so the panel's cycle stays a
    /// plain index walk.
    pub const OSC2_WAVEFORMS: [Waveform; 23] = [
        Waveform::Sine,
        Waveform::Saw,
        Waveform::Square,
        Waveform::Triangle,
        Waveform::Noise,
        Waveform::OrganFull,
        Waveform::OrganJazz,
        Waveform::OrganBright,
        Waveform::OrganHollow,
        Waveform::Metallic,
        Waveform::Vox,
        Waveform::GlassTone,
        Waveform::Mellow,
        Waveform::Buzz,
        Waveform::Principal,
        Waveform::Clarinet,
        Waveform::Reed,
        Waveform::Piano,
        Waveform::Vibes,
        Waveform::Nylon,
        Waveform::VoxAah,
        Waveform::VoxOoh,
        Waveform::Gedeckt,
    ];

    /// A number that names the *sound* rather than the variant.
    ///
    /// The discriminant is an implementation detail: variants are declared in the
    /// order the panel cycles them, so inserting one into the computed group
    /// renumbers every table behind it — which is exactly what `pluck` did, and
    /// what changed not a single sample. Anything pinning a sound — a golden
    /// master, a test that records what a patch renders — has to name the
    /// waveform by something the sound depends on.
    ///
    /// A computed shape is its discriminant. They are declared first and a new
    /// one appends at the end of the group, so a variant already in a file or a
    /// recording never changes its number. A table is its place in `TIMBRES`,
    /// offset far enough above the computed group that appending a shape cannot
    /// reach it.
    #[cfg(test)]
    pub fn stable_id(self) -> i32 {
        match self.table() {
            Some(index) => TABLE_ID_BASE + index as i32,
            None => self as i32,
        }
    }

    /// Which wavetable this waveform reads, or `None` for the computed shapes.
    ///
    /// An `if` rather than `then_some`, which evaluates its argument eagerly and
    /// so subtracts the offset even when there is nothing to offset — underflow,
    /// and a panic in a debug build, for `Sine`.
    pub const fn table(self) -> Option<usize> {
        let index = self as usize;
        if index >= WAVETABLE_FIRST {
            Some(index - WAVETABLE_FIRST)
        } else {
            None
        }
    }

    pub fn from_f32(v: f32) -> Self {
        // A straight index into `ALL`, which is only valid because the variants
        // are declared in order from zero. That is asserted, not assumed.
        if v.is_finite() {
            let index = v.round() as i32;
            if (0..Waveform::ALL.len() as i32).contains(&index) {
                return Waveform::ALL[index as usize];
            }
        }
        Waveform::Sine
    }
}

/// Which of the Chamberlin state-variable filter's outputs is heard.
///
/// The filter computes all three every sample anyway, so this is a choice of
/// which one to pass on, not extra arithmetic.
/// Lowpass is the default because that is the only thing this filter could do
/// before the choice existed — so it is also what a file that predates it
/// means.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterType {
    #[default]
    Lowpass = 0,
    Highpass = 1,
    Bandpass = 2,
    /// What is left when the band is taken out: `low + high`. The
    /// state-variable filter computes both anyway, so this costs one add.
    Notch = 3,
    /// The band alone, out of phase with the rest: `low - high`. Free for the
    /// same reason, and the reason this filter has five outputs rather than
    /// three.
    Peak = 4,
}

impl FilterType {
    pub const ALL: [FilterType; 5] = [
        FilterType::Lowpass,
        FilterType::Highpass,
        FilterType::Bandpass,
        FilterType::Notch,
        FilterType::Peak,
    ];

    /// The short form, which is what the panel row shows: `LP`, `HP`, `BP`,
    /// `NT`, `PK`.
    pub fn short_name(self) -> &'static str {
        match self {
            FilterType::Lowpass => "LP",
            FilterType::Highpass => "HP",
            FilterType::Bandpass => "BP",
            FilterType::Notch => "NT",
            FilterType::Peak => "PK",
        }
    }

    pub fn from_f32(v: f32) -> Self {
        match v as i32 {
            1 => FilterType::Highpass,
            2 => FilterType::Bandpass,
            3 => FilterType::Notch,
            4 => FilterType::Peak,
            _ => FilterType::Lowpass,
        }
    }
}

/// Which waveform the `position` row morphs towards, for every waveform.
///
/// Computed at compile time: it is two dozen constants and there is no reason
/// for the audio thread to look anything up to find them.
const MORPH_NEXT: [Waveform; Waveform::ALL.len()] = build_morph_table();

const fn build_morph_table() -> [Waveform; Waveform::ALL.len()] {
    let mut next = Waveform::ALL;
    let mut index = 0;
    while index < Waveform::ALL.len() {
        next[index] = morph_partner(index);
        index += 1;
    }
    next
}

/// The next waveform in `ALL` that sounds at the same octave, wrapping within
/// that group.
///
/// The group is an **octave group** and not the whole list, because two tables
/// an octave apart cannot be blended at one phase: the drawbar registrations
/// advance their phase at half the rate of everything else, so a blend across
/// that line would play one of the two an octave out. Wrapping within the group
/// keeps every pair honest, and it is also why the two halves of the palette
/// morph among themselves rather than into each other.
///
/// `pluck` is its own group. A string is not a spectrum — there is nothing to
/// blend it with — so the position row leaves a plucked voice alone.
const fn morph_partner(index: usize) -> Waveform {
    let waveform = Waveform::ALL[index];
    if matches!(waveform, Waveform::Pluck) {
        return Waveform::Pluck;
    }
    let octave = waveform_octave(waveform);
    let len = Waveform::ALL.len();
    let mut step = 1;
    while step <= len {
        let candidate = Waveform::ALL[(index + step) % len];
        if !matches!(candidate, Waveform::Pluck) && waveform_octave(candidate) == octave {
            return candidate;
        }
        step += 1;
    }
    waveform
}

/// How far below the played note a waveform sounds.
///
/// Zero for a computed shape, which is a shape at the pitch that was played, and
/// the table's own answer for a stored cycle.
const fn waveform_octave(waveform: Waveform) -> i8 {
    match waveform.table() {
        Some(index) => crate::wavetable::octave(index),
        None => 0,
    }
}

/// The rate a waveform's phase advances at, given the note's frequency.
///
/// A drawbar table's fundamental is the 16' drawbar, an octave below the key
/// that was pressed, so its phase has to advance at half the rate. Exactly half
/// — a power of two — so the unshifted case stays a plain `freq` and every old
/// sound renders unchanged.
fn table_rate(waveform: Waveform, freq: f32) -> f32 {
    match waveform.table().map(crate::wavetable::octave) {
        Some(0) | None => freq,
        Some(-1) => freq * 0.5,
        Some(other) => freq * (2.0f32).powi(other as i32),
    }
}

/// Advance a phase by a step, landing inside one cycle.
///
/// The step may be negative — a cross-modulated rate can run the oscillator
/// backwards, which is through-zero FM and is well defined for an accumulator —
/// so `fract` will not do it: `fract` of a negative number is negative, and a
/// negative phase reads a table at index zero rather than wrapping. The common
/// case is one compare; only a step that leaves the cycle pays for the
/// remainder.
fn advance(phase: f32, step: f32) -> f32 {
    if !step.is_finite() {
        return phase;
    }
    let next = phase + step;
    if (0.0..1.0).contains(&next) {
        next
    } else {
        next.rem_euclid(1.0)
    }
}

/// A cheap saturator: unity slope at zero, bounded by one, monotonic.
///
/// `tanh` would be the obvious choice and is what the effect rack uses, but the
/// rack runs once on the bus and this runs once per voice — a hundred and
/// twenty-one times a sample in the worst case, where a libm call is the whole
/// cost of the feature. `x / (1 + |x|)` is one divide, has the same shape to
/// within a couple of decibels everywhere that matters, and cannot leave the
/// range.
fn soft_clip(x: f32) -> f32 {
    x / (1.0 + x.abs())
}

/// The Casio-style phase-distortion warp: a two-segment bend of the cycle.
///
/// The breakpoint sits at `0.5 - amount * 0.499`, so at amount 0 it is the
/// middle of the cycle and each half of the wave is mapped onto itself: the
/// rising half is multiplied by `0.5 / 0.5`, which is exactly one, and the
/// falling half by `0.5 / (1 - 0.5)`, also exactly one. As the breakpoint moves
/// towards zero the rising half is squeezed into a shorter and shorter span and
/// the falling half is stretched to fill the rest, which turns a sine into a
/// ramp without moving its period. That is the whole trick, and the reason it
/// sounds unlike a filter: the waveform's shape changes and its pitch does not.
fn warp_phase(phase: f32, amount: f32) -> f32 {
    let breakpoint = 0.5 - amount.clamp(0.0, 1.0) * 0.499;
    if phase < breakpoint {
        phase * (0.5 / breakpoint)
    } else {
        0.5 + (phase - breakpoint) * (0.5 / (1.0 - breakpoint))
    }
}

/// Pre-gain at full drive.
const DRIVE_PRE_GAIN: f32 = 9.0;

/// The frequency deviation at full depth in `FmMode::Linear`.
///
/// A fixed number of hertz, which is the whole point: at 1.5 kHz a bass note
/// gets several times its own frequency in swing and a note three octaves up
/// gets a fraction of it.
const LINEAR_FM_HZ: f32 = 1500.0;

/// How many octaves either way `FmMode::Expo` can push the carrier.
const EXPO_FM_OCTAVES: f32 = 2.5;

/// The mean of `2^(octaves · sin)`, which is what an exponential cross-modulation
/// does to the pitch.
///
/// `E[2^(D sin)]` is `I_0(D ln 2)`, a modified Bessel function, and dividing by
/// it is what keeps an exponential X-Mod in tune. Without it the mode is
/// unusable in a chord: the mean of a convex function of a zero-mean modulator
/// sits *above* the function of its mean — Jensen's inequality, and the reason
/// an analog X-Mod goes sharp — and the note plays up to a hundred and forty
/// cents high at the top of the row. That asymmetry inside a cycle is the sound
/// and is kept; only its average is taken back out.
///
/// The series is the small-argument one, `Σ (x/2)^{2k} / (k!)²`, six terms,
/// which is within a tenth of a percent over the range the row can reach and is
/// a handful of multiplies rather than a Bessel call. It is exact for a sine
/// modulator, which is what a cross-modulated patch almost always is; a saw or a
/// square modulator has a different mean and stays a few cents out, which is a
/// property of the waveform rather than a fault in this.
fn expo_mean_shift(octaves: f32) -> f32 {
    let half = octaves * std::f32::consts::LN_2 * 0.5;
    let square = half * half;
    let quartic = square * square;
    1.0 + square
        + quartic / 4.0
        + quartic * square / 36.0
        + quartic * quartic / 576.0
        + quartic * quartic * square / 14_400.0
}

/// The phase offset at full `feedback`, in cycles.
///
/// Smaller than the cross-modulation's, because a loop that reads its own output
/// is self-exciting: a little goes a long way, and the top of the range is meant
/// to be chaotic rather than merely bright.
const FEEDBACK_CYCLES: f32 = 0.5;

/// Where the ring modulator's direct-current blocker sits.
///
/// The product of two waves an octave apart has a constant term — a sine
/// multiplied by itself is half direct current — and this filter has unity gain
/// at DC, so the offset would ride into the reverb and the master limiter
/// exactly as the square wave's bias would.
const RING_DC_HZ: f32 = 20.0;

/// How far a full FM depth bends the carrier's phase, in cycles.
///
/// One cycle is a modulation index of about six radians, which is already
/// clangorous; a little over that leaves the top of the row somewhere to go
/// rather than making most of it unusable.
const FM_DEPTH_CYCLES: f32 = 1.4;

/// How far velocity can close the filter, in octaves, at full depth.
const VELOCITY_CUTOFF_OCTAVES: f32 = 4.0;

/// How far velocity can move the pulse width either side of full velocity.
const VELOCITY_PWM_SPAN: f32 = 0.45;

/// The most the damping control can take out of a string's loop.
///
/// Not all the way to one: the damping one-pole delays the loop as well as
/// damping it, and the delay it adds has to stay smaller than the shortest
/// string the top of the keyboard can ask for.
const MAX_PLUCK_DAMP: f32 = 0.8;

/// The lowest pitch a string is tuned for, and so the length it is sized for.
const MIN_STRING_HZ: f32 = 20.0;

/// Samples in one voice's string.
fn string_capacity(sample_rate: f32) -> usize {
    ((sample_rate / MIN_STRING_HZ).ceil() as usize + 4).max(16)
}

/// Which domain the second oscillator cross-modulates the first one in.
///
/// The three are genuinely different sounds, and the difference is where the
/// *index* goes as the note moves. Phase modulation spends a fixed number of
/// cycles per sample whatever the pitch, so a patch sounds the same in every
/// register. The two frequency domains do not.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FmMode {
    /// The modulator is added to the carrier's phase. A fixed index, and the
    /// sound of every DX-style patch: bells, tines, brass.
    #[default]
    Phase = 0,
    /// The modulator is added to the carrier's *frequency*, as a deviation in
    /// hertz with a fixed maximum.
    ///
    /// The index is the deviation over the modulator's own frequency, so it
    /// grows as the note falls: an octave down is twice the index. That is the
    /// growl — huge under a bass, barely there at the top of the keyboard — and
    /// it is the reason this is a mode of its own rather than a bigger number on
    /// the phase row. A deviation larger than the carrier's frequency runs the
    /// phase backwards, which is *through-zero* FM and comes free here because
    /// the phase is an accumulator.
    Linear = 1,
    /// The carrier's frequency is multiplied, which is what the analog
    /// cross-modulation this is named after does.
    ///
    /// The index is then the ratio between the two oscillators and does not
    /// change with the note at all, so the clang stays put across the keyboard.
    /// It also cannot pass through zero: the pitch rises further than it falls,
    /// which is the asymmetry an analog X-Mod has and a digital one does not.
    Expo = 2,
}

impl FmMode {
    pub const ALL: [FmMode; 3] = [FmMode::Phase, FmMode::Linear, FmMode::Expo];

    pub fn name(self) -> &'static str {
        match self {
            FmMode::Phase => "phase",
            FmMode::Linear => "linear",
            FmMode::Expo => "expo",
        }
    }

    pub fn from_f32(v: f32) -> Self {
        match v as i32 {
            1 => FmMode::Linear,
            2 => FmMode::Expo,
            _ => FmMode::Phase,
        }
    }
}

/// The shape a per-voice LFO runs through.
///
/// Deliberately the same four shapes the oscillator offers minus noise: a
/// random modulator is a different feature (sample-and-hold) with different
/// plumbing, and guessing at it here would be worse than not having it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LfoWave {
    #[default]
    Sine = 0,
    Triangle = 1,
    Square = 2,
    Saw = 3,
}

impl LfoWave {
    pub const ALL: [LfoWave; 4] = [
        LfoWave::Sine,
        LfoWave::Triangle,
        LfoWave::Square,
        LfoWave::Saw,
    ];

    pub fn name(self) -> &'static str {
        match self {
            LfoWave::Sine => "sine",
            LfoWave::Triangle => "triangle",
            LfoWave::Square => "square",
            LfoWave::Saw => "saw",
        }
    }

    pub fn from_f32(v: f32) -> Self {
        match v as i32 {
            1 => LfoWave::Triangle,
            2 => LfoWave::Square,
            3 => LfoWave::Saw,
            _ => LfoWave::Sine,
        }
    }
}

// -----------------------------------------------------------------------------
// ChannelParams / SynthParams
// -----------------------------------------------------------------------------

/// Thirteen band gains, shared with the audio thread.
///
/// The equaliser runs on the *bus*, not in the voice — see [`crate::eq`] — so a
/// voice never reads these. They live in `ChannelParams` anyway because the
/// composed channel is what travels between the UI and the audio layer, and
/// `apply_channel` and `capture_channel` have to stay exact inverses of one
/// another for a save and a load to round-trip.
#[derive(Clone)]
pub struct EqParams {
    gains: [SharedF32; crate::eq::EQ_BANDS],
}

impl EqParams {
    fn flat() -> Self {
        EqParams {
            gains: std::array::from_fn(|_| SharedF32::new(0.0)),
        }
    }

    pub fn curve(&self) -> EqCurve {
        EqCurve {
            gains: std::array::from_fn(|i| self.gains[i].get()),
        }
    }

    pub fn set(&self, curve: &EqCurve) {
        for (dst, gain) in self.gains.iter().zip(curve.gains.iter()) {
            dst.set(*gain);
        }
    }

    /// Move one band, leaving the other eleven where they are.
    ///
    /// What the EQ panel's arrows use: the live parameters are the only copy of
    /// the curve, so an edit has to be a single band rather than a whole-curve
    /// rewrite from a snapshot that may already be stale.
    pub fn set_band(&self, band: usize, db: f32) {
        if let Some(dst) = self.gains.get(band) {
            dst.set(db.clamp(range::EQ_GAIN.0, range::EQ_GAIN.1));
        }
    }

    pub fn band(&self, band: usize) -> f32 {
        self.gains.get(band).map(|g| g.get()).unwrap_or(0.0)
    }
}

/// One tap's published band levels, linear, 0..1 for a full-scale sine.
///
/// Linear rather than decibels on purpose: the audio thread publishes the level
/// it measured and the panel decides what scale to draw it on, so a display
/// change never reaches into the callback.
#[derive(Clone)]
pub struct AnalyzerTap {
    pub levels: [SharedF32; crate::eq::EQ_BANDS],
}

impl AnalyzerTap {
    fn silent() -> Self {
        AnalyzerTap {
            levels: std::array::from_fn(|_| SharedF32::new(0.0)),
        }
    }

    /// The last level published for one band.
    pub fn level(&self, band: usize) -> f32 {
        self.levels.get(band).map(|l| l.get()).unwrap_or(0.0)
    }
}

/// One effect slot's live controls.
///
/// A kind and a subtype are numbers here rather than a shared enum, because a
/// number is the one thing every other live parameter in this crate is. The
/// callback reads the pair back into an [`Fx`] once per buffer, and that is where
/// a combination a file or a keystroke left half-changed becomes something valid.
#[derive(Clone)]
pub struct FxSlotParams {
    pub kind: SharedF32,
    /// An index into the kind's own list of variants, not into the flat enum.
    pub subtype: SharedF32,
    pub params: [SharedF32; FX_PARAMS],
}

impl FxSlotParams {
    fn empty() -> Self {
        FxSlotParams::from_fx(&Fx::none())
    }

    fn from_fx(fx: &Fx) -> Self {
        FxSlotParams {
            kind: SharedF32::new(fx.kind.index() as f32),
            subtype: SharedF32::new(subtype_index(fx) as f32),
            params: std::array::from_fn(|i| SharedF32::new(fx.param(i))),
        }
    }

    /// The effect as it stands, made valid.
    pub fn fx(&self) -> Fx {
        Fx {
            kind: FxKind::from_index(self.kind.get() as i32),
            subtype: FxSubtype::SubtypeNone,
            params: std::array::from_fn(|i| self.params[i].get()),
        }
        .with_subtype_index(self.subtype.get())
        .normalised()
    }

    /// Write a whole effect into the slot.
    pub fn set(&self, fx: &Fx) {
        self.kind.set(fx.kind.index() as f32);
        self.subtype.set(subtype_index(fx) as f32);
        for (dst, value) in self.params.iter().zip(fx.params) {
            dst.set(value);
        }
    }

    /// Move to a variant of the current kind, loading that variant's defaults.
    ///
    /// The numbers move with the variant, because the numbers are what makes the
    /// variants different: `plate` and `hall` are the same four slots with a
    /// different size and damping, so keeping the previous variant's values would
    /// make the choice inaudible. A variant is a starting point, not a label.
    pub fn set_subtype_index(&self, index: usize) {
        let kind = FxKind::from_index(self.kind.get() as i32);
        let subtypes = kind.subtypes();
        let index = if index < subtypes.len() { index } else { 0 };
        self.subtype.set(index as f32);
        let defaults = kind.defaults(subtypes[index]);
        for (dst, value) in self.params.iter().zip(defaults) {
            dst.set(value);
        }
    }

    /// The variant this slot is on, as an index into its kind's list.
    pub fn subtype_index(&self) -> usize {
        let kind = FxKind::from_index(self.kind.get() as i32);
        let len = kind.subtypes().len();
        (self.subtype.get() as i32).rem_euclid(len as i32) as usize
    }

    pub fn set_kind(&self, kind: FxKind) {
        self.kind.set(kind.index() as f32);
        self.subtype.set(0.0);
        // The parameters belong to the kind that declared them, so a change of
        // kind writes that kind's defaults rather than leaving five numbers that
        // meant something else.
        let defaults = kind.defaults(kind.default_subtype());
        for (dst, value) in self.params.iter().zip(defaults) {
            dst.set(value);
        }
    }

    pub fn param(&self, index: usize) -> f32 {
        self.params.get(index).map(|p| p.get()).unwrap_or(0.0)
    }

    pub fn set_param(&self, index: usize, value: f32) {
        if let Some(slot) = self.params.get(index) {
            slot.set(value);
        }
    }
}

/// Where a subtype sits in its kind's own list.
fn subtype_index(fx: &Fx) -> usize {
    fx.kind
        .subtypes()
        .iter()
        .position(|subtype| *subtype == fx.subtype)
        .unwrap_or(0)
}

/// One register's insert chain: six slots, in order.
#[derive(Clone)]
pub struct FxChainParams {
    pub slots: [FxSlotParams; CHAIN_SLOTS],
}

impl FxChainParams {
    fn empty() -> Self {
        FxChainParams {
            slots: std::array::from_fn(|_| FxSlotParams::empty()),
        }
    }

    /// The chain as the audio layer takes it, once per buffer.
    pub fn chain(&self) -> [Fx; CHAIN_SLOTS] {
        std::array::from_fn(|i| self.slots[i].fx())
    }

    /// Whether anything in the rack is doing something.
    ///
    /// Checked once per buffer rather than per sample: a rack of six empty slots
    /// is the shipped state of every ensemble in the palette, and walking it
    /// eighteen times a sample to discover that would be the whole cost of the
    /// feature at rest.
    pub fn any(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| !FxKind::from_index(slot.kind.get() as i32).is_none())
    }

    pub fn set(&self, chain: &[Fx; CHAIN_SLOTS]) {
        for (slot, fx) in self.slots.iter().zip(chain) {
            slot.set(fx);
        }
    }
}

/// The live spectrum's shared block: one readout per register plus the mix, and
/// the release rate that shapes all four.
///
/// It rides in `SynthParams` because that is the one block the audio callback
/// and the UI both hold, but nothing here is part of a *sound*: it is a readout,
/// it is never serialised, and it never reaches `MixerSettings`.
#[derive(Clone)]
pub struct AnalyzerParams {
    /// Envelope release, in decibels per second.
    pub release: SharedF32,
    /// Whether anybody is looking at the readout, 0 or 1.
    ///
    /// The banks are 52 biquads a sample and they are a *readout*: they feed
    /// nothing in the mix, and the only consumer of the thirteen levels is the
    /// Spectrum panel — one of seven stops on the Tab key. So the interface says
    /// whether that panel is on screen, and the callback skips forty-eight
    /// filters a sample when it is not, which is most of the time.
    ///
    /// On by default, because off is a thing the *interface* decides and every
    /// other caller — the render harness, the benchmarks, a test that wants to
    /// watch the levels — should get a live analyser without having to ask for
    /// one.
    pub enabled: SharedF32,
    pub taps: [AnalyzerTap; ANALYZER_TAPS],
}

impl AnalyzerParams {
    fn defaults() -> Self {
        AnalyzerParams {
            release: SharedF32::new(crate::analyzer::SPEEDS[crate::analyzer::DEFAULT_SPEED].0),
            enabled: SharedF32::new(1.0),
            taps: std::array::from_fn(|_| AnalyzerTap::silent()),
        }
    }

    /// One tap's readout, clamped rather than panicking.
    pub fn tap(&self, tap: usize) -> &AnalyzerTap {
        self.taps.get(tap).unwrap_or(&self.taps[0])
    }
}

#[derive(Clone)]
pub struct ChannelParams {
    pub volume: SharedF32,
    pub waveform: SharedF32,
    /// How much white noise is mixed in *alongside* the oscillator, 0..1.
    ///
    /// Distinct from `Waveform::Noise`, which is noise *instead of* a tone. This
    /// is the snare/breath case: a body plus a burst of air on top of it.
    pub noise_level: SharedF32,
    /// Duty cycle of the square, 0.05..0.95.
    ///
    /// 0.5 is the even square this oscillator always used to be; anything else
    /// is the hollow, reedy half of its range.
    pub pulse_width: SharedF32,
    pub attack: SharedF32,
    pub decay: SharedF32,
    pub sustain: SharedF32,
    pub release: SharedF32,
    /// 0 keeps the segments straight lines; 1 bends them the way a capacitor
    /// does. Shaping only — the segment still takes exactly as long.
    pub env_curve: SharedF32,
    /// Portamento time in seconds, 0 for none.
    pub glide: SharedF32,
    pub cutoff: SharedF32,
    pub resonance: SharedF32,
    pub filter_type: SharedF32,
    /// How far the filter envelope moves the cutoff, -1..1, in octaves at full.
    pub filter_env: SharedF32,
    pub filter_attack: SharedF32,
    pub filter_decay: SharedF32,
    /// How much the cutoff follows the sounding pitch, 0..1.
    ///
    /// At 1 the cutoff tracks the keyboard one-for-one, so a sound keeps its
    /// brightness up the range instead of getting duller.
    pub key_track: SharedF32,
    /// LFO depth to pitch, 0..1 = 0..100 cents.
    pub lfo_pitch: SharedF32,
    /// LFO depth to cutoff, 0..1 = 0..4 octaves.
    pub lfo_cutoff: SharedF32,
    /// LFO depth to amplitude, 0..1 = 0..full tremolo.
    pub lfo_amp: SharedF32,
    /// LFO depth to the square's duty cycle, 0..1.
    ///
    /// Only audible on `square`, which is exactly the point: pulse-width
    /// modulation is what a string machine used instead of a chorus, and it is
    /// the reason the LFO has a fourth destination.
    pub lfo_pwm: SharedF32,
    /// Voices stacked per note, 1..4. Anything above 1 spreads across `detune`.
    pub unison: SharedF32,
    /// Peak detune spread in cents, each side of centre.
    pub detune: SharedF32,
    /// Drive into the filter, 0..1. Read only when non-zero, so every patch that
    /// does not use it renders exactly as it did before the control existed.
    pub drive: SharedF32,
    /// Velocity to cutoff, 0..1 = 0..4 octaves down at no velocity.
    pub vel_cutoff: SharedF32,
    /// Velocity to pulse width, 0..1.
    pub vel_pwm: SharedF32,
    /// Wavetable position, 0..1: this waveform blended into the next one in its
    /// octave group.
    pub position: SharedF32,
    /// Phase distortion, 0..1. Zero is exactly identity.
    pub phase_dist: SharedF32,
    /// The second oscillator's waveform, as a `Waveform` discriminant.
    pub osc2_waveform: SharedF32,
    /// Its interval from the first, in semitones.
    pub osc2_interval: SharedF32,
    /// How much of it is mixed in, 0..1.
    pub osc2_level: SharedF32,
    /// How hard it bends the first oscillator, 0..1.
    pub osc2_fm: SharedF32,
    /// Which domain that depth is spent in, as an [`FmMode`] discriminant.
    pub fm_mode: SharedF32,
    /// The oscillator bending its own phase with its own previous sample, 0..1.
    pub feedback: SharedF32,
    /// The two oscillators multiplied together, mixed in alongside them, 0..1.
    pub osc2_ring: SharedF32,
    /// A plucked string's ring, in seconds to -60 dB.
    pub pluck_decay: SharedF32,
    /// How fast its upper partials die, 0..1.
    pub pluck_damp: SharedF32,
    /// How long it is excited, as a fraction of one period.
    pub pluck_burst: SharedF32,
    pub transpose: SharedF32,
    pub reverb_send: SharedF32,
    pub pan: SharedF32,
    /// This register's own thirteen-band curve.
    ///
    /// Where the sound sits, not what it is: it belongs to the placement, so
    /// swapping the instrument in a register leaves the curve alone.
    pub eq: EqParams,
    /// How much of this register goes to the delay send, 0..1.
    pub delay_send: SharedF32,
    /// The insert chain: what is done *to* the part, after the curve and before
    /// the fader.
    pub chain: FxChainParams,
}

impl ChannelParams {
    fn defaults(volume: f32, cutoff: f32) -> Self {
        ChannelParams {
            volume: SharedF32::new(volume),
            waveform: SharedF32::new(Waveform::Sine as i32 as f32),
            noise_level: SharedF32::new(0.0),
            pulse_width: SharedF32::new(0.5),
            attack: SharedF32::new(0.005),
            decay: SharedF32::new(0.05),
            sustain: SharedF32::new(0.7),
            release: SharedF32::new(0.1),
            env_curve: SharedF32::new(0.0),
            glide: SharedF32::new(0.0),
            cutoff: SharedF32::new(cutoff),
            resonance: SharedF32::new(0.2),
            filter_type: SharedF32::new(FilterType::Lowpass as i32 as f32),
            filter_env: SharedF32::new(0.0),
            filter_attack: SharedF32::new(0.01),
            filter_decay: SharedF32::new(0.2),
            key_track: SharedF32::new(0.0),
            lfo_pitch: SharedF32::new(0.0),
            lfo_cutoff: SharedF32::new(0.0),
            lfo_amp: SharedF32::new(0.0),
            lfo_pwm: SharedF32::new(0.0),
            unison: SharedF32::new(1.0),
            detune: SharedF32::new(0.0),
            drive: SharedF32::new(0.0),
            vel_cutoff: SharedF32::new(0.0),
            vel_pwm: SharedF32::new(0.0),
            position: SharedF32::new(0.0),
            phase_dist: SharedF32::new(0.0),
            osc2_waveform: SharedF32::new(Waveform::Sine as i32 as f32),
            osc2_interval: SharedF32::new(0.0),
            osc2_level: SharedF32::new(0.0),
            osc2_fm: SharedF32::new(0.0),
            fm_mode: SharedF32::new(FmMode::Phase as i32 as f32),
            feedback: SharedF32::new(0.0),
            osc2_ring: SharedF32::new(0.0),
            pluck_decay: SharedF32::new(2.5),
            pluck_damp: SharedF32::new(0.35),
            pluck_burst: SharedF32::new(0.9),
            transpose: SharedF32::new(0.0),
            reverb_send: SharedF32::new(0.0),
            pan: SharedF32::new(0.0),
            eq: EqParams::flat(),
            delay_send: SharedF32::new(0.0),
            chain: FxChainParams::empty(),
        }
    }
}

#[derive(Clone)]
pub struct SynthParams {
    pub low: ChannelParams,
    pub mid: ChannelParams,
    pub high: ChannelParams,
    /// How much reverb is added on top of the dry signal, 0..1.
    ///
    /// Additive, not a wet/dry balance: the dry path never sees this value. The
    /// name is historical — the ensemble files still store it as `reverb_mix`,
    /// and renaming the key would break saved ones.
    pub reverb_mix: SharedF32,
    pub master_volume: SharedF32,
    pub master_mute: SharedF32,
    /// LFO rate in Hz, shared by all three channels.
    ///
    /// The rate and the shape are global while the *destinations* are per
    /// channel, because one vibrato wobbling the whole chord is what the ear
    /// expects; three LFOs at three rates is a chorus, and a chorus is a
    /// different feature.
    pub lfo_rate: SharedF32,
    pub lfo_wave: SharedF32,
    /// The curve on the whole mix, applied to the finished stereo pair.
    ///
    /// After the reverb and before the master gain, so it trims the sound the
    /// player actually hears rather than one part of it. The metronome click
    /// passes through it too, which is the honest reading of "master": the click
    /// is already inside the reverb and under the master volume.
    pub master_eq: EqParams,
    /// The reverb send's return, with its type pinned by the panel.
    ///
    /// The same engine the register chains use, wired to a send instead of into a
    /// rack — which is the whole point of building one engine rather than four. An
    /// aux unit runs fully wet: `reverb_mix` is what decides how much of it is
    /// heard, so the unit never blends the dry signal back in.
    pub aux_reverb: FxSlotParams,
    /// The delay send's return, on the same terms.
    pub aux_delay: FxSlotParams,
    /// How much of the delay's return is added, 0..1.
    pub delay_mix: SharedF32,
    /// The tempo the synced delay follows, published from the transport.
    ///
    /// It lives here rather than being read from the transport because the audio
    /// thread has no transport — the scheduler owns it — so the one number a
    /// tempo-synced effect needs is carried the same way every other parameter is.
    pub tempo: SharedF32,
    /// The live spectrum. Not a sound: a readout of the four signals that come
    /// out of the blocks above.
    pub analyzer: AnalyzerParams,
}

impl SynthParams {
    pub fn defaults() -> Self {
        SynthParams {
            low: ChannelParams::defaults(4.0, 4000.0),
            mid: ChannelParams::defaults(4.0, 4000.0),
            high: ChannelParams::defaults(4.0, 4000.0),
            reverb_mix: SharedF32::new(0.0),
            master_volume: SharedF32::new(5.0),
            master_mute: SharedF32::new(0.0),
            lfo_rate: SharedF32::new(5.0),
            lfo_wave: SharedF32::new(LfoWave::Sine as i32 as f32),
            master_eq: EqParams::flat(),
            analyzer: AnalyzerParams::defaults(),
            aux_reverb: FxSlotParams::from_fx(&crate::voice::default_reverb_unit()),
            aux_delay: FxSlotParams::from_fx(&crate::voice::default_delay_unit()),
            delay_mix: SharedF32::new(0.0),
            tempo: SharedF32::new(120.0),
        }
    }

    /// The equaliser belonging to `target`, by [`crate::eq::EqTarget`] order.
    pub fn eq_at(&self, target: crate::eq::EqTarget) -> &EqParams {
        match target {
            crate::eq::EqTarget::Low => &self.low.eq,
            crate::eq::EqTarget::Mid => &self.mid.eq,
            crate::eq::EqTarget::High => &self.high.eq,
            crate::eq::EqTarget::Master => &self.master_eq,
        }
    }
}

/// One interpolated sample of a stored cycle, at `phase` in `0..1`.
///
/// Linear rather than nearest-neighbour: a thousand and twenty-four samples is
/// coarse enough that picking the nearest one would put a quiet buzz an octave
/// and a half up, which is exactly the artefact a band-limited table exists to
/// avoid.
fn read_table(table: &[f32; crate::wavetable::TABLE_LEN], phase: f32) -> f32 {
    let position = phase * crate::wavetable::TABLE_LEN as f32;
    // `phase` is a `fract` and so cannot reach 1.0, and a NaN or a negative
    // saturates to zero on the cast. The clamp is therefore unreachable — and it
    // is here anyway, because the failure it would prevent is an out-of-bounds
    // panic on the audio thread, and one integer compare is not a price worth
    // weighing against that.
    let index = (position as usize).min(crate::wavetable::TABLE_LEN - 1);
    let frac = position - index as f32;
    let next = if index + 1 == crate::wavetable::TABLE_LEN {
        0
    } else {
        index + 1
    };
    let a = table[index];
    a + (table[next] - a) * frac
}

/// Where unison voice `u` of `n` sits, in cents, for a spread of `spread`.
///
/// Symmetric about the note, so the stack stays in tune with itself: with two
/// voices they land at ±spread, with three at −spread, 0, +spread. One voice is
/// never detuned, which is what makes `unison = 1` the untouched sound.
fn unison_spread(n: usize, u: usize, spread: f32) -> f32 {
    if n < 2 {
        return 0.0;
    }
    (u as f32 / (n - 1) as f32 - 0.5) * 2.0 * spread
}

// -----------------------------------------------------------------------------
// Voice
// -----------------------------------------------------------------------------

#[derive(Copy, Clone, PartialEq, Eq)]
enum EnvState {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Octaves of cutoff the filter envelope can add at full depth.
const FILTER_ENV_OCTAVES: f32 = 5.0;
/// Octaves of cutoff the LFO can add at full depth.
const LFO_CUTOFF_OCTAVES: f32 = 4.0;
/// Semitones of vibrato the pitch LFO can add at full depth.
const LFO_PITCH_SEMITONES: f32 = 1.0;

/// The shape of one envelope segment, blended between a straight line and the
/// curve a capacitor gives.
///
/// Applied to the *normalised* position within the segment, so a segment still
/// takes exactly the time it was given and still lands on the same endpoint —
/// to within a part in 10^7, since the arithmetic is `from + span * (…)` in
/// `f32` and `1/3` is not representable. What *is* exact is the held level: the
/// sustain case arrives with a zero span and returns its input untouched, which
/// is what keeps `sustain = 0.7` from quietly becoming `0.49` when the curve is
/// turned up. And `k = 0` is bit-for-bit the old straight-line envelope rather
/// than a close approximation of it, which is the whole compatibility story.
fn shape_env(x: f32, from: f32, to: f32, k: f32) -> f32 {
    let k = k.clamp(0.0, 1.0);
    let span = to - from;
    if k <= 0.0 || span.abs() < 1e-6 {
        return x;
    }
    let y = ((x - from) / span).clamp(0.0, 1.0);
    // Fast out of the gate, slow into the target: how every analogue envelope
    // actually moves, and most of why one sounds "snappy" and another does not.
    let curved = y * (2.0 - y);
    from + span * (y + (curved - y) * k)
}

/// The coefficients that are constant for a whole buffer.
///
/// `Voice::tick` runs 48,000 times a second per voice, and two of the things it
/// computes there depend only on the note and on parameters: the note's own
/// frequency, through `midi_to_hz` and its `powf`, and the state-variable
/// filter's coefficient, through a `sin`. Between them they are about four
/// points of a core at a hundred and twenty voices — measured, in
/// `PERFORMANCE.md` §7 and §8.
///
/// Neither is *always* constant, which is why this is a cache with a pair of
/// flags rather than a value. The note moves within a buffer when the voice is
/// gliding or when a pitch LFO is running; the cutoff moves when the filter
/// envelope, a cutoff LFO, key tracking or velocity-to-cutoff is in use. When a
/// flag is clear the per-sample path runs exactly as it always did, so a patch
/// that uses any of those is bit-for-bit the patch it was.
///
/// The cached values are computed with the same expressions the per-sample path
/// uses, with the terms that the guard has proved to be zero folded in as zeros
/// — which is what makes the legacy oracle a meaningful acceptance test rather
/// than a re-record. The one thing it does move is *when* a coefficient notices
/// a trigger: a note struck while a buffer is being rendered is heard at the
/// start of the next one rather than at the sample it arrived on. That is at
/// most 10.7 ms at 512 frames, it is the trade the buffer-granularity decision
/// in `PERFORMANCE.md` §9 is about, and every event in the offline harness is
/// applied between buffers anyway, so the render suite cannot see it.
#[derive(Clone, Copy)]
struct Coefficients {
    /// Whether `note` and `freq` may be used.
    pitch: bool,
    note: f32,
    freq: f32,
    /// Whether `f` may be used.
    filter: bool,
    f: f32,
}

impl Coefficients {
    /// Values no control can take, so the first buffer computes rather than
    /// trusting a default — the same trick `string_damp` uses.
    const COLD: Coefficients = Coefficients {
        pitch: false,
        note: 0.0,
        freq: 0.0,
        filter: false,
        f: 0.0,
    };
}

struct Voice {
    gate: SharedF32,
    midi_note: SharedF32,
    glide_from: SharedF32,
    glide_duration_secs: SharedF32,
    hold_secs: SharedF32,
    release_override: SharedF32,
    /// Per-voice gain, 0..1.
    ///
    /// The channels already have a volume, but a rhythm take needs its own: the
    /// layers of one pattern share a channel and must still be able to sit at
    /// different levels, which is what "each earlier take is quieter" means.
    gain: SharedF32,
    /// How far this voice sits off its note, in cents. Unison detune only.
    detune_cents: SharedF32,
    /// How hard this note was hit, 0..1.
    ///
    /// The rhythm accent, and one for anything played by hand. It is already in
    /// `gain` — that is what an accent *is* — and it arrives a second time
    /// because an accent is a change of tone as well as of level on anything
    /// with a keyboard, and only the voice can make that second change.
    velocity: SharedF32,
    /// Whether this voice has ever sounded.
    ///
    /// Decides whether a trigger glides in from the last note it played or
    /// starts where it is: gliding the very first note of a session in from
    /// middle C is not portamento, it is a mistake.
    armed: SharedF32,
    channel: ChannelParams,
    /// Clones of the two global LFO settings, so a voice can read the rate
    /// without being handed all of `SynthParams`.
    lfo_rate: SharedF32,
    lfo_wave: SharedF32,
    /// The stored cycles, shared by every voice and built before the stream
    /// starts. A plain reference rather than an `Arc`: they are immutable and
    /// outlive every voice, so the audio thread pays a pointer dereference and
    /// never a refcount.
    tables: &'static crate::wavetable::Tables,
    sample_rate: f32,
    /// Recomputed once per buffer by [`Voice::cache`].
    coeffs: Coefficients,

    phase: f32,
    /// The second oscillator's phase, which runs whether or not the first does.
    osc2_phase: f32,
    /// The first oscillator's own last sample, for `feedback`.
    last_osc: f32,
    /// The ring modulator's direct-current estimate.
    ring_lp: f32,
    /// How fast that estimate follows, from the sample rate.
    ring_coef: f32,
    /// The plucked string: a delay line whose length is the note's period.
    ///
    /// One `Vec` per voice, sized for the lowest note the voice can be asked for
    /// and built with the pool on the main thread. The audio thread moves a
    /// write head through it and never allocates.
    string: Vec<f32>,
    string_write: usize,
    string_lp: f32,
    /// The loop's coefficients, recomputed only when one of the string's own
    /// controls moves. A `powf` and a divide a sample would be more than the
    /// rest of the string put together.
    string_t60: f32,
    string_damp: f32,
    string_gain: f32,
    /// The loop's length in samples, which is the note's period less the
    /// damping filter's own delay. Cached with the other two because it is part
    /// of what the loop's loss is computed from.
    string_delay: f32,
    /// Set on a trigger and cleared once the string has been primed, because the
    /// string is as long as the note's period and the note is only known once
    /// the envelope has started.
    pluck_arm: bool,
    env_state: EnvState,
    env_value: f32,
    /// The level the current release started from, so the curve is applied to
    /// `level -> 0` instead of to `1 -> 0`.
    release_start: f32,
    /// Position through the filter contour, shaped where it is used.
    filter_env_value: f32,
    filter_env_state: EnvState,
    lfo_phase: f32,
    glide_pos: f32,
    elapsed: f32,
    svf_low: f32,
    svf_band: f32,
    noise_seed: u32,
}

impl Voice {
    fn new(
        channel: ChannelParams,
        lfo_rate: SharedF32,
        lfo_wave: SharedF32,
        sample_rate: f32,
        seed: u32,
    ) -> Self {
        Voice {
            gate: SharedF32::new(0.0),
            midi_note: SharedF32::new(60.0),
            glide_from: SharedF32::new(60.0),
            glide_duration_secs: SharedF32::new(0.0),
            hold_secs: SharedF32::new(0.0),
            release_override: SharedF32::new(0.0),
            gain: SharedF32::new(1.0),
            detune_cents: SharedF32::new(0.0),
            velocity: SharedF32::new(1.0),
            armed: SharedF32::new(0.0),
            channel,
            lfo_rate,
            lfo_wave,
            tables: crate::wavetable::tables(),
            sample_rate,
            coeffs: Coefficients::COLD,
            phase: 0.0,
            osc2_phase: 0.0,
            last_osc: 0.0,
            ring_lp: 0.0,
            ring_coef: 1.0 - (-TAU * RING_DC_HZ / sample_rate).exp(),
            string: vec![0.0; string_capacity(sample_rate)],
            string_write: 0,
            string_lp: 0.0,
            string_t60: 0.0,
            // Values no control can take, so the first plucked sample always
            // computes the loop's constants rather than trusting a default.
            string_damp: -1.0,
            string_gain: 1.0,
            string_delay: -1.0,
            pluck_arm: false,
            env_state: EnvState::Idle,
            env_value: 0.0,
            release_start: 0.0,
            filter_env_value: 0.0,
            filter_env_state: EnvState::Idle,
            lfo_phase: 0.0,
            glide_pos: 1.0,
            elapsed: 0.0,
            svf_low: 0.0,
            svf_band: 0.0,
            noise_seed: seed | 1,
        }
    }

    fn handle(&self) -> VoiceHandle {
        VoiceHandle {
            gate: self.gate.clone(),
            midi_note: self.midi_note.clone(),
            glide_from: self.glide_from.clone(),
            glide_duration_secs: self.glide_duration_secs.clone(),
            hold_secs: self.hold_secs.clone(),
            release_override: self.release_override.clone(),
            gain: self.gain.clone(),
            detune_cents: self.detune_cents.clone(),
            velocity: self.velocity.clone(),
            armed: self.armed.clone(),
        }
    }

    /// One sample of white noise, `-1..1`.
    ///
    /// xorshift32 rather than a table or a shared generator: it costs three
    /// shifts, keeps no state that has to stay in step between voices, and is
    /// far past what an ear can tell from ideal noise.
    fn next_noise(&mut self) -> f32 {
        let mut x = self.noise_seed;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.noise_seed = x;
        ((x >> 8) as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
    }

    /// One waveform's value at a phase, with the pulse width to hand.
    ///
    /// `pluck` is not one of these. A string is a delay line rather than a shape
    /// — it has to know the note's frequency and it remembers what it played a
    /// moment ago — so it is read where the frequency is known, and it returns
    /// silence here. The second oscillator's row does not offer it for the same
    /// reason: one voice has one string.
    fn shape(&mut self, waveform: Waveform, phase: f32, width: f32) -> f32 {
        match waveform {
            Waveform::Sine => (phase * TAU).sin(),
            Waveform::Saw => 2.0 * phase - 1.0,
            Waveform::Square => {
                // Two levels chosen so the *mean* is zero: `2(1-w)` for the
                // width-`w` part of the cycle and `-2w` for the rest. A plain
                // +1/-1 pulse has a mean of `2w - 1`, and this filter has unity
                // gain at DC, so that bias would pass straight through into the
                // reverb and the master limiter. At w = 0.5 both levels are
                // exactly +1 and -1, which is the square this always was, and
                // the narrower the pulse the lower its RMS — a thin pulse really
                // does carry less energy.
                if phase < width {
                    2.0 * (1.0 - width)
                } else {
                    -2.0 * width
                }
            }
            Waveform::Triangle => 4.0 * (phase - 0.5).abs() - 1.0,
            Waveform::Noise => self.next_noise(),
            Waveform::Pluck => 0.0,
            // Everything left is a stored cycle. The lookup is the whole cost of
            // the feature: one index, one interpolation, one multiply.
            table => {
                let index = table.table().expect("the remaining variants are tables");
                read_table(self.tables.cycle(index), phase)
            }
        }
    }

    /// Recompute the string's loop constants when anything they depend on moves.
    ///
    /// A `powf` and a divide a sample would cost more than the rest of the
    /// string put together; three float compares that almost always find nothing
    /// to do cost nothing. The string is the one waveform whose coefficients
    /// come from its controls *and* from the note, which is why the delay is
    /// part of the key rather than only the two knobs: the loop's loss is levied
    /// once per pass, and how many passes fit in a second is the pitch.
    fn string_tune(&mut self, freq: f32) {
        let t60 = self
            .channel
            .pluck_decay
            .get()
            .clamp(range::PLUCK_DECAY.0, range::PLUCK_DECAY.1);
        let damp = self.channel.pluck_damp.get().clamp(0.0, 1.0) * MAX_PLUCK_DAMP;
        // The damping one-pole delays the loop as well as damping it, and a loop
        // rings at the frequency whose *total* phase is a whole number of cycles:
        // subtracting its delay is what puts the note in tune rather than a
        // little flat.
        let correction = if damp > 0.0 { damp / (1.0 - damp) } else { 0.0 };
        let period = self.sample_rate / freq.max(MIN_STRING_HZ);
        let delay = (period - correction).clamp(2.0, self.string.len() as f32 - 2.0);

        if t60 != self.string_t60 || damp != self.string_damp || delay != self.string_delay {
            self.string_t60 = t60;
            self.string_damp = damp;
            self.string_delay = delay;
            // The loss per pass that loses 60 dB over `t60` seconds. Per *pass*
            // and not per sample: the loop's gain is levied once every `delay`
            // samples, so a low note — which takes longer to go round — needs a
            // loss closer to one to decay in the same time. Dividing by the
            // sample rate alone would make the ring time proportional to the
            // note, which is the one thing a decay control must not be.
            self.string_gain = 10f32.powf(-3.0 * delay / (t60 * self.sample_rate));
        }
    }

    /// Prime the string with its excitation and put the write head where the
    /// first read will find it.
    fn pluck_string(&mut self, freq: f32) {
        self.string_tune(freq);
        let delay = self.string_delay;
        // One period plus the interpolation's far tap. Everything outside is
        // stale, and the read head never reaches it.
        let prime = ((delay.ceil() as usize) + 2).min(self.string.len());
        let burst = (self.channel.pluck_burst.get().clamp(0.05, 1.0) * delay) as usize;
        for i in 0..prime {
            let excited = if i < burst { self.next_noise() } else { 0.0 };
            self.string[i] = excited;
        }
        // The read head runs `delay` behind the write head, so starting the write
        // head one period in puts the excitation exactly where the first read
        // looks. Without this the note would open with a period of the buffer's
        // stale end, which reads as a delay rather than as a pluck.
        self.string_write = (delay.ceil() as usize).min(self.string.len() - 1);
        self.string_lp = 0.0;
    }

    /// One sample of the plucked string.
    ///
    /// The line is circular and the write head holds the newest sample, so the
    /// loop is closed by reading `delay` samples *behind* it: whatever is written
    /// now is read again `delay` samples later, which is what makes the line ring
    /// at `sample_rate / delay`. That is the whole of Karplus-Strong — a delay,
    /// a loss, and a burst to start it — and it is why the model has partials
    /// that die at different rates and are slightly inharmonic, which no
    /// single-cycle table can be.
    fn string_tick(&mut self, freq: f32) -> f32 {
        self.string_tune(freq);
        let len = self.string.len();
        let delay = self.string_delay;
        let write = self.string_write.min(len - 1);

        let mut position = write as f32 - delay;
        if position < 0.0 {
            position += len as f32;
        }
        let index = (position as usize).min(len - 1);
        let next = if index + 1 == len { 0 } else { index + 1 };
        let a = self.string[index];
        let frac = position - index as f32;
        let tapped = a + (self.string[next] - a) * frac;

        // The loop's two losses: a one-pole that takes the top off as the note
        // rings, and the gain that decides how long it takes to get there. Their
        // product is always below one, so the string can only ever decay.
        self.string_lp = tapped * (1.0 - self.string_damp) + self.string_lp * self.string_damp;
        self.string[write] = self.string_lp * self.string_gain;
        self.string_write = if write + 1 == len { 0 } else { write + 1 };
        tapped
    }

    /// Recompute the buffer-constant coefficients, once per buffer.
    ///
    /// Called by the engine before it renders a buffer, not by `tick`. Every
    /// guard here is a proof that the value cannot move within the buffer; when
    /// one fails the corresponding flag is cleared and `tick` falls back to
    /// computing the value per sample exactly as it always did.
    ///
    /// The early-out is the same one `tick` uses: a voice with its envelope
    /// closed and its gate down is silent, and computing coefficients for it
    /// would be work for a buffer that is going to be a hundred and twenty-eight
    /// loads of zero.
    fn cache(&mut self) {
        if self.env_state == EnvState::Idle && self.gate.get() <= 0.5 {
            self.coeffs.pitch = false;
            self.coeffs.filter = false;
            return;
        }

        // The note. `glide_pos` is pinned to 1.0 while there is no glide to
        // perform, which makes `base` a pure function of the two trigger values
        // — and the expression is written out rather than simplified to `to`, so
        // the arithmetic is the same arithmetic.
        let glide_active = self.glide_duration_secs.get() > 0.0;
        let pitch_depth = self.channel.lfo_pitch.get().clamp(0.0, 1.0);
        self.coeffs.pitch = !glide_active && pitch_depth == 0.0;
        if self.coeffs.pitch {
            let from = self.glide_from.get();
            let to = self.midi_note.get();
            let base = from + (to - from) * 1.0;
            let note = (base
                + self.channel.transpose.get()
                + self.detune_cents.get() / 100.0
                + 0.0 * LFO_PITCH_SEMITONES)
                .clamp(0.0, 127.0);
            self.coeffs.note = note;
            self.coeffs.freq = midi_to_hz(note).clamp(20.0, self.sample_rate * 0.45);
        }

        // The filter coefficient. It depends on the note only through key
        // tracking, so a patch that does not track the keyboard can cache it
        // even while it glides; a filter envelope or a cutoff LFO rules it out
        // entirely, because both are read from state that moves per sample.
        let key_track = self.channel.key_track.get().clamp(0.0, 1.0);
        let env_amount = self.channel.filter_env.get().clamp(-1.0, 1.0);
        let lfo_cut_depth = self.channel.lfo_cutoff.get().clamp(0.0, 1.0);
        self.coeffs.filter =
            env_amount == 0.0 && lfo_cut_depth == 0.0 && (key_track == 0.0 || self.coeffs.pitch);
        if self.coeffs.filter {
            let vel_cutoff = self.channel.vel_cutoff.get().clamp(0.0, 1.0);
            let velocity = self.velocity.get().clamp(0.0, 1.0);
            let key_factor = if key_track > 0.0 {
                ((self.coeffs.note - 60.0) / 12.0 * key_track).exp2()
            } else {
                1.0
            };
            let vel_factor = if vel_cutoff > 0.0 {
                (vel_cutoff * VELOCITY_CUTOFF_OCTAVES * (velocity - 1.0)).exp2()
            } else {
                1.0
            };
            let cutoff = (self.channel.cutoff.get() * key_factor * 1.0 * 1.0 * vel_factor)
                .clamp(20.0, self.sample_rate * 0.4);
            let q = 1.0 - self.channel.resonance.get().clamp(0.0, 0.99);
            self.coeffs.f = (2.0 * (PI * cutoff / self.sample_rate).sin()).min(2.0 - q);
        }
    }

    /// One sample, or nothing at all.
    ///
    /// The early-out is the whole of this function and the body is somewhere
    /// else, which is a deliberate split rather than a tidy-up. The pool is a
    /// hundred and twenty-one voices and at any moment most of them are silent —
    /// a chord of three notes at unison one wakes six — so the engine walks a
    /// hundred and fifteen voices that have nothing to say. Checking for that is
    /// one relaxed load and a branch, and it belongs in the loop; the five
    /// hundred lines behind it do not, and keeping them out of line means the
    /// idle path never pays a call into them.
    ///
    /// `!gate_on` rather than a second comparison, because the two have to agree
    /// on a NaN: `gate.get() > 0.5` is false for one, so `<= 0.5` would be too.
    #[inline]
    fn tick(&mut self) -> f32 {
        // A silent voice costs one relaxed load. Unison can stack four voices
        // per note, so there are four times as many of them to get through, and
        // at any moment most are idle.
        let gate_on = self.gate.get() > 0.5;
        if self.env_state == EnvState::Idle && !gate_on {
            return 0.0;
        }
        self.tick_sounding()
    }

    /// One sample of a voice that is making a sound.
    fn tick_sounding(&mut self) -> f32 {
        let dt = 1.0 / self.sample_rate;
        let gate_on = self.gate.get() > 0.5;

        let prev_state = self.env_state;

        // ---- amp envelope ----
        let a = self.channel.attack.get().max(0.001);
        let d = self.channel.decay.get().max(0.001);
        let s = self.channel.sustain.get().clamp(0.0, 1.0);
        let r_default = self.channel.release.get().max(0.001);
        let r_override = self.release_override.get();
        let r = if r_override > 0.0 {
            r_override
        } else {
            r_default
        };

        match self.env_state {
            EnvState::Idle => {
                self.env_value = 0.0;
                if gate_on {
                    self.env_state = EnvState::Attack;
                }
            }
            EnvState::Attack => {
                self.env_value += dt / a;
                if self.env_value >= 1.0 {
                    self.env_value = 1.0;
                    self.env_state = EnvState::Decay;
                }
                if !gate_on {
                    self.release_start = self.env_value;
                    self.env_state = EnvState::Release;
                }
            }
            EnvState::Decay => {
                self.env_value -= dt / d * (1.0 - s);
                if self.env_value <= s {
                    self.env_value = s;
                    self.env_state = EnvState::Sustain;
                }
                if !gate_on {
                    self.release_start = self.env_value;
                    self.env_state = EnvState::Release;
                }
            }
            EnvState::Sustain => {
                self.env_value = s;
                if !gate_on {
                    self.release_start = self.env_value;
                    self.env_state = EnvState::Release;
                }
            }
            EnvState::Release => {
                self.env_value -= dt / r;
                if self.env_value <= 0.0001 {
                    self.env_value = 0.0;
                    self.env_state = EnvState::Idle;
                }
                if gate_on {
                    self.env_state = EnvState::Attack;
                }
            }
        }

        // Reset glide + elapsed when a new trigger fires.
        if prev_state != EnvState::Attack && self.env_state == EnvState::Attack {
            self.glide_pos = 0.0;
            self.elapsed = 0.0;
            self.release_start = 0.0;
            self.filter_env_value = 0.0;
            self.filter_env_state = EnvState::Attack;
            // Each note starts its own modulation cycle, so a chord's vibrato
            // arrives with the chord instead of wherever a free-running LFO
            // happened to be when the key went down.
            self.lfo_phase = 0.0;
            // And so does the second oscillator, so a two-oscillator patch has
            // the same attack every time rather than a phase that drifts.
            self.osc2_phase = 0.0;
            // The feedback and ring states are *not* reset: they are part of the
            // sound in progress, and zeroing the feedback on every note would
            // make a folded saw click at each attack.

            self.pluck_arm = true;
        }

        if self.env_state == EnvState::Idle {
            self.filter_env_value = 0.0;
            self.filter_env_state = EnvState::Idle;
            return 0.0;
        }

        // ---- filter contour: its own attack and decay, ignoring the gate ----
        //
        // The amp envelope cannot double as this. A horn wants the filter to
        // open over a 600 ms attack while the note itself starts in 100 ms, and
        // an acid line wants the filter shut again long before the note ends.
        // An AD contour with no sustain is what both of those are.
        // Read unconditionally, because it is what decides whether the contour
        // runs at all. The two times are read inside the guard: they are only
        // wanted when there is a contour to time.
        let env_amount = self.channel.filter_env.get().clamp(-1.0, 1.0);
        // Skipped entirely when the contour is switched off. It restarts on
        // every trigger, so a voice that does not use it cannot tell.
        let contour = if env_amount != 0.0 {
            let fa = self.channel.filter_attack.get().max(0.001);
            let fd = self.channel.filter_decay.get().max(0.001);
            match self.filter_env_state {
                EnvState::Attack => {
                    self.filter_env_value += dt / fa;
                    if self.filter_env_value >= 1.0 {
                        self.filter_env_value = 1.0;
                        self.filter_env_state = EnvState::Decay;
                    }
                }
                EnvState::Decay => {
                    self.filter_env_value -= dt / fd;
                    if self.filter_env_value <= 0.0 {
                        self.filter_env_value = 0.0;
                        self.filter_env_state = EnvState::Idle;
                    }
                }
                _ => self.filter_env_value = 0.0,
            }
            // Shaped where it is used rather than where it is advanced: leaving
            // the endpoint quickly is what makes a sweep sound like a sweep.
            match self.filter_env_state {
                EnvState::Attack => {
                    let v = self.filter_env_value;
                    v * (2.0 - v)
                }
                EnvState::Decay => {
                    let v = self.filter_env_value;
                    v * v
                }
                _ => 0.0,
            }
        } else {
            0.0
        };

        // ---- LFO ----
        let lfo_pitch_depth = self.channel.lfo_pitch.get().clamp(0.0, 1.0);
        let lfo_cut_depth = self.channel.lfo_cutoff.get().clamp(0.0, 1.0);
        let lfo_amp_depth = self.channel.lfo_amp.get().clamp(0.0, 1.0);
        let lfo_pwm_depth = self.channel.lfo_pwm.get().clamp(0.0, 1.0);
        let wanted = lfo_pitch_depth > 0.0
            || lfo_cut_depth > 0.0
            || lfo_amp_depth > 0.0
            || lfo_pwm_depth > 0.0;
        // Not advanced when nothing reads it: the phase restarts on every
        // trigger, so a voice that never uses the LFO cannot tell the
        // difference, and an unmodulated voice pays neither the load nor the
        // sine.
        let lfo = if wanted {
            let lfo_rate = self.lfo_rate.get().clamp(0.01, 40.0);
            self.lfo_phase = (self.lfo_phase + lfo_rate * dt).fract();
            match LfoWave::from_f32(self.lfo_wave.get()) {
                LfoWave::Sine => (self.lfo_phase * TAU).sin(),
                LfoWave::Triangle => 4.0 * (self.lfo_phase - 0.5).abs() - 1.0,
                LfoWave::Square => {
                    if self.lfo_phase < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                LfoWave::Saw => 2.0 * self.lfo_phase - 1.0,
            }
        } else {
            0.0
        };

        // ---- glide / pitch ----
        let glide_dur = self.glide_duration_secs.get();
        if glide_dur > 0.0 {
            let hold = self.hold_secs.get();
            // A hold of zero means "no auto-release". The metronome click is the
            // only thing that sets one; a played note is released by the gate.
            if hold > 0.0 && self.elapsed >= glide_dur + hold {
                self.gate.set(0.0);
            }
            self.elapsed += dt;

            if self.glide_pos < 1.0 {
                self.glide_pos = (self.glide_pos + dt / glide_dur).min(1.0);
            }
        } else {
            self.glide_pos = 1.0;
        }

        // `cache` has already proved that this buffer's note cannot move, which
        // is true of every patch without glide or a pitch LFO — most of them.
        // When it has not, this is the expression it always was.
        let note = if self.coeffs.pitch {
            self.coeffs.note
        } else {
            let from = self.glide_from.get();
            let to = self.midi_note.get();
            let base = from + (to - from) * self.glide_pos;
            let transpose = self.channel.transpose.get();
            let detune = self.detune_cents.get() / 100.0;
            let vibrato = lfo * lfo_pitch_depth * LFO_PITCH_SEMITONES;
            (base + transpose + detune + vibrato).clamp(0.0, 127.0)
        };
        let freq = if self.coeffs.pitch {
            self.coeffs.freq
        } else {
            midi_to_hz(note).clamp(20.0, self.sample_rate * 0.45)
        };

        let waveform = Waveform::from_f32(self.channel.waveform.get());

        // A plucked string is primed here rather than where it is read, because
        // its length is the note's period and the note is only known now. Every
        // other waveform ignores the flag, which is one branch a sample for a
        // voice that has already decided to be a string.
        if self.pluck_arm {
            self.pluck_arm = false;
            if waveform == Waveform::Pluck {
                self.pluck_string(freq);
            }
        }

        // ---- velocity ----
        //
        // It is already in `gain` — that is what an accent *is* — and it arrives
        // a second time because on anything with a keyboard an accent is a
        // change of tone as well as of level, and only the voice can make the
        // second change. Both depths at zero leave the two arms below untouched.
        let velocity = self.velocity.get().clamp(0.0, 1.0);
        let vel_cutoff = self.channel.vel_cutoff.get().clamp(0.0, 1.0);
        let vel_pwm = self.channel.vel_pwm.get().clamp(0.0, 1.0);

        // ---- pulse width, and the two things that move it ----
        //
        // A duty cycle rather than a fixed half: 0.5 is the even square this
        // used to be, and either side of it is the hollow, reedy half of the
        // range. Two levels are chosen so the *mean* is zero — see `shape`.
        let mut width = self.channel.pulse_width.get();
        if lfo_pwm_depth > 0.0 {
            width += lfo * lfo_pwm_depth * 0.45;
        }
        if vel_pwm > 0.0 {
            // Zero at full velocity, so a patch that never accents anything is
            // not reshaped by this row merely being in the file. A soft hit is a
            // *wider* pulse: fewer harmonics and less energy, which is the
            // direction a soft hit goes on every instrument that has one.
            width += vel_pwm * (1.0 - velocity) * VELOCITY_PWM_SPAN;
        }
        let width = width.clamp(0.05, 0.95);

        // ---- the second oscillator ----
        //
        // Not started at all when nothing reads it. That is what keeps a patch
        // written before the second oscillator existed rendering to the sample:
        // `second` stays exactly zero, `mixed` below is `osc` untouched, and not
        // one multiply is done differently.
        let osc2_waveform = Waveform::from_f32(self.channel.osc2_waveform.get());
        let osc2_level = self.channel.osc2_level.get().clamp(0.0, 1.0);
        let osc2_fm = self.channel.osc2_fm.get().clamp(0.0, 1.0);
        // The ring modulator is a third reason to run the second oscillator, and
        // the one that is easy to forget: it needs the modulator just as much as
        // a level or a depth does, and a gate that misses it renders silence.
        let ring = self.channel.osc2_ring.get().clamp(0.0, 1.0);
        let second = if osc2_level > 0.0 || osc2_fm > 0.0 || ring > 0.0 {
            let interval = self.channel.osc2_interval.get().clamp(-24.0, 24.0);
            let note2 = (note + interval).clamp(0.0, 127.0);
            let freq2 = midi_to_hz(note2).clamp(20.0, self.sample_rate * 0.45);
            self.osc2_phase = advance(self.osc2_phase, table_rate(osc2_waveform, freq2) * dt);
            self.shape(osc2_waveform, self.osc2_phase, width)
        } else {
            0.0
        };

        // ---- cross-modulation, and what domain it is spent in ----
        //
        // Phase modulation adds the modulator to the phase, so the index is a
        // fixed number of cycles whatever the note. The two frequency domains
        // move the *rate* instead, which is the difference between a DX bell and
        // an analog X-Mod: the index follows the note in the linear domain, and
        // follows the interval in the exponential one.
        let fm_mode = FmMode::from_f32(self.channel.fm_mode.get());
        let bent = second * osc2_fm;
        let carrier = match fm_mode {
            FmMode::Linear => freq + bent * LINEAR_FM_HZ,
            FmMode::Expo => {
                let octaves = (bent * EXPO_FM_OCTAVES).clamp(-EXPO_FM_OCTAVES, EXPO_FM_OCTAVES);
                // Divided by the mean the swing leaves behind, so the note stays
                // where it was asked for. The depth and not the signed value: the
                // average is over a cycle of the modulator.
                let span = (osc2_fm * EXPO_FM_OCTAVES).clamp(0.0, EXPO_FM_OCTAVES);
                freq * octaves.exp2() / expo_mean_shift(span)
            }
            FmMode::Phase => freq,
        };
        // Bounded to the same limit an unmodulated note is: past half a cycle a
        // sample the oscillator is not being modulated any more, it is aliasing.
        let carrier = if carrier.is_finite() {
            carrier.clamp(-self.sample_rate * 0.45, self.sample_rate * 0.45)
        } else {
            freq
        };
        // The rate the *table* wants, so a drawbar registration still advances at
        // half speed however hard it is being bent.
        self.phase = advance(self.phase, table_rate(waveform, carrier) * dt);

        // ---- the first oscillator: FM, then phase distortion, then a shape ----
        let mut phase = self.phase;
        if fm_mode == FmMode::Phase && osc2_fm > 0.0 {
            // Added to the phase before the lookup, so the carrier's pitch never
            // moves and the sidebands stay symmetric — which is the difference
            // between a DX-style bell and a wobbly detune.
            phase = (phase + bent * FM_DEPTH_CYCLES).rem_euclid(1.0);
        }
        let feedback = self.channel.feedback.get().clamp(0.0, 1.0);
        if feedback > 0.0 {
            // An operator reading its own last sample. One sample of delay, which
            // is what makes it fold: the phase is bent by wherever the wave was a
            // moment ago, so a sine becomes a ramp and a ramp becomes noise.
            phase = (phase + self.last_osc * feedback * FEEDBACK_CYCLES).rem_euclid(1.0);
        }
        let phase_dist = self.channel.phase_dist.get().clamp(0.0, 1.0);
        if phase_dist > 0.0 {
            phase = warp_phase(phase, phase_dist);
        }

        let position = self.channel.position.get().clamp(0.0, 1.0);
        let osc = if waveform == Waveform::Pluck {
            // A string is not a spectrum, so there is nothing for the position
            // row to blend it with and the morph table gives it itself.
            self.string_tick(freq)
        } else {
            let partner = MORPH_NEXT[waveform as usize];
            if position <= 0.0 || partner == waveform {
                self.shape(waveform, phase, width)
            } else if position >= 1.0 {
                // The far end is the partner alone rather than a blend that
                // rounds to it, so "one" means exactly the next waveform and a
                // test can say so to the sample.
                self.shape(partner, phase, width)
            } else {
                // A crossfade of two cycles read at the same phase, which is
                // what a wavetable position is: the two spectra are added in
                // proportion rather than one being filtered towards the other.
                let a = self.shape(waveform, phase, width);
                let b = self.shape(partner, phase, width);
                a + (b - a) * position
            }
        };

        self.last_osc = osc;

        let mixed = if osc2_level > 0.0 {
            osc + second * osc2_level
        } else {
            osc
        };
        // Ring modulation, which is a *product* rather than a sum: the two
        // oscillators' sum and difference tones, both of which move with the
        // note. The rack's `ringmod` cannot do this — its oscillator is a fixed
        // hertz, so it rings at one pitch whatever is played.
        let mixed = if ring > 0.0 {
            let product = osc * second;
            // The direct-current blocker, because a sine multiplied by itself is
            // half direct current and this filter has unity gain at DC.
            self.ring_lp += (product - self.ring_lp) * self.ring_coef;
            mixed + (product - self.ring_lp) * ring
        } else {
            mixed
        };

        // Noise *alongside* the oscillator, which is the snare and breath case:
        // a body and a burst of air are both wanted, and a waveform can only
        // offer one or the other.
        let noise_level = self.channel.noise_level.get().clamp(0.0, 1.0);
        let source = if noise_level > 0.0 {
            mixed + self.next_noise() * noise_level
        } else {
            mixed
        };

        let (env_from, env_to) = match self.env_state {
            EnvState::Attack => (0.0, 1.0),
            EnvState::Decay => (1.0, s),
            EnvState::Sustain => (s, s),
            EnvState::Release => (self.release_start, 0.0),
            EnvState::Idle => (0.0, 0.0),
        };
        let amp = shape_env(
            self.env_value,
            env_from,
            env_to,
            self.channel.env_curve.get(),
        );
        let tremolo = 1.0 - lfo_amp_depth * (0.5 - 0.5 * lfo);
        let input = source * amp * tremolo;

        // ---- drive, on the way into the filter ----
        //
        // Skipped at zero rather than multiplied by one, so a patch that does not
        // use it renders to the sample: `soft_clip` is not the identity even at a
        // pre-gain of one.
        let drive = self.channel.drive.get().clamp(0.0, 1.0);
        let input = if drive > 0.0 {
            soft_clip(input * (1.0 + drive * DRIVE_PRE_GAIN))
        } else {
            input
        };

        // ---- resonant state-variable filter ----
        let resonance = self.channel.resonance.get().clamp(0.0, 0.99);

        let q = 1.0 - resonance;
        // The coefficient. `cache` has already done this arithmetic once for the
        // buffer when nothing in it can move — which is the common case, and the
        // one worth four points of a core.
        //
        // Everything below is the per-sample path, unchanged, including the five
        // ways into the cutoff: the knob, key tracking, the filter envelope, a
        // cutoff LFO, and how hard the note was hit. Each is skipped rather than
        // multiplied by one, so a patch that does not use it does not pay for it.
        //
        // The clamp to the Chamberlin stability limit, `f < 2 - q`, is the last
        // step of both paths. Without it the filter diverges to NaN — and stays
        // there, because a NaN in the state is a NaN for ever, and `tanh(NaN)`
        // poisons the whole mix rather than one voice. It costs nothing and
        // changes nothing that used to be reachable: at the panel's own maximum
        // cutoff the coefficient is exactly 1.0, and `2 - q` is never below 1.0
        // because `q <= 1`, so the clamp cannot bind on any setting a user can
        // dial in. It bites only where `filter env`, `key track` or `lfo cutoff`
        // have multiplied the cutoff past that — the region the modulation made
        // reachable.
        let f = if self.coeffs.filter {
            self.coeffs.f
        } else {
            let key_track = self.channel.key_track.get().clamp(0.0, 1.0);
            let key_factor = if key_track > 0.0 {
                ((note - 60.0) / 12.0 * key_track).exp2()
            } else {
                1.0
            };

            let env_factor = if env_amount != 0.0 && contour != 0.0 {
                (contour * env_amount * FILTER_ENV_OCTAVES).exp2()
            } else {
                1.0
            };

            let lfo_factor = if lfo_cut_depth > 0.0 {
                (lfo * lfo_cut_depth * LFO_CUTOFF_OCTAVES).exp2()
            } else {
                1.0
            };

            // The fifth way into the cutoff, and the only one that is not a knob
            // on a panel: how hard the note was hit. Zero at full velocity, so a
            // patch with no accents in it is not darkened by the row existing.
            let vel_factor = if vel_cutoff > 0.0 {
                (vel_cutoff * VELOCITY_CUTOFF_OCTAVES * (velocity - 1.0)).exp2()
            } else {
                1.0
            };

            let cutoff =
                (self.channel.cutoff.get() * key_factor * env_factor * lfo_factor * vel_factor)
                    .clamp(20.0, self.sample_rate * 0.4);
            (2.0 * (PI * cutoff / self.sample_rate).sin()).min(2.0 - q)
        };

        let low = self.svf_low + f * self.svf_band;
        let high = input - low - q * self.svf_band;
        let band = f * high + self.svf_band;
        self.svf_low = low;
        self.svf_band = band;

        // The filter has computed all three all along; this only picks one.
        let filtered = match FilterType::from_f32(self.channel.filter_type.get()) {
            FilterType::Lowpass => low,
            FilterType::Highpass => high,
            FilterType::Bandpass => band,
            // The two outputs the state-variable form gives away. `low + high`
            // is everything but the band and `low - high` is the band against
            // it; both fall out of the three the filter already computes, so
            // they cost one add each and no state.
            FilterType::Notch => low + high,
            FilterType::Peak => low - high,
        };

        // Gain last, so one take sitting quieter also feeds less reverb.
        filtered * self.gain.get().clamp(0.0, 1.0)
    }
}

// -----------------------------------------------------------------------------
// VoiceHandle
// -----------------------------------------------------------------------------

#[derive(Clone)]
struct VoiceHandle {
    gate: SharedF32,
    midi_note: SharedF32,
    glide_from: SharedF32,
    glide_duration_secs: SharedF32,
    hold_secs: SharedF32,
    release_override: SharedF32,
    gain: SharedF32,
    detune_cents: SharedF32,
    velocity: SharedF32,
    armed: SharedF32,
}

impl VoiceHandle {
    fn reset(&self) {
        self.release_override.set(0.0);
        self.glide_duration_secs.set(0.0);
        self.gate.set(0.0);
    }

    /// Start a note at a given level, optionally gliding in from wherever this
    /// voice last was and sitting `detune_cents` off pitch.
    ///
    /// `glide_secs` of zero is an instant pitch change, which is what a voice
    /// with no portamento asks for; `detune_cents` is unison's, and is zero for
    /// every voice but the outer ones of a stack.
    fn trigger_at(&self, note: u8, gain: f32, velocity: f32, glide_secs: f32, detune_cents: f32) {
        self.release_override.set(0.0);
        self.hold_secs.set(0.0);
        self.gain.set(gain.clamp(0.0, 1.0));
        self.velocity.set(velocity.clamp(0.0, 1.0));
        self.detune_cents.set(detune_cents);
        // Gliding in from the previous note only makes sense once there *was* a
        // previous note.
        let was_armed = self.armed.get() > 0.5;
        self.glide_from.set(if was_armed {
            self.midi_note.get()
        } else {
            note as f32
        });
        self.armed.set(1.0);
        self.glide_duration_secs
            .set(if was_armed { glide_secs } else { 0.0 });
        self.midi_note.set(note as f32);
        self.gate.set(1.0);
    }

    /// Glide from a start pitch to a target pitch over `duration_secs`,
    /// then hold for `hold_secs` before auto-releasing, at a level.
    ///
    /// The level is the metronome's own: the click borrows the mid channel's tone
    /// but must not have to fight that channel's gain to be turned down.
    fn trigger_glide_at(&self, from: u8, to: u8, duration_secs: f32, hold_secs: f32, gain: f32) {
        self.release_override.set(0.0);
        self.gain.set(gain.clamp(0.0, 1.0));
        // The metronome has no velocity to speak of: a click is either there or
        // it is not.
        self.velocity.set(1.0);
        self.detune_cents.set(0.0);
        self.glide_from.set(from as f32);
        self.midi_note.set(to as f32);
        self.glide_duration_secs.set(duration_secs);
        self.hold_secs.set(hold_secs);
        self.gate.set(1.0);
    }
}

// -----------------------------------------------------------------------------
// Dry / reverb summing
// -----------------------------------------------------------------------------

/// Gain applied to the reverb return at full level.
///
/// This is the same ×3 the old wet/dry crossfade applied at its maximum, so
/// full reverb is as loud as it always was. What changed is that the dry signal
/// is no longer faded out underneath it.
const REVERB_RETURN_GAIN: f32 = 3.0;

/// Sum the dry stereo pair with the delay return.
///
/// Additive like the reverb's, but at unity where the tank's is at three: the
/// tank's output is quiet because its combs divide by four and multiply by
/// `1 - feedback`, and a delay's is a copy of the signal. A return gain here
/// would be a second volume control on an effect that already has one.
fn mix_delay(dry_l: f32, dry_r: f32, wet: f32, level: f32) -> (f32, f32) {
    let echo = wet * level.clamp(0.0, 1.0);
    (dry_l + echo, dry_r + echo)
}

/// Sum the dry stereo pair with the reverb return.
///
/// Additive on purpose: reverb only ever adds. `level` 0 leaves the dry signal
/// untouched, and the return depends on the wet tank and the level alone — never
/// on the dry — so no setting can subtract from it. The old crossfade multiplied
/// the dry by `1 - level`, which is why turning reverb up used to hollow out the
/// instrument.
fn mix_reverb(dry_l: f32, dry_r: f32, wet: f32, level: f32) -> (f32, f32) {
    let reverb = wet * level.clamp(0.0, 1.0) * REVERB_RETURN_GAIN;
    (dry_l + reverb, dry_r + reverb)
}

// -----------------------------------------------------------------------------
// Allocation
// -----------------------------------------------------------------------------

fn allocate(notes: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    if notes.is_empty() {
        return (vec![], vec![], vec![]);
    }
    let mut sorted: Vec<u8> = notes.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    match sorted.len() {
        0 => (vec![], vec![], vec![]),
        1 => {
            let n = sorted[0] as i16;
            let low = (n - 12).clamp(0, 127) as u8;
            let high = (n + 12).clamp(0, 127) as u8;
            (vec![low], vec![sorted[0]], vec![high])
        }
        2 => {
            let low = sorted[0];
            let top = sorted[1] as i16;
            let high = (top + 12).clamp(0, 127) as u8;
            (vec![low], vec![sorted[1]], vec![high])
        }
        _ => {
            let low = sorted[0];
            let high = sorted[sorted.len() - 1];
            let mid = sorted[1..sorted.len() - 1].to_vec();
            (vec![low], mid, vec![high])
        }
    }
}

// -----------------------------------------------------------------------------
// Synth
// -----------------------------------------------------------------------------

/// The handles for one stab group.
#[derive(Clone)]
struct StabGroup {
    low: Vec<VoiceHandle>,
    mid: Vec<VoiceHandle>,
    high: Vec<VoiceHandle>,
}

impl StabGroup {
    /// Every handle in the group.
    fn handles(&self) -> impl Iterator<Item = &VoiceHandle> {
        self.low.iter().chain(&self.mid).chain(&self.high)
    }

    fn reset(&self) {
        for handle in self.handles() {
            handle.reset();
        }
    }
}

/// The audio-side voices for one stab group, owned by the callback.
struct GroupVoices {
    low: Vec<Voice>,
    mid: Vec<Voice>,
    high: Vec<Voice>,
}

/// Build one group's voices and their handles.
/// One voice's noise seed, from a counter shared by the whole synth.
///
/// A counter rather than a constant per group, because a constant reset for each
/// group gives voice `i` the *same* seed in every one of them. Two rhythm layers
/// playing the same note with noise would then produce bit-identical streams and
/// sum coherently — four times the level and audibly not noise any more.
fn next_noise_seed(counter: &mut u32) -> u32 {
    // The multiplier is a value the counter never visits, so no voice can be
    // handed zero and no two voices can be handed the same seed.
    *counter = counter.wrapping_add(1);
    counter.wrapping_mul(2_654_435_761) | 1
}

fn build_group(params: &SynthParams, sample_rate: f32, seed: &mut u32) -> (StabGroup, GroupVoices) {
    let mut handles_low = Vec::with_capacity(GROUP_LOW);
    let mut handles_mid = Vec::with_capacity(GROUP_MID);
    let mut handles_high = Vec::with_capacity(GROUP_HIGH);
    let mut audio_low = Vec::with_capacity(GROUP_LOW);
    let mut audio_mid = Vec::with_capacity(GROUP_MID);
    let mut audio_high = Vec::with_capacity(GROUP_HIGH);

    for _ in 0..GROUP_LOW {
        let v = Voice::new(
            params.low.clone(),
            params.lfo_rate.clone(),
            params.lfo_wave.clone(),
            sample_rate,
            next_noise_seed(seed),
        );
        handles_low.push(v.handle());
        audio_low.push(v);
    }
    for _ in 0..GROUP_MID {
        let v = Voice::new(
            params.mid.clone(),
            params.lfo_rate.clone(),
            params.lfo_wave.clone(),
            sample_rate,
            next_noise_seed(seed),
        );
        handles_mid.push(v.handle());
        audio_mid.push(v);
    }
    for _ in 0..GROUP_HIGH {
        let v = Voice::new(
            params.high.clone(),
            params.lfo_rate.clone(),
            params.lfo_wave.clone(),
            sample_rate,
            next_noise_seed(seed),
        );
        handles_high.push(v.handle());
        audio_high.push(v);
    }

    (
        StabGroup {
            low: handles_low,
            mid: handles_mid,
            high: handles_high,
        },
        GroupVoices {
            low: audio_low,
            mid: audio_mid,
            high: audio_high,
        },
    )
}

/// The trigger side of the synth: the handles the interface writes and the audio
/// thread reads.
///
/// Split off from [`Engine`] because the two halves live on different threads.
/// The engine is moved into the audio callback and is only ever touched there;
/// this is a handful of `Arc`s that the interface keeps, and every method on it
/// amounts to a few atomic stores.
#[derive(Clone)]
pub struct Voices {
    /// One per stab group: an onset retriggers only its own group, so
    /// overlapping takes layer instead of cutting each other.
    groups: Vec<StabGroup>,
    click: Vec<VoiceHandle>,
}

impl Voices {
    /// Start a chord on one stab group at a given gain and velocity.
    ///
    /// `params` is passed in rather than held because the numbers that decide
    /// how a note is *triggered* — unison, detune, glide — are the same live
    /// parameters the audio thread reads, and a second copy of them here would
    /// be a second thing to keep in step.
    pub fn play_stab(
        &self,
        group: usize,
        notes: &[u8],
        gain: f32,
        velocity: f32,
        params: &SynthParams,
    ) {
        let Some(slot) = self.groups.get(group) else {
            return;
        };
        slot.reset();
        let (l, m, h) = allocate(notes);
        Self::trigger_block(&slot.low, &params.low, &l, gain, velocity);
        Self::trigger_block(&slot.mid, &params.mid, &m, gain, velocity);
        Self::trigger_block(&slot.high, &params.high, &h, gain, velocity);
    }

    /// Fire one note list down a block of voices, `UNISON_MAX` per note.
    ///
    /// Note `i` owns the contiguous slice `i * UNISON_MAX .. (i + 1) *
    /// UNISON_MAX`, and only the first `unison` of that slice is started. The
    /// rest were released by `reset`, so a voice that *was* sounding — the
    /// spare slot of a stack that just got narrower, or a note that dropped out
    /// of the chord — rings out over its release time rather than being cut.
    /// That way changing the unison count never has to reallocate a voice pool
    /// mid-playback.
    fn trigger_block(
        handles: &[VoiceHandle],
        channel: &ChannelParams,
        notes: &[u8],
        gain: f32,
        velocity: f32,
    ) {
        let n = (channel.unison.get().round() as i32).clamp(1, UNISON_MAX as i32) as usize;
        let spread = channel.detune.get().clamp(0.0, MAX_DETUNE_CENTS);
        let glide = channel.glide.get().clamp(0.0, MAX_GLIDE_SECS);
        // How a stack sums depends on whether it is detuned, and normalising
        // for the wrong one is a bug in both directions: `n` *detuned* copies add
        // incoherently and grow with `sqrt(n)`, while `n` copies at the same
        // frequency — which is what unison with a spread of zero is — add
        // coherently and grow with `n`. Dividing a coherent stack by `sqrt(n)`
        // makes it up to 2× too loud at four voices, and dividing a detuned one
        // by `n` would make it needlessly quiet.
        // A step rather than a smooth blend, deliberately: a spread of 0.001
        // cents is still a coherent stack over any note short enough to hear,
        // so the fully honest rule would need the note's length and frequency
        // too. The step is right at both ends and only arguable at the boundary,
        // and the panel's smallest step is 2 cents.
        let divisor = if spread > 0.0 {
            (n as f32).sqrt()
        } else {
            n as f32
        };
        let level = gain.clamp(0.0, 1.0) / divisor;

        for (i, &note) in notes.iter().enumerate() {
            for u in 0..n {
                let Some(handle) = handles.get(i * UNISON_MAX + u) else {
                    continue;
                };
                handle.trigger_at(note, level, velocity, glide, unison_spread(n, u, spread));
            }
        }
    }

    /// Release one stab group.
    pub fn stop_stab(&self, group: usize) {
        if let Some(slot) = self.groups.get(group) {
            slot.reset();
        }
    }

    /// Release every group.
    ///
    /// Used when playback is interrupted: a seek or a stop abandons the
    /// schedule, so the releases the plan still owed will never arrive and the
    /// notes have to be cut here.
    pub fn silence(&self) {
        for group in &self.groups {
            group.reset();
        }
    }

    /// A short metronome blip, audible only while a rhythm is being recorded.
    ///
    /// `sound` picks one of [`CLICK_SOUNDS`]; `volume` is the metronome's own
    /// level, applied on the voice so it can be set independently of the mid
    /// channel the click borrows its tone from.
    pub fn play_click(&self, strong: bool, sound: usize, volume: f32) {
        if let Some(handle) = self.click.first() {
            let preset = &CLICK_SOUNDS[sound.min(CLICK_SOUNDS.len() - 1)];
            handle.reset();
            let note = if strong {
                preset.strong_note
            } else {
                preset.weak_note
            };
            handle.trigger_glide_at(note, note, preset.attack_secs, preset.hold_secs, volume);
        }
    }
}

pub struct Synth {
    params: SynthParams,
    voices: Voices,
    timing: Arc<crate::timing::Callback>,
    _stream: cpal::Stream,
}

// -----------------------------------------------------------------------------
// The audio engine
// -----------------------------------------------------------------------------

/// Everything the audio callback owns.
///
/// This used to be the body of the closure handed to cpal, which meant the only
/// way to run the signal path was to have an audio device and the only way to
/// look at what it produced was to listen. The body is the same code; it lives
/// in a method now so that a test can drive it, a benchmark can time it, and an
/// assertion can be made about what it allocates.
///
/// Nothing here is behind a lock and nothing here allocates. `Synth` keeps the
/// *handles* — its `play_stab` writes atomics that these voices read — and the
/// engine keeps the voices themselves.
pub struct Engine {
    groups: Vec<GroupVoices>,
    click: Vec<Voice>,
    fx: FxBank,
    analyzer: Analyzer,
    low_eq: Eq,
    mid_eq: Eq,
    high_eq: Eq,
    master_eq: Eq,
    low_eq_state: EqState,
    mid_eq_state: EqState,
    high_eq_state: EqState,
    master_eq_l: EqState,
    master_eq_r: EqState,
    sample_rate: f32,
    channels: usize,
    /// Whether the analyser ran for the buffer before this one, so the off-to-on
    /// edge can be spotted and the banks cleared on it.
    analyzing: bool,
    /// What each buffer cost against the deadline it had. Shared with whatever
    /// is watching — the output tap, or nothing at all in a test.
    timing: Arc<crate::timing::Callback>,
}

impl Engine {
    /// Build the audio side: the voice pool, the effect bank, the curves and the
    /// analyser.
    ///
    /// Everything that allocates happens here, on the main thread, before a
    /// single sample is asked for. The trigger handles come back separately
    /// because they belong to the interface thread; the engine itself is what
    /// goes to the audio callback.
    pub fn build(
        params: &SynthParams,
        sample_rate: f32,
        channels: usize,
        timing: Arc<crate::timing::Callback>,
    ) -> (Self, Voices) {
        // Build the wavetables here, before a single voice exists. `Voice::new`
        // would do it lazily anyway, but that would leave the work one refactor
        // away from happening inside the audio callback — where allocating a few
        // dozen kilobytes and running a few thousand `sin` calls is not
        // something to discover by ear.
        let _tables = crate::wavetable::tables();

        // One counter for every voice in the synth, so the seeds are distinct
        // across groups and not merely within one.
        let mut seed = 0u32;

        let mut handlers = Vec::with_capacity(STAB_GROUPS + 1);
        let mut audio_groups: Vec<GroupVoices> = Vec::with_capacity(STAB_GROUPS + 1);
        // One past the stab groups: the audition voice, mixed and released like
        // any other and reached only from the UI thread.
        for _ in 0..=STAB_GROUPS {
            let (group, voices) = build_group(params, sample_rate, &mut seed);
            handlers.push(group);
            audio_groups.push(voices);
        }

        let mut audio_click: Vec<Voice> = Vec::with_capacity(CLICK_VOICES);
        let mut click = Vec::with_capacity(CLICK_VOICES);
        for _ in 0..CLICK_VOICES {
            // The mid channel, because a click sits in the range the player is
            // already listening to. Drawn from the same counter, so a click's
            // noise is never the same stream as a voice's.
            let v = Voice::new(
                params.mid.clone(),
                params.lfo_rate.clone(),
                params.lfo_wave.clone(),
                sample_rate,
                next_noise_seed(&mut seed),
            );
            click.push(v.handle());
            audio_click.push(v);
        }

        let voices = Voices {
            groups: handlers,
            click,
        };

        // The effect bank, the analyser and the four curves are built here too:
        // eighteen chain slots, two aux units and 65 biquads' worth of sections,
        // all allocated before the stream starts.
        let engine = Engine {
            groups: audio_groups,
            click: audio_click,
            fx: FxBank::new(sample_rate),
            analyzer: Analyzer::new(sample_rate),
            low_eq: Eq::new(sample_rate),
            mid_eq: Eq::new(sample_rate),
            high_eq: Eq::new(sample_rate),
            master_eq: Eq::new(sample_rate),
            low_eq_state: EqState::default(),
            mid_eq_state: EqState::default(),
            high_eq_state: EqState::default(),
            master_eq_l: EqState::default(),
            master_eq_r: EqState::default(),
            sample_rate,
            channels,
            analyzing: true,
            timing,
        };
        (engine, voices)
    }

    /// What each buffer cost against the deadline it had.
    ///
    /// The audio thread writes it and the interface thread reads it, so the
    /// reading is always a moment behind — which is the only kind of reading a
    /// real-time thread can offer.
    pub fn timing(&self) -> &Arc<crate::timing::Callback> {
        &self.timing
    }

    /// The sample rate the engine was built for.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// How many interleaved channels one frame of `out` holds.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// One buffer of the real signal path.
    ///
    /// `params` is the live parameter set, passed in rather than captured, which
    /// is what makes this callable from a test. `peak` is the meter's atomic, if
    /// anything is watching it.
    pub fn process(&mut self, data: &mut [f32], params: &SynthParams, peak: Option<&AtomicU32>) {
        // One `Instant::now()` here and one at the end, against a deadline of
        // ten milliseconds. The alternative — a build with the timing compiled
        // out — measures a program that does not exist.
        let started = Instant::now();
        let deadline = Duration::from_secs_f64(
            data.len() as f64 / self.channels as f64 / self.sample_rate as f64,
        );

        let master_muted = params.master_mute.get() > 0.5;
        let master_gain = if master_muted {
            0.0
        } else {
            (params.master_volume.get() / 7.0).clamp(0.0, 1.0)
        };

        let low_gain = (params.low.volume.get() / 7.0).clamp(0.0, 1.0);
        let mid_gain = (params.mid.volume.get() / 7.0).clamp(0.0, 1.0);
        let high_gain = (params.high.volume.get() / 7.0).clamp(0.0, 1.0);

        let low_pan = params.low.pan.get().clamp(-1.0, 1.0);
        let mid_pan = params.mid.pan.get().clamp(-1.0, 1.0);
        let high_pan = params.high.pan.get().clamp(-1.0, 1.0);

        let low_l = (1.0 - low_pan) * 0.5;
        let low_r = (1.0 + low_pan) * 0.5;
        let mid_l = (1.0 - mid_pan) * 0.5;
        let mid_r = (1.0 + mid_pan) * 0.5;
        let high_l = (1.0 - high_pan) * 0.5;
        let high_r = (1.0 + high_pan) * 0.5;

        let low_send = params.low.reverb_send.get().clamp(0.0, 1.0);
        let mid_send = params.mid.reverb_send.get().clamp(0.0, 1.0);
        let high_send = params.high.reverb_send.get().clamp(0.0, 1.0);

        let low_delay_send = params.low.delay_send.get().clamp(0.0, 1.0);
        let mid_delay_send = params.mid.delay_send.get().clamp(0.0, 1.0);
        let high_delay_send = params.high.delay_send.get().clamp(0.0, 1.0);

        let reverb_mix = params.reverb_mix.get().clamp(0.0, 1.0);
        let delay_mix = params.delay_mix.get().clamp(0.0, 1.0);
        let tempo = params.tempo.get();
        let aux_reverb = params.aux_reverb.fx();
        let aux_delay = params.aux_delay.fx();
        // A return that is turned down is not run at all. Both returns are
        // additive, so at zero they contribute exactly nothing — and not
        // running them means a unit turned up mid-loop starts from silence
        // rather than from a tank that has been ringing unheard.
        let reverb_running = reverb_mix > 0.0;
        let delay_running = delay_mix > 0.0;

        // The chains, and whether each has anything in it: a rack of six
        // empty slots is the shipped state of every ensemble, and walking
        // it per sample to discover that would be the whole cost of the
        // feature at rest.
        let low_chain = params.low.chain.chain();
        let mid_chain = params.mid.chain.chain();
        let high_chain = params.high.chain.chain();
        let low_active = params.low.chain.any();
        let mid_active = params.mid.chain.any();
        let high_active = params.high.chain.any();

        // Once per buffer, not once per sample: `set` compares the curve
        // it already has and returns immediately, so a panel that is not
        // being touched costs four dozen float compares per buffer.
        self.low_eq.set(&params.low.eq.curve());
        self.mid_eq.set(&params.mid.eq.curve());
        self.high_eq.set(&params.high.eq.curve());
        self.master_eq.set(&params.master_eq.curve());
        self.analyzer
            .set_speed(params.analyzer.release.get(), self.sample_rate);

        // Whether anybody is looking at the spectrum, read once for the buffer.
        // Turning it back on clears the banks first, so the first frame draws the
        // sound that is playing now rather than the one that was playing when the
        // panel was last closed.
        let analyzing = params.analyzer.enabled.get() > 0.5;
        if analyzing && !self.analyzing {
            self.analyzer.clear();
        }
        self.analyzing = analyzing;

        // Once per buffer, before a single sample: the note's frequency and the
        // filter's coefficient for every voice that can have them computed here
        // rather than 48,000 times a second. A hundred and twenty-one voices
        // times a `powf` and a `sin` is the whole point of the pass, and it costs
        // two relaxed loads per voice to decide whether it is allowed.
        for group in self.groups.iter_mut() {
            for v in group
                .low
                .iter_mut()
                .chain(&mut group.mid)
                .chain(&mut group.high)
            {
                v.cache();
            }
        }
        for v in self.click.iter_mut() {
            v.cache();
        }

        for frame in data.chunks_mut(self.channels) {
            let mut prog_l = 0.0;
            let mut prog_m = 0.0;
            let mut prog_h = 0.0;
            let mut click_out = 0.0;

            for group in self.groups.iter_mut() {
                for v in group.low.iter_mut() {
                    prog_l += v.tick();
                }
                for v in group.mid.iter_mut() {
                    prog_m += v.tick();
                }
                for v in group.high.iter_mut() {
                    prog_h += v.tick();
                }
            }
            // The click bypasses the progression's mute and gain: it is
            // a rehearsal aid, not part of the music.
            for v in self.click.iter_mut() {
                click_out += v.tick();
            }

            // The order is the channel strip: the curve shapes the part,
            // the chain performs on it, and the fader sets its level —
            // and all of it is before the pan, so it is what is sent to
            // the reverb as well as what is heard. Shaping a part and
            // then reverbing it is what a mixer does; the alternative
            // would put an effected part in an uneffected space.
            let shaped_l = self.low_eq.tick(&mut self.low_eq_state, prog_l);
            let shaped_m = self.mid_eq.tick(&mut self.mid_eq_state, prog_m);
            let shaped_h = self.high_eq.tick(&mut self.high_eq_state, prog_h);
            let chained_l = if low_active {
                self.fx.chain(0, &low_chain, tempo, shaped_l)
            } else {
                shaped_l
            };
            let chained_m = if mid_active {
                self.fx.chain(1, &mid_chain, tempo, shaped_m)
            } else {
                shaped_m
            };
            let chained_h = if high_active {
                self.fx.chain(2, &high_chain, tempo, shaped_h)
            } else {
                shaped_h
            };

            let low_out = chained_l * low_gain;
            let mid_out = (chained_m * mid_gain) + click_out;
            let high_out = chained_h * high_gain;

            // The register meters read what each part contributes: after
            // its curve and its fader, before the pan, so a register that
            // is muted reads nothing and a register that is panned hard
            // still reads its own level.
            if analyzing {
                self.analyzer.tick(0, low_out);
                self.analyzer.tick(1, mid_out);
                self.analyzer.tick(2, high_out);
            }

            let left = low_out * low_l + mid_out * mid_l + high_out * high_l;
            let right = low_out * low_r + mid_out * mid_r + high_out * high_r;

            let send_sum =
                |a: f32, b: f32, c: f32| (low_out * a + mid_out * b + high_out * c) * 0.3;

            let (mixed_l, mixed_r) = if reverb_running {
                let wet =
                    self.fx
                        .reverb(&aux_reverb, tempo, send_sum(low_send, mid_send, high_send));
                mix_reverb(left, right, wet, reverb_mix)
            } else {
                (left, right)
            };

            let (mixed_l, mixed_r) = if delay_running {
                let echo = self.fx.delay(
                    &aux_delay,
                    tempo,
                    send_sum(low_delay_send, mid_delay_send, high_delay_send),
                );
                mix_delay(mixed_l, mixed_r, echo, delay_mix)
            } else {
                (mixed_l, mixed_r)
            };

            // Last stop before the master gain, so this is the tone
            // control for the whole instrument.
            let final_l = self.master_eq.tick(&mut self.master_eq_l, mixed_l);
            let final_r = self.master_eq.tick(&mut self.master_eq_r, mixed_r);

            let sample_l = (final_l * master_gain).tanh();
            let sample_r = (final_r * master_gain).tanh();

            // And the master meter reads what leaves the device: the
            // finished pair, after the curve, the gain and the clip.
            if analyzing {
                self.analyzer.tick(3, (sample_l + sample_r) * 0.5);
            }

            if let Some(peak) = peak {
                let peak_val = sample_l.abs().max(sample_r.abs());
                let cur = f32::from_bits(peak.load(Ordering::Relaxed));
                if peak_val > cur {
                    peak.store(peak_val.to_bits(), Ordering::Relaxed);
                }
            }

            if self.channels == 1 {
                frame[0] = (sample_l + sample_r) * 0.5;
            } else {
                frame[0] = sample_l;
                frame[1] = sample_r;
                for s in frame.iter_mut().skip(2) {
                    *s = 0.0;
                }
            }
        }

        // Once per buffer rather than once per sample: fifty-two stores
        // at the end of a buffer is nothing, and it means the panel's
        // reads are never torn across a buffer boundary.
        //
        // Skipped along with the banks when nobody is looking: the levels then
        // hold the last thing the panel drew, which is the honest reading of a
        // readout that is not being read.
        if analyzing {
            for (tap, out) in params.analyzer.taps.iter().enumerate() {
                for (slot, level) in out.levels.iter().zip(self.analyzer.levels(tap)) {
                    slot.set(level);
                }
            }
        }

        self.timing.record(started.elapsed(), deadline);
    }
}

impl Synth {
    pub fn new(
        peak_tap: Option<Arc<AtomicU32>>,
        timing: Arc<crate::timing::Callback>,
    ) -> Result<Self, Box<dyn Error>> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no audio output device available")?;
        let config = device.default_output_config()?;
        let sample_rate = config.sample_rate().0 as f32;
        let channels = config.channels() as usize;

        let params = SynthParams::defaults();

        let (mut engine, voices) = Engine::build(&params, sample_rate, channels, timing.clone());

        let stream_params = params.clone();
        let err_fn = |err| eprintln!("audio stream error: {}", err);

        let stream = device.build_output_stream(
            &config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                engine.process(data, &stream_params, peak_tap.as_deref())
            },
            err_fn,
            None,
        )?;
        stream.play()?;

        Ok(Synth {
            params,
            voices,
            timing,
            _stream: stream,
        })
    }

    /// What each buffer cost against the deadline it had.
    pub fn timing(&self) -> &Arc<crate::timing::Callback> {
        &self.timing
    }

    pub fn params(&self) -> &SynthParams {
        &self.params
    }

    // ---- stab groups ----

    /// Start a chord on one stab group at a given gain.
    ///
    /// Only that group is retriggered, which is what lets the layers of a
    /// pattern overlap: a hit on take 2 never cuts take 1.
    pub fn play_stab(&self, group: usize, notes: &[u8], gain: f32, velocity: f32) {
        self.voices
            .play_stab(group, notes, gain, velocity, &self.params);
    }

    /// Release one stab group.
    pub fn stop_stab(&self, group: usize) {
        self.voices.stop_stab(group);
    }

    /// Release every group.
    ///
    /// Used when playback is interrupted: a seek or a stop abandons the
    /// schedule, so the releases the plan still owed will never arrive and the
    /// notes have to be cut here.
    pub fn silence(&self) {
        self.voices.silence();
    }

    // ---- metronome ----

    /// A short metronome blip, audible only while a rhythm is being recorded.
    ///
    /// `sound` picks one of [`CLICK_SOUNDS`]; `volume` is the metronome's own
    /// level, applied on the voice so it can be set independently of the mid
    /// channel the click borrows its tone from.
    pub fn play_click(&self, strong: bool, sound: usize, volume: f32) {
        self.voices.play_click(strong, sound, volume);
    }

    // ---- channel bridge ----

    /// Snapshot the live sound: three composed channels and the global block.
    ///
    /// `note_length` is transport state, not synth state, so the caller supplies
    /// it; assembling an `Ensemble` out of this is the caller's job, because only
    /// the caller knows which instrument each register was loaded from.
    pub fn capture_channels(&self, note_length: f32) -> ([ComposedChannel; 3], MixerSettings) {
        (
            [
                capture_channel(&self.params.low),
                capture_channel(&self.params.mid),
                capture_channel(&self.params.high),
            ],
            capture_mixer(&self.params, note_length),
        )
    }
}

/// Apply three composed channels and the global block to a parameter set.
///
/// The audio layer deliberately knows nothing about ensembles or instruments: by
/// the time anything reaches here a placement has been resolved against the
/// library and composed with its voice, and all that is left is two dozen numbers
/// per register.
///
/// A free function rather than a method on `Synth`, for the same reason
/// `apply_mixer` is one: this needs only `SynthParams`, and a `Synth` owns an
/// audio device a test cannot create.
pub fn apply_channels(dst: &SynthParams, channels: [&ComposedChannel; 3], mixer: &MixerSettings) {
    for (dst, src) in [&dst.low, &dst.mid, &dst.high].into_iter().zip(channels) {
        apply_channel(dst, src);
    }
    apply_mixer(dst, mixer);
}

/// Write the global block: the mixer, the two aux units and the master curve.
///
/// A free function for the same reason `apply_channel` is one: everything here
/// needs only `SynthParams`, and a `Synth` owns an audio device that no test can
/// create. Splitting it is what makes the mixer testable at all — the aux units
/// are two whole effects now, and "does an ensemble's reverb survive a save and
/// a load" is not a question to leave to the one code path that cannot be run.
pub(crate) fn apply_mixer(dst: &SynthParams, src: &MixerSettings) {
    dst.reverb_mix.set(src.reverb_mix);
    dst.delay_mix.set(src.delay_mix);
    dst.master_volume.set(src.master_volume);
    dst.lfo_rate.set(src.lfo_rate);
    dst.lfo_wave.set(src.lfo_wave as i32 as f32);
    dst.master_eq.set(&src.master_eq);
    dst.aux_reverb.set(&src.reverb);
    dst.aux_delay.set(&src.delay);
    dst.master_mute.set(if src.master_mute { 1.0 } else { 0.0 });
}

/// The global block as it stands. `note_length` is transport state, so the caller
/// supplies it.
pub(crate) fn capture_mixer(src: &SynthParams, note_length: f32) -> MixerSettings {
    MixerSettings {
        reverb_mix: src.reverb_mix.get(),
        delay_mix: src.delay_mix.get(),
        master_volume: src.master_volume.get(),
        master_mute: src.master_mute.get() > 0.5,
        lfo_rate: src.lfo_rate.get(),
        lfo_wave: LfoWave::from_f32(src.lfo_wave.get()),
        note_length,
        master_eq: src.master_eq.curve(),
        reverb: src.aux_reverb.fx(),
        delay: src.aux_delay.fx(),
    }
}

pub(crate) fn apply_channel(dst: &ChannelParams, src: &ComposedChannel) {
    let v = &src.voice;
    dst.delay_send.set(src.delay_send);
    dst.chain.set(&src.chain);
    dst.volume.set(src.volume);
    dst.waveform.set(v.waveform as i32 as f32);
    dst.noise_level.set(v.noise_level);
    dst.pulse_width.set(v.pulse_width);
    dst.attack.set(v.attack);
    dst.decay.set(v.decay);
    dst.sustain.set(v.sustain);
    dst.release.set(v.release);
    dst.env_curve.set(v.env_curve);
    dst.glide.set(v.glide);
    dst.cutoff.set(v.cutoff);
    dst.resonance.set(v.resonance);
    dst.filter_type.set(v.filter_type as i32 as f32);
    dst.filter_env.set(v.filter_env);
    dst.filter_attack.set(v.filter_attack);
    dst.filter_decay.set(v.filter_decay);
    dst.key_track.set(v.key_track);
    dst.lfo_pitch.set(v.lfo_pitch);
    dst.lfo_cutoff.set(v.lfo_cutoff);
    dst.lfo_amp.set(v.lfo_amp);
    dst.lfo_pwm.set(v.lfo_pwm);
    dst.unison.set(v.unison);
    dst.detune.set(v.detune);
    dst.transpose.set(src.transpose);
    dst.reverb_send.set(src.reverb_send);
    dst.pan.set(src.pan);
    dst.eq.set(&src.eq);
}

/// Write only what a sound *is*, leaving where it sits alone.
///
/// This is what auditioning an instrument does: two dozen parameters change and
/// the register keeps its level, its pan, its transpose, how much reverb it
/// sends and its thirteen-band curve. Swapping a sound should not rebalance the
/// mix it is being tried in, and it should not throw away the equalisation
/// somebody just dialled in for that part.
pub(crate) fn apply_voice(dst: &ChannelParams, src: &VoicePatch) {
    dst.waveform.set(src.waveform as i32 as f32);
    dst.noise_level.set(src.noise_level);
    dst.pulse_width.set(src.pulse_width);
    dst.attack.set(src.attack);
    dst.decay.set(src.decay);
    dst.sustain.set(src.sustain);
    dst.release.set(src.release);
    dst.env_curve.set(src.env_curve);
    dst.glide.set(src.glide);
    dst.cutoff.set(src.cutoff);
    dst.resonance.set(src.resonance);
    dst.filter_type.set(src.filter_type as i32 as f32);
    dst.filter_env.set(src.filter_env);
    dst.filter_attack.set(src.filter_attack);
    dst.filter_decay.set(src.filter_decay);
    dst.key_track.set(src.key_track);
    dst.lfo_pitch.set(src.lfo_pitch);
    dst.lfo_cutoff.set(src.lfo_cutoff);
    dst.lfo_amp.set(src.lfo_amp);
    dst.lfo_pwm.set(src.lfo_pwm);
    dst.unison.set(src.unison);
    dst.detune.set(src.detune);
    dst.drive.set(src.drive);
    dst.vel_cutoff.set(src.vel_cutoff);
    dst.vel_pwm.set(src.vel_pwm);
    dst.position.set(src.position);
    dst.phase_dist.set(src.phase_dist);
    dst.osc2_waveform.set(src.osc2_waveform as i32 as f32);
    dst.osc2_interval.set(src.osc2_interval);
    dst.osc2_level.set(src.osc2_level);
    dst.osc2_fm.set(src.osc2_fm);
    dst.fm_mode.set(src.fm_mode as i32 as f32);
    dst.feedback.set(src.feedback);
    dst.osc2_ring.set(src.osc2_ring);
    dst.pluck_decay.set(src.pluck_decay);
    dst.pluck_damp.set(src.pluck_damp);
    dst.pluck_burst.set(src.pluck_burst);
}

/// The voice half of a live channel.
pub(crate) fn capture_voice(src: &ChannelParams) -> VoicePatch {
    VoicePatch {
        waveform: Waveform::from_f32(src.waveform.get()),
        noise_level: src.noise_level.get(),
        pulse_width: src.pulse_width.get(),
        attack: src.attack.get(),
        decay: src.decay.get(),
        sustain: src.sustain.get(),
        release: src.release.get(),
        env_curve: src.env_curve.get(),
        glide: src.glide.get(),
        cutoff: src.cutoff.get(),
        resonance: src.resonance.get(),
        filter_type: FilterType::from_f32(src.filter_type.get()),
        filter_env: src.filter_env.get(),
        filter_attack: src.filter_attack.get(),
        filter_decay: src.filter_decay.get(),
        key_track: src.key_track.get(),
        lfo_pitch: src.lfo_pitch.get(),
        lfo_cutoff: src.lfo_cutoff.get(),
        lfo_amp: src.lfo_amp.get(),
        lfo_pwm: src.lfo_pwm.get(),
        unison: src.unison.get(),
        detune: src.detune.get(),
        drive: src.drive.get(),
        vel_cutoff: src.vel_cutoff.get(),
        vel_pwm: src.vel_pwm.get(),
        position: src.position.get(),
        phase_dist: src.phase_dist.get(),
        osc2_waveform: Waveform::from_f32(src.osc2_waveform.get()),
        osc2_interval: src.osc2_interval.get(),
        osc2_level: src.osc2_level.get(),
        osc2_fm: src.osc2_fm.get(),
        fm_mode: FmMode::from_f32(src.fm_mode.get()),
        feedback: src.feedback.get(),
        osc2_ring: src.osc2_ring.get(),
        pluck_decay: src.pluck_decay.get(),
        pluck_damp: src.pluck_damp.get(),
        pluck_burst: src.pluck_burst.get(),
    }
}

/// The whole composed channel, voice and placement together.
pub(crate) fn capture_channel(src: &ChannelParams) -> ComposedChannel {
    ComposedChannel {
        voice: capture_voice(src),
        volume: src.volume.get(),
        transpose: src.transpose.get(),
        reverb_send: src.reverb_send.get(),
        delay_send: src.delay_send.get(),
        pan: src.pan.get(),
        eq: src.eq.curve(),
        chain: src.chain.chain(),
    }
}

pub fn midi_to_hz(note: f32) -> f32 {
    440.0 * 2.0f32.powf((note - 69.0) / 12.0)
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ensemble::Ensemble;
    use crate::fx::{P0, P4};

    // ---- the effect bridge ----

    #[test]
    fn a_chain_and_both_sends_survive_the_live_parameters() {
        // `apply_channel` and `capture_channel` have to stay exact inverses: a
        // panel that writes a chain and a save that reads it back are the same
        // two functions, so a field missed by either is a field that silently
        // resets. The existing inverse test only moves scalars; this one moves a
        // rack.
        let params = SynthParams::defaults();
        let mut channel = ComposedChannel::neutral(4.0, 4000.0);
        channel.delay_send = 0.35;
        channel.reverb_send = 0.2;
        channel.chain[0] = Fx::variant(FxKind::Fuzz, FxSubtype::Germanium);
        channel.chain[5] = Fx::variant(FxKind::Reverb, FxSubtype::Room);
        apply_channel(&params.mid, &channel);
        assert_eq!(capture_channel(&params.mid), channel);

        // And the rack knows there is something in it, without walking six slots
        // per sample to find out.
        assert!(params.mid.chain.any());
        assert!(!params.low.chain.any(), "the other registers are untouched");
        assert_eq!(params.mid.delay_send.get(), 0.35);
        assert_eq!(params.low.delay_send.get(), 0.0);
    }

    #[test]
    fn an_empty_rack_reports_itself_empty_however_it_was_filled() {
        // The one check that keeps the feature free at rest: three registers of
        // six empty slots is the shipped state of every ensemble.
        let params = SynthParams::defaults();
        for channel in [&params.low, &params.mid, &params.high] {
            assert!(!channel.chain.any());
        }
        // A slot that is `none` in six different ways is still empty.
        params.low.chain.slots[3].set_kind(FxKind::None);
        assert!(!params.low.chain.any());
        params.low.chain.slots[3].set_kind(FxKind::Tremolo);
        assert!(params.low.chain.any());
    }

    #[test]
    fn the_two_aux_units_and_both_returns_survive_apply_and_capture() {
        let params = SynthParams::defaults();
        let mut mixer = capture_mixer(&params, 1.0);
        mixer.reverb_mix = 0.42;
        mixer.delay_mix = 0.31;
        mixer.reverb = Fx::variant(FxKind::Reverb, FxSubtype::Plate);
        mixer.reverb.set_param(P0, 0.77);
        mixer.delay = Fx::variant(FxKind::Delay, FxSubtype::Tape);
        mixer.delay.set_param(P4, 1.0);

        apply_mixer(&params, &mixer);
        assert_eq!(capture_mixer(&params, 1.0), mixer);
        // The units really did move, rather than the comparison passing on two
        // defaults.
        assert_eq!(params.aux_reverb.fx().subtype, FxSubtype::Plate);
        assert_eq!(params.aux_delay.fx().param(P4), 1.0);
        assert_eq!(params.delay_mix.get(), 0.31);
    }

    #[test]
    fn the_tempo_a_synced_delay_follows_is_a_live_parameter() {
        // The audio thread has no transport, so the tempo has to reach it the way
        // everything else does. A synced delay reading a stale 120 would be a
        // delay that locks to the wrong bar.
        let params = SynthParams::defaults();
        assert_eq!(params.tempo.get(), 120.0);
        params.tempo.set(96.0);
        assert_eq!(params.tempo.get(), 96.0);
    }

    #[test]
    fn a_slot_left_half_changed_still_reads_as_a_real_effect() {
        // A panel changes a type and a variant in two writes, and the callback
        // can read the atomic between them. What comes out has to be a real
        // effect rather than a panic or a zeroed sound.
        let params = SynthParams::defaults();
        let slot = &params.low.chain.slots[0];
        slot.set_kind(FxKind::Delay);
        // The kind is a delay and the variant is still whatever the last one was.
        let fx = slot.fx();
        assert_eq!(fx.kind, FxKind::Delay);
        assert!(
            FxKind::Delay.subtypes().contains(&fx.subtype),
            "{:?} is not a delay variant",
            fx.subtype
        );
        // And a value from nowhere lands on a real kind rather than panicking.
        slot.kind.set(99_999.0);
        assert!(!slot.fx().kind.name().is_empty());
        slot.kind.set(f32::NAN);
        assert!(!slot.fx().kind.name().is_empty());
    }

    // ---- effect routing, at the level the callback runs it ----

    #[test]
    fn midi_to_hz_a4_is_440() {
        assert!((midi_to_hz(69.0) - 440.0).abs() < 0.001);
    }

    #[test]
    fn midi_to_hz_octave_doubles() {
        assert!((midi_to_hz(81.0) / midi_to_hz(69.0) - 2.0).abs() < 0.001);
    }

    #[test]
    fn shared_f32_round_trips() {
        let s = SharedF32::new(1.5);
        assert_eq!(s.get(), 1.5);
        s.set(2.75);
        assert_eq!(s.get(), 2.75);
    }

    #[test]
    fn waveform_from_f32_matches_variants() {
        assert_eq!(Waveform::from_f32(0.0), Waveform::Sine);
        assert_eq!(Waveform::from_f32(1.0), Waveform::Saw);
        assert_eq!(Waveform::from_f32(2.0), Waveform::Square);
        assert_eq!(Waveform::from_f32(3.0), Waveform::Triangle);
    }

    // ---- reverb is additive ----

    #[test]
    fn zero_reverb_leaves_the_dry_signal_alone() {
        // The old crossfade multiplied the dry by `1 - level`; at zero level
        // that happened to be a no-op, so this pins the identity down.
        assert_eq!(mix_reverb(0.5, -0.25, 0.8, 0.0), (0.5, -0.25));
    }

    #[test]
    fn reverb_only_ever_adds_to_the_dry_signal() {
        // The defining property: the return is a function of the wet tank and
        // the level, never of the dry. So no setting can subtract from it.
        for level in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let (l, r) = mix_reverb(0.37, -0.42, 0.11, level);
            let expected = 0.11 * level * REVERB_RETURN_GAIN;
            assert!(
                (l - (0.37 + expected)).abs() < 1e-6,
                "left dry was altered at level {}",
                level
            );
            assert!(
                (r - (-0.42 + expected)).abs() < 1e-6,
                "right dry was altered at level {}",
                level
            );
        }
    }

    #[test]
    fn full_reverb_adds_three_times_the_wet_signal() {
        // Deliberately the same gain the old crossfade used at maximum, so
        // full reverb is as loud as it always was.
        let (l, _) = mix_reverb(0.0, 0.0, 0.2, 1.0);
        assert!((l - 0.6).abs() < 1e-6);
    }

    #[test]
    fn the_reverb_level_is_clamped() {
        let (l, _) = mix_reverb(0.0, 0.0, 0.2, 5.0);
        assert!(
            (l - 0.6).abs() < 1e-6,
            "an over-range level must not run away"
        );
        let (l, _) = mix_reverb(0.0, 0.0, 0.2, -1.0);
        assert_eq!(l, 0.0);
    }

    #[test]
    fn silence_in_stays_silence_out() {
        assert_eq!(mix_reverb(0.0, 0.0, 0.0, 1.0), (0.0, 0.0));
    }

    #[test]
    fn a_pulse_has_no_direct_current_whatever_its_width() {
        // The bug the review caught: a plain +1/-1 pulse with a duty of w has a
        // mean of 2w-1, and this filter passes DC with unity gain, so the bias
        // would reach the reverb and the master limiter. Two levels scaled by
        // the width fix it. Measured as the mean of the output, which is exactly
        // what the old tests could not see.
        for width in [0.05, 0.2, 0.35, 0.5, 0.65, 0.8, 0.95] {
            let mut v = square_at(width);
            v.channel.cutoff.set(8000.0);
            let all = play(&mut v, 60, 48_000);
            // Skip the attack and the filter's settling so the mean is of the
            // steady state.
            let steady = &all[2_400..];
            let mean: f32 = steady.iter().sum::<f32>() / steady.len() as f32;

            // The window is not a whole number of cycles — 261.6 Hz into 48 kHz
            // is 183.5 samples — so a little of the waveform's own swing leaks
            // into the mean, bounded by about one cycle's worth of it. That is
            // the tolerance; the offset this guards against is not close to it.
            let cycles = steady.len() as f32 * 261.63 / 48_000.0;
            let leak = 2.0 / cycles;
            assert!(
                mean.abs() < leak,
                "a pulse of width {} carries a DC offset of {}, past the {} a \
                 partial cycle can explain",
                width,
                mean,
                leak
            );

            // How much the old +1/-1 form would have been off by. If this ever
            // stops being far larger than the tolerance above, the test has
            // stopped being able to tell the bug from the measurement.
            // An even pulse has no offset to guard against, so this only means
            // anything for the widths where the old form would have been wrong.
            if (width - 0.5).abs() > 0.01 {
                let buggy = (2.0 * width - 1.0).abs();
                assert!(
                    buggy > leak * 20.0,
                    "the test can no longer distinguish the offset it guards against"
                );
            }
        }
    }

    #[test]
    fn an_even_pulse_is_the_square_it_always_was() {
        // Width 0.5 has to give exactly +1 and -1, or the compatibility replay
        // would have failed — this says why it did not.
        let mut v = square_at(0.5);
        v.channel.cutoff.set(8000.0);
        let all = play(&mut v, 60, 4_800);
        let peak = all.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            (peak - 1.0).abs() < 1e-3,
            "an even square peaks at 1, got {}",
            peak
        );
    }

    #[test]
    fn a_narrow_pulse_carries_less_energy_than_an_even_one() {
        // The DC-free form makes the RMS fall with the width — `4w(1-w)` — which
        // is what a thinner pulse physically is. Worth pinning, because it means
        // narrowing a pulse does not silently get louder.
        let energy = |width: f32| {
            let mut v = square_at(width);
            v.channel.cutoff.set(8000.0);
            rms(&play(&mut v, 60, 48_000))
        };
        let even = energy(0.5);
        for width in [0.15, 0.3, 0.7, 0.85] {
            assert!(
                energy(width) < even,
                "width {} should carry less energy than an even square",
                width
            );
        }
    }

    #[test]
    fn noise_is_reachable_from_the_control_value() {
        // The panel stores a waveform as a float and reads it back through
        // `from_f32`, so a variant the conversion does not know about is a
        // waveform the UI can select and never hear.
        for wave in Waveform::ALL {
            assert_eq!(Waveform::from_f32(wave as i32 as f32), wave);
        }
        assert_eq!(Waveform::from_f32(4.0), Waveform::Noise);
        assert_eq!(Waveform::from_f32(99.0), Waveform::Sine, "out of range");
        for kind in FilterType::ALL {
            assert_eq!(FilterType::from_f32(kind as i32 as f32), kind);
        }
        for wave in LfoWave::ALL {
            assert_eq!(LfoWave::from_f32(wave as i32 as f32), wave);
        }
    }

    // ---- unison, wired up ----

    /// Build a block of voices and their handles, as `build_group` does.
    fn voice_block(n: usize, channel: &ChannelParams) -> (Vec<Voice>, Vec<VoiceHandle>) {
        let voices: Vec<Voice> = (0..n)
            .map(|i| {
                Voice::new(
                    channel.clone(),
                    SharedF32::new(0.0),
                    SharedF32::new(0.0),
                    48_000.0,
                    0x1000 + i as u32,
                )
            })
            .collect();
        let handles = voices.iter().map(|v| v.handle()).collect();
        (voices, handles)
    }

    #[test]
    fn unison_stacks_detuned_voices_on_each_note() {
        let channel = ChannelParams::defaults(4.0, 4000.0);
        channel.unison.set(3.0);
        channel.detune.set(20.0);
        channel.glide.set(0.25);

        let (voices, handles) = voice_block(GROUP_MID, &channel);
        Voices::trigger_block(&handles, &channel, &[60, 64], 0.9, 1.0);

        let expected_level = 0.9 / 3.0f32.sqrt();
        for note in 0..2 {
            let base = note * UNISON_MAX;
            let cents: Vec<f32> = (0..3)
                .map(|u| voices[base + u].detune_cents.get())
                .collect();
            assert_eq!(
                cents,
                vec![-20.0, 0.0, 20.0],
                "note {} should be a symmetric stack",
                note
            );
            for u in 0..3 {
                let v = &voices[base + u];
                assert!(v.gate.get() > 0.5, "note {} voice {} is silent", note, u);
                assert_eq!(v.midi_note.get(), if note == 0 { 60.0 } else { 64.0 });
                assert!(
                    (v.gain.get() - expected_level).abs() < 1e-6,
                    "unison should be level-normalised"
                );
                assert_eq!(
                    v.glide_duration_secs.get(),
                    0.0,
                    "first note does not glide"
                );
            }
            // The unused slot of the block stays silent rather than being left
            // running from whatever played before.
            assert!(voices[base + 3].gate.get() < 0.5, "spare slot must be idle");
        }
    }

    #[test]
    fn one_voice_per_note_is_the_untouched_case() {
        let channel = ChannelParams::defaults(4.0, 4000.0);
        let (voices, handles) = voice_block(GROUP_MID, &channel);
        Voices::trigger_block(&handles, &channel, &[60], 0.8, 1.0);

        assert!(voices[0].gate.get() > 0.5);
        assert_eq!(voices[0].detune_cents.get(), 0.0);
        assert_eq!(voices[0].gain.get(), 0.8, "no normalisation for one voice");
        for v in &voices[1..] {
            assert!(v.gate.get() < 0.5);
            assert_eq!(v.detune_cents.get(), 0.0, "a silent voice is never detuned");
        }
    }

    #[test]
    fn a_wide_stack_reaches_the_edges_of_its_range() {
        let channel = ChannelParams::defaults(4.0, 4000.0);
        channel.unison.set(4.0);
        channel.detune.set(50.0);
        let (voices, handles) = voice_block(GROUP_LOW, &channel);
        Voices::trigger_block(&handles, &channel, &[60], 1.0, 1.0);
        let cents: Vec<f32> = (0..4).map(|u| voices[u].detune_cents.get()).collect();
        let expected = [-50.0, -50.0 / 3.0, 50.0 / 3.0, 50.0];
        for (got, want) in cents.iter().zip(expected) {
            assert!(
                (got - want).abs() < 1e-4,
                "detune spread was {:?}, wanted {:?}",
                cents,
                expected
            );
        }
    }

    #[test]
    fn a_chord_wider_than_the_pool_drops_the_extra_notes_instead_of_panicking() {
        // `allocate` can hand the mid channel more notes than it has blocks for
        // — a seven-note chord gives it five — and the trigger must ignore what
        // it cannot hold rather than run off the end.
        let channel = ChannelParams::defaults(4.0, 4000.0);
        let (voices, handles) = voice_block(GROUP_MID, &channel);
        let notes: Vec<u8> = (60..67).collect();
        Voices::trigger_block(&handles, &channel, &notes, 1.0, 1.0);
        // Note `i` owns the block starting at `i * UNISON_MAX`, so with one
        // voice per note the four the pool can hold sound and the rest are
        // dropped rather than running off the end.
        assert_eq!(voices.len(), MID_NOTES * UNISON_MAX);
        for note in 0..MID_NOTES {
            assert!(
                voices[note * UNISON_MAX].gate.get() > 0.5,
                "note {} should have sounded",
                note
            );
        }
    }

    #[test]
    fn a_hostile_unison_count_is_clamped_at_trigger_time() {
        // A hand-edited file can say anything. Zero voices would mean
        // silence and five would run past the block it owns.
        for (asked, expected) in [(0.0, 1usize), (-3.0, 1), (9.0, UNISON_MAX), (4.4, 4)] {
            let channel = ChannelParams::defaults(4.0, 4000.0);
            channel.unison.set(asked);
            let (voices, handles) = voice_block(GROUP_LOW, &channel);
            Voices::trigger_block(&handles, &channel, &[60], 1.0, 1.0);
            let sounding = voices.iter().filter(|v| v.gate.get() > 0.5).count();
            assert_eq!(
                sounding, expected,
                "unison {} sounded {} voices",
                asked, sounding
            );
        }
    }

    #[test]
    fn every_voice_in_the_whole_synth_gets_its_own_noise_seed() {
        // Two voices handed the same seed produce identical noise, which sums
        // coherently instead of decorrelating — several times as loud and
        // audibly not noise any more. Distinct *within* a group is not enough:
        // the seed has to be distinct across the groups too, because two rhythm
        // layers sound at the same time.
        let params = SynthParams::defaults();
        let mut counter = 0u32;
        let mut seeds: Vec<u32> = Vec::new();
        for _ in 0..=STAB_GROUPS {
            let (_, voices) = build_group(&params, 48_000.0, &mut counter);
            seeds.extend(
                voices
                    .low
                    .iter()
                    .chain(&voices.mid)
                    .chain(&voices.high)
                    .map(|v| v.noise_seed),
            );
        }
        // Plus the click voice, which is drawn from the same counter.
        let click_voice = Voice::new(
            params.mid.clone(),
            params.lfo_rate.clone(),
            params.lfo_wave.clone(),
            48_000.0,
            next_noise_seed(&mut counter),
        );
        seeds.push(click_voice.noise_seed);

        assert_eq!(
            seeds.len(),
            (STAB_GROUPS + 1) * (GROUP_LOW + GROUP_MID + GROUP_HIGH) + 1
        );
        let total = seeds.len();
        seeds.sort_unstable();
        seeds.dedup();
        assert_eq!(seeds.len(), total, "every voice needs a distinct seed");
        assert!(!seeds.contains(&0), "a zero seed never leaves zero");
    }

    #[test]
    fn a_noise_seed_counter_is_never_zero_and_never_repeats() {
        // The counter is walked once per voice at start-up, so this covers the
        // span the synth actually uses and then some.
        let mut counter = 0u32;
        let seeds: Vec<u32> = (0..1_000).map(|_| next_noise_seed(&mut counter)).collect();
        assert!(!seeds.contains(&0));
        let mut sorted = seeds.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), seeds.len());
    }

    #[test]
    fn the_voice_pool_is_sized_for_the_widest_stack() {
        // The arithmetic the audio callback's cost depends on. If `GROUP_*` and
        // `UNISON_MAX` ever disagree, a unison stack either loses voices or runs
        // past its block.
        assert_eq!(GROUP_LOW, LOW_NOTES * UNISON_MAX);
        assert_eq!(GROUP_MID, MID_NOTES * UNISON_MAX);
        assert_eq!(GROUP_HIGH, HIGH_NOTES * UNISON_MAX);
        let per_group = GROUP_LOW + GROUP_MID + GROUP_HIGH;
        let total = (STAB_GROUPS + 1) * per_group + CLICK_VOICES;
        // The number the real-time budget is about; see the note on `tick`.
        assert_eq!(total, 121, "the voice pool changed size");
    }

    /// A fresh voice and its handle, triggered exactly as `play_stab` would.
    fn triggered_voice(channel: &ChannelParams, note: u8, gain: f32) -> Voice {
        let v = Voice::new(
            channel.clone(),
            SharedF32::new(0.0),
            SharedF32::new(0.0),
            48_000.0,
            0x2468_ace0,
        );
        let h = v.handle();
        h.trigger_at(note, gain, 1.0, 0.0, 0.0);
        v
    }

    /// A stack's summed output, which is what the mix actually hears.
    fn stack_output(channel: &ChannelParams, notes: &[u8], gain: f32) -> Vec<f32> {
        let (mut voices, handles) = voice_block(GROUP_LOW, channel);
        Voices::trigger_block(&handles, channel, notes, gain, 1.0);
        (0..12_000)
            .map(|_| voices.iter_mut().map(|v| v.tick()).sum::<f32>())
            .collect()
    }

    #[test]
    fn a_coherent_unison_stack_is_no_louder_than_its_single_voice() {
        // Unison with no spread is `n` *identical* voices, which add coherently
        // and grow with `n` rather than with `sqrt(n)`. Normalising such a stack
        // by the square root is what the review caught: it made four voices 2×
        // too loud. Dividing by `n` makes the sum exactly one voice again, and
        // "exactly" is testable, so it is tested exactly.
        for n in 1..=UNISON_MAX {
            let channel = ChannelParams::defaults(4.0, 4000.0);
            channel.unison.set(n as f32);
            channel.detune.set(0.0);
            let stacked = stack_output(&channel, &[60], 1.0);

            let single = channel.clone();
            single.unison.set(1.0);
            let mut one = triggered_voice(&single, 60, 1.0);
            let reference: Vec<f32> = (0..12_000).map(|_| one.tick()).collect();

            assert_eq!(stacked.len(), reference.len());
            for (i, (a, b)) in stacked.iter().zip(&reference).enumerate() {
                // Dividing by 1, 2 or 4 is exact in binary; dividing by 3 is
                // not, so three voices land a bit in the last place away from
                // one. Anything looser than that would be hiding a real error,
                // and anything tighter would be asserting something untrue.
                let exact = n.is_power_of_two();
                if exact {
                    assert_eq!(
                        a.to_bits(),
                        b.to_bits(),
                        "a stack of {} coherent voices is not one voice at sample {}: {} vs {}",
                        n,
                        i,
                        a,
                        b
                    );
                } else {
                    let slack = a.abs() * 1e-6 + 1e-9;
                    assert!(
                        (a - b).abs() <= slack,
                        "a stack of {} coherent voices drifted from one at sample {}: {} vs {}",
                        n,
                        i,
                        a,
                        b
                    );
                }
            }
        }
    }

    #[test]
    fn a_detuned_stack_stays_in_the_same_neighbourhood_as_one_voice() {
        // Detuned copies add incoherently, so the sum only grows with sqrt(n)
        // and the normalisation is by sqrt(n). The result should sit near one
        // voice's level rather than at some multiple of it.
        let channel = ChannelParams::defaults(4.0, 4000.0);
        channel.unison.set(4.0);
        channel.detune.set(12.0);
        let stacked = stack_output(&channel, &[60], 1.0);

        let single = channel.clone();
        single.unison.set(1.0);
        let mut one = triggered_voice(&single, 60, 1.0);
        let reference: Vec<f32> = (0..12_000).map(|_| one.tick()).collect();

        let ratio = rms(&stacked) / rms(&reference);
        assert!(
            (0.4..2.5).contains(&ratio),
            "a detuned stack should stay near one voice, ratio {}",
            ratio
        );
    }

    /// How much of a core the voice pool actually costs, measured.
    ///
    /// Ignored by default because it is a benchmark, not an assertion. Run it
    /// with:
    ///
    /// ```text
    /// cargo test --release --bin chord-tool the_voice_pool -- --ignored --nocapture
    /// ```
    ///
    /// This is the number that decides whether the pool can grow, so it is worth
    /// having rather than estimating.
    #[test]
    #[ignore = "benchmark: measures the audio callback's cost"]
    fn the_voice_pool_costs_what_we_think_it_does() {
        use std::time::Instant;

        // The wavetable case is here to keep the claim honest: a stored cycle is
        // supposed to cost one interpolated lookup, no more than the sine it
        // replaces.
        for (label, notes, modulate, waveform) in [
            (
                "plain triad, no modulation",
                vec![60u8, 64, 67],
                false,
                Waveform::Sine,
            ),
            (
                "wide chord, no modulation",
                vec![55, 60, 64, 67, 71, 74],
                false,
                Waveform::Sine,
            ),
            (
                "wide chord, every modulation on",
                vec![55, 60, 64, 67, 71, 74],
                true,
                Waveform::Sine,
            ),
            (
                "wide chord, drawbar organ tables",
                vec![55, 60, 64, 67, 71, 74],
                false,
                Waveform::OrganFull,
            ),
            (
                "wide chord, buzz tables, modulated",
                vec![55, 60, 64, 67, 71, 74],
                true,
                Waveform::Buzz,
            ),
        ] {
            let params = SynthParams::defaults();
            for ch in [&params.low, &params.mid, &params.high] {
                ch.waveform.set(waveform as i32 as f32);
                ch.unison.set(UNISON_MAX as f32);
                ch.detune.set(12.0);
                if modulate {
                    ch.noise_level.set(0.3);
                    ch.env_curve.set(0.8);
                    ch.key_track.set(0.7);
                    ch.filter_env.set(0.8);
                    ch.filter_attack.set(0.05);
                    ch.filter_decay.set(0.4);
                    ch.lfo_pitch.set(0.4);
                    ch.lfo_cutoff.set(0.5);
                    ch.lfo_amp.set(0.4);
                    ch.lfo_pwm.set(0.5);
                }
            }
            params.lfo_rate.set(5.0);

            // Every group sounding at once: the worst case the scheduler can
            // actually produce, since four rhythm layers plus the audition can
            // overlap.
            let mut counter = 0u32;
            let mut groups: Vec<GroupVoices> = Vec::new();
            for _ in 0..(STAB_GROUPS + 1) {
                let (handles, voices) = build_group(&params, 48_000.0, &mut counter);
                Voices::trigger_block(&handles.low, &params.low, &[notes[0]], 0.5, 1.0);
                let mid = &notes[1..notes.len() - 1];
                Voices::trigger_block(&handles.mid, &params.mid, mid, 0.5, 1.0);
                Voices::trigger_block(
                    &handles.high,
                    &params.high,
                    &[notes[notes.len() - 1]],
                    0.5,
                    1.0,
                );
                groups.push(voices);
            }

            let samples = 48_000; // one second
            let start = Instant::now();
            let mut sink = 0.0f32;
            for _ in 0..samples {
                for g in groups.iter_mut() {
                    for v in g.low.iter_mut().chain(&mut g.mid).chain(&mut g.high) {
                        sink += v.tick();
                    }
                }
            }
            let elapsed = start.elapsed();
            let voices = groups.len() * (GROUP_LOW + GROUP_MID + GROUP_HIGH);
            let realtime = elapsed.as_secs_f64();
            println!(
                "{:<34} {:>4} voices  {:>8.1} ms/s  {:>5.1}% of one core  (sink {:.1})",
                label,
                voices,
                realtime * 1000.0,
                realtime * 100.0,
                sink
            );
        }
    }

    /// What the equaliser's 65 biquads actually cost, measured.
    ///
    /// Ignored by default, like the voice-pool benchmark above. Run it with:
    ///
    /// ```text
    /// cargo test --release --bin chord-tool the_equaliser -- --ignored --nocapture
    /// ```
    ///
    /// The claim it checks is that putting the EQ on the buses rather than in the
    /// voices is what makes it affordable at all: thirteen sections times three
    /// registers plus two master channels is 65 filters a sample, where the same
    /// curve per voice would be thirteen times
    /// `(STAB_GROUPS + 1) * (GROUP_LOW + GROUP_MID + GROUP_HIGH)`.
    #[test]
    #[ignore = "benchmark: measures the equaliser's cost"]
    fn the_equaliser_costs_what_we_think_it_does() {
        use std::time::Instant;

        // Every band moved, so nothing is skipped: the worst case a curve can be.
        let curve = EqCurve {
            gains: [
                6.0, -6.0, 4.5, -4.5, 3.0, -3.0, 6.0, -6.0, 4.5, -4.5, 3.0, -3.0, 6.0,
            ],
        };
        let eqs: Vec<Eq> = (0..4)
            .map(|_| {
                let mut eq = Eq::new(48_000.0);
                eq.set(&curve);
                eq
            })
            .collect();
        // Five signal paths, four coefficient sets: the master curve's sections
        // are designed once and run on left and right with their own histories.
        let bank_of = [0usize, 1, 2, 3, 3];
        let mut states = [EqState::default(); 5];

        let samples = 48_000; // one second
        let start = Instant::now();
        let mut sink = 0.0f32;
        for i in 0..samples {
            let x = (i as f32 * 0.01).sin() * 0.5;
            for (path, state) in states.iter_mut().enumerate() {
                sink += eqs[bank_of[path]].tick(state, x);
            }
        }
        let elapsed = start.elapsed().as_secs_f64();
        println!(
            "{:<34} {:>4} filters  {:>8.2} ms/s  {:>5.2}% of one core  (sink {:.3})",
            "all five buses, every band moved",
            bank_of.len() * crate::eq::EQ_BANDS,
            elapsed * 1000.0,
            elapsed * 100.0,
            sink
        );
        assert!(sink.is_finite());
    }

    /// What the live spectrum costs, measured.
    ///
    /// Ignored by default, like the two above. Run it with:
    ///
    /// ```text
    /// cargo test --release --bin chord-tool the_analyser -- --ignored --nocapture
    /// ```
    ///
    /// The number that justifies a filter bank over a transform here: thirteen
    /// bands times four taps is a fixed fifty-two biquads a sample, whatever the
    /// signal is doing.
    #[test]
    #[ignore = "benchmark: measures the analyser's cost"]
    fn the_analyser_costs_what_we_think_it_does() {
        use std::time::Instant;

        let mut analyzer = Analyzer::new(48_000.0);
        let samples = 48_000; // one second
        let start = Instant::now();
        for i in 0..samples {
            let x = (i as f32 * 0.017).sin() * 0.5;
            for tap in 0..ANALYZER_TAPS {
                analyzer.tick(tap, x);
            }
        }
        let elapsed = start.elapsed().as_secs_f64();
        let sink: f32 = (0..ANALYZER_TAPS)
            .flat_map(|tap| analyzer.levels(tap))
            .sum();
        println!(
            "{:<34} {:>4} filters  {:>8.2} ms/s  {:>5.2}% of one core  (sink {:.3})",
            "live spectrum, four taps",
            ANALYZER_TAPS * crate::eq::EQ_BANDS,
            elapsed * 1000.0,
            elapsed * 100.0,
            sink
        );
        assert!(sink.is_finite());
    }

    #[test]
    #[ignore = "benchmark: measures a fully loaded effect rack's cost"]
    fn the_effect_rack_costs_what_we_think_it_does() {
        use std::time::Instant;

        // The worst case the feature can be put in: every slot of all three
        // racks filled, plus both aux units, driven with a real signal and
        // nothing bypassed. This is the number to weigh against the dry path,
        // and the reason the racks are built from one preallocated bank rather
        // than from per-slot allocation.
        let chain = [
            Fx::variant(FxKind::Distortion, FxSubtype::Overdrive),
            Fx::variant(FxKind::Chorus, FxSubtype::Chorus),
            Fx::variant(FxKind::Delay, FxSubtype::Digital),
            Fx::variant(FxKind::Phaser, FxSubtype::Phaser),
            Fx::variant(FxKind::Tremolo, FxSubtype::Sine),
            Fx::variant(FxKind::Reverb, FxSubtype::Room),
        ];
        let aux_reverb = Fx::variant(FxKind::Reverb, FxSubtype::Hall);
        let aux_delay = Fx::variant(FxKind::Delay, FxSubtype::Tape);

        let mut bank = FxBank::new(48_000.0);
        let samples = 48_000; // one second
        let start = Instant::now();
        let mut sink = 0.0f32;
        for i in 0..samples {
            let x = (i as f32 * 0.017).sin() * 0.5;
            let mut out = 0.0;
            for register in 0..3 {
                out += bank.chain(register, &chain, 120.0, x);
            }
            out += bank.reverb(&aux_reverb, 120.0, x);
            out += bank.delay(&aux_delay, 120.0, x);
            sink += out;
        }
        let elapsed = start.elapsed().as_secs_f64();
        println!(
            "{:<34} {:>4} effects {:>8.2} ms/s  {:>5.2}% of one core  (sink {:.3})",
            "rack: 18 inserts + 2 aux",
            3 * CHAIN_SLOTS + 2,
            elapsed * 1000.0,
            elapsed * 100.0,
            sink
        );
        assert!(sink.is_finite());
    }

    /// How far sharp or flat the strongest frequency near `hz` is, in cents.
    ///
    /// A tuning test rather than a spectral one, and the only measure that can
    /// see what an exponential cross-modulation does to a note: the sidebands
    /// are not the problem there, the average rate is.
    fn cents_off(samples: &[f32], hz: f32, sample_rate: f32) -> f32 {
        let mut best = (hz, 0.0f32);
        let mut probe = hz * 0.9;
        let step = hz * 0.001;
        while probe < hz * 1.1 {
            let m = magnitude_at(samples, probe, sample_rate);
            if m > best.1 {
                best = (probe, m);
            }
            probe += step;
        }
        1200.0 * (best.0 / hz).log2()
    }

    /// The energy-weighted mean harmonic number of `samples`.
    ///
    /// The brightness measure the cross-modulation tests use, and it has to be
    /// this rather than one sideband's amplitude: an FM spectrum's second
    /// harmonic passes through *nulls* as the index rises — that is what a Bessel
    /// function does — so "is the second harmonic bigger" is a question whose
    /// answer goes up and down while the sound gets steadily brighter.
    fn brightness(samples: &[f32], f: f32, sample_rate: f32) -> f32 {
        let mut weight = 0.0f32;
        let mut total = 0.0f32;
        for harmonic in 1..=12 {
            let m = magnitude_at(samples, f * harmonic as f32, sample_rate);
            weight += m * harmonic as f32;
            total += m;
        }
        if total > 0.0 {
            weight / total
        } else {
            0.0
        }
    }

    /// A sine carrier cross-modulated at a depth, in a domain, at a note.
    fn cross_modulated(mode: FmMode, depth: f32, note: u8) -> Vec<f32> {
        let mut v = probe_voice(Waveform::Sine);
        v.channel.osc2_waveform.set(Waveform::Sine as i32 as f32);
        v.channel.osc2_fm.set(depth);
        v.channel.fm_mode.set(mode as i32 as f32);
        play(&mut v, note, 24_000)
    }

    #[test]
    fn the_three_domains_move_the_index_in_three_different_ways() {
        // The whole reason there is a mode row. Phase and exponential both keep
        // the index where it is as the note moves; linear does not, and that is
        // what a growl is. Measured as a second harmonic against the fundamental,
        // which is the cheapest thing that moves when sidebands appear.
        let index = |mode: FmMode, note: u8| {
            let out = cross_modulated(mode, 0.5, note);
            brightness(&out, midi_to_hz(note as f32), 48_000.0)
        };
        for mode in [FmMode::Phase, FmMode::Expo] {
            let low = index(mode, 36);
            let high = index(mode, 84);
            assert!(
                (low / high - 1.0).abs() < 0.2,
                "{} should carry the same index up the keyboard: {low} against {high}",
                mode.name()
            );
        }
        // Linear is the odd one out, and in the direction that makes it a growl:
        // an octave down is twice the index, so three octaves down is eight times.
        assert!(
            index(FmMode::Linear, 36) > index(FmMode::Linear, 84) * 4.0,
            "linear modulation has to grow as the note falls: {} against {}",
            index(FmMode::Linear, 36),
            index(FmMode::Linear, 84)
        );
    }

    #[test]
    fn the_exponential_domain_is_compensated_and_stays_in_tune() {
        // `E[2^(D sin)]` is above one, so an uncompensated exponential X-Mod plays
        // sharp — a hundred and forty cents at the top of the row, which in a
        // chord is not a colour but a wrong note. What is kept is the asymmetry
        // inside a cycle; what is taken back out is its average.
        for depth in [0.0, 0.1, 0.3, 0.5, 1.0] {
            for note in [48u8, 72] {
                let out = cross_modulated(FmMode::Expo, depth, note);
                let cents = cents_off(&out, midi_to_hz(note as f32), 48_000.0);
                assert!(
                    cents.abs() < 15.0,
                    "expo at depth {depth} on note {note} sits {cents:+.0} cents out"
                );
            }
        }
    }

    #[test]
    fn linear_cross_modulation_runs_the_oscillator_backwards() {
        // A deviation wider than the carrier's frequency makes the rate negative
        // for part of every cycle, which is through-zero FM. It is free here
        // because the phase is an accumulator — and it is the one thing that can
        // make the phase leave `0..1` the wrong way, so the phase has to wrap
        // rather than go negative and start reading a table at index zero.
        let note = 60u8;
        let carrier = midi_to_hz(note as f32);
        assert!(
            LINEAR_FM_HZ > carrier,
            "the test is only meaningful while the deviation can exceed the note"
        );
        let out = cross_modulated(FmMode::Linear, 1.0, note);
        for sample in &out {
            assert!(sample.is_finite(), "through-zero FM produced {sample}");
        }
        let cents = cents_off(&out, carrier, 48_000.0);
        assert!(
            cents.abs() < 20.0,
            "and the average rate is still the note: {cents:+.0} cents"
        );
    }

    #[test]
    fn feedback_folds_a_sine_into_a_saw_and_then_into_noise() {
        // An operator reading its own last sample: a sine becomes a ramp, which
        // is a saw, and a saw folded back on itself becomes noise. Both are
        // wanted — the first is the cheapest saw in the synth, the second is a
        // percussion and breath source — so the test is that the row *travels*
        // rather than that it stops somewhere.
        let run = |fb: f32| {
            let mut v = probe_voice(Waveform::Sine);
            v.channel.feedback.set(fb);
            let out = play(&mut v, 60, 24_000);
            let f = midi_to_hz(60.0);
            (
                magnitude_at(&out, f * 2.0, 48_000.0),
                magnitude_at(&out, f * 3.0, 48_000.0),
                flux(&out),
            )
        };
        let (clean_h2, clean_h3, clean_flux) = run(0.0);
        assert!(clean_h2 < 0.01 && clean_h3 < 0.01, "a sine is a sine");
        let (folded_h2, folded_h3, _) = run(0.2);
        assert!(
            folded_h2 > 0.2 && folded_h3 > 0.05,
            "a folded sine has the harmonics of a ramp: {folded_h2} {folded_h3}"
        );
        let (_, _, noisy_flux) = run(1.0);
        assert!(
            noisy_flux > clean_flux * 5.0,
            "and past the fold it is broadband: {noisy_flux} against {clean_flux}"
        );
        // Still a voice, not a runaway.
        let loud = run(1.0);
        assert!(loud.0.is_finite() && loud.1.is_finite());
    }

    #[test]
    fn the_ring_modulator_sums_and_differences_without_direct_current() {
        // A product rather than a sum, so what appears is the sum and the
        // difference of the two oscillators — and both move with the note, which
        // is what the effect rack's `ringmod` cannot do with its fixed hertz.
        let note = 60u8;
        let f = midi_to_hz(note as f32);
        let fifth = midi_to_hz(67.0);
        let run = |ring: f32| {
            let mut v = probe_voice(Waveform::Sine);
            v.channel.osc2_waveform.set(Waveform::Sine as i32 as f32);
            v.channel.osc2_interval.set(7.0);
            v.channel.osc2_ring.set(ring);
            let out = play(&mut v, note, 24_000);
            (
                magnitude_at(&out, fifth - f, 48_000.0),
                magnitude_at(&out, f + fifth, 48_000.0),
                out.iter().sum::<f32>() / out.len() as f32,
            )
        };
        let (quiet_low, quiet_high, _) = run(0.0);
        assert!(quiet_low < 0.01 && quiet_high < 0.01, "nothing to sum yet");
        let (low, high, mean) = run(1.0);
        assert!(
            low > 0.4 && high > 0.4,
            "the difference and the sum: {low} {high}"
        );
        // The blocker, because a sine times itself is half direct current and
        // this filter passes DC straight through to the reverb.
        assert!(
            mean.abs() < 0.01,
            "the ring modulator carries {mean:+.4} of direct current"
        );
    }

    #[test]
    fn cross_modulation_is_not_run_unless_it_is_asked_for() {
        // Three new rows, and a patch that touches none of them has to render to
        // the sample — the same promise every other control here makes.
        let mut v = probe_voice(Waveform::Saw);
        v.channel.osc2_waveform.set(Waveform::Square as i32 as f32);
        v.channel.osc2_interval.set(7.0);
        v.channel.osc2_fm.set(0.0);
        v.channel.fm_mode.set(FmMode::Linear as i32 as f32);
        v.channel.feedback.set(0.0);
        v.channel.osc2_ring.set(0.0);
        assert_eq!(
            play(&mut v, 60, 4_000),
            probe_note(Waveform::Saw, 60, 4_000)
        );
    }

    #[test]
    fn every_shipped_instrument_makes_a_sound() {
        // The same guard the ensembles have, one level down: a hundred and fifty
        // voices of hand-written numbers, and the failure nobody catches by eye
        // is one that renders silence or a NaN — and a NaN in one voice poisons
        // the whole mix rather than one note.
        for instrument in crate::instrument::builtin_instruments() {
            let channel = ChannelParams::defaults(4.0, 4000.0);
            apply_voice(&channel, &instrument.voice);
            let mut v = Voice::new(
                channel,
                SharedF32::new(5.0),
                SharedF32::new(LfoWave::Sine as i32 as f32),
                48_000.0,
                0x2468_ace0,
            );
            let all = play(&mut v, 60, 24_000);
            let bad = all.iter().filter(|s| !s.is_finite()).count();
            assert_eq!(
                bad, 0,
                "{} produced {bad} non-finite samples",
                instrument.name
            );
            let peak = all.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(peak > 1e-3, "{} renders silence", instrument.name);
            // A resonant filter rings above its input, so the bound is loose on
            // purpose: this is here to catch a runaway, not to police a level.
            assert!(
                peak < 16.0,
                "{} peaks at {peak}, which is a runaway rather than a voice",
                instrument.name
            );
        }
    }

    /// One shipped instrument, played at a note and rendered.
    fn instrument_note_at(name: &str, note: u8, samples: usize) -> Vec<f32> {
        let instrument = crate::instrument::builtin_instruments()
            .into_iter()
            .find(|i| i.name == name)
            .unwrap_or_else(|| panic!("no instrument called {name}"));
        let channel = ChannelParams::defaults(4.0, 4000.0);
        apply_voice(&channel, &instrument.voice);
        let mut v = Voice::new(
            channel,
            SharedF32::new(5.0),
            SharedF32::new(LfoWave::Sine as i32 as f32),
            48_000.0,
            0x2468_ace0,
        );
        play(&mut v, note, samples)
    }

    fn instrument_note(name: &str, samples: usize) -> Vec<f32> {
        instrument_note_at(name, 60, samples)
    }

    #[test]
    fn the_plucked_instruments_ring_for_as_long_as_they_say() {
        // The plucked patches are the first content the string model has, and
        // what tells them apart is exactly how long they ring. A patch whose
        // `pluck_decay` and whose measured tail disagree is one that lies about
        // itself, and it is the failure that reading the numbers cannot catch:
        // every value is in range and the sound is simply not the one described.
        let tail = |name: &str| {
            let all = instrument_note(name, 24_000);
            rms(&all[all.len() - 2_000..])
        };
        let steel = tail("Steel Guitar");
        let muted = tail("Muted Guitar");
        assert!(
            muted < 1e-4,
            "a palm-muted string is over well before half a second: {muted}"
        );
        assert!(
            steel > muted * 100.0,
            "and a steel string is not: {steel} against {muted}"
        );
        assert!(
            tail("Sitar") > steel,
            "the sitar's six seconds outlast the guitar's four and a half"
        );
    }

    #[test]
    fn the_growl_instruments_brighten_towards_the_bass_and_the_clanging_ones_do_not() {
        // The two frequency domains, pinned through the patches that use them
        // rather than through the engine alone. A linear cross-modulation has to
        // get brighter as the note falls — that is the whole reason it is not the
        // phase row — and an exponential one has to be the same at both ends of
        // the keyboard. A patch whose *numbers* say `linear` and whose sound does
        // not growl is the failure this exists for.
        let bright = |name: &str, note: u8| {
            brightness(
                &instrument_note_at(name, note, 24_000),
                midi_to_hz(note as f32),
                48_000.0,
            )
        };
        for name in ["Growl Bass", "Through-Zero Bass"] {
            let low = bright(name, 36);
            let high = bright(name, 84);
            assert!(
                low > high * 2.0,
                "{name} should growl in the bass: {low:.2} against {high:.2}"
            );
        }
        for name in ["X-Mod Clang", "X-Mod Organ"] {
            let low = bright(name, 36);
            let high = bright(name, 84);
            assert!(
                (low / high - 1.0).abs() < 0.25,
                "{name} should carry the same index up the keyboard: {low:.2} against \
                 {high:.2}"
            );
        }
    }

    #[test]
    fn the_library_exercises_every_control_the_voice_has() {
        // A control the voice grew and no patch uses is a control nobody will
        // find. This is the check that keeps the palette and the engine in step
        // in the direction that is easy to forget: the engine can gain a feature
        // and every test still pass.
        let voices: Vec<VoicePatch> = crate::instrument::builtin_instruments()
            .into_iter()
            .map(|i| i.voice)
            .collect();
        let any = |f: &dyn Fn(&VoicePatch) -> bool| voices.iter().any(f);
        for (label, used) in [
            ("a plucked string", any(&|v| v.waveform == Waveform::Pluck)),
            (
                "a notch filter",
                any(&|v| v.filter_type == FilterType::Notch),
            ),
            ("a peak filter", any(&|v| v.filter_type == FilterType::Peak)),
            ("filter drive", any(&|v| v.drive > 0.1)),
            ("velocity to cutoff", any(&|v| v.vel_cutoff > 0.1)),
            ("velocity to pulse width", any(&|v| v.vel_pwm > 0.1)),
            ("a wavetable position", any(&|v| v.position > 0.1)),
            ("phase distortion", any(&|v| v.phase_dist > 0.1)),
            ("a second oscillator", any(&|v| v.osc2_level > 0.1)),
            ("FM", any(&|v| v.osc2_fm > 0.1)),
            (
                "a frequency domain for the cross modulation",
                any(&|v| v.fm_mode != FmMode::Phase),
            ),
            (
                "the linear cross-modulation",
                any(&|v| v.fm_mode == FmMode::Linear && v.osc2_fm > 0.1),
            ),
            (
                "the exponential cross-modulation",
                any(&|v| v.fm_mode == FmMode::Expo && v.osc2_fm > 0.1),
            ),
            ("operator feedback", any(&|v| v.feedback > 0.1)),
            ("per-voice ring modulation", any(&|v| v.osc2_ring > 0.1)),
            (
                "a non-default pluck decay",
                any(&|v| (v.pluck_decay - 2.5).abs() > 0.1),
            ),
            (
                "a non-default pluck damping",
                any(&|v| (v.pluck_damp - 0.35).abs() > 0.05),
            ),
            (
                "a non-default bow point",
                any(&|v| (v.pluck_burst - 0.9).abs() > 0.05),
            ),
        ] {
            assert!(used, "no shipped instrument uses {label}");
        }
    }

    #[test]
    fn the_filter_never_diverges_however_hard_it_is_driven() {
        // Every combination that multiplies the cutoff: a static setting, a
        // filter contour at full depth either way, key tracking at the extremes
        // of the keyboard, and the LFO at full depth. NaN here is not a subtle
        // artefact — it sticks in the filter state for ever and `tanh(NaN)`
        // silences the entire mix rather than one voice.
        //
        // The cutoffs run past the panel's own maximum on purpose: the DSP can
        // be pushed there by modulation even though no single control reaches
        // it, and that is exactly where the Chamberlin form used to diverge.
        let cases = [
            (0.0, 0.0, 0.0, 60),
            (1.0, 0.0, 0.0, 60),
            (-1.0, 0.0, 0.0, 60),
            (1.0, 1.0, 1.0, 60),
            (1.0, 1.0, 1.0, 108),
            (1.0, 1.0, 1.0, 24),
            (1.0, 1.0, 0.0, 127),
        ];
        for cutoff in [20.0, 200.0, 3000.0, 8000.0, 12000.0, 19200.0] {
            for resonance in [0.0, 0.3, 0.6, 0.8, 0.95, 0.99] {
                for (filter_env, key_track, lfo_cutoff, note) in cases {
                    let mut v = voice(48_000.0, 8.0, LfoWave::Square);
                    v.channel.cutoff.set(cutoff);
                    v.channel.resonance.set(resonance);
                    v.channel.filter_env.set(filter_env);
                    v.channel.filter_attack.set(0.002);
                    v.channel.filter_decay.set(0.3);
                    v.channel.key_track.set(key_track);
                    v.channel.lfo_cutoff.set(lfo_cutoff);
                    v.channel.attack.set(0.001);
                    v.channel.decay.set(0.001);
                    v.channel.sustain.set(1.0);

                    let all = play(&mut v, note, 24_000);
                    let bad = all.iter().filter(|s| !s.is_finite()).count();
                    assert_eq!(
                        bad, 0,
                        "cutoff {} resonance {} env {} track {} lfo {} note {} \
                         produced {} non-finite samples",
                        cutoff, resonance, filter_env, key_track, lfo_cutoff, note, bad
                    );
                    // Still a filter rather than a limiter: sound came out.
                    let peak = all[12_000..].iter().fold(0.0f32, |m, s| m.max(s.abs()));
                    assert!(
                        peak > 0.0,
                        "cutoff {} resonance {} went silent instead",
                        cutoff,
                        resonance
                    );
                }
            }
        }
    }

    /// Every shipped ensemble, resolved against the shipped library.
    ///
    /// The palette is two files now, so a test that wants to hear it has to do
    /// what the app does: look each placement's instrument up and compose.
    fn shipped_ensembles() -> Vec<crate::ensemble::Ensemble> {
        crate::ensemble::builtin_ensembles()
    }

    fn resolved_channels(
        ensemble: &crate::ensemble::Ensemble,
    ) -> [crate::voice::ComposedChannel; 3] {
        let library = crate::instrument::builtin_instruments();
        let (channels, missing) = ensemble.resolve(|name| {
            library
                .iter()
                .find(|i| i.name == name)
                .map(|i| i.voice.clone())
        });
        assert!(
            missing.is_empty(),
            "{} names instruments that are not in the library: {:?}",
            ensemble.name,
            missing
        );
        channels
    }

    #[test]
    fn every_shipped_ensemble_makes_a_sound() {
        // Forty-odd ensembles of hand-written numbers, plus a library of voices.
        // The failure nobody would catch by eye is one that renders silence or,
        // worse, a NaN — and a NaN in one voice poisons the whole mix, not just
        // that register.
        for ensemble in shipped_ensembles() {
            let channels = resolved_channels(&ensemble);
            let mut total = 0.0f32;
            for (which, src) in Ensemble::REGISTERS.iter().zip(channels.iter()) {
                let channel = ChannelParams::defaults(4.0, 4000.0);
                apply_channel(&channel, src);
                let mut v = Voice::new(
                    channel,
                    SharedF32::new(ensemble.mixer.lfo_rate),
                    SharedF32::new(ensemble.mixer.lfo_wave as i32 as f32),
                    48_000.0,
                    0x1357_9bdf,
                );
                let all = play(&mut v, 60, 24_000);
                let bad = all.iter().filter(|s| !s.is_finite()).count();
                assert_eq!(
                    bad, 0,
                    "{} / {} produced {} non-finite samples",
                    ensemble.name, which, bad
                );
                total += all.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            }
            assert!(
                total > 1e-3,
                "{} renders silence across all three registers",
                ensemble.name
            );
        }
    }

    /// How different two ensembles sound, as the sum of their sample-by-sample
    /// difference over half a second of the same note.
    ///
    /// Zero means "the same sound twice". Rendered rather than compared
    /// field-by-field on purpose: two whose numbers all differ slightly
    /// are one sound, and two whose numbers differ in one place can be two.
    fn ensemble_distance(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum()
    }

    #[test]
    fn no_two_shipped_ensembles_are_the_same_sound() {
        // The palette was grown by adding variants of instruments, and the way
        // that goes wrong is a variant that is a copy with a number changed that
        // matters to nobody. This is the guard.
        //
        // The threshold is deliberately far below the closest pair that actually
        // ships — `Kalimba` and `Pizzicato`, which share a plucked-string
        // spectrum and diverge in decay and filter — so it catches a duplicate
        // rather than second-guessing a legitimate variant.
        let closest_shipped = 500.0;
        let rendered: Vec<(String, Vec<f32>)> = shipped_ensembles()
            .into_iter()
            .map(|ensemble| {
                let channels = resolved_channels(&ensemble);
                let channel = ChannelParams::defaults(4.0, 4000.0);
                apply_channel(&channel, &channels[1]);
                let mut v = Voice::new(
                    channel,
                    SharedF32::new(ensemble.mixer.lfo_rate),
                    SharedF32::new(ensemble.mixer.lfo_wave as i32 as f32),
                    48_000.0,
                    0x2468_1357,
                );
                (ensemble.name.clone(), play(&mut v, 60, 24_000))
            })
            .collect();

        for i in 0..rendered.len() {
            for j in (i + 1)..rendered.len() {
                let distance = ensemble_distance(&rendered[i].1, &rendered[j].1);
                assert!(
                    distance > closest_shipped,
                    "{} and {} are the same sound ({} apart)",
                    rendered[i].0,
                    rendered[j].0,
                    distance
                );
            }
        }
    }
    // ---- the wavetable oscillator ----

    /// The magnitude of `hz` in `samples`, by direct correlation.
    ///
    /// A one-bin DFT: enough to ask "is the played note in this sound, and is
    /// the octave below it", which is the whole claim the drawbar tables make.
    fn magnitude_at(samples: &[f32], hz: f32, sample_rate: f32) -> f32 {
        let step = TAU * hz / sample_rate;
        let (mut re, mut im) = (0.0f32, 0.0f32);
        for (i, s) in samples.iter().enumerate() {
            re += s * (step * i as f32).cos();
            im += s * (step * i as f32).sin();
        }
        2.0 * (re * re + im * im).sqrt() / samples.len() as f32
    }

    /// A steady note on `waveform`, with nothing between it and the output.
    fn plain_note(waveform: Waveform, note: u8) -> Vec<f32> {
        let mut v = voice(48_000.0, 0.0, LfoWave::Sine);
        v.channel.waveform.set(waveform as i32 as f32);
        v.channel.attack.set(0.001);
        v.channel.decay.set(0.001);
        v.channel.sustain.set(1.0);
        v.channel.cutoff.set(8000.0);
        v.channel.resonance.set(0.0);
        play(&mut v, note, 48_000)
    }

    /// Whether the energy near `hz` is *at* `hz` rather than a mistuning of it.
    ///
    /// Deliberately not "is `hz` the loudest frequency": a plucked string's
    /// second harmonic is often as loud as its fundamental, which is physics and
    /// not a fault. What this holds is that the note is where it was asked for
    /// and not smeared off it, which is what a wrong loop length or a warped
    /// period does. The probes are a detune either way and three intervals that
    /// are not harmonics of anything.
    fn tuned_at(samples: &[f32], hz: f32, sample_rate: f32) -> bool {
        let here = magnitude_at(samples, hz, sample_rate);
        here > 0.0005
            && [0.97, 1.03, 0.75, 1.25, 1.5]
                .iter()
                .all(|ratio| here > magnitude_at(samples, hz * ratio, sample_rate) * 2.0)
    }

    /// A voice set up for a measurement: full sustain, wide open, no resonance.
    ///
    /// Separate from [`probe_note`] so that a test which changes one thing can
    /// compare against the unchanged sound rather than against a differently
    /// set-up voice.
    fn probe_voice(waveform: Waveform) -> Voice {
        // Not `mut`: every control on a channel is an atomic behind a shared
        // reference, which is what lets the audio thread read one while the
        // panel writes it.
        let v = voice(48_000.0, 0.0, LfoWave::Sine);
        v.channel.waveform.set(waveform as i32 as f32);
        v.channel.attack.set(0.001);
        v.channel.decay.set(0.001);
        v.channel.sustain.set(1.0);
        v.channel.cutoff.set(8000.0);
        v.channel.resonance.set(0.0);
        v
    }

    fn probe_note(waveform: Waveform, note: u8, samples: usize) -> Vec<f32> {
        play(&mut probe_voice(waveform), note, samples)
    }

    // ---- the free tier: two more filter outputs, drive, and velocity ----

    #[test]
    fn notch_and_peak_are_the_two_outputs_the_filter_gives_away() {
        // `low + high` and `low - high`, from the three outputs the
        // state-variable filter has always computed. This is a test of a wire,
        // not of an algorithm: the notch has to have a null where the lowpass
        // has its resonance, and the peak has to have its energy there.
        let f = midi_to_hz(60.0);
        let response = |filter: FilterType| {
            let mut v = voice(48_000.0, 0.0, LfoWave::Sine);
            v.channel.waveform.set(Waveform::Sine as i32 as f32);
            v.channel.attack.set(0.001);
            v.channel.decay.set(0.001);
            v.channel.sustain.set(1.0);
            v.channel.cutoff.set(f);
            v.channel.resonance.set(0.9);
            v.channel.filter_type.set(filter as i32 as f32);
            let out = play(&mut v, 60, 24_000);
            magnitude_at(&out, f, 48_000.0)
        };
        let low = response(FilterType::Lowpass);
        let high = response(FilterType::Highpass);
        let band = response(FilterType::Bandpass);
        let notch = response(FilterType::Notch);
        let peak = response(FilterType::Peak);

        assert!(low > 0.5, "the resonant lowpass is loud at its corner");
        assert!(
            notch < low * 0.2,
            "the notch must null the band: {notch} against {low}"
        );
        assert!(
            notch < high * 0.2 && notch < band * 0.2,
            "and null it against the other two as well: {notch} {high} {band}"
        );
        assert!(
            peak > notch * 5.0,
            "the peak is the band against the rest: {peak} against {notch}"
        );
        for value in [low, high, band, notch, peak] {
            assert!(value.is_finite());
        }
    }

    #[test]
    fn drive_saturates_the_filter_input_without_a_make_up_gain() {
        // A pre-gain into a saturator with nothing taken back out: a sine gains
        // its odd harmonics and comes out louder, which is what the knob does on
        // the hardware it imitates. The voice's own gain is what carries the
        // level, so the comparison is made at a fixed, quiet gain.
        let f = midi_to_hz(60.0);
        let run = |drive: f32| {
            let mut v = probe_voice(Waveform::Sine);
            v.channel.drive.set(drive);
            v.gain.set(0.25);
            let out = play(&mut v, 60, 24_000);
            (
                rms(&out),
                magnitude_at(&out, f * 3.0, 48_000.0),
                out.iter().fold(0.0f32, |m, s| m.max(s.abs())),
            )
        };
        let (plain_rms, plain_h3, _) = run(0.0);
        let (driven_rms, driven_h3, driven_peak) = run(1.0);

        assert!(plain_h3 < 0.01, "a sine has no third harmonic: {plain_h3}");
        assert!(
            driven_h3 > plain_h3 * 20.0,
            "a driven one does: {driven_h3} against {plain_h3}"
        );
        assert!(
            driven_rms > plain_rms,
            "and drive adds level rather than taking it: {driven_rms} against {plain_rms}"
        );
        assert!(
            driven_peak < 1.0,
            "and the saturator cannot leave the range: {driven_peak}"
        );
    }

    #[test]
    fn velocity_does_nothing_until_a_patch_asks_it_to() {
        // Both depths at zero: two very different velocities render the same
        // sound. The loudness of a note is its gain, which is where the accent
        // already was; these two rows are the *tone*, and a patch that does not
        // want them must get nothing.
        let run = |velocity: f32| {
            let mut v = probe_voice(Waveform::Saw);
            v.velocity.set(velocity);
            play(&mut v, 60, 4_000)
        };
        assert_eq!(run(0.1), run(1.0));
    }

    #[test]
    fn velocity_closes_the_filter_and_widens_the_pulse() {
        // The two destinations, separately, because they are separate claims: a
        // soft hit is darker, and a soft hit is a wider and therefore thinner
        // pulse. Neither of them touches the gain.
        let dark = |velocity: f32| {
            let mut v = probe_voice(Waveform::Saw);
            v.channel.cutoff.set(1200.0);
            v.channel.vel_cutoff.set(1.0);
            v.velocity.set(velocity);
            flux(&play(&mut v, 60, 24_000))
        };
        assert!(
            dark(0.25) < dark(1.0) * 0.6,
            "a soft hit is darker: {} against {}",
            dark(0.25),
            dark(1.0)
        );

        let wide = |velocity: f32| {
            let mut v = probe_voice(Waveform::Square);
            v.channel.vel_pwm.set(1.0);
            v.velocity.set(velocity);
            rms(&play(&mut v, 60, 24_000))
        };
        assert!(
            wide(0.25) < wide(1.0) * 0.8,
            "and a wider pulse carries less energy: {} against {}",
            wide(0.25),
            wide(1.0)
        );
    }

    #[test]
    fn velocity_at_full_is_the_unmodulated_sound() {
        // The two depths at their maximum and velocity at one must render what
        // the depths at zero render, or every hand-played chord in the palette
        // would change the moment a patch carried the rows.
        for depth in [0.0, 1.0] {
            let mut v = voice(48_000.0, 0.0, LfoWave::Sine);
            v.channel.waveform.set(Waveform::Square as i32 as f32);
            v.channel.attack.set(0.001);
            v.channel.decay.set(0.001);
            v.channel.sustain.set(1.0);
            v.channel.cutoff.set(2000.0);
            v.channel.vel_cutoff.set(depth);
            v.channel.vel_pwm.set(depth);
            v.velocity.set(1.0);
            let full = play(&mut v, 60, 4_000);

            let mut w = voice(48_000.0, 0.0, LfoWave::Sine);
            w.channel.waveform.set(Waveform::Square as i32 as f32);
            w.channel.attack.set(0.001);
            w.channel.decay.set(0.001);
            w.channel.sustain.set(1.0);
            w.channel.cutoff.set(2000.0);
            let plain = play(&mut w, 60, 4_000);
            assert_eq!(full, plain, "velocity at full is not identity at {depth}");
        }
    }

    // ---- the wavetable position ----

    #[test]
    fn the_position_morphs_into_the_next_waveform_in_its_octave_group() {
        // The pairs, and the two properties that make them usable: they never
        // cross an octave — a drawbar table advances its phase at half the rate
        // of everything else — and a plucked string is its own group because it
        // is not a spectrum at all.
        let octave = |w: Waveform| w.table().map(crate::wavetable::octave).unwrap_or(0);
        for (index, waveform) in Waveform::ALL.iter().enumerate() {
            let partner = MORPH_NEXT[index];
            if *waveform == Waveform::Pluck {
                assert_eq!(partner, Waveform::Pluck);
                continue;
            }
            assert_ne!(
                partner,
                Waveform::Pluck,
                "{waveform:?} morphs into a string"
            );
            assert_eq!(
                octave(partner),
                octave(*waveform),
                "{waveform:?} morphs into {partner:?}, an octave away"
            );
        }
        assert_eq!(MORPH_NEXT[Waveform::Sine as usize], Waveform::Saw);
        assert_eq!(MORPH_NEXT[Waveform::Noise as usize], Waveform::Metallic);
        assert_eq!(MORPH_NEXT[Waveform::Gedeckt as usize], Waveform::Sine);
        assert_eq!(
            MORPH_NEXT[Waveform::OrganFull as usize],
            Waveform::OrganJazz
        );
    }

    #[test]
    fn position_one_is_exactly_the_next_waveform() {
        // The far end of the row is the partner itself rather than a blend that
        // happens to round to it, which is what makes this comparable to the
        // sample.
        for (from, to) in [
            (Waveform::Sine, Waveform::Saw),
            (Waveform::Gedeckt, Waveform::Sine),
            (Waveform::OrganFull, Waveform::OrganJazz),
            (Waveform::Piano, Waveform::Vibes),
        ] {
            let mut v = voice(48_000.0, 0.0, LfoWave::Sine);
            v.channel.waveform.set(from as i32 as f32);
            v.channel.attack.set(0.001);
            v.channel.decay.set(0.001);
            v.channel.sustain.set(1.0);
            v.channel.cutoff.set(8000.0);
            v.channel.resonance.set(0.0);
            v.channel.position.set(1.0);
            let blended = play(&mut v, 60, 4_000);
            assert_eq!(blended, probe_note(to, 60, 4_000), "{from:?} into {to:?}");
        }
    }

    #[test]
    fn the_position_is_a_blend_of_two_spectra_and_not_a_tilt_of_one() {
        // The claim the row exists for. `glass` puts its energy in the fourth
        // and sixth harmonics and `mellow` has a strong fundamental; half way
        // between them has *both*, where a filter could only have made one of
        // them quieter.
        let mut v = voice(48_000.0, 0.0, LfoWave::Sine);
        v.channel.waveform.set(Waveform::GlassTone as i32 as f32);
        v.channel.attack.set(0.001);
        v.channel.decay.set(0.001);
        v.channel.sustain.set(1.0);
        v.channel.cutoff.set(8000.0);
        v.channel.resonance.set(0.0);
        v.channel.position.set(0.5);
        let out = play(&mut v, 60, 24_000);
        let f = midi_to_hz(60.0);

        let glass = probe_note(Waveform::GlassTone, 60, 24_000);
        let mellow = probe_note(Waveform::Mellow, 60, 24_000);
        let fourth = magnitude_at(&out, f * 4.0, 48_000.0);
        assert!(
            fourth > 0.0 && fourth < magnitude_at(&glass, f * 4.0, 48_000.0),
            "half way is quieter in glass's harmonic than glass is"
        );
        assert!(
            fourth > magnitude_at(&mellow, f * 4.0, 48_000.0),
            "and louder in it than mellow is, which a filter could not do while              leaving the fundamental alone"
        );
    }

    #[test]
    fn the_position_costs_nothing_at_zero() {
        let mut v = probe_voice(Waveform::Buzz);
        v.channel.position.set(0.0);
        assert_eq!(
            play(&mut v, 60, 4_000),
            probe_note(Waveform::Buzz, 60, 4_000)
        );
    }

    // ---- phase distortion ----

    #[test]
    fn phase_distortion_adds_harmonics_without_moving_the_note() {
        // The whole point of the control: a sine becomes a ramp and stays in
        // tune, which is what tells it apart from a filter and from a pitch
        // change.
        let f = midi_to_hz(60.0);
        let run = |amount: f32| {
            let mut v = voice(48_000.0, 0.0, LfoWave::Sine);
            v.channel.waveform.set(Waveform::Sine as i32 as f32);
            v.channel.attack.set(0.001);
            v.channel.decay.set(0.001);
            v.channel.sustain.set(1.0);
            v.channel.cutoff.set(8000.0);
            v.channel.resonance.set(0.0);
            v.channel.phase_dist.set(amount);
            play(&mut v, 60, 24_000)
        };
        let plain = run(0.0);
        let warped = run(0.9);

        // The floor is the window's own leakage rather than zero: twenty-four
        // thousand samples of a 261 Hz sine put a couple of thousandths into a
        // bin a whole octave up.
        assert!(
            magnitude_at(&plain, f * 2.0, 48_000.0) < 0.01,
            "a sine has no second harmonic to start with: {}",
            magnitude_at(&plain, f * 2.0, 48_000.0)
        );
        assert!(
            magnitude_at(&warped, f * 2.0, 48_000.0) > 0.05,
            "and a warped one does: {}",
            magnitude_at(&warped, f * 2.0, 48_000.0)
        );
        assert!(
            tuned_at(&warped, f, 48_000.0),
            "the note is where it was: the period is untouched"
        );
        assert_eq!(
            plain,
            probe_note(Waveform::Sine, 60, 24_000),
            "and zero is exactly the unwarped phase"
        );
    }

    // ---- the second oscillator ----

    #[test]
    fn the_second_oscillator_is_not_run_unless_it_is_asked_for() {
        // Both of its depths at zero has to leave the first oscillator's output
        // alone to the sample, or every patch in the library would change.
        let mut v = probe_voice(Waveform::Saw);
        v.channel.osc2_waveform.set(Waveform::Square as i32 as f32);
        v.channel.osc2_interval.set(7.0);
        v.channel.osc2_level.set(0.0);
        v.channel.osc2_fm.set(0.0);
        assert_eq!(
            play(&mut v, 60, 4_000),
            probe_note(Waveform::Saw, 60, 4_000)
        );
    }

    #[test]
    fn the_second_oscillator_brings_its_own_interval() {
        // A fifth above on a second sine, level only: the fifth is in the
        // output and the first oscillator is untouched underneath it.
        let mut v = voice(48_000.0, 0.0, LfoWave::Sine);
        v.channel.waveform.set(Waveform::Sine as i32 as f32);
        v.channel.attack.set(0.001);
        v.channel.decay.set(0.001);
        v.channel.sustain.set(1.0);
        v.channel.cutoff.set(8000.0);
        v.channel.resonance.set(0.0);
        v.channel.osc2_waveform.set(Waveform::Sine as i32 as f32);
        v.channel.osc2_interval.set(7.0);
        v.channel.osc2_level.set(0.8);
        let out = play(&mut v, 60, 24_000);
        let f = midi_to_hz(60.0);
        let fifth = midi_to_hz(67.0);
        assert!(
            magnitude_at(&out, fifth, 48_000.0) > 0.1,
            "the interval is there: {}",
            magnitude_at(&out, fifth, 48_000.0)
        );
        assert!(magnitude_at(&out, f, 48_000.0) > 0.1, "and so is the note");
    }

    #[test]
    fn fm_puts_sidebands_where_a_sine_had_none() {
        // A sine carrier and a sine modulator: the carrier is still the note,
        // and the harmonics that were not there are the point.
        let f = midi_to_hz(60.0);
        let run = |depth: f32| {
            let mut v = voice(48_000.0, 0.0, LfoWave::Sine);
            v.channel.waveform.set(Waveform::Sine as i32 as f32);
            v.channel.attack.set(0.001);
            v.channel.decay.set(0.001);
            v.channel.sustain.set(1.0);
            v.channel.cutoff.set(8000.0);
            v.channel.resonance.set(0.0);
            v.channel.osc2_waveform.set(Waveform::Sine as i32 as f32);
            v.channel.osc2_interval.set(0.0);
            v.channel.osc2_level.set(0.0);
            v.channel.osc2_fm.set(depth);
            play(&mut v, 60, 24_000)
        };
        let plain = run(0.0);
        let bell = run(0.7);
        assert!(magnitude_at(&plain, f * 3.0, 48_000.0) < 0.01);
        assert!(
            magnitude_at(&bell, f * 3.0, 48_000.0) > 0.05,
            "the third harmonic is a sideband: {}",
            magnitude_at(&bell, f * 3.0, 48_000.0)
        );
        assert!(
            tuned_at(&bell, f, 48_000.0),
            "and the carrier is still the note, because this is phase and not              frequency"
        );
    }

    // ---- the plucked string ----

    #[test]
    fn the_string_is_in_tune_across_the_range() {
        // The loop rings at `sample_rate / delay`, and the damping one-pole
        // delays the loop as well as damping it — so the delay is the period
        // *less* that delay. Without the correction every note is flat, and the
        // flatter the more damping there is.
        for note in [36u8, 48, 60, 72, 84] {
            let mut v = voice(48_000.0, 0.0, LfoWave::Sine);
            v.channel.waveform.set(Waveform::Pluck as i32 as f32);
            v.channel.attack.set(0.001);
            v.channel.decay.set(0.001);
            v.channel.sustain.set(1.0);
            v.channel.cutoff.set(8000.0);
            v.channel.resonance.set(0.0);
            v.channel.pluck_decay.set(6.0);
            v.channel.pluck_damp.set(0.5);
            let out = play(&mut v, note, 24_000);
            let hz = midi_to_hz(note as f32);
            assert!(
                tuned_at(&out, hz, 48_000.0),
                "note {note} should ring at {hz} Hz"
            );
        }
    }

    #[test]
    fn the_decay_control_sets_how_long_the_string_rings() {
        let tail = |seconds: f32| {
            let mut v = probe_voice(Waveform::Pluck);
            v.channel.pluck_decay.set(seconds);
            v.channel.pluck_damp.set(0.15);
            let out = play(&mut v, 60, 24_000);
            rms(&out[out.len() - 2_000..])
        };
        // Half a second after the pluck: a tenth-of-a-second string has gone and
        // a six-second one has barely begun to fade. The ratio is the claim —
        // an absolute level would only be measuring the excitation. It is also
        // the test that catches a decay levied per *sample* rather than per
        // loop, which makes the ring time proportional to the note.
        let short = tail(0.1);
        let long = tail(6.0);
        assert!(
            short < 0.002,
            "a tenth of a second is over well before half a second: {short}"
        );
        assert!(
            long > short * 20.0,
            "and a six second string is still ringing: {long} against {short}"
        );
    }

    #[test]
    fn the_damping_control_takes_the_top_off_the_tail() {
        let tail = |damp: f32| {
            let mut v = probe_voice(Waveform::Pluck);
            v.channel.pluck_decay.set(6.0);
            v.channel.pluck_damp.set(damp);
            let out = play(&mut v, 60, 48_000);
            flux(&out[out.len() / 2..])
        };
        assert!(
            tail(0.9) < tail(0.0),
            "a damped string is darker after half a second"
        );
    }

    #[test]
    fn the_string_is_a_source_and_not_a_shape() {
        // `pluck` is its own morph group: a delay line is not a spectrum, so the
        // position row leaves it alone rather than blending it with a table.
        assert_eq!(
            MORPH_NEXT[Waveform::Pluck as usize],
            Waveform::Pluck,
            "a string has nothing to blend with"
        );
        let mut v = voice(48_000.0, 0.0, LfoWave::Sine);
        v.channel.waveform.set(Waveform::Pluck as i32 as f32);
        v.channel.attack.set(0.001);
        v.channel.decay.set(0.001);
        v.channel.sustain.set(1.0);
        v.channel.position.set(1.0);
        let with_position = play(&mut v, 60, 4_000);

        let mut w = voice(48_000.0, 0.0, LfoWave::Sine);
        w.channel.waveform.set(Waveform::Pluck as i32 as f32);
        w.channel.attack.set(0.001);
        w.channel.decay.set(0.001);
        w.channel.sustain.set(1.0);
        let without = play(&mut w, 60, 4_000);
        assert_eq!(with_position, without);
    }

    #[test]
    #[ignore = "benchmark: measures the new oscillator features' cost"]
    fn the_new_oscillator_features_cost_what_we_think_they_do() {
        use std::time::Instant;

        // Each feature against the plain voice it is added to. They are all
        // per-voice, so this is the number that decides whether a patch can
        // afford them — and every one of them is skipped at its neutral value,
        // which the last row is here to show.
        // Two passes, and only the second is printed. The first case in a
        // benchmark like this one always measures slow — the CPU has not settled
        // and the caches are cold — and comparing a slow first case against warm
        // ones reads as "the plain voice costs more than the plain voice".
        for pass in 0..2 {
            for (label, setup) in [
                ("sine, nothing on", 0usize),
                ("+ wavetable position", 1),
                ("+ phase distortion", 2),
                ("+ filter drive", 3),
                ("+ second oscillator (FM)", 4),
                ("+ linear cross-modulation", 6),
                ("+ operator feedback", 7),
                ("+ ring modulation", 8),
                ("plucked string", 5),
            ] {
                let params = SynthParams::defaults();
                for ch in [&params.low, &params.mid, &params.high] {
                    ch.waveform.set(Waveform::Saw as i32 as f32);
                    ch.cutoff.set(4000.0);
                    match setup {
                        1 => ch.position.set(0.5),
                        2 => ch.phase_dist.set(0.6),
                        3 => ch.drive.set(0.7),
                        4 => {
                            ch.osc2_waveform.set(Waveform::Sine as i32 as f32);
                            ch.osc2_interval.set(7.0);
                            ch.osc2_level.set(0.5);
                            ch.osc2_fm.set(0.4);
                        }
                        5 => ch.waveform.set(Waveform::Pluck as i32 as f32),
                        6 => {
                            ch.osc2_waveform.set(Waveform::Sine as i32 as f32);
                            ch.osc2_fm.set(0.4);
                            ch.fm_mode.set(FmMode::Linear as i32 as f32);
                        }
                        7 => ch.feedback.set(0.4),
                        8 => {
                            ch.osc2_waveform.set(Waveform::Sine as i32 as f32);
                            ch.osc2_interval.set(7.0);
                            ch.osc2_ring.set(0.9);
                        }
                        _ => {}
                    }
                }

                let mut counter = 0u32;
                let mut groups: Vec<GroupVoices> = Vec::new();
                for _ in 0..(STAB_GROUPS + 1) {
                    let (handles, voices) = build_group(&params, 48_000.0, &mut counter);
                    Voices::trigger_block(&handles.low, &params.low, &[45], 0.5, 1.0);
                    Voices::trigger_block(&handles.mid, &params.mid, &[57, 60, 64], 0.5, 1.0);
                    Voices::trigger_block(&handles.high, &params.high, &[76], 0.5, 1.0);
                    groups.push(voices);
                }
                let samples = 48_000;
                let start = Instant::now();
                let mut sink = 0.0f32;
                for _ in 0..samples {
                    for g in groups.iter_mut() {
                        for v in g.low.iter_mut().chain(&mut g.mid).chain(&mut g.high) {
                            sink += v.tick();
                        }
                    }
                }
                let elapsed = start.elapsed().as_secs_f64();
                let voices = groups.len() * (GROUP_LOW + GROUP_MID + GROUP_HIGH);
                if pass == 1 {
                    println!(
                        "{:<26} {:>4} voices  {:>8.1} ms/s  {:>5.1}% of one core  (sink {:.1})",
                        label,
                        voices,
                        elapsed * 1000.0,
                        elapsed * 100.0,
                        sink
                    );
                }
            }
        }
    }

    #[test]
    fn the_string_survives_every_setting_it_offers() {
        // A delay line in a feedback loop is the one place here where a mistake
        // is not a wrong sound but an unbounded one.
        for decay in [range::PLUCK_DECAY.0, 1.0, range::PLUCK_DECAY.1] {
            for damp in [0.0, 0.5, 1.0] {
                for burst in [range::PLUCK_BURST.0, 0.5, 1.0] {
                    for note in [21u8, 60, 108] {
                        let mut v = voice(48_000.0, 0.0, LfoWave::Sine);
                        v.channel.waveform.set(Waveform::Pluck as i32 as f32);
                        v.channel.attack.set(0.001);
                        v.channel.decay.set(0.001);
                        v.channel.sustain.set(1.0);
                        v.channel.pluck_decay.set(decay);
                        v.channel.pluck_damp.set(damp);
                        v.channel.pluck_burst.set(burst);
                        let out = play(&mut v, note, 24_000);
                        for sample in &out {
                            assert!(
                                sample.is_finite() && sample.abs() < 4.0,
                                "note {note} decay {decay} damp {damp} burst \
                                 {burst} produced {sample}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn an_organ_registration_sounds_the_played_note_with_the_sixteen_foot_below_it() {
        // The claim the whole octave trick exists for, measured in the audio
        // rather than in the table: `888000000` is the 16', 5 1/3' and 8'
        // drawbars, so the played note is there and so is the octave below it,
        // and nothing above the third harmonic was asked for.
        let note = 60;
        let f = midi_to_hz(note as f32);
        let all = plain_note(Waveform::OrganJazz, note);

        let played = magnitude_at(&all, f, 48_000.0);
        let below = magnitude_at(&all, f * 0.5, 48_000.0);
        let fifth = magnitude_at(&all, f * 1.5, 48_000.0);
        let octave_up = magnitude_at(&all, f * 2.0, 48_000.0);

        assert!(played > 0.05, "the played note is missing: {}", played);
        assert!(below > 0.05, "the 16' drawbar is missing: {}", below);
        assert!(fifth > 0.02, "the 5 1/3' drawbar is missing: {}", fifth);
        assert!(
            octave_up < played * 0.5,
            "the 4' drawbar was not asked for: {} against {}",
            octave_up,
            played
        );
    }

    #[test]
    fn a_table_timbre_at_its_own_pitch_has_nothing_below_the_played_note() {
        // The counterpart: everything that is not a drawbar registration puts
        // its fundamental on the played note, so there should be no subharmonic.
        for waveform in [
            Waveform::Metallic,
            Waveform::Vox,
            Waveform::GlassTone,
            Waveform::Mellow,
            Waveform::Buzz,
        ] {
            let f = midi_to_hz(60.0);
            let all = plain_note(waveform, 60);
            let played = magnitude_at(&all, f, 48_000.0);
            let below = magnitude_at(&all, f * 0.5, 48_000.0);
            assert!(
                played > 0.02,
                "{:?} does not sound the played note: {}",
                waveform,
                played
            );
            assert!(
                below < played * 0.1,
                "{:?} has an unexpected subharmonic: {} against {}",
                waveform,
                below,
                played
            );
        }
    }

    #[test]
    fn the_table_timbres_are_all_different_sounds() {
        // Cheap insurance against an index being off by one and two rows
        // quietly becoming the same waveform.
        let rendered: Vec<Vec<f32>> = Waveform::ALL[WAVETABLE_FIRST..]
            .iter()
            .map(|w| plain_note(*w, 60))
            .collect();
        for i in 0..rendered.len() {
            for j in (i + 1)..rendered.len() {
                let difference: f32 = rendered[i]
                    .iter()
                    .zip(rendered[j].iter())
                    .map(|(a, b)| (a - b).abs())
                    .sum();
                assert!(
                    difference > 100.0,
                    "{:?} and {:?} render almost identically",
                    Waveform::ALL[WAVETABLE_FIRST + i],
                    Waveform::ALL[WAVETABLE_FIRST + j]
                );
            }
        }
    }

    #[test]
    fn a_table_timbre_is_not_a_sine_and_is_in_tune() {
        // The played note has to be where it would be for any other waveform:
        // the table changes the spectrum, not the pitch.
        let f = midi_to_hz(60.0);
        let table = plain_note(Waveform::Buzz, 60);
        let sine = plain_note(Waveform::Sine, 60);

        // In tune: the fundamental dominates, both against the octave and
        // against the neighbouring semitone.
        assert!(magnitude_at(&table, f, 48_000.0) > magnitude_at(&table, f * 2.0, 48_000.0));
        let neighbour = midi_to_hz(61.0);
        assert!(
            magnitude_at(&table, f, 48_000.0) > magnitude_at(&table, neighbour, 48_000.0) * 5.0,
            "the table timbre is not centred on the played note"
        );

        // Not a sine: `buzz` has sixteen harmonics where a sine has one.
        let difference: f32 = table
            .iter()
            .zip(sine.iter())
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(difference > 1000.0, "the table timbre is a sine");
    }

    #[test]
    fn the_waveform_list_and_the_timbre_list_are_the_same_list() {
        // The only thing joining `Waveform`'s table-backed variants to
        // `wavetable::TIMBRES` is their order, and order is exactly what drifts.
        for (i, waveform) in Waveform::ALL.iter().enumerate() {
            assert_eq!(
                *waveform as usize, i,
                "Waveform::ALL must be in discriminant order"
            );
        }
        assert_eq!(
            WAVETABLE_FIRST, 6,
            "a computed shape added at the end of the tail would shift every table"
        );
        assert_eq!(
            Waveform::ALL.len() - WAVETABLE_FIRST,
            crate::wavetable::TIMBRES.len(),
            "a waveform with no table, or a table with no waveform"
        );
        for (offset, timbre) in crate::wavetable::TIMBRES.iter().enumerate() {
            let waveform = Waveform::ALL[WAVETABLE_FIRST + offset];
            assert_eq!(waveform.table(), Some(offset));
            assert_eq!(
                waveform.name(),
                timbre.label,
                "the panel name and the timbre have drifted apart"
            );
        }
        // And the computed shapes are not tables.
        for waveform in &Waveform::ALL[..WAVETABLE_FIRST] {
            assert_eq!(waveform.table(), None, "{:?}", waveform);
        }
    }

    // ---- the compatibility guarantee, by replay ----

    /// The voice exactly as it was before any of this existed.
    ///
    /// A second implementation, deliberately, written from the pre-build-out
    /// source: the only way to make "an existing sound renders exactly as it did"
    /// mean something is to render both and compare the bits. Checking that each
    /// new factor happens to be 1.0 is an argument; this is evidence.
    struct LegacyVoice {
        gate: SharedF32,
        midi_note: SharedF32,
        glide_from: SharedF32,
        glide_duration_secs: SharedF32,
        hold_secs: SharedF32,
        release_override: SharedF32,
        gain: SharedF32,
        channel: ChannelParams,
        sample_rate: f32,
        phase: f32,
        env_state: EnvState,
        env_value: f32,
        glide_pos: f32,
        elapsed: f32,
        svf_low: f32,
        svf_band: f32,
    }

    /// Deliberately not rustfmt-ed: this is a copy of the pre-build-out source, and
/// it is only useful as an oracle while it can still be read against it.
#[rustfmt::skip]
impl LegacyVoice {
        fn from(channel: &ChannelParams, sample_rate: f32) -> Self {
            LegacyVoice {
                gate: SharedF32::new(0.0),
                midi_note: SharedF32::new(60.0),
                glide_from: SharedF32::new(60.0),
                glide_duration_secs: SharedF32::new(0.0),
                hold_secs: SharedF32::new(0.0),
                release_override: SharedF32::new(0.0),
                gain: SharedF32::new(1.0),
                channel: channel.clone(),
                sample_rate,
                phase: 0.0,
                env_state: EnvState::Idle,
                env_value: 0.0,
                glide_pos: 1.0,
                elapsed: 0.0,
                svf_low: 0.0,
                svf_band: 0.0,
            }
        }

        fn tick(&mut self) -> f32 {
            let dt = 1.0 / self.sample_rate;
            let gate_on = self.gate.get() > 0.5;
            let prev_state = self.env_state;

            let a = self.channel.attack.get().max(0.001);
            let d = self.channel.decay.get().max(0.001);
            let s = self.channel.sustain.get().clamp(0.0, 1.0);
            let r_default = self.channel.release.get().max(0.001);
            let r_override = self.release_override.get();
            let r = if r_override > 0.0 { r_override } else { r_default };

            match self.env_state {
                EnvState::Idle => {
                    self.env_value = 0.0;
                    if gate_on {
                        self.env_state = EnvState::Attack;
                    }
                }
                EnvState::Attack => {
                    self.env_value += dt / a;
                    if self.env_value >= 1.0 {
                        self.env_value = 1.0;
                        self.env_state = EnvState::Decay;
                    }
                    if !gate_on {
                        self.env_state = EnvState::Release;
                    }
                }
                EnvState::Decay => {
                    self.env_value -= dt / d * (1.0 - s);
                    if self.env_value <= s {
                        self.env_value = s;
                        self.env_state = EnvState::Sustain;
                    }
                    if !gate_on {
                        self.env_state = EnvState::Release;
                    }
                }
                EnvState::Sustain => {
                    self.env_value = s;
                    if !gate_on {
                        self.env_state = EnvState::Release;
                    }
                }
                EnvState::Release => {
                    self.env_value -= dt / r;
                    if self.env_value <= 0.0001 {
                        self.env_value = 0.0;
                        self.env_state = EnvState::Idle;
                    }
                    if gate_on {
                        self.env_state = EnvState::Attack;
                    }
                }
            }

            if prev_state != EnvState::Attack && self.env_state == EnvState::Attack {
                self.glide_pos = 0.0;
                self.elapsed = 0.0;
            }

            if self.env_state == EnvState::Idle {
                return 0.0;
            }

            let glide_dur = self.glide_duration_secs.get();
            if glide_dur > 0.0 {
                let total = glide_dur + self.hold_secs.get();
                if self.elapsed >= total {
                    self.gate.set(0.0);
                }
                self.elapsed += dt;
                if self.glide_pos < 1.0 {
                    self.glide_pos = (self.glide_pos + dt / glide_dur).min(1.0);
                }
            } else {
                self.glide_pos = 1.0;
            }

            let from = self.glide_from.get();
            let to = self.midi_note.get();
            let base = from + (to - from) * self.glide_pos;
            let transpose = self.channel.transpose.get();
            let note = (base + transpose).clamp(0.0, 127.0);
            let freq = midi_to_hz(note).clamp(20.0, self.sample_rate * 0.45);
            self.phase = (self.phase + freq * dt).fract();

            let osc = match Waveform::from_f32(self.channel.waveform.get()) {
                Waveform::Sine => (self.phase * TAU).sin(),
                Waveform::Saw => 2.0 * self.phase - 1.0,
                Waveform::Square => {
                    if self.phase < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                Waveform::Triangle => 4.0 * (self.phase - 0.5).abs() - 1.0,
                // A wildcard, deliberately. The old `from_f32` had no arm for
                // anything above `Triangle` and fell through to `Sine`, so that
                // — not silence — is what *any* higher control value used to
                // render, noise and every table-backed timbre included. Written
                // as a catch-all rather than a list so that a waveform added
                // later cannot quietly make this oracle wrong in the one
                // direction that looks like a real finding.
                _ => (self.phase * TAU).sin(),
            };

            let input = osc * self.env_value;

            let cutoff = self
                .channel
                .cutoff
                .get()
                .clamp(20.0, self.sample_rate * 0.4);
            let resonance = self.channel.resonance.get().clamp(0.0, 0.99);

            let f = 2.0 * (PI * cutoff / self.sample_rate).sin();
            let q = 1.0 - resonance;

            let low = self.svf_low + f * self.svf_band;
            let high = input - low - q * self.svf_band;
            let band = f * high + self.svf_band;
            self.svf_low = low;
            self.svf_band = band;

            low * self.gain.get().clamp(0.0, 1.0)
        }
    }

    /// Render the same note through both implementations and compare the bits.
    ///
    /// Bit-for-bit, not "close enough": a sound is a set of numbers a player
    /// chose, and the promise is that loading an old one does not change it. A
    /// tolerance would hide exactly the kind of drift this guards against.
    fn assert_old_voice_is_reproduced(name: &str, setup: impl Fn(&ChannelParams)) {
        let sample_rate = 48_000.0;
        let channel = ChannelParams::defaults(4.0, 4000.0);
        setup(&channel);
        let mut legacy = LegacyVoice::from(&channel, sample_rate);
        let mut modern = Voice::new(
            channel.clone(),
            SharedF32::new(0.0),
            SharedF32::new(0.0),
            sample_rate,
            0x1234_5678,
        );

        for v in [&mut legacy.midi_note, &mut modern.midi_note] {
            v.set(67.0);
        }
        for v in [&mut legacy.glide_from, &mut modern.glide_from] {
            v.set(67.0);
        }

        // Both gates open before the first tick, as a trigger leaves them.
        legacy.gate.set(1.0);
        modern.gate.set(1.0);

        // Play, release, and — the interesting part — open the gate again.
        // `Release -> Attack` is where the modern code does the most new work:
        // `release_start` is captured, the filter contour restarts, the LFO
        // phase resets, and the `prev_state != Attack` guard is what decides
        // whether any of that runs. It is also the path a held note reaches
        // every time a held chord changes.
        for i in 0..90_000 {
            if i == 24_000 {
                legacy.gate.set(0.0);
                modern.gate.set(0.0);
            }
            if i == 30_000 {
                legacy.gate.set(1.0);
                modern.gate.set(1.0);
            }
            if i == 60_000 {
                legacy.gate.set(0.0);
                modern.gate.set(0.0);
            }
            let (a, b) = (legacy.tick(), modern.tick());
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "{} diverged at sample {}: {} vs {}",
                name,
                i,
                a,
                b
            );
        }
    }

    #[test]
    fn an_untouched_voice_renders_exactly_as_it_did_before() {
        assert_old_voice_is_reproduced("defaults", |_| {});
    }

    #[test]
    fn the_release_and_retrigger_path_is_reproduced_too() {
        // The harness releases once and re-gates once for every case, but the
        // shapes matter: a slow release that is still falling when the gate
        // comes back, and one that has already finished, take different arms.
        assert_old_voice_is_reproduced("long release, retriggered mid-fall", |ch| {
            ch.attack.set(0.05);
            ch.decay.set(0.8);
            ch.sustain.set(0.5);
            ch.release.set(2.0);
        });
        assert_old_voice_is_reproduced("short release, long since silent", |ch| {
            ch.release.set(0.001);
            ch.sustain.set(0.9);
        });
        assert_old_voice_is_reproduced("no sustain at all, retriggered", |ch| {
            ch.attack.set(0.001);
            ch.decay.set(0.2);
            ch.sustain.set(0.0);
            ch.release.set(0.3);
        });
    }

    #[test]
    fn every_old_waveform_and_envelope_shape_still_renders_the_same() {
        // The four waveforms that existed, not `Waveform::ALL`: this is a replay
        // of the old code, and the old code had no noise. `LegacyVoice` maps a
        // Noise control value onto a sine, faithfully, so adding it here would
        // fail for the right reason and look like the wrong one.
        for wave in [
            Waveform::Sine,
            Waveform::Saw,
            Waveform::Square,
            Waveform::Triangle,
        ] {
            assert_old_voice_is_reproduced(&format!("{:?}", wave), |ch| {
                ch.waveform.set(wave as i32 as f32);
            });
        }
        assert_old_voice_is_reproduced("slow attack, deep decay", |ch| {
            ch.attack.set(0.4);
            ch.decay.set(1.5);
            ch.sustain.set(0.15);
            ch.release.set(1.8);
        });
        assert_old_voice_is_reproduced("instant on, no sustain", |ch| {
            ch.attack.set(0.001);
            ch.decay.set(0.05);
            ch.sustain.set(0.0);
            ch.release.set(0.001);
        });
        assert_old_voice_is_reproduced("wide open and resonant", |ch| {
            ch.cutoff.set(8000.0);
            ch.resonance.set(0.95);
            ch.waveform.set(Waveform::Saw as i32 as f32);
        });
        // The pair where the new stability clamp sits exactly on its limit:
        // `f` is 1.0 and `2 - q` is 1.0, so `min` must be a no-op. If the clamp
        // is ever given a safety margin this case is what catches it.
        assert_old_voice_is_reproduced("wide open with no resonance at all", |ch| {
            ch.cutoff.set(8000.0);
            ch.resonance.set(0.0);
            ch.waveform.set(Waveform::Saw as i32 as f32);
        });
        assert_old_voice_is_reproduced("wide open, resonant, and bright", |ch| {
            ch.cutoff.set(8000.0);
            ch.resonance.set(0.5);
            ch.waveform.set(Waveform::Square as i32 as f32);
        });
        assert_old_voice_is_reproduced("almost shut and transposed", |ch| {
            ch.cutoff.set(200.0);
            ch.resonance.set(0.0);
            ch.transpose.set(-24.0);
            ch.waveform.set(Waveform::Square as i32 as f32);
        });
        assert_old_voice_is_reproduced("transposed up past the top", |ch| {
            ch.transpose.set(24.0);
            ch.pan.set(-1.0);
            ch.reverb_send.set(1.0);
        });
    }

    /// The pool is big enough for the widest chord this tool can build.
    ///
    /// `allocate` sends everything between the outer two notes to the mid
    /// channel, and `trigger_block` silently drops a note it has no block for —
    /// the right thing to do in an audio callback, and invisible if it ever
    /// starts happening. The widest voicing is `DiatonicFull`, at six notes,
    /// which leaves the mid channel four: exactly `MID_NOTES`. This sweeps every
    /// transformation the grammar can produce, at every degree, in both modes,
    /// so that "it cannot happen" is measured rather than assumed.
    ///
    /// A *new* transformation would not be caught here unless it is added to
    /// `Transformation::ALL`, which is the one gap in this guard and the reason
    /// that list sits next to the enum rather than in the test.
    #[test]
    fn no_chord_this_tool_can_build_overflows_the_mid_channel() {
        use crate::music::{ChordSpec, Scale, ScaleDegree, Transformation};
        let mut widest = 0;
        for scale in [Scale::Major, Scale::Minor] {
            for tonic in 0..12u8 {
                let key = crate::music::Key::new(60 + tonic, scale);
                for degree in [
                    ScaleDegree::I,
                    ScaleDegree::II,
                    ScaleDegree::III,
                    ScaleDegree::IV,
                    ScaleDegree::V,
                    ScaleDegree::VI,
                    ScaleDegree::VII,
                ] {
                    for transformation in Transformation::ALL {
                        let spec = ChordSpec {
                            degree,
                            transformation,
                        };
                        let notes = crate::music::voice(&key, &spec);
                        assert!(!notes.is_empty(), "{:?} on {:?} is empty", spec, key);
                        let (low, mid, high) = allocate(&notes);
                        assert!(
                            mid.len() <= MID_NOTES,
                            "{:?} on {:?} needs {} mid voices: {:?}",
                            spec,
                            key,
                            mid.len(),
                            notes
                        );
                        assert_eq!(low.len(), 1);
                        assert_eq!(high.len(), 1);
                        widest = widest.max(notes.len());
                    }
                }
            }
        }
        assert_eq!(
            widest,
            LOW_NOTES + MID_NOTES + HIGH_NOTES,
            "the widest chord and the pool have drifted apart"
        );
    }

    // ---- the parameter surface ----

    #[test]
    fn unison_spread_is_symmetric_and_centred() {
        assert_eq!(unison_spread(1, 0, 25.0), 0.0, "one voice is never detuned");
        assert_eq!(unison_spread(2, 0, 25.0), -25.0);
        assert_eq!(unison_spread(2, 1, 25.0), 25.0);
        assert_eq!(unison_spread(3, 1, 25.0), 0.0);
        assert_eq!(unison_spread(4, 0, 50.0), -50.0);
        assert_eq!(unison_spread(4, 3, 50.0), 50.0);
    }

    #[test]
    fn unison_spread_collapses_when_there_is_nothing_to_spread() {
        for u in 0..UNISON_MAX {
            assert_eq!(unison_spread(2, u, 0.0), 0.0);
        }
    }

    // ---- envelope shaping ----

    #[test]
    fn a_straight_envelope_is_exactly_the_old_one() {
        // Not "close to": a voice written before `env_curve` existed must render
        // bit-for-bit the same samples, so the shaping has to be an identity at
        // zero rather than a blend that happens to land on the same value.
        for i in 0..=100 {
            let x = i as f32 / 100.0;
            assert_eq!(shape_env(x, 0.0, 1.0, 0.0), x);
            assert_eq!(shape_env(x, 1.0, 0.4, 0.0), x);
            assert_eq!(shape_env(x, 0.4, 0.0, 0.0), x);
        }
    }

    #[test]
    fn a_curved_envelope_still_lands_on_both_ends() {
        // The whole reason the curve is applied to the normalised position: the
        // segment keeps its timing and its endpoints, and only its shape
        // changes. To a part in 10^7 rather than exactly — `from + span * (…)`
        // in `f32` cannot promise the last bit — which is why the comparison
        // below is a tolerance and the sustain one further down is not.
        for k in [0.25, 0.5, 1.0] {
            assert!((shape_env(0.0, 0.0, 1.0, k) - 0.0).abs() < 1e-6);
            assert!((shape_env(1.0, 0.0, 1.0, k) - 1.0).abs() < 1e-6);
            assert!((shape_env(1.0, 1.0, 0.4, k) - 1.0).abs() < 1e-6);
            assert!((shape_env(0.4, 1.0, 0.4, k) - 0.4).abs() < 1e-6);
            assert!((shape_env(0.4, 0.4, 0.0, k) - 0.4).abs() < 1e-6);
            assert!((shape_env(0.0, 0.4, 0.0, k) - 0.0).abs() < 1e-6);
        }
    }

    #[test]
    fn a_curved_envelope_leaves_the_endpoint_faster() {
        // Halfway through a rise, a curved segment is already past halfway; and
        // since the same shape is applied to the falling side normalised, half
        // way through a decay it is already *below* halfway.
        let rising = shape_env(0.5, 0.0, 1.0, 1.0);
        assert!(
            rising > 0.5,
            "a rise should be ahead of itself, got {}",
            rising
        );
        let falling = shape_env(0.5, 1.0, 0.0, 1.0);
        assert!(
            falling < 0.5,
            "a fall should be ahead of itself, got {}",
            falling
        );
    }

    #[test]
    fn a_sustain_level_is_never_curved() {
        // The bug this guards against: shaping the held level as well as the
        // approach to it, which would quietly turn sustain 0.7 into 0.49. This
        // one is exact and asserted as such, because a held level arrives with a
        // zero span and takes the early return.
        for s in [0.0, 0.25, 0.7, 1.0] {
            for k in [0.0, 0.5, 1.0] {
                assert_eq!(shape_env(s, s, s, k), s);
            }
        }
    }

    // ---- helpers for driving a real voice ----

    /// A voice with no gate and no note, at a sample rate low enough to run
    /// thousands of samples in a test without being slow.
    fn voice(sample_rate: f32, lfo_rate: f32, lfo_wave: LfoWave) -> Voice {
        Voice::new(
            ChannelParams::defaults(4.0, 4000.0),
            SharedF32::new(lfo_rate),
            SharedF32::new(lfo_wave as i32 as f32),
            sample_rate,
            0x1234_5678,
        )
    }

    /// Open the gate, play `note`, and render `samples` samples.
    fn play(v: &mut Voice, note: u8, samples: usize) -> Vec<f32> {
        v.midi_note.set(note as f32);
        v.glide_from.set(note as f32);
        v.gate.set(1.0);
        (0..samples).map(|_| v.tick()).collect()
    }

    /// How much the signal moves from sample to sample.
    ///
    /// A crude brightness meter, and enough for the claims being made here: a
    /// signal with more high frequency in it moves further between samples, so
    /// "the filter opened" and "the filter stayed shut" are distinguishable
    /// without pulling in an FFT.
    fn flux(samples: &[f32]) -> f32 {
        samples.windows(2).map(|w| (w[1] - w[0]).abs()).sum()
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    /// Rising zero crossings, which is a frequency count for a pitched signal.
    fn crossings(samples: &[f32]) -> usize {
        samples
            .windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count()
    }

    // ---- the filter envelope ----

    #[test]
    fn the_filter_envelope_opens_the_filter_and_then_lets_it_shut() {
        // An octave and a half above a cutoff at the bottom of its range, so
        // "open" and "shut" are genuinely different sounds rather than a decibel
        // apart — measuring at middle C would only just clear the cutoff.
        let build = |amount: f32| {
            let v = voice(48_000.0, 0.0, LfoWave::Sine);
            v.channel.cutoff.set(200.0);
            v.channel.filter_env.set(amount);
            v.channel.filter_attack.set(0.002);
            v.channel.filter_decay.set(0.20);
            v
        };
        let mut dull = build(0.0);
        let mut bright = build(1.0);

        let a = play(&mut dull, 84, 48_000);
        let b = play(&mut bright, 84, 48_000);

        let early = 4_800; // the first tenth of a second: the contour is open
        assert!(
            rms(&b[..early]) > rms(&a[..early]) * 3.0,
            "the contour should open the filter: {} vs {}",
            rms(&b[..early]),
            rms(&a[..early])
        );

        // By half a second the contour has decayed to nothing and the two are
        // the same sound again.
        let ratio = rms(&b[24_000..]) / rms(&a[24_000..]).max(f32::MIN_POSITIVE);
        assert!(
            (0.9..1.1).contains(&ratio),
            "the contour should be spent by now, ratio {}",
            ratio
        );
    }

    #[test]
    fn a_negative_filter_envelope_closes_instead_of_opening() {
        // A long decay on purpose: the contour has to still be high across the
        // window being measured, or the average of "shut, then open again" is
        // not distinguishable from "never moved".
        let build = |amount: f32| {
            let v = voice(48_000.0, 0.0, LfoWave::Sine);
            v.channel.cutoff.set(6000.0);
            v.channel.filter_env.set(amount);
            v.channel.filter_attack.set(0.002);
            v.channel.filter_decay.set(2.0);
            v
        };
        let mut open = build(0.0);
        let mut shut = build(-1.0);

        let a = play(&mut open, 84, 2_400);
        let b = play(&mut shut, 84, 2_400);
        assert!(
            rms(&b) < rms(&a) * 0.5,
            "a negative amount should duck the cutoff: {} vs {}",
            rms(&b),
            rms(&a)
        );
    }

    // ---- key tracking ----

    #[test]
    fn key_tracking_keeps_a_voice_bright_up_the_keyboard() {
        let mut fixed = voice(48_000.0, 0.0, LfoWave::Sine);
        fixed.channel.cutoff.set(300.0);
        let mut tracked = voice(48_000.0, 0.0, LfoWave::Sine);
        tracked.channel.cutoff.set(300.0);
        tracked.channel.key_track.set(1.0);

        // Two octaves above middle C: at full tracking the cutoff has moved up
        // by the same two octaves and the note is no longer buried.
        let a = play(&mut fixed, 84, 4_800);
        let b = play(&mut tracked, 84, 4_800);
        assert!(
            rms(&b) > rms(&a) * 2.0,
            "key tracking should let the note through: {} vs {}",
            rms(&b),
            rms(&a)
        );
    }

    #[test]
    fn key_tracking_is_anchored_at_middle_c() {
        let mut tracked = voice(48_000.0, 0.0, LfoWave::Sine);
        tracked.channel.cutoff.set(300.0);
        tracked.channel.key_track.set(1.0);
        let mut fixed = voice(48_000.0, 0.0, LfoWave::Sine);
        fixed.channel.cutoff.set(300.0);

        // Middle C is the anchor, so tracking changes nothing there.
        let a = play(&mut fixed, 60, 2_400);
        let b = play(&mut tracked, 60, 2_400);
        assert!(
            (rms(&b) - rms(&a)).abs() < 1e-6,
            "middle C should be untouched: {} vs {}",
            rms(&b),
            rms(&a)
        );
    }

    // ---- filter type ----

    #[test]
    fn the_filter_type_selects_which_output_is_heard() {
        let render = |kind: FilterType| {
            let v = voice(48_000.0, 0.0, LfoWave::Sine);
            v.channel.cutoff.set(200.0);
            v.channel.filter_type.set(kind as i32 as f32);
            v
        };
        // A high note against a very low cutoff: the lowpass should bury it and
        // the highpass should let it past.
        let mut low = render(FilterType::Lowpass);
        let mut high = render(FilterType::Highpass);
        let mut band = render(FilterType::Bandpass);
        let l = play(&mut low, 84, 4_800);
        let h = play(&mut high, 84, 4_800);
        let b = play(&mut band, 84, 4_800);
        assert!(
            rms(&h) > rms(&l) * 10.0,
            "highpass {} lowpass {}",
            rms(&h),
            rms(&l)
        );
        assert!(
            rms(&b) > rms(&l),
            "bandpass {} lowpass {}",
            rms(&b),
            rms(&l)
        );
    }

    // ---- the LFO ----

    #[test]
    fn the_amplitude_lfo_tremolos_and_a_depth_of_zero_does_not() {
        // Two tenth-of-a-second windows half a cycle apart at 5 Hz: one lands on
        // the LFO's trough and the other on its peak. Both start well after the
        // 2 ms attack, so nothing here is measuring the envelope instead.
        let halves = |depth: f32| {
            let mut v = voice(48_000.0, 5.0, LfoWave::Sine);
            v.channel.lfo_amp.set(depth);
            v.channel.attack.set(0.001);
            v.channel.decay.set(0.001);
            v.channel.sustain.set(1.0);
            let all = play(&mut v, 60, 14_400); // 0.3 s
            (rms(&all[4_800..9_600]), rms(&all[9_600..14_400]))
        };

        let (trough, peak) = halves(1.0);
        assert!(
            peak > trough * 2.0,
            "full depth should swing the level: {} vs {}",
            peak,
            trough
        );

        let (a, b) = halves(0.0);
        assert!(
            (a - b).abs() / a < 0.02,
            "no depth should be steady: {} vs {}",
            a,
            b
        );
    }

    #[test]
    fn the_pitch_lfo_bends_the_note_both_ways() {
        let mut v = voice(48_000.0, 2.0, LfoWave::Sine);
        v.channel.lfo_pitch.set(1.0);
        v.channel.sustain.set(1.0);
        // 0.5 s at 2 Hz is one cycle. The first quarter is the rising half of
        // the LFO, the third quarter the falling half, so the same note is bent
        // sharp first and flat afterwards.
        let all = play(&mut v, 84, 24_000);
        let sharp = crossings(&all[..6_000]);
        let flat = crossings(&all[12_000..18_000]);
        assert!(
            sharp > flat,
            "the first quarter should be sharper: {} vs {}",
            sharp,
            flat
        );
    }

    #[test]
    fn a_pitch_lfo_at_zero_depth_leaves_the_note_alone() {
        let mut v = voice(48_000.0, 20.0, LfoWave::Square);
        v.channel.sustain.set(1.0);
        let all = play(&mut v, 84, 24_000);
        let a = crossings(&all[..6_000]);
        let b = crossings(&all[12_000..18_000]);
        assert!(
            a.abs_diff(b) <= 1,
            "no depth should mean no wobble: {} vs {}",
            a,
            b
        );
    }

    // ---- noise ----

    #[test]
    fn noise_is_not_a_tone() {
        let mut v = voice(48_000.0, 0.0, LfoWave::Sine);
        v.channel.waveform.set(Waveform::Noise as i32 as f32);
        v.channel.sustain.set(1.0);
        let all = play(&mut v, 60, 4_800);
        // A tone at this note crosses zero in a regular pattern; noise does not.
        let gaps: Vec<usize> = (0..all.len() - 1)
            .filter(|&i| all[i] <= 0.0 && all[i + 1] > 0.0)
            .collect();
        let distinct: std::collections::HashSet<usize> =
            gaps.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(
            distinct.len() > 20,
            "noise should not have a period, got {} distinct gaps",
            distinct.len()
        );
    }

    #[test]
    fn the_noise_level_adds_air_on_top_of_a_tone() {
        let mut clean = voice(48_000.0, 0.0, LfoWave::Sine);
        clean.channel.sustain.set(1.0);
        let mut dirty = voice(48_000.0, 0.0, LfoWave::Sine);
        dirty.channel.sustain.set(1.0);
        dirty.channel.noise_level.set(1.0);

        let a = play(&mut clean, 60, 4_800);
        let b = play(&mut dirty, 60, 4_800);
        assert!(
            flux(&b) > flux(&a) * 5.0,
            "noise should add high frequency: {} vs {}",
            flux(&b),
            flux(&a)
        );
    }

    #[test]
    fn a_noise_seed_is_never_zero() {
        // xorshift is stuck at zero for ever if it is ever handed a zero seed.
        let v = Voice::new(
            ChannelParams::defaults(4.0, 4000.0),
            SharedF32::new(0.0),
            SharedF32::new(0.0),
            48_000.0,
            0,
        );
        assert_ne!(v.noise_seed, 0);
    }

    // ---- pulse width ----

    /// The fraction of samples that are positive, which for a square is its duty
    /// cycle. The filter is wide open and unresonant so it cannot tilt the sign.
    fn positive_fraction(samples: &[f32]) -> f32 {
        samples.iter().filter(|s| **s > 0.0).count() as f32 / samples.len() as f32
    }

    fn square_at(width: f32) -> Voice {
        let v = voice(48_000.0, 0.0, LfoWave::Sine);
        v.channel.waveform.set(Waveform::Square as i32 as f32);
        v.channel.pulse_width.set(width);
        v.channel.cutoff.set(8000.0);
        v.channel.resonance.set(0.0);
        v.channel.attack.set(0.001);
        v.channel.decay.set(0.001);
        v.channel.sustain.set(1.0);
        v
    }

    #[test]
    fn half_pulse_width_is_the_even_square_it_always_was() {
        let mut v = square_at(0.5);
        let all = play(&mut v, 60, 48_000);
        let duty = positive_fraction(&all);
        assert!(
            (duty - 0.5).abs() < 0.01,
            "an even square is half positive, got {}",
            duty
        );
    }

    #[test]
    fn pulse_width_moves_the_duty_cycle() {
        for width in [0.2, 0.35, 0.65, 0.8] {
            let mut v = square_at(width);
            let all = play(&mut v, 60, 48_000);
            let duty = positive_fraction(&all);
            assert!(
                (duty - width).abs() < 0.02,
                "width {} should give a duty of about {}, got {}",
                width,
                width,
                duty
            );
        }
    }

    #[test]
    fn pulse_width_modulation_sweeps_the_duty_cycle() {
        // A slow square LFO at full depth: the first quarter of its cycle pushes
        // the duty one way and the third quarter the other, so the two windows
        // must differ by a lot.
        let mut v = {
            let n = voice(48_000.0, 2.0, LfoWave::Square);
            n.channel.waveform.set(Waveform::Square as i32 as f32);
            n.channel.cutoff.set(8000.0);
            n.channel.resonance.set(0.0);
            n.channel.attack.set(0.001);
            n.channel.decay.set(0.001);
            n.channel.sustain.set(1.0);
            n.channel.lfo_pwm.set(1.0);
            n
        };
        let all = play(&mut v, 60, 48_000);
        // At 2 Hz one cycle is 24000 samples; the LFO is positive over the first
        // half and negative over the second, so the two halves of the cycle are
        // pushed in opposite directions.
        let first = positive_fraction(&all[..12_000]);
        let second = positive_fraction(&all[12_000..24_000]);
        assert!(
            (first - second).abs() > 0.5,
            "the duty cycle should swing: {} vs {}",
            first,
            second
        );
    }

    #[test]
    fn pulse_width_is_clamped_away_from_a_flat_line() {
        // The DSP clamps rather than trusting the parameter, because a width of
        // 0 or 1 is a DC offset, not a sound.
        let mut v = square_at(0.5);
        v.channel.pulse_width.set(0.0);
        v.channel.lfo_pwm.set(1.0);
        v.channel.lfo_amp.set(0.0);
        let all = play(&mut v, 60, 48_000);
        let duty = positive_fraction(&all);
        assert!(
            (0.04..0.96).contains(&duty),
            "a hard-clamped width must still be a square, got {}",
            duty
        );
    }

    // ---- glide ----

    #[test]
    fn the_first_note_does_not_glide_in_from_middle_c() {
        let v = voice(48_000.0, 0.0, LfoWave::Sine);
        let h = v.handle();
        h.trigger_at(72, 1.0, 1.0, 0.5, 0.0);
        assert_eq!(v.glide_from.get(), 72.0, "nothing to glide from yet");
        assert_eq!(v.glide_duration_secs.get(), 0.0);
    }

    #[test]
    fn a_second_note_glides_from_the_last_one() {
        let v = voice(48_000.0, 0.0, LfoWave::Sine);
        let h = v.handle();
        h.trigger_at(60, 1.0, 1.0, 0.5, 0.0);
        h.trigger_at(72, 1.0, 1.0, 0.5, 0.0);
        assert_eq!(v.glide_from.get(), 60.0);
        assert_eq!(v.midi_note.get(), 72.0);
        assert_eq!(v.glide_duration_secs.get(), 0.5);
    }

    #[test]
    fn glide_does_not_auto_release_a_held_note() {
        // The metronome times its own release with `hold_secs`; a played note
        // has no hold, and a glide time on its own must never be read as one.
        // Otherwise every voice with portamento would cut its own notes off
        // after the glide time.
        let v = voice(48_000.0, 0.0, LfoWave::Sine);
        let h = v.handle();
        h.trigger_at(60, 1.0, 1.0, 0.25, 0.0);
        h.trigger_at(72, 1.0, 1.0, 0.25, 0.0);
        let mut v = v;
        play(&mut v, 72, 48_000); // a full second, four times the glide
        assert!(
            v.gate.get() > 0.5,
            "the gate should still be open after the glide finished"
        );
    }

    #[test]
    fn the_metronome_still_releases_itself() {
        let v = voice(48_000.0, 0.0, LfoWave::Sine);
        let h = v.handle();
        h.trigger_glide_at(60, 72, 0.05, 0.05, 1.0);
        let mut v = v;
        play(&mut v, 72, 24_000); // 0.5 s, far past 0.05 + 0.05
        assert!(
            v.gate.get() < 0.5,
            "a click with a hold should have released itself"
        );
    }

    #[test]
    fn detune_moves_a_voice_off_its_note() {
        let mut plain = voice(48_000.0, 0.0, LfoWave::Sine);
        plain.channel.sustain.set(1.0);
        let mut sharp = voice(48_000.0, 0.0, LfoWave::Sine);
        sharp.channel.sustain.set(1.0);
        sharp.detune_cents.set(50.0);

        let a = play(&mut plain, 84, 24_000);
        let b = play(&mut sharp, 84, 24_000);
        let flat = crossings(&a[12_000..]);
        let raised = crossings(&b[12_000..]);
        assert!(
            raised > flat,
            "50 cents up should be a higher count: {} vs {}",
            raised,
            flat
        );
    }

    #[test]
    fn allocate_empty() {
        let (l, m, h) = allocate(&[]);
        assert!(l.is_empty() && m.is_empty() && h.is_empty());
    }

    #[test]
    fn allocate_one_note() {
        let (l, m, h) = allocate(&[60]);
        assert_eq!(l, vec![48]);
        assert_eq!(m, vec![60]);
        assert_eq!(h, vec![72]);
    }

    #[test]
    fn allocate_two_notes() {
        let (l, m, h) = allocate(&[60, 64]);
        assert_eq!(l, vec![60]);
        assert_eq!(m, vec![64]);
        assert_eq!(h, vec![76]);
    }

    #[test]
    fn allocate_five_notes() {
        let (l, m, h) = allocate(&[60, 64, 67, 71, 74]);
        assert_eq!(l, vec![60]);
        assert_eq!(m, vec![64, 67, 71]);
        assert_eq!(h, vec![74]);
    }

    #[test]
    fn allocate_sorts_and_dedupes() {
        let (l, m, h) = allocate(&[67, 60, 64, 60]);
        assert_eq!(l, vec![60]);
        assert_eq!(m, vec![64]);
        assert_eq!(h, vec![67]);
    }
}
