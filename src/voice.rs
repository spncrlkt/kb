//! What a sound is, and where it is put.
//!
//! The synthesiser used to have one channel type, and it fused two different
//! decisions: **what a sound is** — waveform, envelope, filter, modulation — and
//! **where it sits in the ensemble** — level, pan, transpose, reverb send. That
//! fusion is why an instrument could not be moved between registers without
//! dragging a register's choices along with it, and why a library of instruments
//! had nowhere to live.
//!
//! So there are three levels now:
//!
//! - a [`VoicePatch`] is one sound, and is register-neutral: it means the same
//!   thing in the low register as in the high one,
//! - a `Placement` — see [`crate::ensemble`] — is that sound plus where it sits,
//! - an `Ensemble` is three placements and a mixer.
//!
//! [`ComposedChannel`] survives as the *composed* pair, and is what the audio layer
//! actually takes: `apply_channel` writes all two dozen parameters at once, and
//! capturing reads them back the same way. It is a runtime type and is not
//! serialised — the files hold a voice and a placement.

use serde::{Deserialize, Serialize};

use crate::eq::EqCurve;
use crate::fx::{Fx, CHAIN_SLOTS};
use crate::synth::{FilterType, FmMode, LfoWave, Waveform};

// -----------------------------------------------------------------------------
// Serde defaults
// -----------------------------------------------------------------------------

/// Everything added to a voice after the first files were written is
/// `#[serde(default)]`ed to the value that leaves an older sound exactly as it
/// was: no noise, a straight envelope, no glide, a lowpass with no modulation,
/// one voice per note.
///
/// `Default` covers the zeroes. The rest need a function because "neutral" is
/// not "zero" for them, and the reason is worth stating precisely rather than
/// dramatically. `unison` is only *behaviourally* neutral at 1 — the trigger
/// clamps it, so an old file defaulting to 0 would not actually fall silent —
/// but 1 is the honest value, because 0 is not a voice count. The two filter
/// times are not load-bearing either while `filter_env` is 0, since nothing
/// reads the contour; they ship non-zero so that a sound which later raises
/// `filter_env` has a usable contour instead of a click.
pub(crate) fn one() -> f32 {
    1.0
}

fn half() -> f32 {
    0.5
}

fn default_filter_attack() -> f32 {
    0.01
}

fn default_filter_decay() -> f32 {
    0.2
}

/// The level a placement is added at when its file does not say.
///
/// Not zero, which would be silence: a placement that lost its level should
/// still be audible, and a file from before levels existed meant "as it was".
fn default_volume() -> f32 {
    4.0
}

/// A plucked string's ring, in seconds to -60 dB.
///
/// Content rather than neutrality: the string is a *source*, so the moment
/// `pluck` is chosen these three have to sound like a string. They are the
/// values a guitar-ish default wants and nothing reads them until then.
fn default_pluck_decay() -> f32 {
    2.5
}

fn default_pluck_damp() -> f32 {
    0.35
}

fn default_pluck_burst() -> f32 {
    0.9
}

// -----------------------------------------------------------------------------
// The voice: what a sound is
// -----------------------------------------------------------------------------

