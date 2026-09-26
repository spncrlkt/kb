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

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::{Deserialize, Serialize};

use crate::presets::{ChannelPatch, MixerPatch, Patch};

// -----------------------------------------------------------------------------
// Voice pool sizes
// -----------------------------------------------------------------------------

/// Voices in one stab group: one low, up to four mid and one high.
///
/// The widest voicing this tool can produce is six notes, so that covers it.
const GROUP_LOW: usize = 1;
const GROUP_MID: usize = 4;
const GROUP_HIGH: usize = 1;

/// Stab groups, one per overlapping take.
///
/// Shared with the arrangement planner, which never puts two simultaneous hits
/// in the same group while a free one exists — so the number here is the deepest
/// a stack of takes can sound before something has to be cut.
const STAB_GROUPS: usize = crate::arrangement::RHYTHM_LAYERS;

/// The click pool: one voice, used by the metronome.
///
/// Its own voice rather than a stab group, because a click is wanted while a
/// take is being recorded — the very moment the groups are busy with the
/// progression — and it must not retrigger anything the player is hearing.
const CLICK_VOICES: usize = 1;

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

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Waveform {
    Sine = 0,
    Saw = 1,
    Square = 2,
    Triangle = 3,
}

impl Waveform {
    pub const ALL: [Waveform; 4] = [
        Waveform::Sine,
        Waveform::Saw,
        Waveform::Square,
        Waveform::Triangle,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Waveform::Sine => "sine",
            Waveform::Saw => "saw",
            Waveform::Square => "square",
            Waveform::Triangle => "triangle",
        }
    }

    pub fn from_f32(v: f32) -> Self {
        match v as i32 {
            1 => Waveform::Saw,
            2 => Waveform::Square,
            3 => Waveform::Triangle,
            _ => Waveform::Sine,
        }
    }
}

// -----------------------------------------------------------------------------
// ChannelParams / SynthParams
// -----------------------------------------------------------------------------

#[derive(Clone)]
pub struct ChannelParams {
    pub volume: SharedF32,
    pub waveform: SharedF32,
    pub attack: SharedF32,
    pub decay: SharedF32,
    pub sustain: SharedF32,
    pub release: SharedF32,
    pub cutoff: SharedF32,
    pub resonance: SharedF32,
    pub transpose: SharedF32,
    pub reverb_send: SharedF32,
    pub pan: SharedF32,
}

