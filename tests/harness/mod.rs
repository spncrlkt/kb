//! The offline render harness: the real audio path, driven from a test.
//!
//! See `PERFORMANCE.md`, stage 1. What runs here is the same
//! [`Engine`](chord_tool::synth::Engine) the cpal callback runs — `Synth::new`
//! builds one and moves it into the stream — so a render is the sound the
//! program makes, not an approximation of it.
//!
//! A render is deterministic. The voice pool's noise seeds come from a counter,
//! the effects and the analyser are pure state machines, and the only things that
//! are not — the scheduler thread and the peak meter — are outside the signal
//! path. Two renders of the same config are therefore bit-identical, which is
//! what lets a block size or a sample rate be the only thing that varies.

#![allow(dead_code)]

use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use chord_tool::ensemble::{builtin_ensembles, Ensemble, Placement};
use chord_tool::eq::EqCurve;
use chord_tool::fx::{Fx, FxKind};
use chord_tool::instrument::builtin_instruments;
use chord_tool::music::PPQ;
use chord_tool::synth::{self, Engine, SynthParams, Voices};
use chord_tool::timing::Callback;
use chord_tool::voice::{ComposedChannel, MixerSettings, VoicePatch};

/// The largest block a render is chopped into.
///
/// Events are quantised down to the block that contains them, which is what the
/// scheduler does in the program: an onset is applied between two buffers. An
/// event at a multiple of this frame is therefore exactly on time at every block
/// size that divides it — which is what makes a block-size comparison a
/// comparison of the *sound* and nothing else.
pub const MAX_BLOCK: usize = 4096;

/// A note list that spells a plain triad, low to high.
pub const TRIAD: [u8; 3] = [60, 64, 67];

// -----------------------------------------------------------------------------
// What to render
// -----------------------------------------------------------------------------

/// Where the three registers' sounds come from.
#[derive(Clone, Debug)]
pub enum Source {
    /// A shipped ensemble, by name, with its placements and its mixer.
    Ensemble(String),
    /// Three shipped instruments, each at its shipping volume with a flat curve
    /// and no inserts, under the default ensemble's mixer.
    Instruments([String; 3]),
    /// The same voice in every register, with the default mixer and nothing
    /// else. The minimal path, for tests that are about the oscillator rather
    /// than the palette.
    Voice(Box<VoicePatch>),
    /// An ensemble built in the test rather than read from the palette, so that
    /// a rack, a curve or a mixer setting no shipped preset uses can still be
    /// put through the real path.
    Built(Box<Ensemble>),
}

/// Something that happens between two buffers.
pub enum Event {
    /// Start a chord on one stab group.
    Stab {
        group: usize,
        notes: Vec<u8>,
        gain: f32,
        velocity: f32,
    },
    /// Release one stab group.
    Release { group: usize },
    /// Release everything.
    Silence,
    /// Fire the metronome.
    Click {
        strong: bool,
        sound: usize,
        volume: f32,
    },
    /// Change the live parameters, with the whole set in hand.
    ///
    /// Applied on the main thread between two buffers, exactly as the interface
    /// applies one, so a parameter that moves is a parameter that moves in the
    /// program too.
    Tweak(Box<dyn Fn(&SynthParams)>),
}

/// A render, as a list of things to play.
pub struct Config {
    pub sample_rate: f32,
    pub channels: usize,
    pub block: usize,
    pub frames: usize,
    pub source: Source,
    events: Vec<(usize, Event)>,
}

impl Config {
    fn new(source: Source) -> Self {
        Config {
            sample_rate: 48_000.0,
            channels: 2,
            block: 512,
            // A shade over a second, rounded up to `MAX_BLOCK` so every block
            // size below it divides the length exactly.
            frames: 12 * MAX_BLOCK,
            source,
            events: Vec::new(),
        }
    }

    /// A shipped ensemble, by name.
    pub fn ensemble(name: &str) -> Self {
        Config::new(Source::Ensemble(name.to_string()))
    }

    /// Three shipped instruments, one per register.
    pub fn instruments(names: [&str; 3]) -> Self {
        Config::new(Source::Instruments(names.map(str::to_string)))
    }

    /// One voice in every register.
    pub fn voice(voice: VoicePatch) -> Self {
        Config::new(Source::Voice(Box::new(voice)))
    }