/// One sound, with nothing in it about where it is played.
///
/// No level, no pan, no transpose: those are decisions about a *position* in the
/// ensemble, and they live in a `Placement`. That split is what lets the same
/// instrument sit under a chord, on top of one, or on its own, and sound like
/// itself in every case.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VoicePatch {
    pub waveform: Waveform,
    /// White noise mixed in alongside the oscillator, 0..1.
    #[serde(default)]
    pub noise_level: f32,
    /// Duty cycle of the square, 0.05..0.95. Half is the even square an older
    /// file means, because that is all it could have been.
    #[serde(default = "half")]
    pub pulse_width: f32,
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
    /// 0 straight lines, 1 the capacitor curve. Shaping only.
    #[serde(default)]
    pub env_curve: f32,
    /// Portamento between notes, in seconds.
    #[serde(default)]
    pub glide: f32,
    pub cutoff: f32,
    pub resonance: f32,
    #[serde(default)]
    pub filter_type: FilterType,
    /// How far the filter envelope moves the cutoff, -1..1.
    #[serde(default)]
    pub filter_env: f32,
    #[serde(default = "default_filter_attack")]
    pub filter_attack: f32,
    #[serde(default = "default_filter_decay")]
    pub filter_decay: f32,
    /// How much the cutoff follows the sounding pitch, 0..1.
    #[serde(default)]
    pub key_track: f32,
    /// LFO to pitch, 0..1 = 0..100 cents.
    #[serde(default)]
    pub lfo_pitch: f32,
    /// LFO to cutoff, 0..1 = 0..4 octaves.
    #[serde(default)]
    pub lfo_cutoff: f32,
    /// LFO to amplitude, 0..1 = full tremolo.
    #[serde(default)]
    pub lfo_amp: f32,
    /// LFO to the square's duty cycle, 0..1.
    #[serde(default)]
    pub lfo_pwm: f32,
    /// Voices stacked per note, 1..4.
    #[serde(default = "one")]
    pub unison: f32,
    /// Peak detune spread in cents, each side of centre.
    #[serde(default)]
    pub detune: f32,

    // ---- filtering, again ----
    /// Drive into the filter, 0..1. A pre-gain into a saturator, with no
    /// make-up: a driven filter is louder as well as richer, which is what the
    /// knob does on the hardware.
    #[serde(default)]
    pub drive: f32,
    /// Velocity to cutoff, 0..1 = 0..4 octaves down at no velocity.
    #[serde(default)]
    pub vel_cutoff: f32,
    /// Velocity to pulse width, 0..1.
    #[serde(default)]
    pub vel_pwm: f32,

    // ---- the oscillator, again ----
    /// Wavetable position, 0..1: 0 is `waveform` alone, 1 is the next waveform
    /// in its octave group.
    ///
    /// This is the one thing a filter cannot do. A filter tilts the *envelope*
    /// of a spectrum; a position moves between two different spectra, which is
    /// how `glass` becomes `vox` rather than merely becoming darker.
    #[serde(default)]
    pub position: f32,
    /// Phase distortion, 0..1. Zero is the unwarped phase, exactly.
    #[serde(default)]
    pub phase_dist: f32,
    /// The second oscillator's own waveform.
    #[serde(default)]
    pub osc2_waveform: Waveform,
    /// Its interval from the first, in semitones.
    #[serde(default)]
    pub osc2_interval: f32,
    /// How much of it is mixed in, 0..1.
    #[serde(default)]
    pub osc2_level: f32,
    /// How hard it bends the first oscillator, 0..1, in the domain `fm_mode`
    /// chooses.
    ///
    /// At zero level and zero depth the second oscillator is not run at all, so
    /// a patch that does not use it costs nothing and renders exactly as it did
    /// before the feature existed.
    #[serde(default)]
    pub osc2_fm: f32,
    /// Which domain that depth is spent in. See [`FnMode`](crate::synth::FmMode).
    #[serde(default)]
    pub fm_mode: FmMode,
    /// The oscillator bending its own phase with its own previous sample, 0..1.
    #[serde(default)]
    pub feedback: f32,
    /// The two oscillators multiplied together, mixed in alongside them, 0..1.
    #[serde(default)]
    pub osc2_ring: f32,

    // ---- the plucked string ----
    /// How long the string rings, in seconds to -60 dB.
    #[serde(default = "default_pluck_decay")]
    pub pluck_decay: f32,
    /// How fast its upper partials die, 0..1.
    #[serde(default = "default_pluck_damp")]
    pub pluck_damp: f32,
    /// How long it is excited, as a fraction of one period. A short burst is a
    /// hard pick; a long one is a soft thumb.
    #[serde(default = "default_pluck_burst")]
    pub pluck_burst: f32,
}

