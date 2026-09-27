//! Effects: what they are, what they offer, and the library of them.
//!
//! One engine, used three ways:
//!
//! - **per register**, as an ordered insert chain of six slots, so a part can be
//!   driven, then modulated, then filtered;
//! - **the master reverb aux**, whose type is pinned to `reverb`;
//! - **the master delay aux**, whose type is pinned to `delay`.
//!
//! The two aux units are not a special case of anything: they are instances of
//! the same `reverb` and `delay` algorithms, wired to a send and a return instead
//! of into a chain. That is the whole point of building one engine rather than
//! four — a reverb is a reverb whether it is on a bus or in a rack.
//!
//! # Type, subtype, preset
//!
//! An [`Fx`] is exactly three things:
//!
//! - a **kind**, the algorithm family: `reverb`, `delay`, `chorus`, `flanger`,
//!   `phaser`, `distortion`, `fuzz`, `bitcrusher`, `ringmod`, `tremolo`,
//!   `filter`, `wah`, `compressor`, `gate` — and `none`, which is an empty slot;
//! - a **subtype**, the variant *within* the family, and a real change of
//!   algorithm rather than a rename: `fold` is a wavefolder and `hard` is a
//!   clipper, `thru-zero` sweeps a flanger's delay through zero and `jet` does
//!   not, `bell` multiplies by two frequencies and `ring` by one;
//! - five **parameter slots**, whose meaning is declared by the kind. The kind
//!   says which are used, what they are called, and what range and step they
//!   have — the same arrangement `range` has for the synth and `BANDS` for the
//!   equaliser.
//!
//! The **preset** is derived from the parameters rather than remembered, the way
//! the equaliser panel's `preset` row and the Synth panel's instrument row
//! already work: a library name when the numbers match one exactly, and `custom`
//! otherwise. So a row can never claim a sound that is not on the screen.
//!
//! # Why the parameters are an opaque array
//!
//! [`Fx::params`] is a fixed `[f32; 5]` with a per-kind meaning rather than a
//! struct per kind. The declaration is what gives it meaning, and the payoff is
//! that an effect is `Copy`, serialises as one line of TOML, and can be swapped
//! wholesale between two slots without a move, a box or a match. Changing the
//! kind or the subtype resets the array to that variant's defaults, so a slot
//! always holds a sound somebody chose — never five numbers left over from
//! something else.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// -----------------------------------------------------------------------------
// Shape
// -----------------------------------------------------------------------------

/// How many effects one register's insert chain holds.
///
/// Six, and an empty slot is [`FxKind::None`] rather than a separate count, so
/// there is one representation of "nothing here" and no count to fall out of
/// step with the slots.
pub const CHAIN_SLOTS: usize = 6;

/// How many parameters any one kind may declare.
///
/// The delay uses all six; most kinds use three. The history is worth recording:
/// this was five until the delay needed a *note value* alongside its
/// milliseconds. A synced delay that stored only milliseconds could not follow
/// the tempo — the nearest division to a fixed number of milliseconds is very
/// nearly that same number of milliseconds at any tempo — so the grid had nothing
/// to hold on to. Milliseconds and note values are different units, and they get
/// different slots.
pub const FX_PARAMS: usize = 6;

/// The parameter slot indices, named for the kinds that share them.
///
/// A parameter's *meaning* is the kind's business; these are the positions, and
/// naming them keeps the DSP from being written against bare numbers.
pub const P0: usize = 0;
pub const P1: usize = 1;
pub const P2: usize = 2;
pub const P3: usize = 3;
pub const P4: usize = 4;
pub const P5: usize = 5;

/// Every parameter slot, in order — the `used` list for callers that use them
/// all. See [`FxPresetStore::name_for_using`] for the ones that do not.
pub const ALL_PARAMS: [usize; FX_PARAMS] = [P0, P1, P2, P3, P4, P5];

/// Whether two effects agree on the listed parameter slots.
fn same_params(a: &Fx, b: &Fx, used: &[usize]) -> bool {
    used.iter().all(|&i| a.param(i) == b.param(i))
}

/// An effect with no parameters at all.
pub const NO_PARAMS: [f32; FX_PARAMS] = [0.0; FX_PARAMS];

// -----------------------------------------------------------------------------
// Kinds
// -----------------------------------------------------------------------------

/// The algorithm family.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FxKind {
    /// An empty slot. Runs nothing and costs nothing.
    #[default]
    None,
    Reverb,
    Delay,
    Chorus,
    Flanger,
    Phaser,
    Distortion,
    Fuzz,
    Bitcrusher,
    #[serde(rename = "ringmod", alias = "ring-mod")]
    RingMod,
    Tremolo,
    Filter,
    Wah,
    Compressor,
    Gate,
}

impl FxKind {
    /// Every kind, in the order the `type` row cycles them: `none` first, then
    /// the families grouped the way they are built — the time-based ones, the
    /// shaping ones, the level ones.
    pub const ALL: [FxKind; 15] = [
        FxKind::None,
        FxKind::Reverb,
        FxKind::Delay,
        FxKind::Chorus,
        FxKind::Flanger,
        FxKind::Phaser,
        FxKind::Distortion,
        FxKind::Fuzz,
        FxKind::Bitcrusher,
        FxKind::RingMod,
        FxKind::Tremolo,
        FxKind::Filter,
        FxKind::Wah,
        FxKind::Compressor,
        FxKind::Gate,
    ];