    /// An ensemble built here rather than shipped.
    pub fn built(ensemble: Ensemble) -> Self {
        Config::new(Source::Built(Box::new(ensemble)))
    }

    pub fn rate(mut self, hz: f32) -> Self {
        self.sample_rate = hz;
        self
    }

    pub fn channels(mut self, n: usize) -> Self {
        self.channels = n;
        self
    }

    pub fn block(mut self, frames: usize) -> Self {
        self.block = frames;
        self.frames = self.frames.div_ceil(frames) * frames;
        self
    }

    /// How long to render, rounded up to a whole number of blocks.
    pub fn seconds(mut self, secs: f32) -> Self {
        self.frames = (secs * self.sample_rate / self.block as f32).ceil() as usize * self.block;
        self
    }

    /// How long to render, in frames, rounded up to a whole number of blocks.
    pub fn frames(mut self, frames: usize) -> Self {
        self.frames = frames.div_ceil(self.block) * self.block;
        self
    }

    /// Do something at a frame.
    pub fn at(mut self, frame: usize, event: Event) -> Self {
        self.events.push((frame, event));
        self
    }

    /// Silence the two registers either side of the middle one.
    ///
    /// One note is voiced as itself with an octave below it and an octave above
    /// it — `allocate` spreads a chord across the three registers — so a test
    /// about *the note* has to ask for the middle register alone.
    pub fn middle_register_only(mut self) -> Self {
        self.events.insert(
            0,
            (
                0,
                Event::Tweak(Box::new(|p: &SynthParams| {
                    p.low.volume.set(0.0);
                    p.high.volume.set(0.0);
                })),
            ),
        );
        self
    }

    /// Hold a chord for the first half of the render and release it.
    ///
    /// The onset is at frame zero, so every block size hears it at the same
    /// instant, and the release is put on a `MAX_BLOCK` boundary for the same
    /// reason.
    pub fn chord(mut self, notes: &[u8]) -> Self {
        let release = (self.frames / 2) / MAX_BLOCK * MAX_BLOCK;
        self.events.push((
            0,
            Event::Stab {
                group: 0,
                notes: notes.to_vec(),
                gain: 1.0,
                velocity: 1.0,
            },
        ));
        self.events.push((release, Event::Release { group: 0 }));
        self
    }
}

// -----------------------------------------------------------------------------
// The worst case
// -----------------------------------------------------------------------------

/// An effect of a kind, with every declared parameter at the top of its range —
/// except `mix`, which stays where the kind put it.
///
/// "Turned all the way up" is not the same as "most expensive" — a shorter delay
/// line is cheaper than a longer one while a faster LFO is dearer than a slow
/// one — but it is exactly reproducible, it reaches the end of every range the
/// panel can set, and every branch an effect has is taken somewhere.
///
/// `mix` is the exception because a fully wet reverb with its predelay at 120 ms
/// and a fully wet delay with its time at two seconds both, correctly, produce
/// nothing at all for the first tenth of a second: an insert with no dry path
/// and a tail that has not arrived yet is silent. That is a true reading of the
/// effect and a useless stress case, so the wet/dry stays balanced and the rest
/// is pushed to the stops.
pub fn maxed(kind: FxKind) -> Fx {
    let subtype = kind
        .subtypes()
        .first()
        .copied()
        .unwrap_or(chord_tool::fx::FxSubtype::SubtypeNone);
    let mut fx = Fx::variant(kind, subtype);
    let mut index = 0;
    while let Some(spec) = fx.spec(index) {
        if spec.label != "mix" {
            fx.set_param(index, spec.range.1);
        }
        index += 1;
    }
    fx
}

/// The six kinds that keep the most state, in the order a rack would hold them.
const WORST_RACK: [FxKind; 6] = [
    FxKind::Reverb,
    FxKind::Delay,
    FxKind::Chorus,
    FxKind::Flanger,
    FxKind::Phaser,
    FxKind::Distortion,
];

