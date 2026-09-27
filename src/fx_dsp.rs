//! The effect algorithms, and the bank of them that the callback runs.
//!
//! Every kind is a method on [`FxState`], which owns the union of the state any
//! of them needs: a delay line, a reverb tank, an allpass chain, a filter, and a
//! handful of envelopes and phases. That union is what lets a slot change its
//! type at runtime without allocating — the memory is all there, and changing the
//! type only changes which parts of it are read.
//!
//! # Cost
//!
//! Twenty states exist: eighteen chain slots (three registers × six) and the two
//! aux units. Each is sized for the **longest** effect rather than for the one it
//! happens to hold, which is the price of a type that can change without the
//! audio thread allocating. Most of that is the two-second delay line, some
//! 380 KB a state — about 7.6 MB in total, allocated once, before the stream
//! starts.
//!
//! A slot holding [`FxKind::None`] costs nothing at all: it is the first arm of
//! the match and it returns its input untouched.
//!
//! # The reverb is the one that had to stay identical
//!
//! `hall` with no pre-delay is the tank this crate shipped before any of this
//! existed, down to the order of the arithmetic in the comb loop. Every shipped
//! ensemble has a reverb level between 0 and 0.62, so the return is not a setting
//! nobody used: `a_shipped_ensemble_still_sounds_the_way_it_did` renders the old
//! tank and the new one side by side and compares the bits.

use crate::fx::{
    division_at, division_millis, Fx, FxKind, FxSubtype, CHAIN_SLOTS, P0, P1, P2, P3, P4, P5,
};

/// The longest delay any effect can ask for, in seconds.
///
/// Two, because a synced delay's longest division is a whole bar and a bar at
/// 120 bpm is two seconds.
pub const MAX_DELAY_SECS: f32 = 2.0;

/// How much longer than the original tank the longest reverb voicing needs.
///
/// `hall` is scale 1 — the tank exactly as it was — and `chamber` is the largest
/// of the others at 1.15. The buffers are allocated at this and the comb's read
/// length moves inside them.
const MAX_REVERB_SCALE: f32 = 1.2;

/// The tank's comb lengths, in samples. Unchanged from the original.
const COMB_DELAYS: [usize; 4] = [1557, 1617, 1491, 1422];
const ALLPASS_DELAYS: [usize; 2] = [225, 556];

/// First-order allpass stages in the phaser's chain.
const PHASER_STAGES: usize = 8;

/// How far each phaser stage's corner sits from the swept centre.
///
/// Eight stages at *the same* corner put all their notches in one narrow band,
/// which is why the first version of this barely moved a 440 Hz tone: the notches
/// swept past each other rather than across the signal. Staggering them over four
/// octaves spreads the notches out, which is what a phaser is — and what the real
/// thing does with its mismatched capacitor tolerances, on purpose.
const PHASER_SPREAD: [f32; PHASER_STAGES] = [0.42, 0.53, 0.67, 0.84, 1.06, 1.33, 1.68, 2.11];

/// How many registers have a chain.
const REGISTERS: usize = 3;

/// Below this a signal is silence, and a signal that is silence is not worth
/// taking a logarithm of.
const FLOOR: f32 = 1.0e-7;

/// Silence a state variable that has decayed past the point of mattering.
///
/// A recursive state with no input left computes `z = z * coef`. Once `z` is
/// small enough that the product rounds back to `z` it stops decaying and parks,
/// for ever, at whatever the last representable value was — and it parks
/// *higher* in the subnormal range, because the representation is coarser there.
/// `where_the_effect_tails_settle` measures it: the flanger, phaser, filter,
/// distortion and wah tails all settle between `1e-45` and `1e-42` twelve
/// seconds after the note is gone, and the whole maximised rack settles at
/// `1.1e-37` and stays there.
///
/// None of that is audible — this threshold is a hundred and forty decibels
/// below full scale, and it is the level the analyser in this same file has
/// treated as silence since before any of the effects existed. What it costs is
/// audible in the only way that matters for a real-time thread: on a CPU that has
/// not been told to flush subnormals, every operation on one is tens to hundreds
/// of times more expensive than the same operation on a normal float, and the
/// effect is loaded for as long as the sound is.
#[inline]
fn settle(x: f32) -> f32 {
    if x.abs() < FLOOR {
        0.0
    } else {
        x
    }
}

// -----------------------------------------------------------------------------
// Primitives
// -----------------------------------------------------------------------------

/// A ring buffer with a fractional read.
///
/// Linear interpolation rather than nearest-neighbour: a chorus sweeps its tap
/// continuously, and stepping between whole samples would put a click in the
/// sweep at exactly the frequencies the effect exists to avoid.
#[derive(Clone)]
struct DelayLine {
    buffer: Vec<f32>,
    index: usize,
}

impl DelayLine {
    fn new(sample_rate: f32) -> Self {
        let len = (MAX_DELAY_SECS * sample_rate).ceil() as usize + 4;
        DelayLine {
            buffer: vec![0.0; len],
            index: 0,
        }
    }

    fn capacity(&self) -> f32 {
        self.buffer.len() as f32 - 2.0
    }

    fn write(&mut self, x: f32) {
        self.buffer[self.index] = settle(x);
        self.index = (self.index + 1) % self.buffer.len();
    }

    /// The sample `delay` samples ago, interpolating between the two nearest.
    fn read(&self, delay: f32) -> f32 {
        let len = self.buffer.len();
        let delay = delay.clamp(1.0, self.capacity());
        let mut pos = self.index as f32 - delay;
        while pos < 0.0 {
            pos += len as f32;
        }
        let base = pos.floor();
        let frac = pos - base;
        let i0 = (base as usize) % len;
        let i1 = (i0 + 1) % len;
        self.buffer[i0] + (self.buffer[i1] - self.buffer[i0]) * frac
    }

    fn clear(&mut self) {
        // Not a loop over the buffer: the tail of a two-second line is silent
        // anyway, and the head is what a new effect would ring with.
        let head = (self.buffer.len() / 8).max(1);
        for sample in &mut self.buffer[..head] {
            *sample = 0.0;
        }
        self.index = 0;
    }
}

/// A one-pole low pass, used as a tone control.
#[derive(Copy, Clone, Default)]
struct OnePole {
    z: f32,
}

impl OnePole {
    fn tick(&mut self, x: f32, coef: f32) -> f32 {
        self.z = settle(self.z + coef * (x - self.z));
        self.z
    }

    fn clear(&mut self) {
        self.z = 0.0;
    }
}

/// A one-pole high pass, as the difference between a signal and its low pass.
#[derive(Copy, Clone, Default)]
struct DcBlock {
    low: OnePole,
}

impl DcBlock {
    /// Removes anything below `hz`, including a constant offset.
    fn tick(&mut self, x: f32, sample_rate: f32, hz: f32) -> f32 {
        let coef = one_pole_coef(hz, sample_rate);
        x - self.low.tick(x, coef)
    }

    fn clear(&mut self) {
        self.low.clear();
    }
}

/// The smoothing coefficient of a one-pole at `hz`.
///
/// The exponential form rather than `2πf/fs`, because the approximation is only
/// good well below Nyquist and a tone control's top end is not.
fn one_pole_coef(hz: f32, sample_rate: f32) -> f32 {
    let x = (-std::f32::consts::TAU * hz.clamp(1.0, sample_rate * 0.49) / sample_rate).exp();
    (1.0 - x).clamp(0.0, 1.0)
}

/// A first-order allpass, the phaser's building block.
///
/// One multiply in the feedback path: `y = -a·x + z`, `z = x + a·y`.
#[derive(Copy, Clone, Default)]
struct Allpass1 {
    z: f32,
}

impl Allpass1 {
    fn tick(&mut self, x: f32, a: f32) -> f32 {
        let y = -a * x + self.z;
        self.z = settle(x + a * y);
        y
    }

    fn clear(&mut self) {
        self.z = 0.0;
    }
}

/// A comb with a damping low pass in its feedback path.
///
/// The read length is a field rather than the buffer's length, so one allocation
/// can serve every reverb voicing. With `length == buffer.len()` this is exactly
/// the comb this crate has always had, arithmetic included.
#[derive(Clone)]
struct Comb {
    buffer: Vec<f32>,
    index: usize,
    length: usize,
    damp_store: f32,
}

impl Comb {
    fn new(delay: usize) -> Self {
        let capacity = (delay as f32 * MAX_REVERB_SCALE).ceil() as usize + 2;
        Comb {
            buffer: vec![0.0; capacity],
            index: 0,
            length: delay,
            damp_store: 0.0,
        }
    }

    fn set_length(&mut self, length: usize) {
        self.length = length.clamp(1, self.buffer.len() - 1);
    }

    fn tick(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let read = (self.index + self.buffer.len() - self.length) % self.buffer.len();
        let output = self.buffer[read];
        self.damp_store = output * (1.0 - damp) + self.damp_store * damp;
        self.buffer[self.index] = input + self.damp_store * feedback;
        self.index = (self.index + 1) % self.buffer.len();
        output
    }

    fn clear(&mut self) {
        for sample in &mut self.buffer {
            *sample = 0.0;
        }
        self.damp_store = 0.0;
        self.index = 0;
    }
}

/// A Schroeder allpass, the reverb's diffuser.
#[derive(Clone)]
struct Allpass {
    buffer: Vec<f32>,
    index: usize,
}

impl Allpass {
    fn new(delay: usize) -> Self {
        Allpass {
            buffer: vec![0.0; delay],
            index: 0,
        }
    }