    pub fn name(self) -> &'static str {
        match self {
            FxKind::None => "none",
            FxKind::Reverb => "reverb",
            FxKind::Delay => "delay",
            FxKind::Chorus => "chorus",
            FxKind::Flanger => "flanger",
            FxKind::Phaser => "phaser",
            FxKind::Distortion => "distortion",
            FxKind::Fuzz => "fuzz",
            FxKind::Bitcrusher => "bitcrusher",
            FxKind::RingMod => "ringmod",
            FxKind::Tremolo => "tremolo",
            FxKind::Filter => "filter",
            FxKind::Wah => "wah",
            FxKind::Compressor => "compressor",
            FxKind::Gate => "gate",
        }
    }

    /// Whether this kind does anything at all.
    pub fn is_none(self) -> bool {
        self == FxKind::None
    }

    /// The kind at an index, wrapping.
    ///
    /// The panel writes a kind as a number, and a number is the one thing a
    /// shared parameter can be. Wrapping rather than panicking, because the
    /// reader is the audio callback.
    pub fn from_index(index: i32) -> Self {
        let len = Self::ALL.len() as i32;
        Self::ALL[index.rem_euclid(len) as usize]
    }

    /// Where this kind sits in [`Self::ALL`], which is the number the panel
    /// writes down.
    pub fn index(self) -> i32 {
        Self::ALL.iter().position(|kind| *kind == self).unwrap_or(0) as i32
    }

    /// The variants this kind offers, in the order the `subtype` row cycles them.
    pub fn subtypes(self) -> &'static [FxSubtype] {
        use FxSubtype::*;
        match self {
            FxKind::None => &[SubtypeNone],
            FxKind::Reverb => &[Hall, Room, Plate, Chamber, Ambience],
            FxKind::Delay => &[Digital, Tape, Analog, Slapback],
            FxKind::Chorus => &[Chorus, Ensemble, Vibrato, Dimension, Rotary],
            FxKind::Flanger => &[Flanger, Jet, ThruZero],
            FxKind::Phaser => &[Phaser, Vibe, Stepped],
            FxKind::Distortion => &[Overdrive, Soft, Hard, Tube, Fold, Rectify],
            FxKind::Fuzz => &[Fuzz, Germanium, Gate, Spit],
            FxKind::Bitcrusher => &[Crush, Decimate, Radio],
            FxKind::RingMod => &[Ring, Bell, Am],
            FxKind::Tremolo => &[Sine, Square, Ramp, Chop],
            FxKind::Filter => &[Lowpass, Highpass, Bandpass, Notch, Peak],
            FxKind::Wah => &[Auto, Pedal, Lfo],
            FxKind::Compressor => &[Comp, Limiter, Punch],
            FxKind::Gate => &[Gate, Stutter, Duck],
        }
    }

    /// The subtype a fresh slot of this kind starts on.
    pub fn default_subtype(self) -> FxSubtype {
        self.subtypes()[0]
    }

    /// The parameters this kind declares, in slot order.
    ///
    /// Only the first `params().len()` entries of [`Fx::params`] mean anything;
    /// the rest are zero and unused, and a test holds every kind to using a
    /// prefix of the slots rather than a scatter.
    pub fn params(self) -> &'static [FxParamSpec] {
        match self {
            FxKind::None => &[],
            FxKind::Reverb => &REVERB_PARAMS,
            FxKind::Delay => &DELAY_PARAMS,
            FxKind::Chorus => &CHORUS_PARAMS,
            FxKind::Flanger => &FLANGER_PARAMS,
            FxKind::Phaser => &PHASER_PARAMS,
            FxKind::Distortion => &DISTORTION_PARAMS,
            FxKind::Fuzz => &FUZZ_PARAMS,
            FxKind::Bitcrusher => &BITCRUSHER_PARAMS,
            FxKind::RingMod => &RINGMOD_PARAMS,
            FxKind::Tremolo => &TREMOLO_PARAMS,
            FxKind::Filter => &FILTER_PARAMS,
            FxKind::Wah => &WAH_PARAMS,
            FxKind::Compressor => &COMPRESSOR_PARAMS,
            FxKind::Gate => &GATE_PARAMS,
        }
    }

    /// The values a slot of this kind and subtype starts on.
    ///
    /// These are *content*: they are what a player hears the first time a slot is
    /// set to a kind, so they are chosen to be usable rather than neutral. Kinds
    /// that declare fewer than [`FX_PARAMS`] slots leave the tail at zero, and
    /// [`Fx::clamp`] keeps it there. A
    /// subtype that does not belong to this kind falls back to the kind's
    /// default, which is what makes a hand-edited or out-of-date file land on a
    /// real sound instead of five zeroes.
    pub fn defaults(self, subtype: FxSubtype) -> [f32; FX_PARAMS] {
        use FxSubtype::*;
        match (self, subtype) {
            (FxKind::None, _) => NO_PARAMS,

            // The reverb ladder. `hall` is the tank this crate has always had,
            // so its defaults are the ones that keep every shipped ensemble
            // sounding exactly as it did; the others scale the same tank's comb
            // lengths and damping. These are voicings, not physical models — see
            // `fx_dsp` for what that does and does not buy.
            (FxKind::Reverb, Hall) => [0.50, 0.20, 0.0, 0.0, 0.0, 0.0],
            (FxKind::Reverb, Room) => [0.38, 0.35, 0.0, 0.0, 0.0, 0.0],
            (FxKind::Reverb, Plate) => [0.62, 0.12, 0.0, 0.0, 0.0, 0.0],
            (FxKind::Reverb, Chamber) => [0.55, 0.28, 0.0, 0.0, 0.0, 0.0],
            (FxKind::Reverb, Ambience) => [0.26, 0.45, 0.0, 0.0, 0.0, 0.0],

            (FxKind::Delay, Digital) => [375.0, 0.35, 0.85, 0.0, 0.0, 7.0],
            (FxKind::Delay, Tape) => [375.0, 0.45, 0.45, 0.0, 0.0, 7.0],
            (FxKind::Delay, Analog) => [300.0, 0.40, 0.35, 0.0, 0.0, 7.0],
            (FxKind::Delay, Slapback) => [95.0, 0.10, 0.60, 0.0, 0.0, 1.0],

            (FxKind::Chorus, Chorus) => [0.80, 0.45, 0.50, 0.50, 0.0, 0.0],
            (FxKind::Chorus, Ensemble) => [0.50, 0.60, 0.80, 0.50, 0.0, 0.0],
            (FxKind::Chorus, Vibrato) => [4.50, 0.35, 0.0, 1.00, 0.0, 0.0],
            (FxKind::Chorus, Dimension) => [1.20, 0.30, 0.60, 0.50, 0.0, 0.0],
            (FxKind::Chorus, Rotary) => [5.50, 0.55, 0.70, 0.70, 0.0, 0.0],

            (FxKind::Flanger, Flanger) => [0.30, 0.60, 0.55, 0.50, 0.0, 0.0],
            (FxKind::Flanger, Jet) => [0.15, 0.85, 0.80, 0.50, 0.0, 0.0],
            (FxKind::Flanger, ThruZero) => [0.25, 0.70, 0.60, 0.50, 0.0, 0.0],

            (FxKind::Phaser, Phaser) => [0.40, 0.65, 0.45, 0.50, 0.0, 0.0],
            (FxKind::Phaser, Vibe) => [0.90, 0.55, 0.10, 0.50, 0.0, 0.0],
            (FxKind::Phaser, Stepped) => [2.50, 0.70, 0.40, 0.50, 0.0, 0.0],

            (FxKind::Distortion, Overdrive) => [14.0, 0.60, 0.0, 1.0, 0.0, 0.0],
            (FxKind::Distortion, Soft) => [18.0, 0.70, 0.0, 1.0, 0.0, 0.0],
            (FxKind::Distortion, Hard) => [12.0, 0.55, -3.0, 1.0, 0.0, 0.0],
            (FxKind::Distortion, Tube) => [20.0, 0.65, 0.0, 1.0, 0.0, 0.0],
            (FxKind::Distortion, Fold) => [6.0, 0.80, -6.0, 1.0, 0.0, 0.0],
            (FxKind::Distortion, Rectify) => [10.0, 0.50, -6.0, 1.0, 0.0, 0.0],

            (FxKind::Fuzz, Fuzz) => [30.0, 0.50, 0.0, -3.0, 0.0, 0.0],
            (FxKind::Fuzz, Germanium) => [26.0, 0.70, 0.0, -3.0, 0.0, 0.0],
            // The gate subtype carries a threshold; the fuzz's own `gate` slot is
            // a threshold too, and the two names are the same idea one level
            // apart.
            (FxKind::Fuzz, Gate) => [34.0, 0.50, 0.25, -3.0, 0.0, 0.0],
            (FxKind::Fuzz, Spit) => [32.0, 0.60, 0.0, -4.0, 0.0, 0.0],

            (FxKind::Bitcrusher, Crush) => [8.0, 0.35, 1.0, 0.0, 0.0, 0.0],
            (FxKind::Bitcrusher, Decimate) => [16.0, 0.55, 1.0, 0.0, 0.0, 0.0],
            (FxKind::Bitcrusher, Radio) => [6.0, 0.45, 0.90, 0.0, 0.0, 0.0],

            (FxKind::RingMod, Ring) => [220.0, 0.80, 1.0, 0.0, 0.0, 0.0],
            (FxKind::RingMod, Bell) => [440.0, 0.60, 1.0, 0.0, 0.0, 0.0],
            (FxKind::RingMod, Am) => [110.0, 0.70, 1.0, 0.0, 0.0, 0.0],

            (FxKind::Tremolo, Sine) => [5.0, 0.60, 1.0, 0.0, 0.0, 0.0],
            (FxKind::Tremolo, Square) => [5.0, 0.80, 1.0, 0.0, 0.0, 0.0],
            (FxKind::Tremolo, Ramp) => [3.0, 0.70, 1.0, 0.0, 0.0, 0.0],
            (FxKind::Tremolo, Chop) => [8.0, 1.00, 1.0, 0.0, 0.0, 0.0],

            (FxKind::Filter, Lowpass) => [1200.0, 0.35, 0.0, 1.0, 0.0, 0.0],
            (FxKind::Filter, Highpass) => [120.0, 0.30, 0.0, 1.0, 0.0, 0.0],
            (FxKind::Filter, Bandpass) => [800.0, 0.55, 0.0, 1.0, 0.0, 0.0],
            (FxKind::Filter, Notch) => [700.0, 0.45, 0.0, 1.0, 0.0, 0.0],
            (FxKind::Filter, Peak) => [1500.0, 0.60, 0.0, 1.0, 0.0, 0.0],

            (FxKind::Wah, Auto) => [0.60, 0.65, 0.70, 1.00, 0.70, 0.0],
            (FxKind::Wah, Pedal) => [0.50, 0.50, 0.60, 1.00, 0.70, 0.0],
            (FxKind::Wah, Lfo) => [0.0, 0.70, 0.65, 1.60, 0.70, 0.0],

            (FxKind::Compressor, Comp) => [-18.0, 3.0, 12.0, 180.0, 4.0, 0.0],
            (FxKind::Compressor, Limiter) => [-3.0, 20.0, 1.0, 60.0, 0.0, 0.0],
            (FxKind::Compressor, Punch) => [-16.0, 4.0, 40.0, 220.0, 6.0, 0.0],

            (FxKind::Gate, Gate) => [-45.0, 2.0, 40.0, 120.0, 8.0, 0.0],
            (FxKind::Gate, Stutter) => [-40.0, 1.0, 0.0, 30.0, 8.0, 0.0],
            (FxKind::Gate, Duck) => [-30.0, 5.0, 10.0, 250.0, 4.0, 0.0],

            // A pair that does not belong together. `Fx` normalises before it can
            // get here; this is the fallback that keeps a stray call harmless.
            _ => self.defaults(self.default_subtype()),
        }
    }
}