/// The Default ensemble with everything the player can turn on, turned on.
///
/// Every one of the eighteen insert slots is filled, every band of every
/// register's curve is at an end of its range, both sends are wide open and both
/// aux units are running. This is the most the bus can be asked to do in one
/// sample, and it is a configuration the panels can actually reach.
pub fn worst_case_ensemble() -> Ensemble {
    let mut ensemble = ensembles()
        .iter()
        .find(|e| e.name == "Default")
        .expect("the shipped palette has a Default ensemble")
        .clone();
    ensemble.name = "Worst case".to_string();

    for placement in [&mut ensemble.low, &mut ensemble.mid, &mut ensemble.high] {
        placement.chain = WORST_RACK.iter().map(|kind| maxed(*kind)).collect();
        placement.eq = EqCurve {
            gains: std::array::from_fn(|i| {
                if i % 2 == 0 {
                    chord_tool::eq::GAIN_RANGE.1
                } else {
                    chord_tool::eq::GAIN_RANGE.0
                }
            }),
        };
        placement.reverb_send = 1.0;
        placement.delay_send = 1.0;
    }

    ensemble.mixer.reverb = maxed(FxKind::Reverb);
    ensemble.mixer.delay = maxed(FxKind::Delay);
    ensemble.mixer.reverb_mix = 1.0;
    ensemble.mixer.delay_mix = 1.0;
    ensemble.mixer.master_volume = 7.0;
    ensemble.mixer.note_length = 1.0;
    ensemble
}

/// A chord wide enough that every voice in a register's block is used.
pub const WIDE: [u8; 6] = [48, 55, 60, 64, 67, 72];

/// Fill the whole voice pool: every stab group plus the audition group, at four
/// voices of unison each, on the wide chord.
///
/// The pool is `(STAB_GROUPS + 1) * (1 + 4 + 1) * UNISON_MAX + 1 = 121`; this
/// asks for 120 of them and leaves the metronome click on the last.
pub fn fill_the_pool(config: Config) -> Config {
    let mut config = config.at(
        0,
        Event::Tweak(Box::new(|p: &SynthParams| {
            for channel in [&p.low, &p.mid, &p.high] {
                channel.unison.set(4.0);
                channel.detune.set(25.0);
            }
        })),
    );
    for group in 0..=chord_tool::synth::AUDITION_GROUP {
        config = config.at(
            0,
            Event::Stab {
                group,
                notes: WIDE.to_vec(),
                gain: 0.9,
                velocity: 1.0,
            },
        );
    }
    config
}

/// Ticks to frames at a tempo.
///
/// The transport counts in ticks of `PPQ` per quarter note, so a frame is
/// `sample_rate * 60 / (tempo * PPQ)` of a tick.
pub fn ticks_to_frames(ticks: u64, tempo: f32, sample_rate: f32) -> usize {
    (ticks as f64 * sample_rate as f64 * 60.0 / (tempo as f64 * PPQ as f64)).round() as usize
}

// -----------------------------------------------------------------------------
// Rendering
// -----------------------------------------------------------------------------

/// One render, deinterleaved.
pub struct Render {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub sample_rate: f32,
    /// What was rendered, for a failure message that says which config broke.
    pub label: String,
}

/// Render a config through the real engine.
///
/// The parameters are built once, before the engine, and handed to both: the
/// `SharedF32`s inside them are the same atomics the audio thread reads, so a
/// [`Event::Tweak`] is a real parameter change and not a copy of one.
pub fn render(config: Config) -> Render {
    let mut prepared = prepare(config);
    prepared.run()
}

/// A config that has been built into an engine, ready to run.
///
/// Split out from [`render`] so that something can be measured *around* the
/// loop rather than through it — the allocation counter is the reason it exists:
/// building a render allocates, by design, and running one must not.
pub struct Prepared {
    /// The engine the audio callback would own.
    pub engine: Engine,
    /// The handles the interface thread would own.
    pub voices: Voices,
    /// The live parameters, shared with the engine.
    pub params: SynthParams,
    /// The buffer size the render will run at.
    pub block: usize,
    /// Interleaved channels per frame.
    pub channels: usize,
    /// How many frames the render will be.
    pub frames: usize,
    events: Vec<(usize, Event)>,
    sample_rate: f32,
    label: String,
}

