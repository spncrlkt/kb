//! A thirteen-band equaliser, and the named curves it is set from.
//!
//! The synth already had a filter per voice, and that filter is part of what a
//! sound *is* — a `cutoff` and a `resonance` are as much a voice's character as
//! its waveform. What it could not do is place a sound in a mix: a lowpass can
//! only take the top off, so it cannot lift a bass, dip a boxy middle or add
//! air, and it cannot be shared by three registers or by the mix as a whole.
//!
//! So this is the second, coarser tool, and it sits **on the buses rather than
//! in the voices**:
//!
//! - one curve per [`Placement`](crate::ensemble::Placement), applied to that
//!   register's bus, so the low, mid and high parts of a chord can each be
//!   carved into shape, and
//! - one curve for the master, applied to the finished stereo pair.
//!
//! That is four coefficient sets and five running filters — 65 biquads per
//! sample — where a per-voice EQ would need thirteen of them per voice and would
//! cost more than everything else in the callback put together. The register
//! bus is where the decision actually belongs anyway: a chord's middle is a
//! *part*, and it is the part that wants its mud removed.
//!
//! # Bypass is exact
//!
//! A curve that is flat in all thirteen bands is not run at all. This is not only
//! an optimisation: it is what lets the equaliser be added to the crate without
//! changing a single sound that was already there. Every shipped ensemble and
//! instrument ships flat, so every one of them renders bit-for-bit as it did
//! before, and the recorded palette fingerprint still holds.
//!
//! # The shipped library is the file
//!
//! `eq_presets.toml` is compiled in with `include_str!` and parsed at start-up,
//! like `instruments.toml` and `ensembles.toml`: editing the file is how the
//! shipped curves are changed. A preset is only ever a *starting point* —
//! applying one copies its thirteen gains into the placement, which then owns
//! them — so a curve that a user edits later never moves a saved ensemble.

use std::f32::consts::{FRAC_1_SQRT_2, PI};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// -----------------------------------------------------------------------------
// Bands
// -----------------------------------------------------------------------------

/// How many bands the equaliser has.
pub const EQ_BANDS: usize = 13;

/// The centre of each band, in Hz.
///
/// Two thirds of an octave apart at the bottom (20 Hz to 125 Hz), an octave
/// through the middle (125 Hz to 8 kHz) and closer than that at the very top.
/// The bottom is the dense end on purpose: the audible octaves below 125 Hz are
/// few and perceptually heavy, which is exactly where a graphic equaliser that
/// stops at twelve bands cannot reach. The two extremes are shelves and the
/// eleven between them are bells.
///
/// One consequence of both end bands being shelves whose corners sit near the
/// edges of the audible range is worth knowing rather than discovering: on a
/// 44.1 kHz device neither ever reaches a flat plateau. +12 dB at the 16 kHz
/// corner is about +6 dB there and less above it, and +12 dB at the 20 Hz corner
/// is about +6 dB at 20 Hz with the real lift below. Each still only touches its
/// own end of the range, which is what it is for: the 20 Hz band lifts the very
/// bottom without thumping 40–100 Hz, which is what the 31.5 Hz bell is for.
pub const BANDS: [f32; EQ_BANDS] = [
    20.0, 31.5, 50.0, 80.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 12500.0, 16000.0,
];

/// What each band is called on screen, short enough to sit in a column.
pub const BAND_LABELS: [&str; EQ_BANDS] = [
    "20", "31.5", "50", "80", "125", "250", "500", "1k", "2k", "4k", "8k", "12.5k", "16k",
];

/// The smallest and largest gain any band can be set to, in decibels.
///
/// Twelve either way is the whole point of a graphic equaliser: enough to
/// reshape a part, not enough to wreck it.
pub const GAIN_RANGE: (f32, f32) = (-12.0, 12.0);

/// How wide the ten bell bands are.
///
/// A little over an octave, which is the value the classic octave graphic uses
/// so that neighbours overlap rather than leaving a notch between them.
const PEAK_Q: f32 = 1.414;

/// How wide the two shelving bands are.
///
/// The RBJ shelf with `S = 1` is exactly `sin(w0) / (2Q)` at `Q = 1/sqrt(2)`, so
/// this is the textbook slope rather than an approximation of it.
const SHELF_Q: f32 = FRAC_1_SQRT_2;

/// The highest a band's corner is allowed to sit, as a fraction of the sample
/// rate.
///
/// A band at the sample rate's Nyquist point has `sin(w0) = 0`, which collapses
/// a bell into a pair of poles sitting *on* the unit circle — a marginally
/// stable resonator rather than a filter. The top band is the only one that can
/// get there, and only on a low-rate device, but clamping is one line and the
/// alternative is a sound that grows instead of decaying.
const MAX_CORNER_FRACTION: f32 = 0.45;

/// The highest band name a preset may not use, because flat is spelled that way.
pub const FLAT_LABEL: &str = "Flat";

/// Which equaliser a panel row is pointed at.
///
/// Three registers and the mix, in the order the panel offers them. Defined here
/// rather than in the UI because [`crate::synth::SynthParams::eq_at`] indexes
/// the live parameters by it, and two orderings that had to agree would
/// eventually not.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EqTarget {
    Low,
    Mid,
    High,
    Master,
}

impl EqTarget {
    pub const ALL: [EqTarget; 4] = [
        EqTarget::Low,
        EqTarget::Mid,
        EqTarget::High,
        EqTarget::Master,
    ];