// -----------------------------------------------------------------------------
// Subtypes
// -----------------------------------------------------------------------------

/// The variant within a kind.
///
/// One flat enum rather than one per kind, because a slot's subtype has to be
/// storable, comparable and serialisable without knowing which kind it belongs
/// to — and because several names are shared across kinds on purpose (`gate` is
/// a kind and a fuzz variant; `chorus` is a kind and its own first variant).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FxSubtype {
    #[default]
    SubtypeNone,

    // reverb
    Hall,
    Room,
    Plate,
    Chamber,
    Ambience,

    // delay
    Digital,
    Tape,
    Analog,
    Slapback,

    // chorus
    Chorus,
    Ensemble,
    Vibrato,
    Dimension,
    Rotary,

    // flanger
    Flanger,
    Jet,
    ThruZero,

    // phaser
    Phaser,
    Vibe,
    Stepped,

    // distortion
    Overdrive,
    Soft,
    Hard,
    Tube,
    Fold,
    Rectify,

    // fuzz
    Fuzz,
    Germanium,
    Gate,
    Spit,

    // bitcrusher
    Crush,
    Decimate,
    Radio,

    // ringmod
    Ring,
    Bell,
    Am,

    // tremolo
    Sine,
    Square,
    Ramp,
    Chop,

    // filter
    Lowpass,
    Highpass,
    Bandpass,
    Notch,
    Peak,

    // wah
    Auto,
    Pedal,
    Lfo,

    // compressor
    Comp,
    Limiter,
    Punch,

    // gate
    Stutter,
    Duck,
}

impl FxSubtype {
    pub fn name(self) -> &'static str {
        match self {
            FxSubtype::SubtypeNone => "none",

            FxSubtype::Hall => "hall",
            FxSubtype::Room => "room",
            FxSubtype::Plate => "plate",
            FxSubtype::Chamber => "chamber",
            FxSubtype::Ambience => "ambience",

            FxSubtype::Digital => "digital",
            FxSubtype::Tape => "tape",
            FxSubtype::Analog => "analog",
            FxSubtype::Slapback => "slapback",

            FxSubtype::Chorus => "chorus",
            FxSubtype::Ensemble => "ensemble",
            FxSubtype::Vibrato => "vibrato",
            FxSubtype::Dimension => "dimension",
            FxSubtype::Rotary => "rotary",

            FxSubtype::Flanger => "flanger",
            FxSubtype::Jet => "jet",
            FxSubtype::ThruZero => "thru-zero",

            FxSubtype::Phaser => "phaser",
            FxSubtype::Vibe => "vibe",
            FxSubtype::Stepped => "stepped",

            FxSubtype::Overdrive => "overdrive",
            FxSubtype::Soft => "soft",
            FxSubtype::Hard => "hard",
            FxSubtype::Tube => "tube",
            FxSubtype::Fold => "fold",
            FxSubtype::Rectify => "rectify",

            FxSubtype::Fuzz => "fuzz",
            FxSubtype::Germanium => "germanium",
            FxSubtype::Gate => "gate",
            FxSubtype::Spit => "spit",

            FxSubtype::Crush => "crush",
            FxSubtype::Decimate => "decimate",
            FxSubtype::Radio => "radio",

            FxSubtype::Ring => "ring",
            FxSubtype::Bell => "bell",
            FxSubtype::Am => "am",

            FxSubtype::Sine => "sine",
            FxSubtype::Square => "square",
            FxSubtype::Ramp => "ramp",
            FxSubtype::Chop => "chop",

            FxSubtype::Lowpass => "lowpass",
            FxSubtype::Highpass => "highpass",
            FxSubtype::Bandpass => "bandpass",
            FxSubtype::Notch => "notch",
            FxSubtype::Peak => "peak",

            FxSubtype::Auto => "auto",
            FxSubtype::Pedal => "pedal",
            FxSubtype::Lfo => "lfo",

            FxSubtype::Comp => "comp",
            FxSubtype::Limiter => "limiter",
            FxSubtype::Punch => "punch",

            FxSubtype::Stutter => "stutter",
            FxSubtype::Duck => "duck",
        }
    }
}

// -----------------------------------------------------------------------------
// Parameters
// -----------------------------------------------------------------------------

/// How a parameter's value is written down and stepped.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FxUnit {
    /// 0..1, drawn as a percentage.
    Percent,
    /// Drawn with a sign, in decibels.
    Decibels,
    /// A modulation rate, two decimal places: 0.28 Hz is a drift and "0 Hz"
    /// would be indistinguishable from stopped.
    Rate,
    /// An audio frequency, whole hertz.
    Hertz,
    /// Milliseconds — or seconds, past a thousand of them.
    Millis,
    /// A whole number of bits.
    Bits,
    /// A compression ratio, as `n:1`.
    Ratio,
    /// A note value: its name, and the milliseconds it is worth at the current
    /// tempo. The one unit that cannot be written down without a tempo.
    Division,
    /// On or off.
    OnOff,
}

/// How an arrow key moves a parameter.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum FxStep {
    /// A fixed amount, which is what a percentage or a decibel wants.
    Linear(f32),
    /// A fixed proportion, which is what anything spanning two orders of
    /// magnitude wants — a cutoff, a delay time, an attack.
    Ratio(f32),
}

/// One declared parameter of one kind.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FxParamSpec {
    pub label: &'static str,
    /// Inclusive.
    pub range: (f32, f32),
    pub step: FxStep,
    pub unit: FxUnit,
}

/// Shorthand for the tables below.
const fn spec(label: &'static str, range: (f32, f32), step: FxStep, unit: FxUnit) -> FxParamSpec {
    FxParamSpec {
        label,
        range,
        step,
        unit,
    }
}