/// Build a config into an engine, a voice pool and a live parameter set.
pub fn prepare(config: Config) -> Prepared {
    assert!(config.block > 0 && config.block <= MAX_BLOCK, "block size");
    assert_eq!(
        config.frames % config.block,
        0,
        "frames must divide by block"
    );
    assert!(config.channels >= 2, "the engine writes a stereo pair");

    let (channels, mixer, label) = resolve(&config.source);
    let params = SynthParams::defaults();
    synth::apply_channels(&params, [&channels[0], &channels[1], &channels[2]], &mixer);

    let (engine, voices) = Engine::build(
        &params,
        config.sample_rate,
        config.channels,
        Callback::shared(),
    );

    let mut events = config.events;
    events.sort_by_key(|(frame, _)| *frame);

    Prepared {
        engine,
        voices,
        params,
        events,
        block: config.block,
        channels: config.channels,
        frames: config.frames,
        sample_rate: config.sample_rate,
        label,
    }
}

impl Prepared {
    /// How long the render will be, in frames.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// What the engine's own instrumentation says it has done.
    pub fn timing(&self) -> &Arc<Callback> {
        self.engine.timing()
    }

    /// Apply every scheduled event now, ignoring when it was due.
    ///
    /// For a test that wants the engine in its loudest state *before* it starts
    /// measuring rather than partway through — the allocation test is the one
    /// that wants this, because what it measures is the callback and not the
    /// interface.
    pub fn trigger_everything(&mut self) {
        for (_, event) in &self.events {
            apply(event, &self.voices, &self.params);
        }
    }

    /// Play everything and collect the samples.
    ///
    /// Takes `&mut self` rather than consuming, so that a caller can hold the
    /// engine afterwards and drive it a buffer at a time.
    pub fn run(&mut self) -> Render {
        let mut next = 0;
        let mut buffer = vec![0.0f32; self.block * self.channels];
        let mut left = Vec::with_capacity(self.frames);
        let mut right = Vec::with_capacity(self.frames);

        let mut start = 0;
        while start < self.frames {
            while next < self.events.len() && self.events[next].0 < start + self.block {
                apply(&self.events[next].1, &self.voices, &self.params);
                next += 1;
            }
            buffer.fill(0.0);
            self.engine.process(&mut buffer, &self.params, None);
            for frame in buffer.chunks_exact(self.channels) {
                left.push(frame[0]);
                right.push(frame[1]);
            }
            start += self.block;
        }

        Render {
            left,
            right,
            sample_rate: self.sample_rate,
            label: self.label.clone(),
        }
    }
}

fn apply(event: &Event, voices: &Voices, params: &SynthParams) {
    match event {
        Event::Stab {
            group,
            notes,
            gain,
            velocity,
        } => voices.play_stab(*group, notes, *gain, *velocity, params),
        Event::Release { group } => voices.stop_stab(*group),
        Event::Silence => voices.silence(),
        Event::Click {
            strong,
            sound,
            volume,
        } => voices.play_click(*strong, *sound, *volume),
        Event::Tweak(f) => f(params),
    }
}

/// The shipped library, parsed once per test binary.
///
/// It is compiled-in TOML, so parsing it is pure, and the sweeps below would
/// otherwise parse a hundred and fifty instruments once per render — which in a
/// debug build costs more than the audio does.
fn instruments() -> &'static [chord_tool::instrument::Instrument] {
    static LIBRARY: std::sync::OnceLock<Vec<chord_tool::instrument::Instrument>> =
        std::sync::OnceLock::new();
    LIBRARY.get_or_init(builtin_instruments)
}

/// The shipped palette, parsed once per test binary.
fn ensembles() -> &'static [chord_tool::ensemble::Ensemble] {
    static PALETTE: std::sync::OnceLock<Vec<chord_tool::ensemble::Ensemble>> =
        std::sync::OnceLock::new();
    PALETTE.get_or_init(builtin_ensembles)
}