    fn tick(&mut self, input: f32, feedback: f32) -> f32 {
        let buffered = self.buffer[self.index];
        let output = -input + buffered;
        self.buffer[self.index] = input + buffered * feedback;
        self.index = (self.index + 1) % self.buffer.len();
        output
    }

    fn clear(&mut self) {
        for sample in &mut self.buffer {
            *sample = 0.0;
        }
        self.index = 0;
    }
}

/// One LFO step, as a phase in `0..1` advanced by `rate` hertz.
#[derive(Copy, Clone, Default)]
struct Lfo {
    phase: f32,
}

impl Lfo {
    fn advance(&mut self, rate: f32, sample_rate: f32) -> f32 {
        self.phase = (self.phase + rate.max(0.0) / sample_rate).fract();
        self.phase
    }

    fn clear(&mut self) {
        self.phase = 0.0;
    }
}

fn sine_of(phase: f32) -> f32 {
    (phase * std::f32::consts::TAU).sin()
}

/// A unipolar 0..1 ramp, the shape a tremolo's `ramp` and a gate's `stutter` use.
fn ramp_of(phase: f32) -> f32 {
    phase
}

/// A square with a duty cycle, so `chop` is a square with a short one.
fn square_of(phase: f32, duty: f32) -> f32 {
    if phase < duty.clamp(0.01, 0.99) {
        1.0
    } else {
        -1.0
    }
}

/// The voicing of a reverb subtype: how long the tank's combs are, and how much
/// darker than the setting it is.
///
/// These are *voicings*, not physical models — the same four combs into the same
/// two allpasses at a different length. `hall` is 1.0 and no extra damping, which
/// is what makes it the tank that was already here.
fn voicing(subtype: FxSubtype) -> (f32, f32) {
    match subtype {
        FxSubtype::Room => (0.62, 0.10),
        FxSubtype::Plate => (0.85, -0.08),
        FxSubtype::Chamber => (1.15, 0.05),
        FxSubtype::Ambience => (0.50, 0.20),
        // Hall, and anything else that reaches here.
        _ => (1.0, 0.0),
    }
}

/// Reflect a signal back into `-1..1` as many times as it takes.
///
/// A wavefolder rather than a clipper: a clipper flattens the peaks, and a folder
/// turns them over, which is where the extra harmonics come from.
fn fold(mut x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    // Bounded, because a NaN or a runaway would otherwise loop for ever.
    for _ in 0..8 {
        if x > 1.0 {
            x = 2.0 - x;
        } else if x < -1.0 {
            x = -2.0 - x;
        } else {
            return x;
        }
    }
    x.clamp(-1.0, 1.0)
}

/// Decibels to a linear factor, through `exp2` rather than `powf`.
///
/// The compressor needs this per sample, and `exp2` is a hardware instruction
/// where `powf` is a library call.
fn db_to_gain(db: f32) -> f32 {
    (db * 0.166_096_42).exp2()
}

fn gain_to_db(gain: f32) -> f32 {
    gain.max(FLOOR).log2() * 6.020_6
}

// -----------------------------------------------------------------------------
// One slot's state
// -----------------------------------------------------------------------------

/// Everything one effect slot can need, whichever effect is in it.
pub struct FxState {
    sample_rate: f32,
    /// An aux unit: it returns its wet signal alone, because the mixer's return
    /// level is what decides how much of it is heard. An insert blends dry with
    /// wet through its own `mix` parameter.
    aux: bool,
    /// What is loaded, so a change of type can reset the state it lands in.
    kind: FxKind,
    subtype: FxSubtype,

    line: DelayLine,
    combs: [Comb; 4],
    allpasses: [Allpass; 2],
    lfo: Lfo,
    phaser: [Allpass1; PHASER_STAGES],
    tone: OnePole,
    band: OnePole,
    dc: DcBlock,
    svf_low: f32,
    svf_band: f32,
    env: f32,
    gain: f32,
    hold_sample: f32,
    hold_count: f32,
    feedback: f32,
    wow_phase: f32,
    ring_phase: f32,
    noise: u32,
}

impl FxState {
    /// A state for a stream at `sample_rate`.
    pub fn new(sample_rate: f32, aux: bool) -> Self {
        FxState {
            sample_rate,
            aux,
            kind: FxKind::None,
            subtype: FxSubtype::SubtypeNone,
            line: DelayLine::new(sample_rate),
            combs: [
                Comb::new(COMB_DELAYS[0]),
                Comb::new(COMB_DELAYS[1]),
                Comb::new(COMB_DELAYS[2]),
                Comb::new(COMB_DELAYS[3]),
            ],
            allpasses: [
                Allpass::new(ALLPASS_DELAYS[0]),
                Allpass::new(ALLPASS_DELAYS[1]),
            ],
            lfo: Lfo::default(),
            phaser: [Allpass1::default(); PHASER_STAGES],
            tone: OnePole::default(),
            band: OnePole::default(),
            dc: DcBlock::default(),
            svf_low: 0.0,
            svf_band: 0.0,
            env: 0.0,
            gain: 1.0,
            hold_sample: 0.0,
            hold_count: 0.0,
            feedback: 0.0,
            wow_phase: 0.0,
            ring_phase: 0.0,
            noise: 0x2545_f491,
        }
    }

    /// How many samples of delay this state can ask for.
    #[cfg(test)]
    #[cfg(test)]
    pub fn max_delay(&self) -> f32 {
        self.line.capacity()
    }

    /// Point the state at an effect, resetting anything a different algorithm
    /// would inherit.
    ///
    /// Only a change of *type* resets. Turning a knob on the delay must not clear
    /// its tail — that would turn every adjustment into a cut-off repeat — so
    /// nothing here is keyed on a parameter value.
    fn reconfigure(&mut self, fx: &Fx) {
        self.kind = fx.kind;
        self.subtype = fx.subtype;
        self.line.clear();
        for comb in &mut self.combs {
            comb.clear();
        }
        for allpass in &mut self.allpasses {
            allpass.clear();
        }
        for stage in &mut self.phaser {
            stage.clear();
        }
        self.lfo.clear();
        self.tone.clear();
        self.band.clear();
        self.dc.clear();
        self.svf_low = 0.0;
        self.svf_band = 0.0;
        self.env = 0.0;
        self.gain = 1.0;
        self.hold_sample = 0.0;
        self.hold_count = 0.0;
        self.feedback = 0.0;
        self.wow_phase = 0.0;
        self.ring_phase = 0.0;

        if fx.kind == FxKind::Reverb {
            let (scale, _) = voicing(fx.subtype);
            for (comb, delay) in self.combs.iter_mut().zip(COMB_DELAYS) {
                comb.set_length((delay as f32 * scale).round() as usize);
            }
        }
    }

    /// One sample through the effect in this slot.
    pub fn tick(&mut self, fx: &Fx, tempo: f32, x: f32) -> f32 {
        if fx.kind != self.kind || fx.subtype != self.subtype {
            self.reconfigure(fx);
        }
        match fx.kind {
            // An empty slot is not `return x` by accident: it is the reason six
            // slots a register are affordable at all.
            FxKind::None => x,
            FxKind::Reverb => self.reverb(fx, x),
            FxKind::Delay => self.delay(fx, tempo, x),
            FxKind::Chorus => self.chorus(fx, x),
            FxKind::Flanger => self.flanger(fx, x),
            FxKind::Phaser => self.phaser(fx, x),
            FxKind::Distortion => self.distortion(fx, x),
            FxKind::Fuzz => self.fuzz(fx, x),
            FxKind::Bitcrusher => self.bitcrusher(fx, x),
            FxKind::RingMod => self.ring_mod(fx, x),
            FxKind::Tremolo => self.tremolo(fx, x),
            FxKind::Filter => self.filter(fx, x),
            FxKind::Wah => self.wah(fx, x),
            FxKind::Compressor => self.compressor(fx, x),
            FxKind::Gate => self.gate(fx, x),
        }
    }

    /// The dry/wet blend, or a pure wet return for an aux unit.
    fn blended(&self, x: f32, wet: f32, mix: f32) -> f32 {
        if self.aux {
            wet
        } else {
            let mix = mix.clamp(0.0, 1.0);
            x * (1.0 - mix) + wet * mix
        }
    }

    /// A "tone" parameter, 0 dark to 1 bright, as a one-pole cutoff.
    fn tone_hz(tone: f32) -> f32 {
        200.0 * (20_000.0f32 / 200.0).powf(tone.clamp(0.0, 1.0))
    }

    fn noise(&mut self) -> f32 {
        self.noise ^= self.noise << 13;
        self.noise ^= self.noise >> 17;
        self.noise ^= self.noise << 5;
        (self.noise >> 8) as f32 / 8_388_608.0 - 1.0
    }

    // ---- reverb ----