/// `0..1`, drawn as a percentage, stepped by one.
const fn pct(label: &'static str) -> FxParamSpec {
    spec(label, (0.0, 1.0), FxStep::Linear(0.01), FxUnit::Percent)
}

/// Decibels, a step of half.
const fn db(label: &'static str, range: (f32, f32)) -> FxParamSpec {
    spec(label, range, FxStep::Linear(0.5), FxUnit::Decibels)
}

const REVERB_PARAMS: [FxParamSpec; 4] = [
    pct("size"),
    pct("damp"),
    spec(
        "predelay",
        (0.0, 120.0),
        FxStep::Linear(1.0),
        FxUnit::Millis,
    ),
    pct("mix"),
];

const DELAY_PARAMS: [FxParamSpec; 6] = [
    spec("time", (1.0, 2000.0), FxStep::Ratio(1.15), FxUnit::Millis),
    spec(
        "feedback",
        (0.0, 0.95),
        FxStep::Linear(0.01),
        FxUnit::Percent,
    ),
    pct("tone"),
    pct("mix"),
    spec("sync", (0.0, 1.0), FxStep::Linear(1.0), FxUnit::OnOff),
    // The note value the delay locks to when `sync` is on, as an index into
    // `DIVISIONS`: the point of it is that the duration is *not* stored, because
    // the duration is the tempo's.
    spec(
        "division",
        (0.0, (DIVISIONS.len() - 1) as f32),
        FxStep::Linear(1.0),
        FxUnit::Division,
    ),
];

const CHORUS_PARAMS: [FxParamSpec; 4] = [
    spec("rate", (0.02, 12.0), FxStep::Ratio(1.15), FxUnit::Rate),
    pct("depth"),
    pct("spread"),
    pct("mix"),
];

const FLANGER_PARAMS: [FxParamSpec; 4] = [
    spec("rate", (0.02, 12.0), FxStep::Ratio(1.15), FxUnit::Rate),
    pct("depth"),
    pct("feedback"),
    pct("mix"),
];

const PHASER_PARAMS: [FxParamSpec; 4] = [
    spec("rate", (0.02, 12.0), FxStep::Ratio(1.15), FxUnit::Rate),
    pct("depth"),
    pct("feedback"),
    pct("mix"),
];

const DISTORTION_PARAMS: [FxParamSpec; 4] = [
    db("drive", (0.0, 40.0)),
    pct("tone"),
    db("level", (-24.0, 12.0)),
    pct("mix"),
];

const FUZZ_PARAMS: [FxParamSpec; 4] = [
    db("drive", (6.0, 48.0)),
    pct("bias"),
    pct("gate"),
    db("level", (-24.0, 12.0)),
];

const BITCRUSHER_PARAMS: [FxParamSpec; 3] = [
    spec("bits", (1.0, 16.0), FxStep::Linear(1.0), FxUnit::Bits),
    pct("rate"),
    pct("mix"),
];

const RINGMOD_PARAMS: [FxParamSpec; 3] = [
    spec("freq", (1.0, 4000.0), FxStep::Ratio(1.15), FxUnit::Hertz),
    pct("depth"),
    pct("mix"),
];

const TREMOLO_PARAMS: [FxParamSpec; 3] = [
    spec("rate", (0.02, 20.0), FxStep::Ratio(1.15), FxUnit::Rate),
    pct("depth"),
    pct("mix"),
];

const FILTER_PARAMS: [FxParamSpec; 4] = [
    spec(
        "cutoff",
        (30.0, 18000.0),
        FxStep::Ratio(1.15),
        FxUnit::Hertz,
    ),
    spec(
        "resonance",
        (0.0, 0.99),
        FxStep::Linear(0.01),
        FxUnit::Percent,
    ),
    db("drive", (0.0, 24.0)),
    pct("mix"),
];

const WAH_PARAMS: [FxParamSpec; 5] = [
    pct("sens"),
    pct("range"),
    spec(
        "resonance",
        (0.0, 0.99),
        FxStep::Linear(0.01),
        FxUnit::Percent,
    ),
    spec("rate", (0.02, 12.0), FxStep::Ratio(1.15), FxUnit::Rate),
    pct("mix"),
];

const COMPRESSOR_PARAMS: [FxParamSpec; 5] = [
    spec(
        "threshold",
        (-60.0, 0.0),
        FxStep::Linear(1.0),
        FxUnit::Decibels,
    ),
    spec("ratio", (1.0, 20.0), FxStep::Linear(0.1), FxUnit::Ratio),
    spec("attack", (0.1, 200.0), FxStep::Ratio(1.2), FxUnit::Millis),
    spec("release", (5.0, 2000.0), FxStep::Ratio(1.2), FxUnit::Millis),
    db("makeup", (-12.0, 24.0)),
];

const GATE_PARAMS: [FxParamSpec; 5] = [
    spec(
        "threshold",
        (-80.0, 0.0),
        FxStep::Linear(1.0),
        FxUnit::Decibels,
    ),
    spec("attack", (0.1, 100.0), FxStep::Ratio(1.2), FxUnit::Millis),
    spec("hold", (0.0, 500.0), FxStep::Linear(5.0), FxUnit::Millis),
    spec("release", (5.0, 2000.0), FxStep::Ratio(1.2), FxUnit::Millis),
    spec("rate", (0.1, 20.0), FxStep::Ratio(1.15), FxUnit::Rate),
];

// -----------------------------------------------------------------------------
// An effect
// -----------------------------------------------------------------------------

/// One effect: a kind, a variant of it, and the five numbers that vary it.
#[derive(Copy, Clone, Debug, PartialEq, Serialize)]
pub struct Fx {
    pub kind: FxKind,
    pub subtype: FxSubtype,
    pub params: [f32; FX_PARAMS],
}

/// What a stored effect looks like before it is checked.
#[derive(Deserialize)]
struct FxFile {
    #[serde(default)]
    kind: FxKind,
    #[serde(default)]
    subtype: FxSubtype,
    /// Absent entirely — a hand-written slot, or one written before this kind
    /// had parameters — means "give me this variant's own starting sound".
    #[serde(default)]
    params: Option<[f32; FX_PARAMS]>,
}

impl<'de> Deserialize<'de> for Fx {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let file = FxFile::deserialize(deserializer)?;
        // A subtype that does not belong to its kind, or a parameter outside its
        // range, is corrected rather than refused: one bad slot in a user's file
        // must not stop the whole chain loading, and a slot left holding five
        // zeroes would be a sound nobody chose.
        let paired = file.kind.subtypes().contains(&file.subtype);
        let subtype = if paired {
            file.subtype
        } else {
            file.kind.default_subtype()
        };
        let mut fx = Fx {
            kind: file.kind,
            subtype,
            // Parameters given for a variant that does not exist were written
            // for something else, so they are discarded rather than clamped:
            // five numbers that meant `drive` and `tone` are not a reverb.
            params: match file.params {
                Some(params) if paired => params,
                _ => file.kind.defaults(subtype),
            },
        };
        fx.clamp();
        Ok(fx)
    }
}

impl Default for Fx {
    fn default() -> Self {
        Fx::none()
    }
}

impl Fx {
    /// An empty slot.
    pub fn none() -> Self {
        Fx {
            kind: FxKind::None,
            subtype: FxSubtype::SubtypeNone,
            params: NO_PARAMS,
        }
    }