/// Turn a source into the three channels the audio layer takes.
fn resolve(source: &Source) -> ([ComposedChannel; 3], MixerSettings, String) {
    let find = |name: &str| {
        instruments()
            .iter()
            .find(|i| i.name == name)
            .map(|i| i.voice.clone())
    };
    let default_mixer = || {
        ensembles()
            .iter()
            .find(|e| e.name == "Default")
            .expect("the shipped palette has a Default ensemble")
            .mixer
            .clone()
    };

    match source {
        Source::Ensemble(name) => {
            let ensemble = ensembles()
                .iter()
                .find(|e| e.name == *name)
                .unwrap_or_else(|| panic!("no shipped ensemble named {name:?}"));
            let (channels, missing) = ensemble.resolve(find);
            assert!(
                missing.is_empty(),
                "{name} names instruments that are not in the library: {missing:?}"
            );
            (channels, ensemble.mixer.clone(), name.clone())
        }
        Source::Instruments(names) => {
            let channels = std::array::from_fn(|i| {
                let name = names[i].as_str();
                let voice =
                    find(name).unwrap_or_else(|| panic!("no shipped instrument named {name:?}"));
                ComposedChannel::compose(voice, &Placement::neutral(name))
            });
            (channels, default_mixer(), names.join(" / "))
        }
        Source::Voice(voice) => {
            let channels = std::array::from_fn(|_| {
                ComposedChannel::compose((**voice).clone(), &Placement::neutral("Voice"))
            });
            (channels, default_mixer(), "Voice".to_string())
        }
        Source::Built(ensemble) => {
            let (channels, missing) = ensemble.resolve(find);
            assert!(missing.is_empty(), "built ensemble: {missing:?}");
            (channels, ensemble.mixer.clone(), ensemble.name.clone())
        }
    }
}

// -----------------------------------------------------------------------------
// Looking at a render
// -----------------------------------------------------------------------------

/// The `count` strongest frequencies, strongest first.
///
/// A diagnostic, not an assertion: when a peak moves, what it moved *to* is
/// usually the whole story.
pub fn top_peaks(mags: &[f32], bin_hz: f32, count: usize) -> Vec<(f32, f32)> {
    let first = ((20.0 / bin_hz) as usize).max(2);
    let mut peaks: Vec<(usize, f32)> = (first..mags.len() - 1)
        .filter(|k| mags[*k] > mags[k - 1] && mags[*k] >= mags[k + 1])
        .map(|k| (k, mags[k]))
        .collect();
    peaks.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    peaks
        .into_iter()
        .take(count)
        .map(|(k, m)| (interpolate(mags, k, bin_hz), m))
        .collect()
}

/// The five numbers a render is fingerprinted by.
///
/// A hash is the strictest possible statement — the same bytes or nothing — but
/// it is only meaningful on the machine that recorded it. These are the portable
/// half: if the sound has moved, one of them has moved with it, and a tolerance
/// can say by how much.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Features {
    /// The strongest frequency, interpolated between bins, in Hz.
    pub peak_hz: f32,
    /// The energy-weighted mean frequency, in Hz.
    pub centroid_hz: f32,
    /// Overall level.
    pub rms: f32,
    /// Peak over RMS: 1.41 for a sine, much higher for a click.
    pub crest: f32,
    /// How long the sound takes to fall 30 dB below its loudest moment.
    pub decay_secs: f32,
}

impl std::fmt::Display for Features {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "peak {:.2} Hz, centroid {:.1} Hz, rms {:.6}, crest {:.3}, decay {:.3} s",
            self.peak_hz, self.centroid_hz, self.rms, self.crest, self.decay_secs
        )
    }
}

impl Features {
    /// Assert two feature sets agree to a fraction of themselves.
    ///
    /// Relative rather than absolute, because these are levels and frequencies:
    /// two cents of pitch is a different number of Hz at the bottom of the
    /// keyboard than at the top.
    pub fn assert_close(&self, other: &Features, tolerance: f32, what: &str) {
        let close = |a: f32, b: f32| (a - b).abs() <= tolerance * a.abs().max(b.abs()).max(1e-6);
        assert!(
            close(self.peak_hz, other.peak_hz)
                && close(self.centroid_hz, other.centroid_hz)
                && close(self.rms, other.rms)
                && close(self.crest, other.crest)
                && close(self.decay_secs, other.decay_secs),
            "{what}: {self} vs {other}, beyond {:.1}%",
            tolerance * 100.0
        );
    }
}

impl Render {
    pub fn frames(&self) -> usize {
        self.left.len()
    }

    /// The stereo pair summed to one channel.
    pub fn mono(&self) -> Vec<f32> {
        self.left
            .iter()
            .zip(&self.right)
            .map(|(l, r)| (l + r) * 0.5)
            .collect()
    }

    pub fn sample(&self, channel: usize, frame: usize) -> f32 {
        if channel == 0 {
            self.left[frame]
        } else {
            self.right[frame]
        }
    }