    /// Four combs into two allpasses, with the length and damping the voicing
    /// asks for.
    ///
    /// With `hall`, no pre-delay and the damping at its default this is bit for
    /// bit the tank that shipped before any of this: the same feedback curve, the
    /// same `out /= 4`, the same `out *= 1 - feedback`, the same two allpasses at
    /// 0.5. Everything added since is behind a parameter that is zero.
    fn reverb(&mut self, fx: &Fx, x: f32) -> f32 {
        let mix = if self.aux { 1.0 } else { fx.param(P3) };
        if mix <= 0.0 {
            return x;
        }
        let size = fx.param(P0).clamp(0.0, 1.0);
        let (_, damp_bias) = voicing(fx.subtype);
        let damp = (fx.param(P1) + damp_bias).clamp(0.0, 1.0);
        let predelay = fx.param(P2).max(0.0);

        let input = if predelay > 0.0 {
            self.line.write(x);
            self.line.read(predelay * 0.001 * self.sample_rate)
        } else {
            x
        };

        let feedback = 0.7 + size * 0.28;
        let mut out = 0.0;
        for comb in &mut self.combs {
            out += comb.tick(input, feedback, damp);
        }
        out /= self.combs.len() as f32;
        out *= 1.0 - feedback;
        for allpass in &mut self.allpasses {
            out = allpass.tick(out, 0.5);
        }
        self.blended(x, out, mix)
    }

    // ---- delay ----

    /// A line, a tone control in its feedback path, and a character.
    fn delay(&mut self, fx: &Fx, tempo: f32, x: f32) -> f32 {
        let mix = if self.aux { 1.0 } else { fx.param(P3) };
        if mix <= 0.0 {
            return x;
        }
        // Milliseconds when it is free, a note value when it is locked. The two
        // are different units and different slots, which is what lets a synced
        // delay actually follow the tempo: a note value is a *duration* that the
        // tempo turns into milliseconds, where a stored number of milliseconds is
        // a duration the tempo cannot touch.
        let millis = if fx.param(P4) > 0.5 {
            let (beats, _) = division_at(fx.param(P5));
            division_millis(beats, tempo)
        } else {
            fx.param(P0)
        };
        let mut samples = (millis * 0.001 * self.sample_rate).clamp(1.0, self.line.capacity());

        let tone = fx.param(P2).clamp(0.0, 1.0);
        let coef = one_pole_coef(Self::tone_hz(tone), self.sample_rate);
        let mut feedback = fx.param(P1).clamp(0.0, 0.95);

        match fx.subtype {
            // One repeat and no more: a slapback is the first reflection, and the
            // point of it is that it does not pile up.
            FxSubtype::Slapback => feedback = feedback.min(0.1),
            // Tape runs at a speed that is never quite constant. The wobble is
            // tiny — a fifth of a percent — because the pitch drift is the
            // character and a chorus is a different effect.
            FxSubtype::Tape => {
                let wow = sine_of(self.wow_phase) * 0.002 + self.noise() * 0.0004;
                samples *= 1.0 + wow;
                self.wow_phase = (self.wow_phase + 0.7 / self.sample_rate).fract();
            }
            _ => {}
        }

        let delayed = self.line.read(samples);
        let toned = self.tone.tick(delayed, coef);
        let mut to_write = x + toned * feedback;
        match fx.subtype {
            // Saturation in the loop is what stops a tape echo getting brighter
            // as it repeats, and what makes it decay rather than merely fade.
            FxSubtype::Tape => to_write = (to_write * 0.8).tanh() * 1.25,
            // A bucket brigade loses both ends: the clock noise is high-passed
            // and the charge transfer is low-passed.
            FxSubtype::Analog => {
                let hp = self
                    .band
                    .tick(to_write, one_pole_coef(180.0, self.sample_rate));
                to_write = (to_write - hp) * 0.9;
                to_write = (to_write * 0.9).tanh() * 1.1;
            }
            _ => {}
        }
        self.line.write(to_write);

        self.blended(x, delayed, mix)
    }

    // ---- chorus, flanger, phaser: the modulated family ----

    /// One modulated tap, in milliseconds.
    fn modulated_tap(&mut self, millis: f32) -> f32 {
        self.line.read((millis * 0.001 * self.sample_rate).max(1.0))
    }

    fn chorus(&mut self, fx: &Fx, x: f32) -> f32 {
        let mix = if self.aux { 1.0 } else { fx.param(P3) };
        if mix <= 0.0 {
            return x;
        }
        let rate = fx.param(P0).max(0.01);
        let depth = fx.param(P1).clamp(0.0, 1.0);
        let spread = fx.param(P2).clamp(0.0, 1.0);
        let phase = self.lfo.advance(rate, self.sample_rate);

        let (base, taps, wet_only, rotary) = match fx.subtype {
            FxSubtype::Ensemble => (18.0, 3, false, false),
            // A vibrato is a chorus with the dry taken away and the delay short
            // enough that the pitch movement is the effect rather than the comb
            // filtering: three milliseconds is a few cycles of delay, not a
            // resonance.
            FxSubtype::Vibrato => (3.0, 1, true, false),
            FxSubtype::Dimension => (8.0, 2, false, false),
            FxSubtype::Rotary => (9.0, 2, false, true),
            _ => (12.0, 1, false, false),
        };
        let base = base + spread * 8.0;
        let swing = 0.6 + depth * 7.0;

        self.line.write(x);
        let mut wet = 0.0;
        for tap in 0..taps {
            let offset = tap as f32 / taps as f32;
            let lfo = sine_of((phase + offset).fract());
            // The dimension's two taps move in opposition, which is what makes it
            // widen rather than warble.
            let sign = if fx.subtype == FxSubtype::Dimension && tap == 1 {
                -1.0
            } else {
                1.0
            };
            wet += self.modulated_tap(base + lfo * swing * sign);
        }
        wet /= taps as f32;

        if rotary {
            // A rotor is a doppler and a tremolo at once, and the tremolo is the
            // half the ear notices: the horn swings past the microphone and the
            // level drops with it.
            let tremolo = 1.0 - depth * 0.5 * (1.0 + sine_of((phase * 0.5).fract())) * 0.5;
            wet *= tremolo;
        }

        if wet_only {
            return wet;
        }
        self.blended(x, wet, mix)
    }

    fn flanger(&mut self, fx: &Fx, x: f32) -> f32 {
        let mix = if self.aux { 1.0 } else { fx.param(P3) };
        if mix <= 0.0 {
            return x;
        }
        let rate = fx.param(P0).max(0.01);
        let depth = fx.param(P1).clamp(0.0, 1.0);
        let feedback = fx.param(P2).clamp(0.0, 0.95);
        let phase = self.lfo.advance(rate, self.sample_rate);

        // The sweep is short — a flanger is a comb filter in the low hundreds of
        // hertz, not a delay.
        let base = 0.4 + depth * 3.0;
        let swing = 0.3 + depth * 4.0;
        let lfo = sine_of(phase);

        self.line.write(x + self.feedback * feedback);
        let wet = match fx.subtype {
            // Two taps whose delays cross through each other, so at the crossing
            // point one of them is at no delay at all. That is the through-zero
            // sound: the notches reach zero hertz and sweep back out.
            FxSubtype::ThruZero => {
                let a = self.modulated_tap(base + swing * 0.5 + lfo * swing * 0.5);
                let b = self.modulated_tap(base + swing * 0.5 - lfo * swing * 0.5);
                (a + b) * 0.5
            }
            _ => self.modulated_tap(base + lfo * swing),
        };
        // The jet is the same filter with the feedback inverted, which moves the
        // notches to where the peaks were.
        let wet = settle(wet);
        self.feedback = if fx.subtype == FxSubtype::Jet {
            -wet
        } else {
            wet
        };
        self.blended(x, wet, mix)
    }

    fn phaser(&mut self, fx: &Fx, x: f32) -> f32 {
        let mix = if self.aux { 1.0 } else { fx.param(P3) };
        if mix <= 0.0 {
            return x;
        }
        let rate = fx.param(P0).max(0.01);
        let depth = fx.param(P1).clamp(0.0, 1.0);
        let feedback = fx.param(P2).clamp(0.0, 0.95);
        let phase = self.lfo.advance(rate, self.sample_rate);

        let stages = if fx.subtype == FxSubtype::Vibe {
            4
        } else {
            PHASER_STAGES
        };
        // A sample-and-hold on the LFO is the whole of the `stepped` variant: the
        // sweep does not glide, it jumps.
        let lfo = if fx.subtype == FxSubtype::Stepped {
            let steps = 6.0;
            let held = (phase * steps).floor() / steps;
            sine_of(held)
        } else {
            sine_of(phase)
        };

        // 200 Hz to 4 kHz, which is where a phaser's notches are audible.
        let sweep = 0.5 + 0.5 * lfo * (0.2 + 0.8 * depth);
        let hz = 200.0 * (20.0f32).powf(sweep.clamp(0.0, 1.0));
        // One tangent, not eight: over this range `tan` is near enough to linear
        // that scaling the tangent is within a percent of scaling the corner, and
        // eight library calls a sample is not a price worth paying for the last
        // percent of a sweep nobody can hear.
        let g = (std::f32::consts::PI * hz / self.sample_rate).tan();

        let mut y = x + self.feedback * feedback;
        for (stage, spread) in self.phaser.iter_mut().zip(PHASER_SPREAD).take(stages) {
            let g = g * spread;
            // `(1 - g) / (1 + g)`, not `(g - 1) / (g + 1)`: the two differ by a
            // sign, and the sign decides which end of the spectrum the pole sits
            // at. The wrong one puts a low corner's pole near Nyquist, so the
            // notches sweep from four to eleven kilohertz — above the signal
            // entirely, and a phaser that measures as doing nothing at all.
            y = stage.tick(y, (1.0 - g) / (1.0 + g));
        }
        let y = settle(y);
        self.feedback = y;
        if fx.subtype == FxSubtype::Vibe {
            return y;
        }
        self.blended(x, y, mix)
    }

    // ---- the shaping family ----