impl VoicePatch {
    /// The plain starting sound at a cutoff.
    pub fn neutral(cutoff: f32) -> Self {
        VoicePatch {
            waveform: Waveform::Sine,
            noise_level: 0.0,
            pulse_width: 0.5,
            attack: 0.005,
            decay: 0.05,
            sustain: 0.7,
            release: 0.1,
            env_curve: 0.0,
            glide: 0.0,
            cutoff,
            resonance: 0.2,
            filter_type: FilterType::Lowpass,
            filter_env: 0.0,
            filter_attack: default_filter_attack(),
            filter_decay: default_filter_decay(),
            key_track: 0.0,
            lfo_pitch: 0.0,
            lfo_cutoff: 0.0,
            lfo_amp: 0.0,
            lfo_pwm: 0.0,
            unison: 1.0,
            detune: 0.0,
            drive: 0.0,
            vel_cutoff: 0.0,
            vel_pwm: 0.0,
            position: 0.0,
            phase_dist: 0.0,
            osc2_waveform: Waveform::Sine,
            osc2_interval: 0.0,
            osc2_level: 0.0,
            osc2_fm: 0.0,
            fm_mode: FmMode::Phase,
            feedback: 0.0,
            osc2_ring: 0.0,
            pluck_decay: default_pluck_decay(),
            pluck_damp: default_pluck_damp(),
            pluck_burst: default_pluck_burst(),
        }
    }
}

// -----------------------------------------------------------------------------
// The composed channel: what the audio layer takes
// -----------------------------------------------------------------------------

/// A voice and a placement, flattened.
///
/// The audio callback wants two dozen numbers per channel and does not care
/// which of them are about the sound and which about its position, so this is
/// what `apply_channel` takes. It is deliberately **not** serialised: a file
/// holds the two halves separately, and this is only how they travel together.
#[derive(Clone, Debug, PartialEq)]
pub struct ComposedChannel {
    pub voice: VoicePatch,
    pub volume: f32,
    pub transpose: f32,
    pub reverb_send: f32,
    pub pan: f32,
    /// The register's thirteen-band curve. Part of where the sound sits rather
    /// than what it is, so it travels with the placement and survives an
    /// instrument swap untouched.
    pub eq: EqCurve,
    /// How much of this register goes to the delay send, 0..1.
    pub delay_send: f32,
    /// The insert chain, in order. Always exactly [`CHAIN_SLOTS`] long here,
    /// whatever the file said — the slots are the rack, and a slot holding
    /// [`FxKind::None`](crate::fx::FxKind::None) is an empty one.
    pub chain: [Fx; CHAIN_SLOTS],
}

impl ComposedChannel {
    /// A voice at the placement it was written for.
    pub fn compose(voice: VoicePatch, placement: &crate::ensemble::Placement) -> Self {
        // A file may hold fewer slots than the rack has, or more; the rack is
        // fixed and the file is not, so the padding and the truncation happen
        // here, once, rather than at every use.
        let mut chain = [Fx::none(); CHAIN_SLOTS];
        for (slot, fx) in chain.iter_mut().zip(placement.chain.iter()) {
            *slot = *fx;
        }
        ComposedChannel {
            voice,
            volume: placement.volume,
            transpose: placement.transpose,
            reverb_send: placement.reverb_send,
            pan: placement.pan,
            eq: placement.eq,
            delay_send: placement.delay_send,
            chain,
        }
    }

    /// The plain channel at a level and a cutoff.
    ///
    /// The composed type is only ever built by composing a voice with a
    /// placement, so this exists for tests that want a channel without caring
    /// where it came from.
    #[cfg(test)]
    pub fn neutral(volume: f32, cutoff: f32) -> Self {
        ComposedChannel {
            voice: VoicePatch::neutral(cutoff),
            volume,
            transpose: 0.0,
            reverb_send: 0.0,
            pan: 0.0,
            eq: EqCurve::flat(),
            delay_send: 0.0,
            chain: [Fx::none(); CHAIN_SLOTS],
        }
    }

    /// Split back into what it is and where it sits.
    ///
    /// `instrument` is the name to record for the voice, which the caller knows
    /// and this cannot: the same numbers may be a library instrument or
    /// something just dialled in.
    pub fn split(&self, instrument: &str) -> crate::ensemble::Placement {
        // Trailing empty slots are dropped: a rack of six `none`s is not a
        // decision, and writing it down would put six lines of nothing in every
        // placement. A gap in the middle is kept, because it is a position
        // somebody chose — and it means the same thing as a shorter chain.
        let used = self
            .chain
            .iter()
            .rposition(|fx| !fx.is_none())
            .map(|last| last + 1)
            .unwrap_or(0);
        crate::ensemble::Placement {
            instrument: instrument.to_string(),
            volume: self.volume,
            transpose: self.transpose,
            reverb_send: self.reverb_send,
            delay_send: self.delay_send,
            pan: self.pan,
            eq: self.eq,
            chain: self.chain[..used].to_vec(),
        }
    }
}