    pub fn peak(&self) -> f32 {
        self.left
            .iter()
            .chain(&self.right)
            .fold(0.0f32, |m, s| m.max(s.abs()))
    }

    pub fn rms(&self) -> f32 {
        let sum: f64 = self
            .left
            .iter()
            .chain(&self.right)
            .map(|s| (*s as f64) * (*s as f64))
            .sum();
        (sum / (2 * self.frames().max(1)) as f64).sqrt() as f32
    }

    /// How many samples are not finite.
    ///
    /// A NaN in the output is not a subtle bug: it is a signal that will be
    /// silent on one device and a full-scale burst of noise on another.
    pub fn non_finite(&self) -> usize {
        self.left
            .iter()
            .chain(&self.right)
            .filter(|s| !s.is_finite())
            .count()
    }

    /// The largest absolute value anywhere, as a check against clipping.
    pub fn overshoot(&self) -> f32 {
        self.peak()
    }

    /// The largest absolute value between two frames.
    pub fn peak_between(&self, from: usize, to: usize) -> f32 {
        (from..to.min(self.frames()))
            .map(|i| self.left[i].abs().max(self.right[i].abs()))
            .fold(0.0f32, f32::max)
    }

    /// Every frame in a range is exactly zero.
    pub fn is_silent(&self, from: usize, to: usize) -> bool {
        (from..to.min(self.frames())).all(|i| self.left[i] == 0.0 && self.right[i] == 0.0)
    }

    /// An FNV-1a hash of every sample's bits.
    ///
    /// Over the bit patterns rather than a rounding of the values, because the
    /// question this answers is "is this the *same* render", and the answer to
    /// that is allowed to be no.
    pub fn hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for sample in self.left.iter().chain(&self.right) {
            for byte in sample.to_bits().to_le_bytes() {
                h ^= byte as u64;
                h = h.wrapping_mul(0x1000_0000_01b3);
            }
        }
        h
    }

    /// The five features, over the middle of the render.
    ///
    /// The middle rather than the whole thing: the first few milliseconds are
    /// the attack — a click, not the sound — and the tail is whatever the
    /// release left behind, and neither says what the patch sounds like.
    pub fn features(&self) -> Features {
        // Inside the sustain: `.chord` releases at the halfway point, so a
        // window that reached past it would be describing two different sounds.
        let window = 16_384.min(self.frames() / 4).max(1024);
        let from = (self.frames() / 8).min(self.frames().saturating_sub(window));
        let slice = &self.mono()[from..from + window];
        // Zero-padded four times over. The transform's own resolution is the
        // window length, which at 16 k points is 2.9 Hz — thirty cents in the
        // bass, and nowhere near enough to say a note has not moved. Padding
        // samples the same transform more finely, and the parabolic fit on the
        // oversampled main lobe recovers the rest.
        let (mags, bin_hz) = spectrum(slice, self.sample_rate, 4);
        Features {
            peak_hz: peak_hz(&mags, bin_hz),
            centroid_hz: centroid_hz(&mags, bin_hz),
            rms: self.rms(),
            crest: self.peak() / self.rms().max(1e-9),
            decay_secs: self.decay_secs(),
        }
    }

    /// The `count` strongest frequencies, strongest first.
    ///
    /// A diagnostic, not an assertion: when a peak moves, what it moved *to* is
    /// usually the whole story.
    pub fn top_peaks(&self, count: usize) -> Vec<(f32, f32)> {
        let window = 16_384.min(self.frames() / 4).max(1024);
        let from = (self.frames() / 8).min(self.frames().saturating_sub(window));
        let slice = &self.mono()[from..from + window];
        let (mags, bin_hz) = spectrum(slice, self.sample_rate, 4);
        top_peaks(&mags, bin_hz, count)
    }

    /// How long the sound takes to fall 30 dB below its loudest 10 ms.
    pub fn decay_secs(&self) -> f32 {
        let mono = self.mono();
        let window = (self.sample_rate * 0.01) as usize;
        if window == 0 || mono.len() < window * 2 {
            return 0.0;
        }
        let levels: Vec<f32> = mono
            .chunks_exact(window)
            .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt())
            .collect();
        let peak = levels.iter().cloned().fold(0.0f32, f32::max);
        if peak <= 0.0 {
            return 0.0;
        }
        let loudest = levels.iter().position(|l| *l >= peak).unwrap_or(0);
        let floor = peak * 0.0316;
        match levels[loudest..].iter().position(|l| *l < floor) {
            Some(offset) => offset as f32 * 0.01,
            None => (levels.len() - loudest) as f32 * 0.01,
        }
    }

    /// Write the render as a 16-bit stereo WAV.
    ///
    /// The escape hatch: when a change sounds wrong and no assertion says why,
    /// `CHORD_TOOL_RENDER=/tmp/out.wav` is how it gets listened to. Audio files
    /// do not belong in the repository, so nothing writes one unless asked.
    pub fn write_wav(&self, path: &Path) -> io::Result<()> {
        let frames = self.frames() as u32;
        let bytes = frames * 4;
        let mut out = Vec::with_capacity(44 + bytes as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + bytes).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&2u16.to_le_bytes()); // channels
        out.extend_from_slice(&(self.sample_rate as u32).to_le_bytes());
        out.extend_from_slice(&((self.sample_rate as u32) * 4).to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&bytes.to_le_bytes());
        for frame in 0..self.frames() {
            for channel in 0..2 {
                let s = self.sample(channel, frame).clamp(-1.0, 1.0);
                out.extend_from_slice(&((s * 32767.0) as i16).to_le_bytes());
            }
        }
        let mut file = std::fs::File::create(path)?;
        file.write_all(&out)
    }
}