    fn distortion(&mut self, fx: &Fx, x: f32) -> f32 {
        let mix = if self.aux { 1.0 } else { fx.param(P3) };
        if mix <= 0.0 {
            return x;
        }
        let drive = db_to_gain(fx.param(P0));
        let tone = fx.param(P1).clamp(0.0, 1.0);
        let level = db_to_gain(fx.param(P2));

        // Pre-emphasis before the drive and de-emphasis after it, which is what
        // makes an overdrive bite instead of merely getting louder.
        let coef = one_pole_coef(700.0, self.sample_rate);
        let bright = x - self.band.tick(x, coef);
        let pre = match fx.subtype {
            FxSubtype::Overdrive | FxSubtype::Tube => x + bright,
            _ => x,
        };

        let driven = pre * drive;
        let shaped = match fx.subtype {
            FxSubtype::Overdrive => driven.tanh(),
            FxSubtype::Soft => driven.tanh(),
            FxSubtype::Hard => driven.clamp(-1.0, 1.0),
            // Asymmetric: a tube amplifies one half of the wave more than the
            // other, and the difference is the even harmonics that make it sound
            // like an amplifier rather than a clipper.
            FxSubtype::Tube => {
                if driven >= 0.0 {
                    driven.tanh()
                } else {
                    (driven * 0.7).tanh() * 0.85
                }
            }
            FxSubtype::Fold => fold(driven * 0.5),
            FxSubtype::Rectify => {
                // Full-wave rectification, re-centred: the octave-up effect. DC
                // is removed because this tool's filter has unity gain at DC and
                // an offset would ride all the way into the reverb.
                let rectified = driven.abs() * 2.0 - 1.0;
                self.dc.tick(rectified, self.sample_rate, 20.0)
            }
            _ => driven.tanh(),
        };

        // Bounded, whatever the shaper did. A clipper and a folder are already
        // inside the range; the rectifier is not — `|x| * 2 - 1` of a heavily
        // driven signal is as large as the drive made it, and a distortion that
        // can return two hundred times its input is a fault rather than a
        // variant.
        let shaped = shaped.clamp(-1.0, 1.0);

        // A post low pass, so `tone` is a real control on every variant.
        let coef = one_pole_coef(Self::tone_hz(tone), self.sample_rate);
        let toned = self.tone.tick(shaped, coef);
        self.blended(x, toned * level, mix)
    }

    fn fuzz(&mut self, fx: &Fx, x: f32) -> f32 {
        let drive = db_to_gain(fx.param(P0));
        let bias = fx.param(P1).clamp(0.0, 1.0);
        let gate = fx.param(P2).clamp(0.0, 1.0);
        let level = db_to_gain(fx.param(P3));

        let mut driven = x * drive;
        if fx.subtype == FxSubtype::Gate && gate > 0.0 {
            // A threshold on the way in, which is what makes a gated fuzz stop
            // between phrases instead of hissing.
            let threshold = gate * 0.5;
            if driven.abs() < threshold {
                driven = 0.0;
            }
        }
        if fx.subtype == FxSubtype::Germanium {
            // The dead zone either side of zero: a germanium pair does not
            // conduct until it is pushed, and the crossover distortion is the
            // sound.
            let dead = 0.15 * bias;
            driven = if driven.abs() < dead {
                0.0
            } else {
                driven - dead * driven.signum()
            };
            driven *= 1.5;
        }
        if fx.subtype == FxSubtype::Spit {
            // A resonant peak before the clipper, so the fuzz has a formant and
            // spits rather than buzzes.
            let coef = one_pole_coef(1400.0, self.sample_rate);
            let low = self.band.tick(driven, coef);
            driven = low * 2.0 - driven;
        }
        let shaped = driven.clamp(-1.0, 1.0);
        let coef = one_pole_coef(Self::tone_hz(0.75), self.sample_rate);
        let toned = self.tone.tick(shaped, coef);
        // Fuzz is an insert with no `mix` of its own: it is a fuzz, not a fuzz on
        // top of a clean signal.
        toned * level
    }

    fn bitcrusher(&mut self, fx: &Fx, x: f32) -> f32 {
        let mix = fx.param(P2).clamp(0.0, 1.0);
        if mix <= 0.0 {
            return x;
        }
        let bits = fx.param(P0).clamp(1.0, 16.0);
        let rate = fx.param(P1).clamp(0.0, 1.0);
        // A hold of one sample is no decimation at all, which is what rate 0 is.
        let hold = if fx.subtype == FxSubtype::Decimate {
            1.0 + rate * 63.0
        } else {
            1.0 + rate * 31.0
        };

        self.hold_count += 1.0;
        if self.hold_count >= hold {
            self.hold_count = 0.0;
            let mut sample = x;
            if fx.subtype != FxSubtype::Decimate {
                let steps = (2.0f32).powf(bits) - 1.0;
                sample = (sample * steps).round() / steps;
            }
            if fx.subtype == FxSubtype::Radio {
                // The band limit is what makes it a radio rather than a
                // crusher: 300 Hz to 3 kHz is the telephone band.
                let hp = self
                    .band
                    .tick(sample, one_pole_coef(300.0, self.sample_rate));
                let low = self
                    .tone
                    .tick(sample - hp, one_pole_coef(3000.0, self.sample_rate));
                sample = low;
            }
            self.hold_sample = settle(sample);
        }
        self.blended(x, self.hold_sample, mix)
    }

    fn ring_mod(&mut self, fx: &Fx, x: f32) -> f32 {
        let mix = fx.param(P2).clamp(0.0, 1.0);
        if mix <= 0.0 {
            return x;
        }
        let freq = fx.param(P0).clamp(1.0, 4000.0);
        let depth = fx.param(P1).clamp(0.0, 1.0);
        let step = freq / self.sample_rate;
        self.ring_phase = (self.ring_phase + step).fract();
        // The second modulator is at 2.74 times the first — one of the Risset
        // bell's partial ratios. An inharmonic pair is the whole trick: a
        // *periodic* table cannot hold 2.74 times a fundamental, which is why the
        // wavetables gave up on bells, and two multiplied oscillators do not have
        // to be periodic at all.
        self.wow_phase = (self.wow_phase + step * 2.74).fract();

        let carrier = match fx.subtype {
            // Two modulators at an inharmonic ratio: the sum and difference
            // frequencies they make with the signal are not whole multiples of
            // it, and that is the bell.
            FxSubtype::Bell => sine_of(self.ring_phase) * 0.6 + sine_of(self.wow_phase) * 0.4,
            // Amplitude modulation leaves the carrier in place; ring modulation
            // takes it out. The offset is the whole difference.
            FxSubtype::Am => 0.5 + 0.5 * sine_of(self.ring_phase),
            _ => sine_of(self.ring_phase),
        };
        let wet = x * (1.0 - depth + depth * carrier);
        self.blended(x, wet, mix)
    }

    fn tremolo(&mut self, fx: &Fx, x: f32) -> f32 {
        let mix = fx.param(P2).clamp(0.0, 1.0);
        if mix <= 0.0 {
            return x;
        }
        let rate = fx.param(P0).max(0.01);
        let depth = fx.param(P1).clamp(0.0, 1.0);
        let phase = self.lfo.advance(rate, self.sample_rate);

        let shape = match fx.subtype {
            FxSubtype::Square => square_of(phase, 0.5),
            FxSubtype::Ramp => ramp_of(phase) * 2.0 - 1.0,
            // A chop is a square with a short duty: mostly off, briefly on, which
            // is a rhythm rather than a wobble.
            FxSubtype::Chop => square_of(phase, 0.25),
            _ => sine_of(phase),
        };
        // Unipolar, so full depth reaches silence rather than inverting.
        let gain = 1.0 - depth * (0.5 - 0.5 * shape);
        self.blended(x, x * gain, mix)
    }

    fn filter(&mut self, fx: &Fx, x: f32) -> f32 {
        let mix = fx.param(P3).clamp(0.0, 1.0);
        if mix <= 0.0 {
            return x;
        }
        let cutoff = fx.param(P0).clamp(20.0, self.sample_rate * 0.45);
        let resonance = fx.param(P1).clamp(0.0, 0.99);
        let drive = db_to_gain(fx.param(P2));

        // Saturation before the filter, not after: it is part of what the filter
        // is being driven with, and the resonance rings on the harmonics it adds.
        let driven = if drive > 1.0 {
            (x * drive).tanh()
        } else {
            x * drive
        };

        let f = (2.0 * (std::f32::consts::PI * cutoff / self.sample_rate).sin())
            .min(2.0 - (1.0 - resonance));
        let q = 1.0 - resonance;
        let low = settle(self.svf_low + f * self.svf_band);
        let high = driven - low - q * self.svf_band;
        let band = settle(f * high + self.svf_band);
        self.svf_low = low;
        self.svf_band = band;

        let wet = match fx.subtype {
            FxSubtype::Highpass => high,
            FxSubtype::Bandpass => band,
            // A notch is the two ends with the middle taken out; a peak is the
            // middle with the ends taken out. Both come free, because this filter
            // computes all three outputs every sample anyway.
            FxSubtype::Notch => low + high,
            FxSubtype::Peak => low - high,
            _ => low,
        };
        self.blended(x, wet, mix)
    }