    /// A fresh effect of a kind, on that kind's default variant and its defaults.
    ///
    /// Test-only: the live controls are [`crate::synth::FxSlotParams`], and this
    /// is how a test spells out an effect to compare one against.
    #[cfg(test)]
    pub fn new(kind: FxKind) -> Self {
        let subtype = kind.default_subtype();
        Fx {
            kind,
            subtype,
            params: kind.defaults(subtype),
        }
    }

    /// A fresh effect of one exact variant.
    pub fn variant(kind: FxKind, subtype: FxSubtype) -> Self {
        let subtype = if kind.subtypes().contains(&subtype) {
            subtype
        } else {
            kind.default_subtype()
        };
        Fx {
            kind,
            subtype,
            params: kind.defaults(subtype),
        }
    }

    pub fn is_none(&self) -> bool {
        self.kind.is_none()
    }

    /// Move to another kind, landing on its first variant and that variant's
    /// defaults.
    ///
    /// The parameters are *replaced* rather than kept, deliberately: five numbers
    /// that meant `drive`, `tone`, `level` and `mix` cannot mean `rate`, `depth`,
    /// `spread` and `mix`, and keeping them would put a sound on screen that
    /// nobody asked for.
    #[cfg(test)]
    pub fn set_kind(&mut self, kind: FxKind) {
        *self = Fx::new(kind);
    }

    /// Move to another variant of the same kind, with that variant's defaults.
    #[cfg(test)]
    pub fn set_subtype(&mut self, subtype: FxSubtype) {
        *self = Fx::variant(self.kind, subtype);
    }

    /// Number of parameters this kind actually uses.
    #[cfg(test)]
    pub fn param_count(&self) -> usize {
        self.kind.params().len()
    }

    /// The declarations for this kind's parameters.
    #[cfg(test)]
    pub fn specs(&self) -> &'static [FxParamSpec] {
        self.kind.params()
    }

    pub fn spec(&self, index: usize) -> Option<&'static FxParamSpec> {
        self.kind.params().get(index)
    }

    /// The raw value in a slot, or zero for a slot this kind does not use.
    pub fn param(&self, index: usize) -> f32 {
        self.params.get(index).copied().unwrap_or(0.0)
    }

    /// Whether a slot is one of this kind's.
    #[cfg(test)]
    pub fn uses(&self, index: usize) -> bool {
        index < self.param_count()
    }

    /// Write a slot, clamped into its declared range.
    ///
    /// A slot the kind does not use is left alone rather than written: the array
    /// is shared storage, and a stray write into the tail would survive a change
    /// of kind.
    pub fn set_param(&mut self, index: usize, value: f32) {
        let Some(spec) = self.spec(index) else {
            return;
        };
        let value = if value.is_finite() {
            value
        } else {
            spec.range.0
        };
        self.params[index] = value.clamp(spec.range.0, spec.range.1);
    }

    /// Pull every used slot inside its range, and zero the unused tail.
    fn clamp(&mut self) {
        for index in 0..FX_PARAMS {
            match self.spec(index) {
                Some(spec) => {
                    let value = self.params[index];
                    let value = if value.is_finite() {
                        value
                    } else {
                        spec.range.0
                    };
                    self.params[index] = value.clamp(spec.range.0, spec.range.1);
                }
                None => self.params[index] = 0.0,
            }
        }
    }

    /// The value one arrow press lands on.
    ///
    /// `coarse` is the `Shift` step, the same convention every other adjustable
    /// value in this app uses.
    pub fn stepped(&self, index: usize, delta: i32, coarse: bool) -> f32 {
        let Some(spec) = self.spec(index) else {
            return 0.0;
        };
        let current = self.param(index);
        let next = match spec.step {
            FxStep::Linear(step) => current + delta as f32 * step * if coarse { 5.0 } else { 1.0 },
            FxStep::Ratio(ratio) => {
                let ratio = if coarse { ratio * ratio * ratio } else { ratio };
                current * ratio.powf(delta as f32)
            }
        };
        next.clamp(spec.range.0, spec.range.1)
    }

    /// How a value in a slot is written down.
    ///
    /// The delay's `time` is the one that needs the whole effect rather than just
    /// the slot: with `sync` on, the milliseconds and the note division they land
    /// on are both worth seeing, and only the pair knows the second.
    pub fn display(&self, index: usize, tempo: f32) -> String {
        let Some(spec) = self.spec(index) else {
            return String::new();
        };
        let value = self.param(index);
        match spec.unit {
            FxUnit::Percent => format!("{:.0}%", value * 100.0),
            FxUnit::Decibels => format!("{:+.1} dB", value),
            FxUnit::Rate => format!("{:.2} Hz", value),
            FxUnit::Hertz => format!("{:.0} Hz", value),
            FxUnit::Millis => {
                if value >= 1000.0 {
                    format!("{:.2} s", value / 1000.0)
                } else {
                    format!("{:.0} ms", value)
                }
            }
            FxUnit::Division => {
                let (beats, name) = division_at(value);
                format!("{}  {:.0} ms", name, division_millis(beats, tempo))
            }
            FxUnit::Bits => format!("{:.0} bit", value),
            FxUnit::Ratio => format!("{:.1}:1", value),
            FxUnit::OnOff => {
                if value > 0.5 {
                    "on".to_string()
                } else {
                    "off".to_string()
                }
            }
        }
    }

    /// The same effect, with its variant chosen by index into its kind's list.
    ///
    /// A variant is a number where it is stored, because a number is what a
    /// shared parameter can be. Out of range wraps rather than panicking, and
    /// [`Self::normalised`] settles the pair afterwards.
    pub fn with_subtype_index(mut self, index: f32) -> Self {
        let subtypes = self.kind.subtypes();
        let index = if index.is_finite() {
            (index as i32).rem_euclid(subtypes.len() as i32) as usize
        } else {
            0
        };
        self.subtype = subtypes[index];
        self
    }

    /// The same effect, with a pair that belongs together and every parameter
    /// inside its range.
    ///
    /// The live controls are atomics that a UI writes one at a time, so the
    /// callback has to be able to turn *any* combination of them into something
    /// valid — including a subtype left over from the kind before, which is what
    /// a two-keystroke change of type looks like from the audio thread.
    pub fn normalised(mut self) -> Self {
        if !self.kind.subtypes().contains(&self.subtype) {
            self.subtype = self.kind.default_subtype();
        }
        self.clamp();
        self
    }

    /// `distortion · overdrive`, for a row that has room for one string.
    pub fn label(&self) -> String {
        if self.is_none() {
            return "—".to_string();
        }
        format!("{} · {}", self.kind.name(), self.subtype.name())
    }
}

// -----------------------------------------------------------------------------
// Tempo sync
// -----------------------------------------------------------------------------

/// The note divisions a synced delay snaps to, in beats, with their names.
///
/// Dotted and triplet values are here because a delay that only does powers of
/// two is a delay nobody uses twice. A beat is a quarter note, so an eighth
/// triplet is a third of one and a dotted eighth is three quarters.
pub const DIVISIONS: [(f32, &str); 13] = [
    (0.125, "1/32"),
    (0.25, "1/16"),
    (1.0 / 3.0, "1/8T"),
    (0.375, "1/16."),
    (0.5, "1/8"),
    (2.0 / 3.0, "1/4T"),
    (0.75, "1/8."),
    (1.0, "1/4"),
    (4.0 / 3.0, "1/2T"),
    (1.5, "1/4."),
    (2.0, "1/2"),
    (3.0, "1/2."),
    (4.0, "1/1"),
];

/// The division at an index, clamped rather than panicking.
pub fn division_at(index: f32) -> (f32, &'static str) {
    let index = if index.is_finite() {
        (index.round().max(0.0) as usize).min(DIVISIONS.len() - 1)
    } else {
        0
    };
    DIVISIONS[index]
}