    pub fn name(self) -> &'static str {
        match self {
            EqTarget::Low => "low",
            EqTarget::Mid => "mid",
            EqTarget::High => "high",
            EqTarget::Master => "master",
        }
    }

    /// Where this target sits in [`Self::ALL`], and so which of the analyser's
    /// taps it reads.
    pub fn index(self) -> usize {
        match self {
            EqTarget::Low => 0,
            EqTarget::Mid => 1,
            EqTarget::High => 2,
            EqTarget::Master => 3,
        }
    }

    /// The target at `index`, clamped rather than panicking.
    pub fn from_index(index: usize) -> Self {
        *Self::ALL.get(index).unwrap_or(&EqTarget::Low)
    }

    /// Whether this is one of the three registers rather than the mix.
    pub fn register(self) -> Option<usize> {
        match self {
            EqTarget::Low => Some(0),
            EqTarget::Mid => Some(1),
            EqTarget::High => Some(2),
            EqTarget::Master => None,
        }
    }
}

// -----------------------------------------------------------------------------
// The curve
// -----------------------------------------------------------------------------

/// Thirteen gains in decibels, one per band.
///
/// Serialised as a bare array — `eq = [0.0, -3.0, ...]` — because that is how a
/// player reads a curve, and because thirteen named keys per placement would bury
/// the rest of the file. See [`EqCurve::is_flat`] for why the neutral value is
/// spelled exactly `0.0`.
#[derive(Copy, Clone, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct EqCurve {
    pub gains: [f32; EQ_BANDS],
}

/// Read the array, whatever length it is.
///
/// A curve whose length is not the current [`EQ_BANDS`] is read as **flat**
/// rather than stretched or truncated into the ladder. Both alternatives move
/// every band: the ladder grows at the *bottom*, so padding a twelve-band curve
/// at the end would shift the whole shape one band up and quietly change the
/// sound of an ensemble somebody had already saved. A curve written against a
/// different band layout has no honest reading here, and doing nothing is the
/// only one that cannot be wrong.
impl<'de> Deserialize<'de> for EqCurve {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = Vec::<f32>::deserialize(deserializer)?;
        if raw.len() != EQ_BANDS {
            return Ok(EqCurve::flat());
        }
        Ok(EqCurve {
            gains: std::array::from_fn(|i| raw[i]),
        })
    }
}

impl EqCurve {
    /// No band moved: the state every shipped ensemble is in.
    pub fn flat() -> Self {
        EqCurve {
            gains: [0.0; EQ_BANDS],
        }
    }

    /// Whether this curve would change the sound at all.
    ///
    /// An exact comparison against zero on purpose. Every step the panel takes
    /// is a multiple of half a decibel, and every preset is written in halves,
    /// so gains add and subtract exactly and a band taken back to zero is
    /// *exactly* zero. That is what makes "return to flat" a true bypass rather
    /// than a filter that is very nearly doing nothing.
    pub fn is_flat(&self) -> bool {
        self.gains.iter().all(|g| *g == 0.0)
    }

    /// The same curve with every band pulled inside [`GAIN_RANGE`].
    ///
    /// A hand-edited file can say anything; nothing downstream should have to
    /// cope with a hundred decibels of boost.
    pub fn clamped(&self) -> Self {
        EqCurve {
            gains: std::array::from_fn(|i| self.gains[i].clamp(GAIN_RANGE.0, GAIN_RANGE.1)),
        }
    }

    pub fn band(&self, band: usize) -> f32 {
        self.gains.get(band).copied().unwrap_or(0.0)
    }
}

impl Default for EqCurve {
    fn default() -> Self {
        EqCurve::flat()
    }
}

// -----------------------------------------------------------------------------
// The filter
// -----------------------------------------------------------------------------