    fn wah(&mut self, fx: &Fx, x: f32) -> f32 {
        let mix = fx.param(P4).clamp(0.0, 1.0);
        if mix <= 0.0 {
            return x;
        }
        let sens = fx.param(P0).clamp(0.0, 1.0);
        let range = fx.param(P1).clamp(0.0, 1.0);
        let resonance = fx.param(P2).clamp(0.0, 0.99);
        let rate = fx.param(P3).max(0.01);

        // Where the pedal sits, 0..1: the envelope, a fixed position, or an LFO.
        let position = match fx.subtype {
            FxSubtype::Pedal => range,
            FxSubtype::Lfo => 0.5 + 0.5 * sine_of(self.lfo.advance(rate, self.sample_rate)),
            _ => {
                // A follower with a fast attack and a slow release, so the peak
                // of a note opens it and the tail does not close it instantly.
                let level = x.abs();
                let coef = if level > self.env {
                    one_pole_coef(30.0, self.sample_rate)
                } else {
                    one_pole_coef(4.0, self.sample_rate)
                };
                self.env = settle(self.env + coef * (level - self.env));
                (self.env * sens * 4.0).clamp(0.0, 1.0)
            }
        };
        let cutoff = 250.0 * (3000.0f32 / 250.0).powf(position * (0.2 + 0.8 * range));

        let f = (2.0 * (std::f32::consts::PI * cutoff / self.sample_rate).sin())
            .min(2.0 - (1.0 - resonance));
        let q = 1.0 - resonance;
        let low = settle(self.svf_low + f * self.svf_band);
        let high = x - low - q * self.svf_band;
        let band = settle(f * high + self.svf_band);
        self.svf_low = low;
        self.svf_band = band;
        self.blended(x, band, mix)
    }

    // ---- the level family ----

    /// A feed-forward compressor, in decibels.
    ///
    /// In decibels because that is what a ratio *means*: 4:1 is four decibels in
    /// for one decibel out, and doing it in the linear domain would be a
    /// different curve wearing the same numbers.
    fn compressor(&mut self, fx: &Fx, x: f32) -> f32 {
        let threshold = fx.param(P0).clamp(-60.0, 0.0);
        let mut ratio = fx.param(P1).clamp(1.0, 20.0);
        let attack = fx.param(P2).max(0.05);
        let mut release = fx.param(P3).max(1.0);
        let makeup = fx.param(P4);

        let knee = match fx.subtype {
            // A limiter has no knee and no patience; punch breathes, because its
            // release is slow enough to be heard as movement.
            FxSubtype::Limiter => {
                ratio = ratio.max(10.0);
                0.0
            }
            FxSubtype::Punch => {
                release = release.max(200.0);
                0.0
            }
            _ => 6.0,
        };
        let attack_coef = one_pole_coef(1000.0 / attack, self.sample_rate);
        let release_coef = one_pole_coef(1000.0 / release, self.sample_rate);

        let level = x.abs();
        let coef = if level > self.env {
            attack_coef
        } else {
            release_coef
        };
        self.env += coef * (level - self.env);

        let db = gain_to_db(self.env);
        let over = db - threshold;
        let reduction = if over <= 0.0 {
            0.0
        } else if knee > 0.0 && over < knee {
            // The standard quadratic knee: the compression fades in over the
            // first six decibels rather than arriving at the threshold.
            let t = over / knee;
            (1.0 - 1.0 / ratio) * over * t * t * 0.5
        } else {
            let soft = if knee > 0.0 {
                (1.0 - 1.0 / ratio) * knee * 0.5
            } else {
                0.0
            };
            soft + (over - knee) * (1.0 - 1.0 / ratio)
        };
        let target = db_to_gain(makeup - reduction);
        // The gain is smoothed on the same detector, so a fast attack does not
        // click and a slow release does not pump on one sample.
        self.gain += coef * (target - self.gain);
        x * self.gain
    }

    fn gate(&mut self, fx: &Fx, x: f32) -> f32 {
        let threshold = fx.param(P0).clamp(-80.0, 0.0);
        let attack = fx.param(P1).max(0.05);
        let hold = fx.param(P2).max(0.0);
        let release = fx.param(P3).max(1.0);
        let rate = fx.param(P4).max(0.01);

        let level = x.abs();
        let db = gain_to_db(level);
        let open = match fx.subtype {
            // A ducker is the same detector the other way up: it closes *above*
            // the threshold instead of below it.
            FxSubtype::Duck => db <= threshold,
            _ => db >= threshold,
        };
        if open {
            self.hold_count = hold * 0.001 * self.sample_rate;
        }
        let wanted = if open || self.hold_count > 0.0 {
            if self.hold_count > 0.0 {
                self.hold_count -= 1.0;
            }
            1.0
        } else {
            0.0
        };
        let coef = if wanted > self.gain {
            one_pole_coef(1000.0 / attack, self.sample_rate)
        } else {
            one_pole_coef(1000.0 / release, self.sample_rate)
        };
        self.gain += coef * (wanted - self.gain);

        let shaped = match fx.subtype {
            // A stutter ignores the signal's own level and chops on the beat,
            // with the gate's envelope deciding how hard the edges are.
            FxSubtype::Stutter => {
                let phase = self.lfo.advance(rate, self.sample_rate);
                if square_of(phase, 0.5) > 0.0 {
                    self.gain
                } else {
                    0.0
                }
            }
            _ => self.gain,
        };
        x * shaped
    }
}

// -----------------------------------------------------------------------------
// The bank
// -----------------------------------------------------------------------------

/// Every effect instance the callback runs: one chain per register, and the two
/// aux units.
pub struct FxBank {
    /// `REGISTERS * CHAIN_SLOTS`, laid out per register so a chain is a slice.
    slots: Vec<FxState>,
    reverb: FxState,
    delay: FxState,
}

impl FxBank {
    /// Build the bank. Called once, on the main thread, before the stream starts.
    pub fn new(sample_rate: f32) -> Self {
        let mut slots = Vec::with_capacity(REGISTERS * CHAIN_SLOTS);
        for _ in 0..(REGISTERS * CHAIN_SLOTS) {
            slots.push(FxState::new(sample_rate, false));
        }
        FxBank {
            slots,
            reverb: FxState::new(sample_rate, true),
            delay: FxState::new(sample_rate, true),
        }
    }

    /// Run one register's whole chain, in order.
    pub fn chain(&mut self, register: usize, chain: &[Fx; CHAIN_SLOTS], tempo: f32, x: f32) -> f32 {
        let start = register * CHAIN_SLOTS;
        let Some(slots) = self.slots.get_mut(start..start + CHAIN_SLOTS) else {
            return x;
        };
        let mut out = x;
        for (slot, fx) in slots.iter_mut().zip(chain) {
            out = slot.tick(fx, tempo, out);
        }
        out
    }

    /// The reverb send's return, fully wet: the mixer's return level decides how
    /// much of it is heard.
    pub fn reverb(&mut self, fx: &Fx, tempo: f32, x: f32) -> f32 {
        self.reverb.tick(fx, tempo, x)
    }

    /// The delay send's return, on the same terms.
    pub fn delay(&mut self, fx: &Fx, tempo: f32, x: f32) -> f32 {
        self.delay.tick(fx, tempo, x)
    }