/// The milliseconds a division is worth at a tempo.
pub fn division_millis(beats: f32, tempo: f32) -> f32 {
    let tempo = if tempo.is_finite() && tempo > 0.0 {
        tempo
    } else {
        120.0
    };
    beats * 60_000.0 / tempo
}

// -----------------------------------------------------------------------------
// The preset library
// -----------------------------------------------------------------------------

/// A named effect: a kind, a variant, and the parameters that make it that sound.
///
/// Serialised by flattening the effect, so a preset is one block of five lines
/// and reads the same way a slot in an ensemble does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FxPreset {
    pub name: String,
    #[serde(flatten)]
    pub fx: Fx,
}

/// The shipped presets, compiled in from the tracked file.
///
/// Panics only on a malformed file, which is a repository bug that
/// `every_shipped_preset_parses` catches before it can be committed.
pub fn builtin_presets() -> Vec<FxPreset> {
    from_toml(include_str!("../fx_presets.toml")).expect("fx_presets.toml is valid TOML")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FxPresetFile {
    presets: Vec<FxPreset>,
}

/// The effect library: the shipped presets with the user's own layered over them.
#[derive(Clone, Debug, Default)]
pub struct FxPresetStore {
    /// Every preset, shipped and user, in library order.
    pub presets: Vec<FxPreset>,
    /// Just the user's own. This is what [`Self::save`] writes.
    user: Vec<FxPreset>,
}

impl FxPresetStore {
    /// Load the shipped library and the user's own file, creating the latter if
    /// missing.
    pub fn load(user_path: &Path) -> io::Result<Self> {
        let mut store = FxPresetStore {
            presets: builtin_presets(),
            user: Vec::new(),
        };
        if user_path.exists() {
            for preset in read(user_path)? {
                store.add(preset);
            }
        } else {
            store.save(user_path)?;
        }
        Ok(store)
    }

    /// Write the user's own entries. The shipped library is never written.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = to_toml(&self.user).map_err(|e| io::Error::other(e.to_string()))?;
        fs::write(path, text)
    }

    /// Add a preset, replacing any entry of the same name.
    pub fn add(&mut self, preset: FxPreset) {
        upsert(&mut self.presets, preset.clone());
        upsert(&mut self.user, preset);
    }

    /// The preset of this name, wherever it came from.
    #[cfg(test)]
    pub fn find(&self, name: &str) -> Option<&FxPreset> {
        self.presets.iter().find(|p| p.name == name)
    }

    /// The presets that belong to this effect's kind and variant, in library
    /// order — which is the list the `preset` row walks.
    pub fn matching(&self, fx: &Fx) -> Vec<&FxPreset> {
        self.presets
            .iter()
            .filter(|p| p.fx.kind == fx.kind && p.fx.subtype == fx.subtype)
            .collect()
    }

    /// The name of the preset this effect is, if it is one.
    pub fn name_for(&self, fx: &Fx) -> Option<&str> {
        self.name_for_using(fx, &ALL_PARAMS)
    }

    /// The same, judging only the parameters in `used`.
    ///
    /// An aux unit has no dry/wet of its own — the mixer's return level decides
    /// how much of it you hear — so its `mix` slot is never written by the panel
    /// and its value in a stored preset is meaningless. Comparing all five slots
    /// would make every aux read as `custom` forever, including the ones the
    /// panel has just loaded.
    pub fn name_for_using(&self, fx: &Fx, used: &[usize]) -> Option<&str> {
        self.matching(fx)
            .into_iter()
            .find(|p| same_params(&p.fx, fx, used))
            .map(|p| p.name.as_str())
    }

    /// Step `delta` places through this effect's presets, wrapping.
    ///
    /// A `custom` effect has no place in the walk to step from, so it starts at
    /// the beginning of the list rather than at an arbitrary entry.
    pub fn step(&self, fx: &Fx, delta: i32) -> Option<&FxPreset> {
        self.step_using(fx, &ALL_PARAMS, delta)
    }

    /// The same walk, positioning by the parameters in `used`.
    pub fn step_using(&self, fx: &Fx, used: &[usize], delta: i32) -> Option<&FxPreset> {
        let matching = self.matching(fx);
        if matching.is_empty() {
            return None;
        }
        let at = matching
            .iter()
            .position(|p| same_params(&p.fx, fx, used))
            .map(|i| i as i32);
        let index = match at {
            Some(i) => (i + delta).rem_euclid(matching.len() as i32) as usize,
            None if delta >= 0 => 0,
            None => matching.len() - 1,
        };
        matching.get(index).copied()
    }

    #[cfg(test)]
    pub fn with_builtins() -> Self {
        FxPresetStore {
            presets: builtin_presets(),
            user: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn from_presets(presets: Vec<FxPreset>) -> Self {
        FxPresetStore {
            presets: presets.clone(),
            user: presets,
        }
    }
}

fn upsert(presets: &mut Vec<FxPreset>, preset: FxPreset) {
    match presets.iter_mut().find(|p| p.name == preset.name) {
        Some(existing) => *existing = preset,
        None => presets.push(preset),
    }
}

fn read(path: &Path) -> io::Result<Vec<FxPreset>> {
    let text = fs::read_to_string(path)?;
    from_toml(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

/// Parse a preset document. Unknown keys are ignored, so a file written by a
/// different build still loads.
pub fn from_toml(text: &str) -> Result<Vec<FxPreset>, toml::de::Error> {
    let file: FxPresetFile = toml::from_str(text)?;
    Ok(file.presets)
}

pub fn to_toml(presets: &[FxPreset]) -> Result<String, toml::ser::Error> {
    toml::to_string_pretty(&FxPresetFile {
        presets: presets.to_vec(),
    })
}

pub fn user_path() -> PathBuf {
    PathBuf::from("fx_presets.user.toml")
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_variant_and_every_variant_belongs_to_one() {
        // The subtype enum is flat and the membership lives in the kind, so the
        // two can drift. This is what holds them together: a variant with no
        // kind is unreachable, and a kind with no variant is a row that cannot be
        // set.
        let mut seen = Vec::new();
        for kind in FxKind::ALL {
            let subtypes = kind.subtypes();
            assert!(!subtypes.is_empty(), "{} offers no variants", kind.name());
            assert_eq!(kind.default_subtype(), subtypes[0]);
            for subtype in subtypes {
                assert!(
                    !seen.contains(subtype) || kind == FxKind::Fuzz || kind == FxKind::Gate,
                    "{} is claimed by two kinds",
                    subtype.name()
                );
                seen.push(*subtype);
                // Every pair must have defaults and a name.
                assert!(!subtype.name().is_empty());
                let defaults = kind.defaults(*subtype);
                assert_eq!(defaults.len(), FX_PARAMS);
            }
        }
        assert_eq!(seen.len(), 55, "a variant was added or removed");
    }

    #[test]
    fn every_default_is_inside_its_own_range() {
        // A fresh slot whose numbers the arrows would snap is a slot that changes
        // the first time it is touched — the same contract the instrument library
        // is held to.
        for kind in FxKind::ALL {
            for subtype in kind.subtypes() {
                let fx = Fx::variant(kind, *subtype);
                for (index, spec) in fx.specs().iter().enumerate() {
                    let value = fx.param(index);
                    assert!(
                        value.is_finite() && (spec.range.0..=spec.range.1).contains(&value),
                        "{} / {} {} is {}, outside {:?}",
                        kind.name(),
                        subtype.name(),
                        spec.label,
                        value,
                        spec.range
                    );
                }
            }
        }
    }

    #[test]
    fn a_kind_only_uses_a_prefix_of_the_slots() {
        // The DSP reads `params[0..count]` and the tail is shared storage, so a
        // kind that used slot 3 without slot 2 would be a hole nothing fills.
        for kind in FxKind::ALL {
            let count = kind.params().len();
            assert!(
                count <= FX_PARAMS,
                "{} declares {} parameters",
                kind.name(),
                count
            );
            let fx = Fx::new(kind);
            for index in 0..FX_PARAMS {
                assert_eq!(
                    fx.uses(index),
                    index < count,
                    "{} slot {}",
                    kind.name(),
                    index
                );
                if index >= count {
                    assert_eq!(fx.param(index), 0.0, "{} has a dirty tail", kind.name());
                }
            }
        }
    }

    #[test]
    fn no_kind_has_an_empty_parameter_list_except_none() {
        for kind in FxKind::ALL {
            if kind.is_none() {
                assert!(kind.params().is_empty());
            } else {
                assert!(
                    !kind.params().is_empty(),
                    "{} offers nothing to adjust",
                    kind.name()
                );
            }
        }
    }

    #[test]
    fn a_kind_is_named_and_labelled_the_way_the_panel_shows_it() {
        assert_eq!(Fx::none().label(), "—");
        let fx = Fx::variant(FxKind::Distortion, FxSubtype::Overdrive);
        assert_eq!(fx.label(), "distortion · overdrive");
    }

    #[test]
    fn changing_kind_replaces_the_parameters_instead_of_keeping_them() {
        // `drive` and `rate` are not the same number under a different name.
        let mut fx = Fx::new(FxKind::Distortion);
        fx.set_param(P0, 30.0);
        assert_eq!(fx.param(P0), 30.0);
        fx.set_kind(FxKind::Chorus);
        assert_eq!(fx.param(P0), FxKind::Chorus.defaults(FxSubtype::Chorus)[P0]);
        assert_eq!(fx.subtype, FxSubtype::Chorus);
        // And an empty slot really is empty.
        fx.set_kind(FxKind::None);
        assert!(fx.is_none());
        assert_eq!(fx.params, NO_PARAMS);
    }

    #[test]
    fn a_subtype_that_does_not_belong_is_replaced_not_kept() {
        let mut fx = Fx::new(FxKind::Distortion);
        fx.set_subtype(FxSubtype::Tape);
        assert_eq!(fx.subtype, FxSubtype::Overdrive, "the kind's first variant");
    }

    #[test]
    fn a_parameter_is_clamped_and_a_nan_is_refused() {
        let mut fx = Fx::new(FxKind::Distortion);
        fx.set_param(P0, 900.0);
        assert_eq!(fx.param(P0), 40.0);
        fx.set_param(P0, -900.0);
        assert_eq!(fx.param(P0), 0.0);
        fx.set_param(P0, f32::NAN);
        assert!(fx.param(P0).is_finite());
        // A slot the kind does not use is left alone rather than written.
        let before = fx.params;
        fx.set_param(P4, 5.0);
        assert_eq!(fx.params, before);
    }

    #[test]
    fn a_linear_step_moves_by_its_step_and_a_ratio_step_by_its_ratio() {
        let mut fx = Fx::new(FxKind::Distortion);
        fx.set_param(P0, 0.0);
        assert_eq!(fx.stepped(P0, 1, false), 0.5, "a half decibel");
        assert_eq!(fx.stepped(P0, 1, true), 2.5, "five of them with shift");
        assert_eq!(fx.stepped(P0, 0, false), 0.0);
        assert_eq!(fx.stepped(P0, -1, false), 0.0, "clamped at the bottom");

        let delay = Fx::new(FxKind::Delay);
        let up = delay.stepped(P0, 1, false);
        assert!(up > delay.param(P0), "time rises by a ratio");
        let down = delay.stepped(P0, -1, false);
        assert!(down < delay.param(P0));
        // And it never leaves the range, however many presses.
        assert_eq!(delay.stepped(P0, 1000, true), 2000.0);
        assert_eq!(delay.stepped(P0, -1000, true), 1.0);
    }

    #[test]
    fn every_unit_renders_something_readable() {
        for kind in FxKind::ALL {
            let fx = Fx::new(kind);
            for index in 0..fx.param_count() {
                let text = fx.display(index, 120.0);
                assert!(
                    !text.is_empty() && !text.contains("NaN") && !text.contains("inf"),
                    "{} {} rendered {:?}",
                    kind.name(),
                    fx.spec(index).unwrap().label,
                    text
                );
            }
            // A slot the kind does not use renders nothing rather than a zero.
            assert_eq!(fx.display(FX_PARAMS, 120.0), "");
        }
    }

    #[test]
    fn a_synced_delay_shows_the_note_value_and_what_it_is_worth() {
        // The delay stores its time in milliseconds when it is free and a note
        // value when it is locked, because those are the two things a player
        // means. The locked one is the whole reason for the separate slot: it is
        // the only one that can follow the tempo.
        let mut fx = Fx::variant(FxKind::Delay, FxSubtype::Digital);
        fx.set_param(P4, 1.0);
        fx.set_param(P5, 7.0); // a quarter note

        assert_eq!(fx.display(P5, 120.0), "1/4  500 ms");
        // Half the tempo, twice the delay: the grid moved, the note value did
        // not, which is what a synced delay is *for*.
        assert_eq!(fx.display(P5, 60.0), "1/4  1000 ms");

        // A triplet is two thirds of a beat, not a quarter of one.
        fx.set_param(P5, 5.0);
        assert_eq!(fx.display(P5, 120.0), "1/4T  333 ms");

        // The milliseconds slot is a plain duration and says so either way.
        fx.set_param(P4, 0.0);
        fx.set_param(P0, 500.0);
        assert_eq!(fx.display(P0, 120.0), "500 ms");
        assert_eq!(fx.display(P0, 60.0), "500 ms");
    }

    #[test]
    fn the_division_table_is_ordered_and_named() {
        let mut previous = 0.0;
        for (beats, name) in DIVISIONS {
            assert!(beats > previous, "{} is out of order", name);
            assert!(!name.is_empty());
            previous = beats;
        }
        // A tempo of zero or a nonsense one must not divide by it.
        assert!(division_millis(1.0, 0.0).is_finite());
        assert!(division_millis(1.0, f32::NAN).is_finite());
        // And an index that is off either end clamps rather than panicking.
        assert_eq!(division_at(-5.0).1, DIVISIONS[0].1);
        assert_eq!(division_at(1.0e9).1, DIVISIONS[DIVISIONS.len() - 1].1);
        assert_eq!(division_at(f32::NAN).1, DIVISIONS[0].1);
        assert_eq!(division_at(7.0).1, "1/4");
    }

    // ---- serde ----

    #[test]
    fn an_effect_round_trips_through_toml() {
        let fx = Fx::variant(FxKind::Flanger, FxSubtype::Jet);
        let text = toml::to_string(&fx).unwrap();
        assert_eq!(toml::from_str::<Fx>(&text).unwrap(), fx);
        // And it is a bare table, not a nest.
        assert!(text.contains("kind = \"flanger\""));
        assert!(text.contains("subtype = \"jet\""));
    }

    #[test]
    fn a_stored_effect_with_no_parameters_gets_its_variant_defaults() {
        // A hand-written slot, or one written before this kind had parameters.
        // Five zeroes would be a sound nobody chose — a distortion with no drive
        // and no level.
        let fx: Fx = toml::from_str("kind = \"distortion\"\nsubtype = \"tube\"").unwrap();
        assert_eq!(fx.params, FxKind::Distortion.defaults(FxSubtype::Tube));
        assert!(fx.param(P0) > 0.0, "the drive should be a real value");
    }

    #[test]
    fn a_stored_effect_with_an_impossible_pair_is_corrected() {
        // A file can say anything. `hall` is not a distortion.
        let fx: Fx = toml::from_str(
            "kind = \"distortion\"\nsubtype = \"hall\"\nparams = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]",
        )
        .unwrap();
        assert_eq!(fx.subtype, FxSubtype::Overdrive);
        assert_eq!(fx.params, FxKind::Distortion.defaults(FxSubtype::Overdrive));
    }

    #[test]
    fn a_stored_effect_with_out_of_range_parameters_is_clamped() {
        let fx: Fx = toml::from_str(
            "kind = \"distortion\"\nsubtype = \"hard\"\nparams = [900.0, -5.0, 40.0, 7.0, 9.0, 11.0]",
        )
        .unwrap();
        assert_eq!(fx.param(P0), 40.0);
        assert_eq!(fx.param(P1), 0.0);
        assert_eq!(fx.param(P2), 12.0);
        assert_eq!(fx.param(P3), 1.0);
        assert_eq!(fx.param(P4), 0.0, "the unused tail is zeroed");
    }

    #[test]
    fn an_empty_or_absent_effect_is_none() {
        let fx: Fx = toml::from_str("").unwrap();
        assert!(fx.is_none());
        assert_eq!(fx, Fx::none());
    }

    // ---- the library ----

    #[test]
    fn every_shipped_preset_parses() {
        let presets = builtin_presets();
        assert!(presets.len() >= 50, "only {} presets", presets.len());
    }

    #[test]
    fn the_preset_names_are_distinct() {
        let presets = builtin_presets();
        let mut names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), presets.len());
    }

    #[test]
    fn every_variant_offers_at_least_one_preset() {
        // The `preset` row is the only way most players will ever hear what a
        // variant can do, so a variant with an empty list is a variant with no
        // door into it.
        let store = FxPresetStore::with_builtins();
        for kind in FxKind::ALL {
            if kind.is_none() {
                continue;
            }
            for subtype in kind.subtypes() {
                let fx = Fx::variant(kind, *subtype);
                assert!(
                    !store.matching(&fx).is_empty(),
                    "{} / {} has no preset",
                    kind.name(),
                    subtype.name()
                );
            }
        }
    }

    #[test]
    fn no_two_presets_of_one_variant_are_the_same_sound() {
        let store = FxPresetStore::with_builtins();
        for kind in FxKind::ALL {
            for subtype in kind.subtypes() {
                let fx = Fx::variant(kind, *subtype);
                let matching = store.matching(&fx);
                for i in 0..matching.len() {
                    for j in (i + 1)..matching.len() {
                        assert_ne!(
                            matching[i].fx.params,
                            matching[j].fx.params,
                            "{} and {} on {} / {} are the same sound",
                            matching[i].name,
                            matching[j].name,
                            kind.name(),
                            subtype.name()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn every_shipped_parameter_is_inside_its_range() {
        for preset in builtin_presets() {
            let fx = preset.fx;
            for index in 0..fx.param_count() {
                let spec = fx.spec(index).unwrap();
                let value = fx.param(index);
                assert!(
                    (spec.range.0..=spec.range.1).contains(&value),
                    "{} {} is {}",
                    preset.name,
                    spec.label,
                    value
                );
            }
        }
    }

    #[test]
    fn an_effect_can_be_found_by_name_and_a_name_by_effect() {
        let store = FxPresetStore::with_builtins();
        for preset in &store.presets {
            assert_eq!(store.find(&preset.name), Some(preset));
            assert_eq!(store.name_for(&preset.fx), Some(preset.name.as_str()));
        }
        assert_eq!(store.find("Nonexistent"), None);
        // A variant of the same kind with different numbers is not that preset.
        let mut custom = store.presets[0].fx;
        custom.set_param(P0, custom.stepped(P0, 1, false));
        if custom.params != store.presets[0].fx.params {
            assert_eq!(store.name_for(&custom), None, "custom");
        }
    }

    #[test]
    fn stepping_walks_one_variants_presets_and_wraps() {
        let store = FxPresetStore::with_builtins();
        let mut fx = Fx::variant(FxKind::Reverb, FxSubtype::Hall);
        let count = store.matching(&fx).len();
        assert!(count > 1, "the hall needs more than one preset to walk");

        // A fresh hall is `custom`: its defaults leave the reverb silent, on
        // purpose, so that every shipped ensemble still sounds the way it did.
        // One press from custom lands on the first preset, which is audible.
        assert_eq!(store.name_for(&fx), None, "a fresh slot is custom");
        fx = store.step(&fx, 1).unwrap().fx;
        let first = store.name_for(&fx).map(|s| s.to_string());
        assert!(first.is_some(), "the first preset should be named");

        // One more step is the second preset, which is a different sound.
        let next = store.step(&fx, 1).unwrap();
        assert_ne!(Some(next.name.as_str()), first.as_deref());
        fx = next.fx;
        let second = store.name_for(&fx).map(|s| s.to_string());

        // A full lap comes back to where the lap started.
        for _ in 0..count {
            fx = store.step(&fx, 1).unwrap().fx;
        }
        assert_eq!(store.name_for(&fx), second.as_deref());

        // And backwards is the inverse.
        fx = store.step(&fx, -1).unwrap().fx;
        assert_eq!(store.name_for(&fx), first.as_deref());

        // A custom sound starts at the beginning of the list.
        let mut custom = Fx::variant(FxKind::Reverb, FxSubtype::Hall);
        custom.set_param(P1, 0.99);
        assert_eq!(store.name_for(&custom), None);
        assert_eq!(
            store.step(&custom, 1).map(|p| p.name.clone()),
            store.matching(&custom).first().map(|p| p.name.clone())
        );
    }

    #[test]
    fn stepping_a_variant_with_no_presets_is_nothing_rather_than_a_panic() {
        let store = FxPresetStore::from_presets(Vec::new());
        assert!(store.step(&Fx::new(FxKind::Chorus), 1).is_none());
        assert!(store.matching(&Fx::new(FxKind::Chorus)).is_empty());
    }

    #[test]
    fn presets_round_trip_through_toml() {
        let store = FxPresetStore::with_builtins();
        let text = to_toml(&store.presets).unwrap();
        assert_eq!(from_toml(&text).unwrap(), store.presets);
        // One preset is a name and a flattened effect, not a nested table.
        assert!(text.contains("name = "));
        assert!(text.contains("kind = "));
        assert!(text.contains("params = ["));
    }

    #[test]
    fn load_creates_the_user_file() {
        let user = temp_path("fx-preset-user");
        let _ = fs::remove_file(&user);
        let store = FxPresetStore::load(&user).unwrap();
        assert!(user.exists(), "the user's file is created to be edited");
        assert_eq!(store.presets.len(), builtin_presets().len());
        let _ = fs::remove_file(&user);
    }

    #[test]
    fn a_saved_preset_shadows_the_shipped_one_by_name() {
        let user = temp_path("fx-preset-shadow");
        let _ = fs::remove_file(&user);

        let mut store = FxPresetStore::load(&user).unwrap();
        let mut mine = builtin_presets()[0].clone();
        let name = mine.name.clone();
        mine.fx.set_param(P0, mine.fx.stepped(P0, 3, false));
        store.add(mine.clone());
        store.save(&user).unwrap();

        let again = FxPresetStore::load(&user).unwrap();
        assert_eq!(again.presets.len(), builtin_presets().len());
        assert_eq!(again.find(&name).unwrap().fx, mine.fx);
        let _ = fs::remove_file(&user);
    }

    fn temp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "chord-tool-test-{}-{}.toml",
            name,
            std::process::id()
        ));
        p
    }
}