/// One biquad, normalised so that `a0` is 1.
///
/// The three recipes are the RBJ audio cookbook's, which is the description
/// every other implementation is checked against.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Section {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Section {
    /// Passes the signal through untouched.
    ///
    /// Used for a band sitting at zero, and for a whole bank that is bypassed:
    /// an exact identity rather than a filter whose coefficients happen to be
    /// close to it.
    const IDENTITY: Section = Section {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn is_identity(&self) -> bool {
        *self == Section::IDENTITY
    }

    /// A bell: `db` added at `f0` and nothing anywhere else.
    fn peaking(sample_rate: f32, f0: f32, q: f32, db: f32) -> Self {
        let a = gain_a(db);
        let (cos_w0, alpha) = shape(sample_rate, f0, q);
        let a2 = 1.0 + alpha / a;
        normalize(
            [
                1.0 + alpha * a,
                -2.0 * cos_w0,
                1.0 - alpha * a,
                -2.0 * cos_w0,
                1.0 - alpha / a,
            ],
            a2,
        )
    }

    /// `db` added everywhere below `f0`, tapering to nothing above it.
    fn low_shelf(sample_rate: f32, f0: f32, q: f32, db: f32) -> Self {
        let a = gain_a(db);
        let (cos_w0, alpha) = shape(sample_rate, f0, q);
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        let a2 = (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
        normalize(
            [
                a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0),
                a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
                -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0),
                (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
            ],
            a2,
        )
    }

    /// `db` added everywhere above `f0`, tapering to nothing below it.
    fn high_shelf(sample_rate: f32, f0: f32, q: f32, db: f32) -> Self {
        let a = gain_a(db);
        let (cos_w0, alpha) = shape(sample_rate, f0, q);
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        let a2 = (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
        normalize(
            [
                a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0),
                a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
                2.0 * ((a - 1.0) - (a + 1.0) * cos_w0),
                (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
            ],
            a2,
        )
    }

    /// A bell with no gain: passes one band and rejects the rest.
    ///
    /// The constant-**peak**-gain form of the RBJ bandpass, so `|H|` is exactly
    /// 1 at `f0` whatever `q` is. That is the property an analyser needs: a
    /// full-scale sine at a band's centre has to read as full scale, or every
    /// reading is scaled by a bandwidth nobody chose. Three multiplies a sample,
    /// because `b1` is zero and `b2` is `-b0`.
    ///
    /// Shared with the equaliser's bells deliberately: one biquad implementation
    /// in the crate, measured and stabilised once, rather than a second one in
    /// the analyser that drifts from it.
    pub(crate) fn bandpass(sample_rate: f32, f0: f32, q: f32) -> Self {
        let (cos_w0, alpha) = shape(sample_rate, f0, q);
        normalize(
            [alpha, 0.0, -alpha, -2.0 * cos_w0, 1.0 - alpha],
            1.0 + alpha,
        )
    }

    /// One sample, direct form II transposed.
    ///
    /// Transposed because the state it keeps is the one that stays well scaled
    /// when the coefficients move — which they do, on every arrow press.
    pub(crate) fn tick(&self, state: &mut BiquadState, x: f32) -> f32 {
        let y = self.b0 * x + state.z1;
        state.z1 = self.b1 * x - self.a1 * y + state.z2;
        state.z2 = self.b2 * x - self.a2 * y;
        y
    }

    /// `|H|` at `freq`, from the coefficients rather than from the audio.
    ///
    /// What the tests check the response with: an analytic answer needs no
    /// settling time, so a shelf can be measured down at 1 Hz where no
    /// affordable amount of audio would give a clean RMS.
    #[cfg(test)]
    fn magnitude_at(&self, freq: f32, sample_rate: f32) -> f32 {
        let w = 2.0 * PI * freq / sample_rate;
        let (c1, s1) = (w.cos(), w.sin());
        let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
        let num_re = self.b0 + self.b1 * c1 + self.b2 * c2;
        let num_im = self.b1 * s1 + self.b2 * s2;
        let den_re = 1.0 + self.a1 * c1 + self.a2 * c2;
        let den_im = self.a1 * s1 + self.a2 * s2;
        let num = (num_re * num_re + num_im * num_im).sqrt();
        let den = (den_re * den_re + den_im * den_im).sqrt();
        if den == 0.0 {
            f32::INFINITY
        } else {
            num / den
        }
    }
}

/// The two numbers every RBJ recipe is built from: `cos(w0)` and `alpha`.
///
/// `q` is folded into `alpha` the usual way. The corner is clamped below
/// Nyquist, which also keeps `w0` away from `pi` and so keeps `alpha` away from
/// zero — see [`MAX_CORNER_FRACTION`].
fn shape(sample_rate: f32, f0: f32, q: f32) -> (f32, f32) {
    let nyquist_limit = sample_rate * MAX_CORNER_FRACTION;
    let f0 = f0.clamp(10.0, nyquist_limit.max(10.0));
    let w0 = 2.0 * PI * f0 / sample_rate;
    (w0.cos(), w0.sin() / (2.0 * q.max(0.01)))
}

/// The shelf-and-bell recipes' amplitude term, from a gain in decibels.
fn gain_a(db: f32) -> f32 {
    10.0f32.powf(db / 40.0)
}

/// Divide a raw `[b0, b1, b2, a1, a2]` by `a0`.
fn normalize(raw: [f32; 5], a0: f32) -> Section {
    Section {
        b0: raw[0] / a0,
        b1: raw[1] / a0,
        b2: raw[2] / a0,
        a1: raw[3] / a0,
        a2: raw[4] / a0,
    }
}

/// The running memory of one biquad.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(crate) struct BiquadState {
    z1: f32,
    z2: f32,
}

/// The running memory of one thirteen-band path.
///
/// Separate from [`Eq`] because the master curve designs its coefficients once
/// and then shapes two signals with them: left and right need the same filter
/// and their own history, and sharing one history would fold the stereo pair
/// together.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct EqState {
    sections: [BiquadState; EQ_BANDS],
}

/// One curve's worth of coefficients, and whether it is bypassed.
#[derive(Clone, Debug)]
pub struct Eq {
    sample_rate: f32,
    sections: [Section; EQ_BANDS],
    curve: EqCurve,
    flat: bool,
}

impl Eq {
    /// A bypassed bank for a stream running at `sample_rate`.
    pub fn new(sample_rate: f32) -> Self {
        Eq {
            sample_rate,
            sections: [Section::IDENTITY; EQ_BANDS],
            curve: EqCurve::flat(),
            flat: true,
        }
    }

    /// Whether the last curve set on this bank was flat.
    ///
    /// An observation for the tests; the callback reads [`Eq::tick`] and does
    /// not need to ask.
    #[cfg(test)]
    pub fn flat(&self) -> bool {
        self.flat
    }

    /// The curve currently designed in.
    #[cfg(test)]
    pub fn curve(&self) -> EqCurve {
        self.curve
    }