// -----------------------------------------------------------------------------
// The ensemble's global block
// -----------------------------------------------------------------------------

/// What a mixer block looked like before the effects existed.
///
/// Read through rather than kept: `reverb_size` was the tank's size and has
/// become the reverb unit's first parameter, so a file written before the
/// effects existed has to be folded into the unit it now describes. Doing it in
/// a `From` rather than at every use is what keeps `MixerSettings` itself free
/// of optional fields — nothing downstream has to know that a key moved.
#[derive(Deserialize)]
struct MixerFile {
    reverb_mix: f32,
    reverb_size: Option<f32>,
    reverb: Option<Fx>,
    delay: Option<Fx>,
    delay_mix: Option<f32>,
    master_volume: f32,
    master_mute: bool,
    #[serde(default = "default_lfo_rate")]
    lfo_rate: f32,
    #[serde(default)]
    lfo_wave: LfoWave,
    #[serde(default = "default_note_length")]
    note_length: f32,
    #[serde(default)]
    master_eq: EqCurve,
}

impl From<MixerFile> for MixerSettings {
    fn from(file: MixerFile) -> Self {
        // The one key that moved into the effect, applied only when the file has
        // no `reverb` table of its own — which is exactly when it was written
        // before the effects existed. A file that carries both means the new one.
        let mut reverb = file.reverb.unwrap_or_else(default_reverb_unit);
        if file.reverb.is_none() {
            if let Some(size) = file.reverb_size {
                reverb.set_param(crate::fx::P0, size);
            }
        }
        MixerSettings {
            reverb_mix: file.reverb_mix,
            reverb,
            delay: file.delay.unwrap_or_else(default_delay_unit),
            delay_mix: file.delay_mix.unwrap_or(0.0),
            master_volume: file.master_volume,
            master_mute: file.master_mute,
            lfo_rate: file.lfo_rate,
            lfo_wave: file.lfo_wave,
            note_length: file.note_length,
            master_eq: file.master_eq,
        }
    }
}

/// The reverb send's return, as an effect.
///
/// `hall` at its defaults, which is the tank this crate has always had — so a
/// mixer with no `reverb` table describes the sound it always described.
pub(crate) fn default_reverb_unit() -> Fx {
    Fx::variant(crate::fx::FxKind::Reverb, crate::fx::FxSubtype::Hall)
}

/// The delay send's return: a quarter note, and silent until its level is up.
pub(crate) fn default_delay_unit() -> Fx {
    Fx::variant(crate::fx::FxKind::Delay, crate::fx::FxSubtype::Digital)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(from = "MixerFile")]