impl ChannelParams {
    fn defaults(volume: f32, cutoff: f32) -> Self {
        ChannelParams {
            volume: SharedF32::new(volume),
            waveform: SharedF32::new(Waveform::Sine as i32 as f32),
            attack: SharedF32::new(0.005),
            decay: SharedF32::new(0.05),
            sustain: SharedF32::new(0.7),
            release: SharedF32::new(0.1),
            cutoff: SharedF32::new(cutoff),
            resonance: SharedF32::new(0.2),
            transpose: SharedF32::new(0.0),
            reverb_send: SharedF32::new(0.0),
            pan: SharedF32::new(0.0),
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
    /// name is historical — `patches.toml` still stores it as `reverb_mix`, and
    /// renaming the key would break saved patches.
    pub reverb_mix: SharedF32,
    pub reverb_size: SharedF32,
    pub master_volume: SharedF32,
    pub master_mute: SharedF32,
    pub mute_progression: SharedF32,
    pub preview_fade: SharedF32,
}

impl SynthParams {
    pub fn defaults() -> Self {
        SynthParams {
            low: ChannelParams::defaults(4.0, 4000.0),
            mid: ChannelParams::defaults(4.0, 4000.0),
            high: ChannelParams::defaults(4.0, 4000.0),
            reverb_mix: SharedF32::new(0.0),
            reverb_size: SharedF32::new(0.5),
            master_volume: SharedF32::new(5.0),
            master_mute: SharedF32::new(0.0),
            mute_progression: SharedF32::new(0.0),
            preview_fade: SharedF32::new(0.03),
        }
    }
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

impl Voice {
    fn new(channel: ChannelParams, sample_rate: f32) -> Self {
        Voice {
            gate: SharedF32::new(0.0),
            midi_note: SharedF32::new(60.0),
            glide_from: SharedF32::new(60.0),
            glide_duration_secs: SharedF32::new(0.0),
            hold_secs: SharedF32::new(0.0),
            release_override: SharedF32::new(0.0),
            gain: SharedF32::new(1.0),
            channel,
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

    fn handle(&self) -> VoiceHandle {
        VoiceHandle {
            gate: self.gate.clone(),
            midi_note: self.midi_note.clone(),
            glide_from: self.glide_from.clone(),
            glide_duration_secs: self.glide_duration_secs.clone(),
            hold_secs: self.hold_secs.clone(),
            release_override: self.release_override.clone(),
            gain: self.gain.clone(),
        }
    }

    fn tick(&mut self) -> f32 {
        let dt = 1.0 / self.sample_rate;
        let gate_on = self.gate.get() > 0.5;

        let prev_state = self.env_state;

        // ---- envelope ----
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

        // Reset glide + elapsed when a new trigger fires.
        if prev_state != EnvState::Attack && self.env_state == EnvState::Attack {
            self.glide_pos = 0.0;
            self.elapsed = 0.0;
        }

        if self.env_state == EnvState::Idle {
            return 0.0;
        }

        // ---- glide / pitch ----
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
        };

        let input = osc * self.env_value;

        // ---- resonant lowpass (Chamberlin SVF) ----
        let cutoff = self.channel.cutoff.get().clamp(20.0, self.sample_rate * 0.4);
        let resonance = self.channel.resonance.get().clamp(0.0, 0.99);

        let f = 2.0 * (PI * cutoff / self.sample_rate).sin();
        let q = 1.0 - resonance;

        let low = self.svf_low + f * self.svf_band;
        let high = input - low - q * self.svf_band;
        let band = f * high + self.svf_band;
        self.svf_low = low;
        self.svf_band = band;

        // Gain last, so one take sitting quieter also feeds less reverb.
        low * self.gain.get().clamp(0.0, 1.0)
    }
}

// -----------------------------------------------------------------------------
// VoiceHandle
// -----------------------------------------------------------------------------

struct VoiceHandle {
    gate: SharedF32,
    midi_note: SharedF32,
    glide_from: SharedF32,
    glide_duration_secs: SharedF32,
    hold_secs: SharedF32,
    release_override: SharedF32,
    gain: SharedF32,
}

impl VoiceHandle {
    fn reset(&self) {
        self.release_override.set(0.0);
        self.glide_duration_secs.set(0.0);
        self.gate.set(0.0);
    }

    /// Instant pitch change at a given level: no glide.
    fn trigger_at(&self, note: u8, gain: f32) {
        self.release_override.set(0.0);
        self.glide_duration_secs.set(0.0);
        self.hold_secs.set(0.0);
        self.gain.set(gain.clamp(0.0, 1.0));
        self.glide_from.set(note as f32);
        self.midi_note.set(note as f32);
        self.gate.set(1.0);
    }

    /// Glide from a start pitch to a target pitch over `duration_secs`,
    /// then hold for `hold_secs` before auto-releasing.
    fn trigger_glide(&self, from: u8, to: u8, duration_secs: f32, hold_secs: f32) {
        self.release_override.set(0.0);
        self.gain.set(1.0);
        self.glide_from.set(from as f32);
        self.midi_note.set(to as f32);
        self.glide_duration_secs.set(duration_secs);
        self.hold_secs.set(hold_secs);
        self.gate.set(1.0);
    }
}

// -----------------------------------------------------------------------------
// Reverb
// -----------------------------------------------------------------------------

const COMB_DELAYS: [usize; 4] = [1557, 1617, 1491, 1422];
const ALLPASS_DELAYS: [usize; 2] = [225, 556];

struct Comb {
    buffer: Vec<f32>,
    index: usize,
    damp_store: f32,
}

impl Comb {
    fn new(delay: usize) -> Self {
        Comb {
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
}

struct Reverb {
    combs: [Comb; 4],
    allpasses: [Allpass; 2],
}

impl Reverb {
    fn new() -> Self {
        Reverb {
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
        }
    }

    fn tick(&mut self, input: f32, size: f32) -> f32 {
        let feedback = 0.7 + size.clamp(0.0, 1.0) * 0.28;
        let damp = 0.2;

        let mut out = 0.0;
        for c in self.combs.iter_mut() {
            out += c.tick(input, feedback, damp);
        }
        out /= self.combs.len() as f32;
        out *= 1.0 - feedback;

        for ap in self.allpasses.iter_mut() {
            out = ap.tick(out, 0.5);
        }
        out
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
fn build_group(params: &SynthParams, sample_rate: f32) -> (StabGroup, GroupVoices) {
    let mut handles_low = Vec::with_capacity(GROUP_LOW);
    let mut handles_mid = Vec::with_capacity(GROUP_MID);
    let mut handles_high = Vec::with_capacity(GROUP_HIGH);
    let mut audio_low = Vec::with_capacity(GROUP_LOW);
    let mut audio_mid = Vec::with_capacity(GROUP_MID);
    let mut audio_high = Vec::with_capacity(GROUP_HIGH);

    for _ in 0..GROUP_LOW {
        let v = Voice::new(params.low.clone(), sample_rate);
        handles_low.push(v.handle());
        audio_low.push(v);
    }
    for _ in 0..GROUP_MID {
        let v = Voice::new(params.mid.clone(), sample_rate);
        handles_mid.push(v.handle());
        audio_mid.push(v);
    }
    for _ in 0..GROUP_HIGH {
        let v = Voice::new(params.high.clone(), sample_rate);
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

pub struct Synth {
    params: SynthParams,
    /// One per stab group: an onset retriggers only its own group, so
    /// overlapping takes layer instead of cutting each other.
    groups: Vec<StabGroup>,
    click: Vec<VoiceHandle>,
    _stream: cpal::Stream,
}

impl Synth {
    pub fn new(peak_tap: Option<Arc<AtomicU32>>) -> Result<Self, Box<dyn Error>> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no audio output device available")?;
        let config = device.default_output_config()?;
        let sample_rate = config.sample_rate().0 as f32;
        let channels = config.channels() as usize;

        let params = SynthParams::defaults();

        let mut groups = Vec::with_capacity(STAB_GROUPS);
        let mut audio_groups: Vec<GroupVoices> = Vec::with_capacity(STAB_GROUPS);
        for _ in 0..STAB_GROUPS {
            let (group, voices) = build_group(&params, sample_rate);
            groups.push(group);
            audio_groups.push(voices);
        }

        let mut audio_click: Vec<Voice> = Vec::with_capacity(CLICK_VOICES);
        let mut click = Vec::with_capacity(CLICK_VOICES);
        for _ in 0..CLICK_VOICES {
            // The mid channel, because a click sits in the range the player is
            // already listening to.
            let v = Voice::new(params.mid.clone(), sample_rate);
            click.push(v.handle());
            audio_click.push(v);
        }

        let mut reverb = Reverb::new();
        let stream_params = params.clone();
        let err_fn = |err| eprintln!("audio stream error: {}", err);

        let stream = device.build_output_stream(
            &config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let master_muted = stream_params.master_mute.get() > 0.5;
                let master_gain = if master_muted {
                    0.0
                } else {
                    (stream_params.master_volume.get() / 7.0).clamp(0.0, 1.0)
                };

                let low_gain = (stream_params.low.volume.get() / 7.0).clamp(0.0, 1.0);
                let mid_gain = (stream_params.mid.volume.get() / 7.0).clamp(0.0, 1.0);
                let high_gain = (stream_params.high.volume.get() / 7.0).clamp(0.0, 1.0);

                let low_pan = stream_params.low.pan.get().clamp(-1.0, 1.0);
                let mid_pan = stream_params.mid.pan.get().clamp(-1.0, 1.0);
                let high_pan = stream_params.high.pan.get().clamp(-1.0, 1.0);

                let low_l = (1.0 - low_pan) * 0.5;
                let low_r = (1.0 + low_pan) * 0.5;
                let mid_l = (1.0 - mid_pan) * 0.5;
                let mid_r = (1.0 + mid_pan) * 0.5;
                let high_l = (1.0 - high_pan) * 0.5;
                let high_r = (1.0 + high_pan) * 0.5;

                let low_send = stream_params.low.reverb_send.get().clamp(0.0, 1.0);
                let mid_send = stream_params.mid.reverb_send.get().clamp(0.0, 1.0);
                let high_send = stream_params.high.reverb_send.get().clamp(0.0, 1.0);

                let reverb_mix = stream_params.reverb_mix.get().clamp(0.0, 1.0);
                let reverb_size = stream_params.reverb_size.get().clamp(0.0, 1.0);
                let prog_mute = if stream_params.mute_progression.get() > 0.5 {
                    0.0
                } else {
                    1.0
                };

                for frame in data.chunks_mut(channels) {
                    let mut prog_l = 0.0;
                    let mut prog_m = 0.0;
                    let mut prog_h = 0.0;
                    let mut click_out = 0.0;

                    for group in audio_groups.iter_mut() {
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
                    for v in audio_click.iter_mut() {
                        click_out += v.tick();
                    }

                    let low_out = prog_l * prog_mute * low_gain;
                    let mid_out = (prog_m * prog_mute * mid_gain) + click_out;
                    let high_out = prog_h * prog_mute * high_gain;

                    let left = low_out * low_l + mid_out * mid_l + high_out * high_l;
                    let right = low_out * low_r + mid_out * mid_r + high_out * high_r;

                    let reverb_in =
                        (low_out * low_send + mid_out * mid_send + high_out * high_send) * 0.3;
                    let wet = reverb.tick(reverb_in, reverb_size);

                    let (final_l, final_r) = mix_reverb(left, right, wet, reverb_mix);

                    let sample_l = (final_l * master_gain).tanh();
                    let sample_r = (final_r * master_gain).tanh();

                    if let Some(ref peak) = peak_tap {
                        let peak_val = sample_l.abs().max(sample_r.abs());
                        let cur = f32::from_bits(peak.load(Ordering::Relaxed));
                        if peak_val > cur {
                            peak.store(peak_val.to_bits(), Ordering::Relaxed);
                        }
                    }

                    if channels == 1 {
                        frame[0] = (sample_l + sample_r) * 0.5;
                    } else {
                        frame[0] = sample_l;
                        frame[1] = sample_r;
                        for s in frame.iter_mut().skip(2) {
                            *s = 0.0;
                        }
                    }
                }
            },
            err_fn,
            None,
        )?;
        stream.play()?;

        Ok(Synth {
            params,
            groups,
            click,
            _stream: stream,
        })
    }

    pub fn params(&self) -> &SynthParams {
        &self.params
    }

    // ---- stab groups ----

    /// Start a chord on one stab group at a given gain.
    ///
    /// Only that group is retriggered, which is what lets the layers of a
    /// pattern overlap: a hit on take 2 never cuts take 1.
    pub fn play_stab(&self, group: usize, notes: &[u8], gain: f32) {
        let Some(slot) = self.groups.get(group) else {
            return;
        };
        slot.reset();
        let (l, m, h) = allocate(notes);
        for (handle, &note) in slot.low.iter().zip(l.iter()) {
            handle.trigger_at(note, gain);
        }
        for (handle, &note) in slot.mid.iter().zip(m.iter()) {
            handle.trigger_at(note, gain);
        }
        for (handle, &note) in slot.high.iter().zip(h.iter()) {
            handle.trigger_at(note, gain);
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

    // ---- metronome ----

    /// A short metronome blip, audible only while a rhythm is being recorded.
    pub fn play_click(&self, strong: bool) {
        if let Some(handle) = self.click.first() {
            handle.reset();
            let note = if strong { 84 } else { 79 };
            handle.trigger_glide(note, note, 0.002, 0.03);
        }
    }

    // ---- patch bridge ----

    pub fn apply_patch(&self, patch: &Patch) {
        apply_channel(&self.params.low, &patch.low);
        apply_channel(&self.params.mid, &patch.mid);
        apply_channel(&self.params.high, &patch.high);
        self.params.reverb_mix.set(patch.mixer.reverb_mix);
        self.params.reverb_size.set(patch.mixer.reverb_size);
        self.params.master_volume.set(patch.mixer.master_volume);
        self.params.master_mute.set(if patch.mixer.master_mute {
            1.0
        } else {
            0.0
        });
        self.params.preview_fade.set(patch.mixer.preview_fade);
    }

    /// Snapshot the current sound design as a named patch.
    ///
    /// `note_length` is transport state, not synth state, so the caller
    /// supplies it (it is stored alongside the mixer for convenience).
    pub fn capture_patch(&self, name: &str, note_length: f32) -> Patch {
        Patch {
            name: name.to_string(),
            low: capture_channel(&self.params.low),
            mid: capture_channel(&self.params.mid),
            high: capture_channel(&self.params.high),
            mixer: MixerPatch {
                reverb_mix: self.params.reverb_mix.get(),
                reverb_size: self.params.reverb_size.get(),
                master_volume: self.params.master_volume.get(),
                master_mute: self.params.master_mute.get() > 0.5,
                preview_fade: self.params.preview_fade.get(),
                note_length,
            },
        }
    }
}

fn apply_channel(dst: &ChannelParams, src: &ChannelPatch) {
    dst.volume.set(src.volume);
    dst.waveform.set(src.waveform as i32 as f32);
    dst.attack.set(src.attack);
    dst.decay.set(src.decay);
    dst.sustain.set(src.sustain);
    dst.release.set(src.release);
    dst.cutoff.set(src.cutoff);
    dst.resonance.set(src.resonance);
    dst.transpose.set(src.transpose);
    dst.reverb_send.set(src.reverb_send);
    dst.pan.set(src.pan);
}

fn capture_channel(src: &ChannelParams) -> ChannelPatch {
    ChannelPatch {
        volume: src.volume.get(),
        waveform: Waveform::from_f32(src.waveform.get()),
        attack: src.attack.get(),
        decay: src.decay.get(),
        sustain: src.sustain.get(),
        release: src.release.get(),
        cutoff: src.cutoff.get(),
        resonance: src.resonance.get(),
        transpose: src.transpose.get(),
        reverb_send: src.reverb_send.get(),
        pan: src.pan.get(),
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

    #[test]
    fn reverb_produces_finite_output() {
        let mut r = Reverb::new();
        let mut last = 0.0;
        for i in 0..10_000 {
            let input = if i == 0 { 1.0 } else { 0.0 };
            last = r.tick(input, 0.5);
            assert!(last.is_finite());
        }
        assert!(last.abs() < 0.01);
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
        assert!((l - 0.6).abs() < 1e-6, "an over-range level must not run away");
        let (l, _) = mix_reverb(0.0, 0.0, 0.2, -1.0);
        assert_eq!(l, 0.0);
    }

    #[test]
    fn silence_in_stays_silence_out() {
        assert_eq!(mix_reverb(0.0, 0.0, 0.0, 1.0), (0.0, 0.0));
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