/// Write a render to `CHORD_TOOL_RENDER`, if it is set.
///
/// Called at the end of every render, so a failing render can be listened to by
/// re-running the one test with the variable set — no code to add and nothing to
/// remember to take out.
pub fn maybe_dump(render: &Render, name: &str) {
    if let Ok(path) = std::env::var("CHORD_TOOL_RENDER") {
        let path = Path::new(&path);
        let target = if path.extension().is_some() {
            path.to_path_buf()
        } else {
            path.join(format!("{name}.wav"))
        };
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        render
            .write_wav(&target)
            .unwrap_or_else(|e| panic!("writing {}: {e}", target.display()));
        eprintln!(
            "wrote {} ({} frames, {:.2} s)",
            target.display(),
            render.frames(),
            render.frames() as f32 / render.sample_rate
        );
    }
}

// -----------------------------------------------------------------------------
// Watching the allocator
// -----------------------------------------------------------------------------
//
// The audio callback must not allocate, and there is no way to observe an
// allocation without standing in front of the global allocator. This is that
// hook, shared by the test binaries that need it.
//
// It is the one `unsafe` in the repository and it is in a test: the library's
// `#![forbid(unsafe_code)]` is about the program, and a hook around `malloc` is
// not. Every method forwards to `System` with the arguments it was given;
// nothing here changes what the allocator does, and the counters live in
// `const`-initialised thread-locals so that reading one cannot itself allocate.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    /// Whether this thread is watching.
    static ARMED: Cell<bool> = const { Cell::new(false) };
    /// Allocations and frees this thread has made since arming.
    static PASSES: Cell<usize> = const { Cell::new(0) };
}

/// Note one pass through the allocator, if this thread is watching.
///
/// Frees are counted as well as allocations. Inside the callback neither should
/// ever happen: a buffer allocated and freed once per buffer is exactly as
/// unacceptable as one that is only allocated.
fn note_allocator_pass() {
    let _ = ARMED.try_with(|armed| {
        if armed.get() {
            let _ = PASSES.try_with(|count| count.set(count.get() + 1));
        }
    });
}

// SAFETY: see the note above — every method forwards to `System` unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_allocator_pass();
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note_allocator_pass();
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_allocator_pass();
        System.realloc(ptr, layout, new_size)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_allocator_pass();
        System.alloc_zeroed(layout)
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Run `f` and report how many times this thread went through the allocator.
///
/// Per thread, so the rest of a parallel test suite — and the test harness
/// itself — can carry on allocating while one test watches its own thread.
pub fn count_allocations<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let was_armed = ARMED.with(|armed| armed.replace(true));
    PASSES.with(|count| count.set(0));
    let value = f();
    let passes = PASSES.with(Cell::get);
    ARMED.with(|armed| armed.set(was_armed));
    (value, passes)
}

// -----------------------------------------------------------------------------
// A little spectral analysis
// -----------------------------------------------------------------------------