    /// Design the thirteen sections for `curve`.
    ///
    /// Called once per audio buffer and does nothing at all when the curve has
    /// not moved, so the cost of having an equaliser is one float comparison per
    /// band per buffer until somebody turns a knob.
    ///
    /// Changing coefficients while a signal is running leaves the filter memory
    /// where it was, which is what a hardware EQ does too: the transient is the
    /// size of the signal already in flight, not a click. A bank that goes flat
    /// is not run and does not clear its memory, so the one case where the stale
    /// value could be old rather than merely recent is a curve dialled back to
    /// flat and then moved again.
    pub fn set(&mut self, curve: &EqCurve) {
        let curve = curve.clamped();
        if self.curve == curve {
            return;
        }
        self.curve = curve;
        self.flat = curve.is_flat();
        if self.flat {
            self.sections = [Section::IDENTITY; EQ_BANDS];
            return;
        }
        self.sections = std::array::from_fn(|i| {
            let db = curve.gains[i];
            if db == 0.0 {
                return Section::IDENTITY;
            }
            let f0 = BANDS[i];
            match i {
                0 => Section::low_shelf(self.sample_rate, f0, SHELF_Q, db),
                last if last == EQ_BANDS - 1 => {
                    Section::high_shelf(self.sample_rate, f0, SHELF_Q, db)
                }
                _ => Section::peaking(self.sample_rate, f0, PEAK_Q, db),
            }
        });
    }

    /// One sample through the cascade.
    ///
    /// A flat bank returns its input unchanged, exactly and without touching
    /// `state` — which is why a shipped ensemble sounds identical to the way it
    /// sounded before any of this existed.
    pub fn tick(&self, state: &mut EqState, x: f32) -> f32 {
        if self.flat {
            return x;
        }
        let mut y = x;
        for (section, memory) in self.sections.iter().zip(state.sections.iter_mut()) {
            if section.is_identity() {
                continue;
            }
            y = section.tick(memory, y);
        }
        y
    }

    /// The gain this bank applies at `freq`, as a linear factor.
    ///
    /// The product of the sections' responses, for anything that wants to draw
    /// a curve without running audio.
    #[cfg(test)]
    pub fn magnitude_at(&self, freq: f32) -> f32 {
        self.sections.iter().fold(1.0f32, |acc, s| {
            acc * s.magnitude_at(freq, self.sample_rate)
        })
    }

    /// The same, in decibels.
    #[cfg(test)]
    pub fn gain_db_at(&self, freq: f32) -> f32 {
        20.0 * self.magnitude_at(freq).max(1e-9).log10()
    }
}

impl Default for Eq {
    fn default() -> Self {
        Eq::new(44_100.0)
    }
}

// -----------------------------------------------------------------------------
// The preset library
// -----------------------------------------------------------------------------

/// One named curve.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EqPreset {
    pub name: String,
    /// A bare array in the file, because [`EqCurve`] is transparent. Missing
    /// entirely, a preset is flat rather than a parse error — the same reading
    /// [`EqCurve`] gives an array of the wrong length.
    #[serde(default)]
    pub gains: EqCurve,
}