pub struct MixerSettings {
    /// How much reverb is added on top of the dry signal, 0..1.
    ///
    /// Additive, never a wet/dry balance. The key keeps its historical name so
    /// files written before the change still load.
    pub reverb_mix: f32,
    /// How much of the delay's return is added, 0..1. Additive like the
    /// reverb's, and 0 by default: a shipped ensemble has no delay in it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub delay_mix: f32,
    pub master_volume: f32,
    pub master_mute: bool,
    /// LFO rate in Hz. Global: one vibrato for the whole chord.
    #[serde(default = "default_lfo_rate")]
    pub lfo_rate: f32,
    #[serde(default)]
    pub lfo_wave: LfoWave,
    /// Fraction of one bar that a chord sustains before releasing.
    /// 0.25 = quarter, 0.5 = half, 0.75 = dotted half, 1.0 = whole.
    #[serde(default = "default_note_length")]
    pub note_length: f32,
    /// The curve on the finished stereo pair. Flat is the default and is left
    /// out of the file entirely, so a shipped ensemble stays as short as it was
    /// and a curve that is doing nothing does not look like a decision.
    #[serde(default, skip_serializing_if = "EqCurve::is_flat")]
    pub master_eq: EqCurve,
    /// The reverb send's return: a whole effect, with its type pinned to reverb
    /// by the panel. Fully wet — `reverb_mix` is what decides how much of it is
    /// heard, so the unit itself never blends the dry signal back in.
    ///
    /// Last, with `delay` after it and nothing after that: an effect is a TOML
    /// *table* and every field above is a value, and TOML requires the values
    /// before the tables. The order is load-bearing, so it is written down here
    /// rather than discovered by a serialiser complaining.
    #[serde(default = "default_reverb_unit")]
    pub reverb: Fx,
    /// The delay send's return, on the same terms.
    #[serde(default = "default_delay_unit")]
    pub delay: Fx,
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

fn default_lfo_rate() -> f32 {
    5.0
}

fn default_note_length() -> f32 {
    1.0
}

/// The level a placement gets, for a caller that has no opinion.
pub(crate) fn default_placement_volume() -> f32 {
    default_volume()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ensemble::Placement;

    #[test]
    fn composing_and_splitting_is_lossless() {
        // The two halves have to add back up to the composed channel exactly, or
        // a save followed by a load would drift the sound.
        let mut channel = ComposedChannel::neutral(4.0, 4000.0);
        channel.volume = 5.5;
        channel.pan = -0.4;
        channel.transpose = 7.0;
        channel.reverb_send = 0.6;
        channel.voice.cutoff = 1234.0;
        channel.voice.waveform = Waveform::Reed;
        channel.voice.unison = 3.0;
        channel.voice.lfo_pwm = 0.45;
        // The equaliser is part of the placement half, so it has to survive the
        // split and the compose just as the level does.
        channel.eq.gains[5] = -4.5;
        channel.eq.gains[11] = 6.0;

        let placement = channel.split("Something");
        assert_eq!(placement.instrument, "Something");
        let back = ComposedChannel::compose(channel.voice.clone(), &placement);
        assert_eq!(back, channel);
    }

    #[test]
    fn a_voice_carries_nothing_about_where_it_sits() {
        // The whole point of the split: these four are not in a `VoicePatch`, so
        // there is no way for a register's choices to travel with an instrument.
        let voice = VoicePatch::neutral(1000.0);
        let json = toml::to_string(&voice).unwrap();
        for placement_field in ["volume", "pan", "transpose", "reverb_send", "eq"] {
            assert!(
                !json.contains(placement_field),
                "a voice should not carry {}: {}",
                placement_field,
                json
            );
        }
        assert!(json.contains("cutoff"));
        assert!(json.contains("waveform"));
    }

    #[test]
    fn a_voice_round_trips_through_toml() {
        let voice = VoicePatch::neutral(2500.0);
        let text = toml::to_string(&voice).unwrap();
        let back: VoicePatch = toml::from_str(&text).unwrap();
        assert_eq!(voice, back);
    }

    #[test]
    fn an_old_voice_file_still_loads_without_the_newer_fields() {
        // The compatibility contract at the voice level: a file written before
        // any of the modulation existed has to come back neutral.
        let old = r#"
waveform = "saw"
attack = 0.01
decay = 0.25
sustain = 0.6
release = 0.4
cutoff = 1800.0
resonance = 0.3
"#;
        let voice: VoicePatch = toml::from_str(old).unwrap();
        assert_eq!(voice.waveform, Waveform::Saw);
        assert_eq!(voice.cutoff, 1800.0);
        let neutral = VoicePatch::neutral(1800.0);
        assert_eq!(voice.noise_level, neutral.noise_level);
        assert_eq!(voice.unison, neutral.unison, "unison must not default to 0");
        assert_eq!(voice.pulse_width, neutral.pulse_width);
        assert_eq!(voice.filter_attack, neutral.filter_attack);
        assert_eq!(voice.filter_decay, neutral.filter_decay);
        assert_eq!(voice.env_curve, neutral.env_curve);
        assert_eq!(voice.lfo_pwm, neutral.lfo_pwm);
    }

    #[test]
    fn a_placement_defaults_to_an_audible_level() {
        // A placement with no `volume` must not be silence.
        let placement: Placement = toml::from_str("instrument = \"Rhodes\"").unwrap();
        assert_eq!(placement.instrument, "Rhodes");
        assert!(placement.volume > 0.0, "a missing level must not mute");
        assert_eq!(placement.transpose, 0.0);
        assert_eq!(placement.pan, 0.0);
    }
    #[test]
    fn a_mixer_written_before_the_effects_loads_with_its_reverb_where_it_belongs() {
        // The old shape: `reverb_size` at the mixer level, and no `reverb` table
        // at all. Every shipped ensemble still says exactly this.
        let old = r#"
reverb_mix = 0.24
reverb_size = 0.8
master_volume = 5.0
master_mute = false
"#;
        let mixer: MixerSettings = toml::from_str(old).unwrap();
        assert_eq!(mixer.reverb_mix, 0.24);
        assert_eq!(
            mixer.reverb.param(crate::fx::P0),
            0.8,
            "the size moved into the unit that replaced it"
        );
        assert_eq!(mixer.reverb.kind, crate::fx::FxKind::Reverb);
        assert_eq!(mixer.reverb.subtype, crate::fx::FxSubtype::Hall);
        assert_eq!(mixer.delay_mix, 0.0, "and nothing new was switched on");

        // A file that carries both means the new one: the old key is read only
        // when there is no table for it to have been a shorthand for.
        let both = r#"
reverb_mix = 0.24
reverb_size = 0.8
master_volume = 5.0
master_mute = false

[reverb]
kind = "reverb"
subtype = "plate"
params = [0.3, 0.1, 0.0, 0.0, 0.0, 0.0]
"#;
        let mixer: MixerSettings = toml::from_str(both).unwrap();
        assert_eq!(mixer.reverb.subtype, crate::fx::FxSubtype::Plate);
        assert_eq!(mixer.reverb.param(crate::fx::P0), 0.3);
    }

    #[test]
    fn a_mixer_round_trips_through_the_new_shape() {
        let mixer = MixerSettings {
            reverb_mix: 0.3,
            delay_mix: 0.2,
            master_volume: 4.0,
            master_mute: false,
            lfo_rate: 3.0,
            lfo_wave: LfoWave::Triangle,
            note_length: 0.5,
            master_eq: EqCurve::flat(),
            reverb: Fx::variant(crate::fx::FxKind::Reverb, crate::fx::FxSubtype::Plate),
            delay: Fx::variant(crate::fx::FxKind::Delay, crate::fx::FxSubtype::Tape),
        };
        let text = toml::to_string_pretty(&mixer).unwrap();
        // The two units are tables and everything else is a value, which is why
        // they come last in the struct.
        assert!(text.contains("[reverb]"), "{}", text);
        assert!(text.contains("[delay]"), "{}", text);
        assert_eq!(toml::from_str::<MixerSettings>(&text).unwrap(), mixer);
    }

    #[test]
    fn a_placement_carries_a_chain_and_both_sends_without_losing_any_of_them() {
        let mut channel = ComposedChannel::neutral(4.0, 4000.0);
        channel.delay_send = 0.4;
        channel.chain[0] = Fx::variant(crate::fx::FxKind::Distortion, crate::fx::FxSubtype::Tube);
        channel.chain[2] = Fx::variant(crate::fx::FxKind::Chorus, crate::fx::FxSubtype::Ensemble);
        channel.chain[3] = Fx::variant(crate::fx::FxKind::Delay, crate::fx::FxSubtype::Tape);

        let placement = channel.split("Something");
        // Three slots are used and the rest are not written down: a rack of six
        // `none`s is not a decision.
        assert_eq!(placement.chain.len(), 4);
        assert_eq!(placement.delay_send, 0.4);
        assert_eq!(
            ComposedChannel::compose(channel.voice.clone(), &placement),
            channel
        );
        // And the gap in the middle survives, because a position is a choice.
        assert!(placement.chain[1].is_none());
    }

    #[test]
    fn a_chain_longer_than_the_rack_is_truncated_rather_than_refused() {
        // A hand-written file can say anything. Six slots are what the audio
        // layer has, so seven become six rather than an error.
        let mut placement: Placement = toml::from_str("instrument = \"Rhodes\"").unwrap();
        placement.chain = vec![Fx::new(crate::fx::FxKind::Tremolo); CHAIN_SLOTS + 3];
        let channel = ComposedChannel::compose(VoicePatch::neutral(1000.0), &placement);
        assert_eq!(channel.chain.len(), CHAIN_SLOTS);
        assert!(channel.chain.iter().all(|fx| !fx.is_none()));
    }
}