/// The magnitude spectrum of `samples`, Hann-windowed and zero-padded.
///
/// Hand-rolled rather than pulled in: the crate has no FFT dependency, these are
/// eight numbers in a test, and an FFT is thirty lines. Deterministic, which is
/// the point — a feature that varies between runs cannot be a gate.
pub fn spectrum(samples: &[f32], sample_rate: f32, pad: usize) -> (Vec<f32>, f32) {
    let n = (samples.len() * pad.max(1)).next_power_of_two().max(2);
    let mut re = vec![0.0f32; n];
    let mut im = vec![0.0f32; n];
    for (i, &s) in samples.iter().enumerate() {
        let phase = 2.0 * std::f32::consts::PI * i as f32 / samples.len() as f32;
        re[i] = s * (0.5 - 0.5 * phase.cos());
    }
    fft(&mut re, &mut im);
    let mags = (0..n / 2)
        .map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt())
        .collect();
    (mags, sample_rate / n as f32)
}

/// In-place iterative radix-2 FFT. `re.len()` must be a power of two.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * std::f32::consts::PI / len as f32;
        let (wr, wi) = (angle.cos(), angle.sin());
        let mut start = 0;
        while start < n {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (ur, ui) = (re[start + k], im[start + k]);
                let (xr, xi) = (re[start + k + len / 2], im[start + k + len / 2]);
                let (vr, vi) = (xr * cr - xi * ci, xr * ci + xi * cr);
                re[start + k] = ur + vr;
                im[start + k] = ui + vi;
                re[start + k + len / 2] = ur - vr;
                im[start + k + len / 2] = ui - vi;
                let next = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = next;
            }
            start += len;
        }
        len <<= 1;
    }
}

/// The strongest frequency, interpolated between the three bins around it.
///
/// A raw bin index is only as good as the window length — 2.9 Hz at 48 kHz and
/// 16 k points, which is 30 cents in the bass. Fitting a parabola through the
/// three magnitudes of the peak recovers most of a bin, and that is what makes a
/// sample-rate comparison about the pitch rather than about the transform.
pub fn peak_hz(mags: &[f32], bin_hz: f32) -> f32 {
    if mags.len() < 3 {
        return 0.0;
    }
    let first = ((20.0 / bin_hz) as usize).max(2);
    let mut best = first;
    for k in first..mags.len() - 1 {
        if mags[k] > mags[best] {
            best = k;
        }
    }
    interpolate(mags, best, bin_hz)
}

/// Where the peak at bin `k` actually is, by fitting a parabola through it and
/// its two neighbours.
///
/// The three points are samples of one lobe, so the vertex of the parabola is
/// where the lobe's centre is — to within a small fraction of a bin, and to
/// whatever precision the lobe was sampled at.
pub fn interpolate(mags: &[f32], k: usize, bin_hz: f32) -> f32 {
    if k == 0 || k + 1 >= mags.len() {
        return k as f32 * bin_hz;
    }
    let (a, b, c) = (mags[k - 1], mags[k], mags[k + 1]);
    let denom = a - 2.0 * b + c;
    let offset = if denom.abs() > 1e-20 {
        (0.5 * (a - c) / denom).clamp(-1.0, 1.0)
    } else {
        0.0
    };
    (k as f32 + offset) * bin_hz
}

/// The energy-weighted mean frequency, above 20 Hz.
///
/// The floor is not decoration: a Hann window on a signal that is still decaying
/// leaks a little into bin zero, and a hundredth of a unit of DC would drag the
/// mean down by hundreds of hertz.
pub fn centroid_hz(mags: &[f32], bin_hz: f32) -> f32 {
    let mut sum = 0.0f64;
    let mut weight = 0.0f64;
    let first = ((20.0 / bin_hz) as usize).max(1);
    for (k, m) in mags.iter().enumerate().skip(first) {
        let power = (*m as f64) * (*m as f64);
        sum += power * (k as f64 * bin_hz as f64);
        weight += power;
    }
    if weight <= 0.0 {
        0.0
    } else {
        (sum / weight) as f32
    }
}

/// Twelve-tone equal temperament: how many cents `hz` is above `reference`.
pub fn cents_above(hz: f32, reference: f32) -> f32 {
    1200.0 * (hz / reference).log2()
}