/// The shipped curves, compiled in from the tracked file.
///
/// Panics only on a malformed file, which is a repository bug that
/// `every_shipped_preset_parses` catches before it can be committed.
pub fn builtin_presets() -> Vec<EqPreset> {
    from_toml(include_str!("../eq_presets.toml")).expect("eq_presets.toml is valid TOML")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct EqPresetFile {
    presets: Vec<EqPreset>,
}

/// The curve library: the shipped presets with the user's own layered over them.
#[derive(Clone, Debug, Default)]
pub struct EqPresetStore {
    /// What the panel offers, in order.
    pub presets: Vec<EqPreset>,
    /// Just the user's own entries. This is what [`Self::save`] writes.
    user: Vec<EqPreset>,
}

impl EqPresetStore {
    /// Load the shipped library and the user's own file, creating the latter if
    /// missing.
    pub fn load(user_path: &Path) -> io::Result<Self> {
        let mut store = EqPresetStore {
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
    pub fn add(&mut self, preset: EqPreset) {
        upsert(&mut self.presets, preset.clone());
        upsert(&mut self.user, preset);
    }

    /// An observation for the tests; the panel works by curve rather than by
    /// name, because the row's value is derived rather than remembered.
    #[cfg(test)]
    pub fn find(&self, name: &str) -> Option<&EqPreset> {
        self.presets.iter().find(|p| p.name == name)
    }

    /// Where `name` sits in the list, for a row that opens on it.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.presets.iter().position(|p| p.name == name)
    }

    /// Step `delta` places through the library, wrapping.
    pub fn step(&self, from: Option<usize>, delta: i32) -> Option<(usize, &EqPreset)> {
        let len = self.presets.len();
        if len == 0 {
            return None;
        }
        let index = match from {
            Some(i) => (i as i32 + delta).rem_euclid(len as i32) as usize,
            None if delta >= 0 => 0,
            None => len - 1,
        };
        Some((index, &self.presets[index]))
    }

    /// The name of the preset this curve is, if it is one.
    ///
    /// Derived rather than remembered, the same way the Synth panel works out
    /// which instrument a register is: a curve that matches a library entry says
    /// so, and anything else says `custom`, so the row can never claim a curve
    /// that is not on the screen.
    pub fn name_for_curve(&self, curve: &EqCurve) -> Option<&str> {
        self.presets
            .iter()
            .find(|p| p.gains == *curve)
            .map(|p| p.name.as_str())
    }

    #[cfg(test)]
    pub fn with_builtins() -> Self {
        EqPresetStore {
            presets: builtin_presets(),
            user: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn from_presets(presets: Vec<EqPreset>) -> Self {
        EqPresetStore {
            presets: presets.clone(),
            user: presets,
        }
    }
}

fn upsert(presets: &mut Vec<EqPreset>, preset: EqPreset) {
    match presets.iter_mut().find(|p| p.name == preset.name) {
        Some(existing) => *existing = preset,
        None => presets.push(preset),
    }
}

fn read(path: &Path) -> io::Result<Vec<EqPreset>> {
    let text = fs::read_to_string(path)?;
    from_toml(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

/// Parse a preset document. Unknown keys are ignored, so a file written by a
/// different build still loads.
pub fn from_toml(text: &str) -> Result<Vec<EqPreset>, toml::de::Error> {
    let file: EqPresetFile = toml::from_str(text)?;
    Ok(file.presets)
}

pub fn to_toml(presets: &[EqPreset]) -> Result<String, toml::ser::Error> {
    toml::to_string_pretty(&EqPresetFile {
        presets: presets.to_vec(),
    })
}

pub fn user_path() -> PathBuf {
    PathBuf::from("eq_presets.user.toml")
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 44_100.0;

    fn one_band(band: usize, db: f32) -> EqCurve {
        let mut curve = EqCurve::flat();
        curve.gains[band] = db;
        curve
    }

    fn bank(curve: &EqCurve) -> Eq {
        let mut eq = Eq::new(SR);
        eq.set(curve);
        eq
    }

    /// Every band at the same gain: the worst case a file can ask for.
    fn all_bands(db: f32) -> EqCurve {
        EqCurve {
            gains: [db; EQ_BANDS],
        }
    }

    /// The gain a signal at `freq` comes out with, measured on the audio path.
    ///
    /// Run for a settling stretch first, then measured over a whole number of
    /// cycles, so the answer can be compared against the analytic one.
    fn measured_gain(eq: &Eq, freq: f32, cycles: usize) -> f32 {
        let period = (SR / freq).round() as usize;
        let n = period * cycles;
        let mut state = EqState::default();
        let sample = |i: usize| (2.0 * PI * freq * i as f32 / SR).sin();
        for i in 0..n {
            let _ = eq.tick(&mut state, sample(i));
        }
        let mut sum = 0.0;
        for i in n..(n * 2) {
            let y = eq.tick(&mut state, sample(i));
            sum += y * y;
        }
        // The input is a unit sine, whose RMS is 1/sqrt(2).
        (sum / n as f32).sqrt() * 2.0f32.sqrt()
    }

    // ---- bypass ----

    #[test]
    fn a_flat_curve_is_bypassed_exactly() {
        let eq = Eq::new(SR);
        assert!(eq.flat());
        let mut state = EqState::default();
        for i in 0..1_000 {
            // Deliberately awkward values: the point is bit equality, not
            // closeness.
            let x = (i as f32 * 0.37).sin() * 12.5 - 3.25;
            assert_eq!(eq.tick(&mut state, x), x);
        }
        // And nothing was written into the filter memory.
        assert_eq!(state, EqState::default());
    }

    #[test]
    fn taking_a_curve_back_to_flat_restores_the_bypass() {
        // The panel steps in halves of a decibel, so a band taken back to zero
        // has to land on *exactly* zero or the filter keeps running.
        let mut eq = Eq::new(SR);
        for step in [-1, -1, -1, -1, -1, -1] {
            let mut curve = eq.curve();
            curve.gains[4] += step as f32 * 0.5;
            eq.set(&curve);
        }
        assert_eq!(eq.curve().gains[4], -3.0);
        assert!(!eq.flat());
        for _ in 0..6 {
            let mut curve = eq.curve();
            curve.gains[4] += 0.5;
            eq.set(&curve);
        }
        assert_eq!(eq.curve().gains[4], 0.0);
        assert!(eq.flat(), "six half-decibel steps must land back on zero");
        assert_eq!(eq.magnitude_at(440.0), 1.0);
    }

    #[test]
    fn a_curve_is_flat_only_when_every_band_is_zero() {
        assert!(EqCurve::flat().is_flat());
        assert!(!one_band(0, 0.5).is_flat());
        assert!(!one_band(EQ_BANDS - 1, -0.5).is_flat());
        assert_eq!(
            one_band(3, 3.0).gains.iter().filter(|g| **g != 0.0).count(),
            1
        );
        assert_eq!(
            EqCurve::flat().gains.iter().filter(|g| **g != 0.0).count(),
            0
        );
    }

    #[test]
    fn a_band_at_zero_is_an_exact_identity() {
        // A curve with one live band still runs the cascade, so the other twelve
        // have to be honest identities rather than near-passes.
        let eq = bank(&one_band(0, 6.0));
        let mut state = EqState::default();
        for i in 0..500 {
            let x = (i as f32 * 0.11).cos();
            let y = eq.tick(&mut state, x);
            assert!(y.is_finite());
        }
        // The bands that are neither the live one nor a shelf are identity
        // sections, and that is what `is_identity` is for.
        assert_eq!(eq.sections[5], Section::IDENTITY);
        assert_ne!(eq.sections[0], Section::IDENTITY);
    }

    // ---- shape ----

    #[test]
    fn a_low_shelf_lifts_the_bottom_and_leaves_the_top() {
        let eq = bank(&one_band(0, 12.0));
        // Well below the 20 Hz corner: the shelf's plateau is the full 12 dB.
        assert!(
            (eq.gain_db_at(2.0) - 12.0).abs() < 0.5,
            "at 2 Hz: {} dB",
            eq.gain_db_at(2.0)
        );
        // Well above it: untouched.
        assert!(
            eq.gain_db_at(8000.0).abs() < 0.5,
            "at 8 kHz: {} dB",
            eq.gain_db_at(8000.0)
        );
        // And the corner itself is the half-gain point, which is what makes it
        // the corner.
        assert!(
            (eq.gain_db_at(BANDS[0]) - 6.0).abs() < 1.0,
            "at the corner: {} dB",
            eq.gain_db_at(BANDS[0])
        );
    }

    #[test]
    fn a_high_shelf_lifts_the_top_and_leaves_the_bottom() {
        let eq = bank(&one_band(EQ_BANDS - 1, 12.0));
        // The corner is the half-gain point, which on a 44.1 kHz device is as
        // far as this band gets: there is under half an octave above 16 kHz.
        assert!(
            (eq.gain_db_at(16_000.0) - 6.0).abs() < 1.0,
            "at the corner: {} dB",
            eq.gain_db_at(16_000.0)
        );
        assert!(
            eq.gain_db_at(20_000.0) > eq.gain_db_at(16_000.0),
            "it must still be rising above the corner"
        );
        assert!(
            eq.gain_db_at(200.0).abs() < 0.5,
            "at 200 Hz: {} dB",
            eq.gain_db_at(200.0)
        );
    }

    #[test]
    fn a_bell_moves_its_own_band_and_very_little_else() {
        // 1 kHz: the middle of the ladder, so both neighbours are a full octave
        // away and two octaves down is still a bell rather than a shelf.
        let band = 7;
        assert_eq!(BANDS[band], 1000.0);
        let eq = bank(&one_band(band, 12.0));
        assert!(
            (eq.gain_db_at(BANDS[band]) - 12.0).abs() < 0.2,
            "at its own centre: {} dB",
            eq.gain_db_at(BANDS[band])
        );
        // An octave either side of a 1.414-Q bell is a few decibels, not twelve.
        for neighbour in [BANDS[band - 1], BANDS[band + 1]] {
            let db = eq.gain_db_at(neighbour);
            assert!(
                db > 1.0 && db < 8.0,
                "an octave away should be a partial lift, got {} dB",
                db
            );
        }
        // Two octaves down is essentially untouched.
        let two_octaves_down = BANDS[band - 2];
        assert!(
            eq.gain_db_at(two_octaves_down).abs() < 2.0,
            "two octaves down ({} Hz): {} dB",
            two_octaves_down,
            eq.gain_db_at(two_octaves_down)
        );
    }

    #[test]
    fn a_cut_is_the_mirror_of_a_boost() {
        for band in [0, 5, EQ_BANDS - 1] {
            let up = bank(&one_band(band, 9.0)).gain_db_at(BANDS[band]);
            let down = bank(&one_band(band, -9.0)).gain_db_at(BANDS[band]);
            assert!(
                (up + down).abs() < 0.3,
                "band {}: +{} against {}",
                band,
                up,
                down
            );
        }
    }

    #[test]
    fn the_cascade_agrees_with_the_coefficients() {
        // The same answer from the audio path and from the algebra, which is
        // what proves `tick` is running the filter the coefficients describe.
        let curve = EqCurve {
            gains: [
                6.0, -3.0, 0.0, 4.5, -6.0, 2.0, 0.0, -1.5, 3.0, 0.0, -4.5, 6.0, -3.0,
            ],
        };
        let eq = bank(&curve);
        for freq in [100.0, 440.0, 1000.0, 3000.0] {
            let analytic = eq.magnitude_at(freq);
            let measured = measured_gain(&eq, freq, 400);
            assert!(
                (measured / analytic - 1.0).abs() < 0.02,
                "at {} Hz: analytic {} against measured {}",
                freq,
                analytic,
                measured
            );
        }
    }

    #[test]
    fn the_audio_path_really_does_lift_and_leave_alone() {
        // The end-to-end version, run on audio rather than on the coefficients.
        // A bell reaches its full gain at its own centre, so this is the one
        // place where the answer is exactly the number on the knob.
        let bell = bank(&one_band(7, 12.0));
        let on_centre = measured_gain(&bell, 1000.0, 400);
        assert!(
            (on_centre / 10.0f32.powf(12.0 / 20.0) - 1.0).abs() < 0.02,
            "1 kHz came out at {}x, not 12 dB up",
            on_centre
        );
        let far_away = measured_gain(&bell, 8000.0, 400);
        assert!(
            (far_away - 1.0).abs() < 0.05,
            "8 kHz came out at {}x",
            far_away
        );

        // A shelf's plateau is *below* its corner, not at it: 8 Hz is well under
        // the 20 Hz corner and is where the boost lives. 15 Hz would not do — it
        // is only two thirds of an octave down, which is the *corner*, not the
        // plateau.
        let shelf = bank(&one_band(0, 12.0));
        let plateau = measured_gain(&shelf, 8.0, 40);
        assert!(plateau > 2.5, "8 Hz came out at {}x", plateau);
        let above = measured_gain(&shelf, 6000.0, 400);
        assert!((above - 1.0).abs() < 0.05, "6 kHz came out at {}x", above);
    }

    // ---- stability ----

    #[test]
    fn every_band_at_both_extremes_stays_finite() {
        // A filter whose poles land on the unit circle grows instead of
        // decaying, and the failure is inaudible for a second and then not. Each
        // band is driven with noise, which excites every mode including the ones
        // a sine would miss.
        let mut seed = 0x1234_5678u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / 8_388_608.0 - 1.0
        };
        for band in 0..EQ_BANDS {
            for db in [12.0, -12.0] {
                let eq = bank(&one_band(band, db));
                let mut state = EqState::default();
                let mut peak = 0.0f32;
                for _ in 0..20_000 {
                    let y = eq.tick(&mut state, noise() * 0.5);
                    assert!(y.is_finite(), "band {} at {} produced {}", band, db, y);
                    peak = peak.max(y.abs());
                }
                assert!(
                    peak < 20.0,
                    "band {} at {} peaked at {} — that is not a filter",
                    band,
                    db,
                    peak
                );
            }
        }
    }

    #[test]
    fn every_band_at_once_stays_finite() {
        // The worst case a hand-edited file can ask for, and the one the corner
        // clamp exists for: all thirteen at the top of the range.
        for db in [12.0, -12.0] {
            let eq = bank(&all_bands(db));
            let mut state = EqState::default();
            let mut peak = 0.0f32;
            for i in 0..20_000 {
                let x = (i as f32 * 0.07).sin() * 0.5;
                let y = eq.tick(&mut state, x);
                assert!(y.is_finite(), "an all-band curve at {} produced {}", db, y);
                peak = peak.max(y.abs());
            }
            assert!(peak < 50.0, "all bands at {} peaked at {}", db, peak);
        }
    }

    #[test]
    fn a_low_rate_device_does_not_put_a_pole_on_nyquist() {
        // At 32 kHz the 16 kHz band is at Nyquist, where `sin(w0)` is zero and a
        // bell degenerates. The clamp is what keeps this a filter.
        let mut eq = Eq::new(32_000.0);
        eq.set(&one_band(EQ_BANDS - 1, 12.0));
        let mut state = EqState::default();
        let mut peak = 0.0f32;
        for i in 0..20_000 {
            let x = (i as f32 * 0.31).sin() * 0.5;
            let y = eq.tick(&mut state, x);
            assert!(y.is_finite());
            peak = peak.max(y.abs());
        }
        assert!(peak < 20.0, "peaked at {}", peak);
    }

    #[test]
    fn an_absurd_gain_from_a_hand_edited_file_is_clamped() {
        let clamped = all_bands(400.0).clamped();
        assert!(clamped.gains.iter().all(|g| *g == GAIN_RANGE.1));
        // And the bank still behaves rather than exploding.
        let eq = bank(&all_bands(400.0));
        assert_eq!(eq.curve().gains[0], GAIN_RANGE.1);
    }

    // ---- the library ----

    #[test]
    fn every_shipped_preset_parses() {
        let presets = builtin_presets();
        assert!(presets.len() >= 20, "only {} eq presets", presets.len());
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
    fn no_two_presets_are_the_same_curve() {
        // Two names for one curve makes the row lie about how much choice there
        // is — the same rule the instrument library is held to.
        let presets = builtin_presets();
        for i in 0..presets.len() {
            for j in (i + 1)..presets.len() {
                assert_ne!(
                    presets[i].gains, presets[j].gains,
                    "{} and {} are the same curve",
                    presets[i].name, presets[j].name
                );
            }
        }
    }

    #[test]
    fn every_shipped_gain_is_inside_the_range_and_on_a_half_decibel() {
        // Half-decibel steps are what make the panel's arithmetic exact, and
        // exactness is what makes returning a band to zero a true bypass.
        for preset in builtin_presets() {
            for (band, gain) in preset.gains.gains.iter().enumerate() {
                assert!(
                    (GAIN_RANGE.0..=GAIN_RANGE.1).contains(gain),
                    "{} band {} is {} dB",
                    preset.name,
                    BAND_LABELS[band],
                    gain
                );
                assert_eq!(
                    gain * 2.0,
                    (gain * 2.0).round(),
                    "{} band {} is {} dB, not a half decibel",
                    preset.name,
                    BAND_LABELS[band],
                    gain
                );
            }
        }
    }

    #[test]
    fn only_flat_is_flat() {
        // `Flat` is the reset the panel cycles to, and it has to be the only
        // curve that bypasses — otherwise the row would name a curve that is
        // doing something as "Flat".
        let presets = builtin_presets();
        for preset in &presets {
            assert_eq!(
                preset.gains.is_flat(),
                preset.name == FLAT_LABEL,
                "{} is flat: {}",
                preset.name,
                preset.gains.is_flat()
            );
        }
        assert!(presets.iter().any(|p| p.name == FLAT_LABEL));
    }

    #[test]
    fn a_curve_can_be_found_by_name_and_a_name_by_curve() {
        let store = EqPresetStore::with_builtins();
        for preset in &store.presets {
            assert_eq!(store.find(&preset.name), Some(preset));
            assert_eq!(
                store.name_for_curve(&preset.gains),
                Some(preset.name.as_str())
            );
        }
        assert_eq!(store.find("Nonexistent"), None);
        let mut custom = EqCurve::flat();
        custom.gains[0] = 0.5;
        assert_eq!(store.name_for_curve(&custom), None);
    }

    #[test]
    fn stepping_walks_the_library_and_wraps() {
        let store = EqPresetStore::with_builtins();
        let len = store.presets.len();
        assert_eq!(store.step(None, 1).unwrap().0, 0);
        assert_eq!(store.step(None, -1).unwrap().0, len - 1);
        assert_eq!(store.step(Some(0), -1).unwrap().0, len - 1);
        assert_eq!(store.step(Some(len - 1), 1).unwrap().0, 0);
        let (index, preset) = store.step(Some(0), 1000).unwrap();
        assert!(index < len);
        assert!(!preset.name.is_empty());
    }

    #[test]
    fn stepping_an_empty_library_is_nothing_rather_than_a_panic() {
        let store = EqPresetStore::from_presets(Vec::new());
        assert!(store.step(None, 1).is_none());
        assert!(store.step(Some(0), 1).is_none());
    }

    #[test]
    fn presets_round_trip_through_toml() {
        let store = EqPresetStore::with_builtins();
        let text = to_toml(&store.presets).unwrap();
        assert_eq!(from_toml(&text).unwrap(), store.presets);
        // And the file really is one readable array per curve.
        assert!(text.contains("gains = ["));
    }

    #[test]
    fn load_creates_the_user_file() {
        let user = temp_path("eq-preset-user");
        let _ = fs::remove_file(&user);
        let store = EqPresetStore::load(&user).unwrap();
        assert!(user.exists(), "the user's file is created to be edited");
        assert_eq!(store.presets.len(), builtin_presets().len());
        let _ = fs::remove_file(&user);
    }

    #[test]
    fn a_saved_preset_shadows_the_shipped_one_by_name() {
        let user = temp_path("eq-preset-shadow");
        let _ = fs::remove_file(&user);

        let mut store = EqPresetStore::load(&user).unwrap();
        let mut mine = builtin_presets()[0].clone();
        let name = mine.name.clone();
        mine.gains.gains[3] = 7.5;
        store.add(mine);
        store.save(&user).unwrap();

        let again = EqPresetStore::load(&user).unwrap();
        assert_eq!(again.presets.len(), builtin_presets().len());
        assert_eq!(again.find(&name).unwrap().gains.gains[3], 7.5);
        let _ = fs::remove_file(&user);
    }

    /// A document that holds one curve.
    ///
    /// A bare array is not a TOML document, so a curve has to be read where it
    /// actually lives — as a placement's or a preset's value — to be tested at
    /// all.
    #[derive(Serialize, Deserialize)]
    struct CurveHolder {
        #[serde(default)]
        gains: EqCurve,
    }

    fn read_curve(array: &str) -> EqCurve {
        let text = format!("gains = {}", array);
        toml::from_str::<CurveHolder>(&text).unwrap().gains
    }

    #[test]
    fn a_curve_of_the_wrong_length_is_read_as_flat() {
        // The ladder has changed length before, and it can again: the low shelf
        // moved from 31.5 Hz to 20 Hz when the thirteenth band was added. A
        // curve written against the old layout has no honest reading — padding
        // it at the end would shift every band up one — so it does nothing
        // instead of quietly becoming a different curve.
        let twelve = "[0.0, -6.0, 3.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]";
        assert!(
            read_curve(twelve).is_flat(),
            "a twelve-band curve must not be stretched"
        );
        assert!(read_curve("[1.0, 2.0]").is_flat(), "nor a short one");
        assert!(read_curve("[]").is_flat(), "nor an empty one");

        // The right length still reads exactly.
        let mut text = String::from("[");
        for i in 0..EQ_BANDS {
            if i > 0 {
                text.push_str(", ");
            }
            text.push_str(&format!("{:.1}", i as f32 * 0.5));
        }
        text.push(']');
        let curve = read_curve(&text);
        assert_eq!(curve.gains[3], 1.5);
        assert!(!curve.is_flat());
    }

    #[test]
    fn a_curve_written_into_a_placement_is_a_bare_array() {
        // `EqCurve` is transparent, so a curve is spelled as thirteen numbers
        // rather than as a nested table — which is what keeps a placement
        // readable and a preset to two lines.
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct PlacementLike {
            instrument: String,
            #[serde(default, skip_serializing_if = "EqCurve::is_flat")]
            eq: EqCurve,
        }
        let placement = PlacementLike {
            instrument: "Rhodes Dark".to_string(),
            eq: one_band(2, -4.5),
        };
        let text = toml::to_string(&placement).unwrap();
        assert!(
            text.contains("eq = [0.0, 0.0, -4.5, 0.0"),
            "it should be one flat array: {}",
            text
        );
        assert!(!text.contains("[eq]"), "it should not nest: {}", text);
        assert_eq!(toml::from_str::<PlacementLike>(&text).unwrap(), placement);

        // And a flat curve is left out of the file entirely.
        let flat = PlacementLike {
            instrument: "Rhodes Dark".to_string(),
            eq: EqCurve::flat(),
        };
        let text = toml::to_string(&flat).unwrap();
        assert!(!text.contains("eq"), "flat should not be written: {}", text);
        assert_eq!(toml::from_str::<PlacementLike>(&text).unwrap(), flat);

        // A preset is the same shape, and a missing `gains` key is flat rather
        // than a parse error.
        assert!(from_toml("[[presets]]\nname = \"Nothing\"")
            .unwrap()
            .first()
            .unwrap()
            .gains
            .is_flat());
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