    /// How many samples the longest effect can delay by, for the tests.
    #[cfg(test)]
    pub fn max_delay(&self) -> f32 {
        self.slots[0].max_delay()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::FxKind::*;

    const SR: f32 = 48_000.0;

    fn state(kind: FxKind) -> (FxState, Fx) {
        let fx = Fx::new(kind);
        (FxState::new(SR, false), fx)
    }

    /// Render `samples` of a sine through one effect.
    fn render(kind: FxKind, freq: f32, samples: usize) -> Vec<f32> {
        let (mut state, fx) = state(kind);
        (0..samples)
            .map(|i| {
                let x = (std::f32::consts::TAU * freq * i as f32 / SR).sin() * 0.5;
                state.tick(&fx, 120.0, x)
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    fn flux(samples: &[f32]) -> f32 {
        samples.windows(2).map(|w| (w[1] - w[0]).abs()).sum()
    }

    #[test]
    fn a_recursive_state_settles_instead_of_parking() {
        // The property `settle` exists for. A one-pole with no input left
        // computes `z = z * coef`, and without the flush it stops decaying once
        // the product rounds back to `z` — and parks *higher* in the subnormal
        // range, where every operation costs tens to hundreds of times more.
        let mut pole = OnePole { z: 1.0 };
        let coef = one_pole_coef(4.0, SR);
        for _ in 0..(SR as usize * 30) {
            pole.tick(0.0, coef);
            assert!(
                pole.z == 0.0 || pole.z >= FLOOR,
                "the pole parked at {:e}",
                pole.z
            );
        }
        assert_eq!(pole.z, 0.0, "thirty seconds of silence must be silence");

        // And the same for the phaser's allpass, whose feedback is where the
        // measured subnormal came from.
        let mut allpass = Allpass1 { z: 1.0 };
        for _ in 0..(SR as usize * 30) {
            allpass.tick(0.0, 0.7);
            assert!(
                allpass.z == 0.0 || allpass.z >= FLOOR,
                "the allpass parked at {:e}",
                allpass.z
            );
        }
        assert_eq!(allpass.z, 0.0);
    }

    #[test]
    fn no_effect_parks_at_any_setting() {
        // The property `settle` exists for, asserted for every algorithm rather
        // than for the five the measurement happened to catch. Each kind is put
        // at the top of every parameter range it declares, driven hard for half a
        // second, and then given silence for ten; the output may still be
        // decaying at the end of that — a filter at 99 % resonance is — but it
        // may never be *parked* in the subnormal range, where the decay stops
        // and the cost of every operation on it multiplies.
        //
        // Twenty seconds rather than ten would be a stronger test of the ones
        // that decay slowly and a slower test of everything else; ten is where
        // the two measured offenders crossed.
        for kind in FxKind::ALL {
            if kind.is_none() {
                continue;
            }
            let subtype = kind
                .subtypes()
                .first()
                .copied()
                .unwrap_or(FxSubtype::SubtypeNone);
            let mut fx = Fx::variant(kind, subtype);
            let mut index = 0;
            while let Some(spec) = fx.spec(index) {
                if spec.label != "mix" {
                    fx.set_param(index, spec.range.1);
                }
                index += 1;
            }

            let mut state = FxState::new(SR, false);
            for i in 0..(SR as usize / 2) {
                let x = (std::f32::consts::TAU * 220.0 * i as f32 / SR).sin() * 0.8;
                state.tick(&fx, 120.0, x);
            }
            for i in 0..(SR as usize * 10) {
                let y = state.tick(&fx, 120.0, 0.0);
                assert!(
                    y == 0.0 || y.abs() >= f32::MIN_POSITIVE * 1024.0,
                    "{kind:?} parked its output at {:e} after {} samples of silence",
                    y,
                    i
                );
            }
        }
    }

    #[test]
    fn the_flush_is_far_below_anything_audible() {
        // Not a matter of taste: `FLOOR` is what the analyser in this file has
        // called silence since before the effects existed, and it is 140 dB below
        // full scale. The test is here so that moving it is a decision somebody
        // has to make on purpose.
        let db = 20.0 * FLOOR.log10();
        assert!(db < -139.0 && db > -141.0, "{db} dB");
        assert_eq!(settle(FLOOR), FLOOR, "at the floor is not below it");
        assert_eq!(settle(-FLOOR), -FLOOR);
        assert_eq!(settle(FLOOR * 0.5), 0.0);
        assert_eq!(settle(-FLOOR * 0.5), 0.0);
        assert_eq!(settle(0.0), 0.0);
        // A normal-sized sample is untouched, sign and all.
        assert_eq!(settle(0.5), 0.5);
        assert_eq!(settle(-0.25), -0.25);
    }

    #[test]
    fn an_empty_slot_is_a_bypass_that_costs_nothing() {
        let mut state = FxState::new(SR, false);
        let fx = Fx::none();
        for i in 0..1_000 {
            let x = (i as f32 * 0.37).sin() * 7.5;
            assert_eq!(state.tick(&fx, 120.0, x), x);
        }
    }

    #[test]
    fn every_kind_and_variant_stays_finite_when_driven_hard() {
        // The one test that has to cover everything: fourteen algorithms, fifty
        // variants, driven with full-scale noise and then with a parameter sweep
        // at both ends of every range. A NaN here is not a subtle artefact — it
        // sticks in a delay line or a comb and silences the whole register.
        let mut seed = 0x1234_5678u32;
        for kind in FxKind::ALL {
            if kind.is_none() {
                continue;
            }
            for subtype in kind.subtypes() {
                for extreme in [0.0f32, 1.0] {
                    let mut fx = Fx::variant(kind, *subtype);
                    for index in 0..fx.param_count() {
                        let spec = fx.spec(index).unwrap();
                        let value = if extreme > 0.5 {
                            spec.range.1
                        } else {
                            spec.range.0
                        };
                        fx.set_param(index, value);
                    }
                    // A delay that is fully wet and fully fed back is the worst
                    // case for a runaway; the level effects want a signal.
                    let mut state = FxState::new(SR, false);
                    let mut last = 0.0;
                    for _ in 0..20_000 {
                        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        let x = (seed >> 8) as f32 / 8_388_608.0 - 1.0;
                        last = state.tick(&fx, 120.0, x);
                        assert!(
                            last.is_finite(),
                            "{} / {} at {} produced {}",
                            kind.name(),
                            subtype.name(),
                            extreme,
                            last
                        );
                    }
                    assert!(
                        last.abs() < 100.0,
                        "{} / {} at {} ended at {}",
                        kind.name(),
                        subtype.name(),
                        extreme,
                        last
                    );
                }
            }
        }
    }

    #[test]
    fn a_reverb_with_its_level_down_does_nothing_at_all() {
        // The shipped default: every ensemble that never turned the reverb up
        // must keep sounding exactly as it did.
        let mut state = FxState::new(SR, false);
        let fx = Fx::variant(Reverb, FxSubtype::Hall);
        assert_eq!(fx.param(P3), 0.0, "the preset default is off");
        for i in 0..500 {
            let x = (i as f32 * 0.11).cos();
            assert_eq!(state.tick(&fx, 120.0, x), x);
        }
    }

    #[test]
    fn the_hall_tank_is_the_one_that_shipped() {
        // The reverb was rewritten into an effect, and `hall` at its defaults has
        // to be the old tank *exactly* — same feedback curve, same four combs into
        // two allpasses, same order of arithmetic. Every shipped ensemble has a
        // reverb level between 0 and 0.62, so this is not a setting nobody used.
        //
        // A second implementation, deliberately, written from the pre-effects
        // source with its own buffers: the only way to make "it sounds the same"
        // mean anything is to render both and compare the bits.
        struct OldComb {
            buffer: Vec<f32>,
            index: usize,
            damp_store: f32,
        }
        impl OldComb {
            fn new(delay: usize) -> Self {
                OldComb {
                    buffer: vec![0.0; delay],
                    index: 0,
                    damp_store: 0.0,
                }
            }
            fn tick(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
                let output = self.buffer[self.index];
                self.damp_store = output * (1.0 - damp) + self.damp_store * damp;
                self.buffer[self.index] = input + self.damp_store * feedback;
                self.index = (self.index + 1) % self.buffer.len();
                output
            }
        }
        struct OldAllpass {
            buffer: Vec<f32>,
            index: usize,
        }
        impl OldAllpass {
            fn new(delay: usize) -> Self {
                OldAllpass {
                    buffer: vec![0.0; delay],
                    index: 0,
                }
            }
            fn tick(&mut self, input: f32, feedback: f32) -> f32 {
                let buffered = self.buffer[self.index];
                let output = -input + buffered;
                self.buffer[self.index] = input + buffered * feedback;
                self.index = (self.index + 1) % self.buffer.len();
                output
            }
        }

        let mut fx = Fx::variant(Reverb, FxSubtype::Hall);
        for size in [0.25f32, 0.5, 0.92] {
            // A fresh oracle and a fresh state per size: a tank has a tail, so
            // reusing either one would compare a decaying room against a silent
            // one and call the difference a rewrite.
            let mut combs = [
                OldComb::new(COMB_DELAYS[0]),
                OldComb::new(COMB_DELAYS[1]),
                OldComb::new(COMB_DELAYS[2]),
                OldComb::new(COMB_DELAYS[3]),
            ];
            let mut allpasses = [
                OldAllpass::new(ALLPASS_DELAYS[0]),
                OldAllpass::new(ALLPASS_DELAYS[1]),
            ];
            // The old `Reverb::tick`, character for character.
            let mut old = |input: f32| {
                let feedback = 0.7 + size.clamp(0.0, 1.0) * 0.28;
                let damp = 0.2;
                let mut out = 0.0;
                for c in combs.iter_mut() {
                    out += c.tick(input, feedback, damp);
                }
                out /= combs.len() as f32;
                out *= 1.0 - feedback;
                for ap in allpasses.iter_mut() {
                    out = ap.tick(out, 0.5);
                }
                out
            };

            // An aux unit, because that is how the tank is used and how its
            // return is summed: fully wet, with the mixer's level on top.
            fx.set_param(P0, size);
            let mut state = FxState::new(SR, true);
            for i in 0..20_000 {
                let x = (i as f32 * 0.017).sin() * 0.5;
                let a = old(x);
                let b = state.tick(&fx, 120.0, x);
                assert_eq!(
                    a.to_bits(),
                    b.to_bits(),
                    "size {} differed at sample {}",
                    size,
                    i
                );
            }
        }
        // And the parameters really are the old constants.
        assert_eq!(fx.param(P1), 0.2, "the default damping is the old constant");
        assert_eq!(fx.param(P2), 0.0, "and there is no pre-delay");
    }

    #[test]
    fn the_reverb_voicings_are_actually_different_tanks() {
        let mut sizes = Vec::new();
        for subtype in [FxSubtype::Hall, FxSubtype::Room, FxSubtype::Plate] {
            let mut state = FxState::new(SR, true);
            let mut fx = Fx::variant(Reverb, subtype);
            fx.set_param(P0, 0.6);
            state.tick(&fx, 120.0, 0.5);
            // The proof is internal and cheap: the comb lengths differ.
            let lengths: Vec<usize> = state.combs.iter().map(|c| c.length).collect();
            sizes.push((subtype.name(), lengths));
        }
        assert_ne!(sizes[0].1, sizes[1].1, "hall and room are the same tank");
        assert_ne!(sizes[0].1, sizes[2].1, "hall and plate are the same tank");
        for (name, lengths) in &sizes {
            assert!(
                lengths.iter().all(|l| *l > 0),
                "{} set a comb to nothing",
                name
            );
        }
    }

    #[test]
    fn a_delay_repeats_at_the_time_it_was_asked_for() {
        // An impulse in, and the echo where the time says it should be.
        let mut state = FxState::new(SR, false);
        let mut fx = Fx::variant(Delay, FxSubtype::Digital);
        fx.set_param(P0, 100.0); // 100 ms
        fx.set_param(P1, 0.5);
        fx.set_param(P3, 1.0); // fully wet, so the echo is not buried in the dry

        let expected = (0.100 * SR) as usize;
        let mut out = Vec::with_capacity(SR as usize);
        for i in 0..(SR as usize / 2) {
            let x = if i == 0 { 1.0 } else { 0.0 };
            out.push(state.tick(&fx, 120.0, x));
        }
        let first = out
            .iter()
            .enumerate()
            .skip(1)
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        assert!(
            first.abs_diff(expected) <= 1,
            "the echo landed at {} rather than {}",
            first,
            expected
        );
        // And it repeats, quieter each time.
        assert!(out[expected * 2].abs() < out[expected].abs());
    }

    #[test]
    fn a_synced_delay_ignores_the_milliseconds_and_follows_the_tempo() {
        let mut state = FxState::new(SR, false);
        let mut fx = Fx::variant(Delay, FxSubtype::Digital);
        fx.set_param(P3, 1.0);
        fx.set_param(P4, 1.0); // sync on
        fx.set_param(P0, 500.0); // a quarter note at 120 bpm

        let at = |tempo: f32, state: &mut FxState, fx: &Fx| {
            // Long enough for the slowest echo to land inside it: a quarter note
            // at 60 bpm is a whole second.
            let mut out = vec![0.0f32; (SR * 1.5) as usize];
            for (i, slot) in out.iter_mut().enumerate() {
                let x = if i == 0 { 1.0 } else { 0.0 };
                *slot = state.tick(fx, tempo, x);
            }
            out.iter()
                .enumerate()
                .skip(1)
                .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
                .map(|(i, _)| i)
                .unwrap()
        };

        let fast = at(120.0, &mut state, &fx);
        let mut state = FxState::new(SR, false);
        let slow = at(60.0, &mut state, &fx);
        assert!(
            slow > fast * 3 / 2,
            "half the tempo should be a longer delay: {} against {}",
            slow,
            fast
        );
    }

    #[test]
    fn the_modulation_effects_move_the_signal_without_changing_its_level() {
        // A chorus, a flanger and a phaser are all time-varying filters: the
        // proof is that the output is not the input, and that the level is
        // broadly where it was — an effect that halves the level is a fault, not
        // a modulation.
        for kind in [Chorus, Flanger, Phaser] {
            // A *slow* LFO and long windows, which is the only way to measure a
            // sweeping notch. A notch crossing 440 Hz in a fifth of a cycle is
            // averaged away by any window short enough to see it move; a quarter
            // of a hertz sits still for long enough that each window is one
            // position on the sweep.
            let mut fx = Fx::new(kind);
            fx.set_param(P0, 0.25);
            let mut state = FxState::new(SR, false);
            let out: Vec<f32> = (0..192_000)
                .map(|i| {
                    let x = (std::f32::consts::TAU * 440.0 * i as f32 / SR).sin() * 0.5;
                    state.tick(&fx, 120.0, x)
                })
                .collect();
            let tail = &out[9_600..];
            let input = 0.5 / 2.0f32.sqrt();
            let ratio = rms(tail) / input;
            assert!(
                (0.3..2.5).contains(&ratio),
                "{} left the level at {}x",
                kind.name(),
                ratio
            );
            // And it really is moving: the comb's notches sweep, so a steady
            // tone's envelope wobbles.
            let windows: Vec<f32> = tail.chunks(12_000).map(rms).collect();
            let low = windows.iter().cloned().fold(f32::MAX, f32::min);
            let high = windows.iter().cloned().fold(0.0f32, f32::max);
            assert!(
                high > low * 1.5,
                "{} is not modulating: {} against {}",
                kind.name(),
                high,
                low
            );
        }
    }

    #[test]
    fn every_distortion_adds_harmonics_and_folds_are_not_clippers() {
        // A clean sine in, and the output has more in it than the input did —
        // measured as sample-to-sample movement, which is a brightness meter.
        let input = render(None, 440.0, 24_000);
        let quiet = flux(&input[12_000..]);
        for subtype in [
            FxSubtype::Overdrive,
            FxSubtype::Soft,
            FxSubtype::Hard,
            FxSubtype::Tube,
            FxSubtype::Rectify,
        ] {
            let mut state = FxState::new(SR, false);
            let mut fx = Fx::variant(Distortion, subtype);
            fx.set_param(P0, 24.0);
            let out: Vec<f32> = (0..24_000)
                .map(|i| {
                    let x = (std::f32::consts::TAU * 440.0 * i as f32 / SR).sin() * 0.5;
                    state.tick(&fx, 120.0, x)
                })
                .collect();
            assert!(
                flux(&out[12_000..]) > quiet * 1.2,
                "{} did not add anything",
                subtype.name()
            );
        }
        // A folder turns the peaks over rather than flattening them, so at a low
        // drive it should *not* be a clipper: the peak stays inside the range
        // while the shape changes.
        assert_eq!(fold(0.25), 0.25, "inside the range it does nothing");
        assert!(
            (fold(1.6) - 0.4).abs() < 1.0e-6,
            "1.6 folded to {}",
            fold(1.6)
        );
        assert!(
            (fold(2.4) + 0.4).abs() < 1.0e-6,
            "2.4 folded to {}",
            fold(2.4)
        );
        // A clipper would flatten both of those to 1.0, which is the difference
        // between a folder and a clipper.
        assert!(fold(1.6) < 1.0 && fold(2.4) < 1.0);
    }

    #[test]
    fn the_bitcrusher_really_quantises() {
        let mut state = FxState::new(SR, false);
        let mut fx = Fx::variant(Bitcrusher, FxSubtype::Crush);
        fx.set_param(P0, 3.0);
        fx.set_param(P1, 0.0);
        fx.set_param(P2, 1.0);
        let mut seen = std::collections::HashSet::new();
        for i in 0..24_000 {
            let x = (std::f32::consts::TAU * 440.0 * i as f32 / SR).sin() * 0.5;
            seen.insert(state.tick(&fx, 120.0, x).to_bits());
        }
        // Three bits is fifteen levels, and the dry sine is not in there.
        assert!(
            seen.len() <= 17,
            "three bits produced {} distinct values",
            seen.len()
        );
    }

    #[test]
    fn the_filter_variants_select_the_outputs_they_claim_to() {
        // A tone well above the cutoff: the low pass should bury it and the high
        // pass let it through, and the notch should be neither.
        let at = |subtype: FxSubtype, freq: f32| {
            let mut state = FxState::new(SR, false);
            let mut fx = Fx::variant(Filter, subtype);
            fx.set_param(P0, 300.0);
            fx.set_param(P1, 0.2);
            fx.set_param(P2, 0.0);
            fx.set_param(P3, 1.0);
            let out: Vec<f32> = (0..24_000)
                .map(|i| {
                    let x = (std::f32::consts::TAU * freq * i as f32 / SR).sin() * 0.5;
                    state.tick(&fx, 120.0, x)
                })
                .collect();
            rms(&out[12_000..])
        };
        let low = at(FxSubtype::Lowpass, 4000.0);
        let high = at(FxSubtype::Highpass, 4000.0);
        let notch = at(FxSubtype::Notch, 4000.0);
        assert!(high > low * 8.0, "high {} low {}", high, low);
        assert!(
            notch > low * 4.0 && notch < high,
            "a notch is neither end: {} against {} and {}",
            notch,
            low,
            high
        );
        // And with the cutoff above the tone, the low pass should let it through.
        let open = at(FxSubtype::Lowpass, 100.0);
        assert!(open > low * 8.0, "open {} shut {}", open, low);
    }

    #[test]
    fn the_compressor_holds_the_peak_down_and_the_limiter_flat() {
        // A loud sine well over the threshold: the output peak should be lower
        // than the input's, and the ratio should decide by how much.
        let loud = 0.9f32;
        let run = |subtype: FxSubtype| {
            let mut state = FxState::new(SR, false);
            let mut fx = Fx::variant(Compressor, subtype);
            fx.set_param(P0, -24.0);
            fx.set_param(P4, 0.0);
            let out: Vec<f32> = (0..48_000)
                .map(|i| {
                    let x = (std::f32::consts::TAU * 220.0 * i as f32 / SR).sin() * loud;
                    state.tick(&fx, 120.0, x)
                })
                .collect();
            peak(&out[24_000..])
        };
        let comp = run(FxSubtype::Comp);
        let limit = run(FxSubtype::Limiter);
        assert!(comp < loud, "the compressor did not compress: {}", comp);
        assert!(
            limit < comp,
            "the limiter should hold harder: {} against {}",
            limit,
            comp
        );
    }

    #[test]
    fn a_gate_closes_below_its_threshold_and_opens_above_it() {
        let mut state = FxState::new(SR, false);
        let mut fx = Fx::variant(Gate, FxSubtype::Gate);
        fx.set_param(P0, -20.0);
        fx.set_param(P1, 1.0);
        fx.set_param(P2, 0.0);
        fx.set_param(P3, 5.0);

        let quiet = 0.01f32; // -40 dB, below the threshold
        let loud = 0.5f32; // -6 dB, above it
                           // The *peak* of the last stretch, not the last sample: a 220 Hz sine
                           // ends wherever its phase puts it, and a gate that is wide open can still
                           // finish on a zero crossing.
        let out = |state: &mut FxState, level: f32| {
            let mut last = Vec::with_capacity(4_800);
            for i in 0..48_000 {
                let x = (std::f32::consts::TAU * 220.0 * i as f32 / SR).sin() * level;
                let y = state.tick(&fx, 120.0, x);
                if i >= 43_200 {
                    last.push(y);
                }
            }
            peak(&last)
        };
        // Held open by a loud tone first, then the quiet one should fade out.
        out(&mut state, loud);
        let quiet_out = out(&mut state, quiet);
        assert!(
            quiet_out < quiet * 0.2,
            "a gated quiet tone came out at {}",
            quiet_out
        );
        let loud_out = out(&mut state, loud);
        assert!(
            loud_out > loud * 0.9,
            "a gated loud tone came out at {}",
            loud_out
        );
    }

    #[test]
    fn the_tremolo_shapes_modulate_the_level() {
        for subtype in [
            FxSubtype::Sine,
            FxSubtype::Square,
            FxSubtype::Ramp,
            FxSubtype::Chop,
        ] {
            let mut state = FxState::new(SR, false);
            let mut fx = Fx::variant(Tremolo, subtype);
            fx.set_param(P1, 1.0); // full depth, so the trough is silence
            fx.set_param(P2, 1.0);
            let out: Vec<f32> = (0..48_000)
                .map(|i| {
                    let x = (std::f32::consts::TAU * 440.0 * i as f32 / SR).sin() * 0.5;
                    state.tick(&fx, 120.0, x)
                })
                .collect();
            let windows: Vec<f32> = out[4_800..].chunks(1_200).map(rms).collect();
            let low = windows.iter().cloned().fold(f32::MAX, f32::min);
            let high = windows.iter().cloned().fold(0.0f32, f32::max);
            assert!(
                high > low * 3.0,
                "{} barely moved the level: {} against {}",
                subtype.name(),
                high,
                low
            );
        }
    }

    #[test]
    fn the_wah_opens_and_closes_on_the_signal() {
        // With the pedal variant the position is fixed, so the same tone comes
        // out differently depending on where the pedal is — which is the whole
        // claim, and the easiest version of it to check.
        let at = |range: f32| {
            let mut state = FxState::new(SR, false);
            let mut fx = Fx::variant(Wah, FxSubtype::Pedal);
            fx.set_param(P1, range);
            fx.set_param(P2, 0.8);
            fx.set_param(P4, 1.0);
            let out: Vec<f32> = (0..24_000)
                .map(|i| {
                    let x = (std::f32::consts::TAU * 600.0 * i as f32 / SR).sin() * 0.5;
                    state.tick(&fx, 120.0, x)
                })
                .collect();
            rms(&out[12_000..])
        };
        let closed = at(0.0);
        let open = at(1.0);
        assert!(
            (closed - open).abs() > closed.min(open) * 0.1,
            "the pedal did nothing: {} against {}",
            closed,
            open
        );
    }

    #[test]
    fn the_ring_modulator_makes_frequencies_that_were_not_there() {
        // A pure 440 Hz in: ring modulating it by 300 Hz gives 140 and 740, so
        // the sample-to-sample movement of the output is nothing like the input's.
        let mut state = FxState::new(SR, false);
        let mut fx = Fx::variant(RingMod, FxSubtype::Ring);
        fx.set_param(P0, 300.0);
        fx.set_param(P1, 1.0);
        fx.set_param(P2, 1.0);
        let out: Vec<f32> = (0..24_000)
            .map(|i| {
                let x = (std::f32::consts::TAU * 440.0 * i as f32 / SR).sin() * 0.5;
                state.tick(&fx, 120.0, x)
            })
            .collect();
        let clean: Vec<f32> = (0..24_000)
            .map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / SR).sin() * 0.5)
            .collect();
        assert!(
            (flux(&out[12_000..]) - flux(&clean[12_000..])).abs() > flux(&clean[12_000..]) * 0.1,
            "the spectrum should not be the input's"
        );
    }

    #[test]
    fn fuzz_and_distortion_are_not_the_same_effect() {
        let mut outputs = Vec::new();
        for kind in [Distortion, Fuzz] {
            let mut state = FxState::new(SR, false);
            let fx = Fx::new(kind);
            let out: Vec<f32> = (0..24_000)
                .map(|i| {
                    let x = (std::f32::consts::TAU * 220.0 * i as f32 / SR).sin() * 0.4;
                    state.tick(&fx, 120.0, x)
                })
                .collect();
            outputs.push(out);
        }
        let difference: f32 = outputs[0]
            .iter()
            .zip(outputs[1].iter())
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(difference > 100.0, "fuzz and distortion render alike");
    }

    #[test]
    fn an_aux_unit_returns_its_wet_signal_alone() {
        // The return level belongs to the mixer, so an aux unit must not blend
        // the dry signal back in: a reverb whose return is turned up would get
        // louder in the dry path too.
        let mut state = FxState::new(SR, true);
        let mut fx = Fx::variant(Reverb, FxSubtype::Hall);
        fx.set_param(P3, 0.0); // the parameter the aux ignores
        fx.set_param(P0, 0.8);
        let mut tail = 0.0;
        for i in 0..24_000 {
            let x = if i == 0 { 1.0 } else { 0.0 };
            tail = state.tick(&fx, 120.0, x);
        }
        assert!(
            tail.abs() > 1.0e-9,
            "the aux reverb should be ringing, not bypassed"
        );
    }

    #[test]
    fn a_bank_runs_a_chain_in_order_and_a_second_order_matters() {
        // Two effects that do not commute: a distortion into a filter is not a
        // filter into a distortion. If the order were being ignored, these would
        // render identically.
        let chain_of = |first: Fx, second: Fx| {
            let mut bank = FxBank::new(SR);
            let mut chain = [Fx::none(); CHAIN_SLOTS];
            chain[0] = first;
            chain[1] = second;
            (0..24_000)
                .map(|i| {
                    let x = (std::f32::consts::TAU * 220.0 * i as f32 / SR).sin() * 0.5;
                    bank.chain(0, &chain, 120.0, x)
                })
                .collect::<Vec<f32>>()
        };
        let drive = Fx::new(Distortion);
        let mut lowpass = Fx::new(Filter);
        lowpass.set_param(P0, 400.0);
        let a = chain_of(drive, lowpass);
        let b = chain_of(lowpass, drive);
        let difference: f32 = a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum();
        assert!(difference > 100.0, "the chain order made no difference");
    }

    #[test]
    fn the_registers_have_their_own_slots() {
        // Three chains in one flat vector: an off-by-one would put the low
        // register's distortion on the mid register too.
        let mut bank = FxBank::new(SR);
        let mut chain = [Fx::none(); CHAIN_SLOTS];
        chain[0] = Fx::new(Distortion);
        let mut out = [0.0f32; 3];
        for i in 0..24_000 {
            let x = (std::f32::consts::TAU * 220.0 * i as f32 / SR).sin() * 0.5;
            out[0] = bank.chain(0, &chain, 120.0, x);
            out[2] = bank.chain(2, &[Fx::none(); CHAIN_SLOTS], 120.0, x);
        }
        assert!(out[0] != out[2], "the chains are not separate");
        // And a register with nothing in its chain passes the signal through.
        let empty = [Fx::none(); CHAIN_SLOTS];
        let x = 0.37f32;
        assert_eq!(bank.chain(1, &empty, 120.0, x), x);
        assert!(bank.max_delay() > 0.0);
    }

    #[test]
    fn changing_the_type_resets_the_state_a_new_effect_would_inherit() {
        // A delay's tail must not come out of a reverb, and a reverb must not
        // start with a delay line's worth of somebody else's sound in it.
        let mut state = FxState::new(SR, false);
        let mut delay = Fx::variant(Delay, FxSubtype::Digital);
        delay.set_param(P3, 1.0);
        delay.set_param(P1, 0.9);
        for i in 0..24_000 {
            let x = (std::f32::consts::TAU * 220.0 * i as f32 / SR).sin();
            state.tick(&delay, 120.0, x);
        }
        // Switching to a reverb and feeding silence: nothing should come out.
        let reverb = Fx::variant(Reverb, FxSubtype::Hall);
        let mut loudest = 0.0f32;
        for _ in 0..4_800 {
            loudest = loudest.max(state.tick(&reverb, 120.0, 0.0).abs());
        }
        assert!(
            loudest < 1.0e-6,
            "the old effect's tail came through: {}",
            loudest
        );
    }

    #[test]
    fn the_tone_control_maps_its_ends_to_something_audible() {
        assert!(FxState::tone_hz(0.0) < 300.0);
        assert!(FxState::tone_hz(1.0) > 15_000.0);
        assert!(FxState::tone_hz(0.5) > 1_000.0 && FxState::tone_hz(0.5) < 3_000.0);
        // The decibel helpers are the ones the shaping effects are built on.
        assert!((db_to_gain(0.0) - 1.0).abs() < 1.0e-6);
        assert!((db_to_gain(-6.0206) - 0.5).abs() < 1.0e-4);
        assert!((gain_to_db(0.5) + 6.0206).abs() < 0.01);
        // And they cope with nonsense rather than producing NaN.
        assert!(fold(f32::NAN) == 0.0);
        assert!(fold(50.0).abs() <= 1.0);
        assert!(gain_to_db(0.0).is_finite());
        assert!(one_pole_coef(0.0, SR).is_finite());
        assert!(one_pole_coef(1.0e9, SR) <= 1.0);
    }
}
