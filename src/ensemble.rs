//! Ensembles: three placements and a mixer.
//!
//! An [`Ensemble`] is three [`Placement`]s, a placement is a named
//! [`Instrument`](crate::instrument::Instrument) plus where it sits, and an
//! instrument is a register-neutral voice. Those are the three levels, and the
//! names are the ones this tool uses for them everywhere.
//!
//! # The shipped files are the source
//!
//! `ensembles.toml` and `instruments.toml` are compiled in with `include_str!`
//! and parsed at start-up, rather than being mirrored by hand-written Rust that
//! a test keeps in step. The difference is not cosmetic: it means the shipped
//! library is *authored data*, so editing it by hand is how it is changed.
//! `rhythms.toml` still keeps Rust as the source and regenerates the file, which
//! is why it has a `regenerate_*` helper and these two do not. A file that went
//! missing is a compile error here instead of a test failure, which is the
//! stronger guarantee.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::eq::EqCurve;
use crate::fx::Fx;
use crate::voice::{ComposedChannel, MixerSettings, VoicePatch};

/// Where one instrument sits in an ensemble.
///
/// Nothing here is about what the sound *is*, and nothing in
/// [`VoicePatch`](crate::voice::VoicePatch) is about where it sits. That is the
/// whole point of the split: moving an instrument between registers changes only
/// this, so auditioning one never disturbs the mix it is being auditioned in.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Placement {
    /// The instrument, by name, resolved against the library at load.
    pub instrument: String,
    #[serde(default = "crate::voice::default_placement_volume")]
    pub volume: f32,
    #[serde(default)]
    pub transpose: f32,
    #[serde(default)]
    pub reverb_send: f32,
    /// How much of this register goes to the delay send, 0..1.
    ///
    /// Beside `reverb_send` rather than with the instrument, for the same reason:
    /// how much of a part is sent to a shared effect is a decision about where
    /// the part sits in the mix.
    #[serde(default)]
    pub delay_send: f32,
    #[serde(default)]
    pub pan: f32,
    /// Where this part sits in the frequency range as well as in the stereo
    /// field: thirteen gains in decibels, flat until somebody moves one.
    ///
    /// Flat is the default and is not written to the file, so a placement that
    /// has no opinion is spelled the same way it was before the equaliser
    /// existed — which is what keeps every shipped ensemble byte-for-byte
    /// compatible and bit-for-bit identical in sound.
    #[serde(default, skip_serializing_if = "EqCurve::is_flat")]
    pub eq: EqCurve,
    /// The insert chain, in order, as long as it needs to be.
    ///
    /// Shorter than the rack when the tail is empty, absent when it is all empty,
    /// and never longer than the rack — `ComposedChannel::compose` pads and
    /// truncates, so the runtime always has exactly six slots to walk. This is
    /// the one place a chain is a `Vec`: the file should not have to say "and
    /// four more of nothing", and the audio thread should not have to cope with a
    /// length that changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chain: Vec<Fx>,
}

/// Three placements and the global block.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Ensemble {
    pub name: String,
    pub low: Placement,
    pub mid: Placement,
    pub high: Placement,
    pub mixer: MixerSettings,
}

impl Placement {
    /// A placement that puts an instrument in a register and decides nothing
    /// else.
    ///
    /// The counterpart of [`VoicePatch::neutral`](crate::voice::VoicePatch::neutral)
    /// and what the panel starts from when a register is empty: everything at its
    /// default, which is the shipping volume, no transposition, no send, no pan,
    /// a flat curve and no inserts.
    pub fn neutral(instrument: impl Into<String>) -> Self {
        Placement {
            instrument: instrument.into(),
            volume: crate::voice::default_placement_volume(),
            transpose: 0.0,
            reverb_send: 0.0,
            delay_send: 0.0,
            pan: 0.0,
            eq: EqCurve::flat(),
            chain: Vec::new(),
        }
    }
}

impl Ensemble {
    /// The three placements in register order, which is the order the panel
    /// draws its columns in.
    pub fn placements(&self) -> [&Placement; 3] {
        [&self.low, &self.mid, &self.high]
    }

    /// The register names, matching `channel_at` in the panel.
    pub const REGISTERS: [&'static str; 3] = ["low", "mid", "high"];

    /// Compose every register into the channels the audio layer takes, resolving
    /// each placement against `find`.
    ///
    /// A placement whose instrument cannot be found falls back to a neutral
    /// voice rather than failing, so one bad name in a user's file cannot stop
    /// the whole palette loading. The names that failed are returned so the
    /// caller can say so; a shipped ensemble resolving nothing is a test
    /// failure, not a runtime surprise.
    pub fn resolve(
        &self,
        find: impl Fn(&str) -> Option<VoicePatch>,
    ) -> ([ComposedChannel; 3], Vec<String>) {
        let mut missing = Vec::new();
        let channels = self.placements().map(|placement| {
            let voice = find(&placement.instrument).unwrap_or_else(|| {
                missing.push(placement.instrument.clone());
                VoicePatch::neutral(4000.0)
            });
            ComposedChannel::compose(voice, placement)
        });
        (channels, missing)
    }
}

/// The shipped ensembles, compiled in from the tracked file.
///
/// Panics only on a malformed file, which is a repository bug that
/// `every_shipped_ensemble_parses` catches before it can be committed.
pub fn builtin_ensembles() -> Vec<Ensemble> {
    from_toml(include_str!("../ensembles.toml")).expect("ensembles.toml is valid TOML")
}

// -----------------------------------------------------------------------------
// Store
// -----------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
struct EnsembleFile {
    ensembles: Vec<Ensemble>,
}

/// The palette: the shipped ensembles with the user's own layered over them.
#[derive(Clone, Debug, Default)]
pub struct EnsembleStore {
    /// What the UI offers, in order.
    pub ensembles: Vec<Ensemble>,
    /// Just the user's own entries. This is what [`Self::save`] writes.
    user: Vec<Ensemble>,
}

impl EnsembleStore {
    /// Load the shipped palette and the user's own file, creating the latter if
    /// missing.
    ///
    /// The shipped ensembles are compiled in, so there is no defaults path and
    /// nothing to lose from a checkout.
    pub fn load(user_path: &Path) -> io::Result<Self> {
        let mut store = EnsembleStore {
            ensembles: builtin_ensembles(),
            user: Vec::new(),
        };
        if user_path.exists() {
            for ensemble in read(user_path)? {
                store.add(ensemble);
            }
        } else {
            store.save(user_path)?;
        }
        Ok(store)
    }

    /// Write the user's own entries. The tracked defaults are never written.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = to_toml(&self.user).map_err(|e| io::Error::other(e.to_string()))?;
        fs::write(path, text)
    }

    /// Add an ensemble, replacing any entry of the same name.
    pub fn add(&mut self, ensemble: Ensemble) {
        upsert(&mut self.ensembles, ensemble.clone());
        upsert(&mut self.user, ensemble);
    }

    pub fn find(&self, name: &str) -> Option<&Ensemble> {
        self.ensembles.iter().find(|e| e.name == name)
    }

    #[cfg(test)]
    pub fn with_builtins() -> Self {
        EnsembleStore {
            ensembles: builtin_ensembles(),
            user: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn from_ensembles(ensembles: Vec<Ensemble>) -> Self {
        EnsembleStore {
            ensembles: ensembles.clone(),
            user: ensembles,
        }
    }
}

fn upsert(ensembles: &mut Vec<Ensemble>, ensemble: Ensemble) {
    match ensembles.iter_mut().find(|e| e.name == ensemble.name) {
        Some(existing) => *existing = ensemble,
        None => ensembles.push(ensemble),
    }
}

fn read(path: &Path) -> io::Result<Vec<Ensemble>> {
    let text = fs::read_to_string(path)?;
    from_toml(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

/// Parse an ensemble document. Unknown keys are ignored, so a file written by a
/// different build still loads.
pub fn from_toml(text: &str) -> Result<Vec<Ensemble>, toml::de::Error> {
    let file: EnsembleFile = toml::from_str(text)?;
    Ok(file.ensembles)
}

pub fn to_toml(ensembles: &[Ensemble]) -> Result<String, toml::ser::Error> {
    toml::to_string_pretty(&EnsembleFile {
        ensembles: ensembles.to_vec(),
    })
}

pub fn user_path() -> PathBuf {
    PathBuf::from("ensembles.user.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::FX_PARAMS;
    use crate::instrument::InstrumentStore;

    fn library() -> InstrumentStore {
        InstrumentStore::with_builtins()
    }

    /// A fingerprint of every shipped register's composed channel.
    ///
    /// A golden master, so any change to a shipped voice or placement has to be
    /// deliberate rather than accidental. The value was computed from the
    /// **single-level** palette, before the split — which is what proves that
    /// moving from one fused channel per register to an instrument plus a
    /// placement changed no sound at all.
    ///
    /// It covers a composed channel's voice and placement numbers and nothing
    /// else; the equaliser is checked separately, by
    /// `no_shipped_ensemble_uses_the_equaliser_yet`.
    ///
    /// The waveform goes in as `Waveform::stable_id` rather than as its
    /// discriminant, because the two are not the same promise. `pluck` was
    /// inserted into the computed group, which renumbered every table-backed
    /// variant by one and changed not a single sample — and a golden master that
    /// cannot tell those two apart is one that gets re-recorded for the wrong
    /// reason sooner or later.
    ///
    /// The value moved once, when that encoding changed: recomputing the old
    /// hash with the pre-`pluck` numbering reproduces `0x57c44c21beafc252`
    /// exactly, which is what establishes that the sounds are the same as they
    /// were and only the way they are written down has changed.
    pub const PALETTE_FINGERPRINT: u64 = 0xbd4d8975e0e8c252;

    /// FNV-1a over the bit patterns of every composed channel, in a fixed field
    /// order. The same sixty-four bits are computed in the migration script, so
    /// the two have to agree on the order and on nothing else.
    fn palette_fingerprint() -> u64 {
        let library = library();
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let mut mix = |bits: u32| {
            hash ^= bits as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        };
        for ensemble in builtin_ensembles() {
            let (channels, _) = ensemble.resolve(|name| library.voice_of(name));
            for channel in channels {
                let v = &channel.voice;
                let ints = [
                    channel.volume,
                    v.waveform.stable_id() as f32,
                    v.noise_level,
                    v.pulse_width,
                    v.attack,
                    v.decay,
                    v.sustain,
                    v.release,
                    v.env_curve,
                    v.glide,
                    v.cutoff,
                    v.resonance,
                    v.filter_type as i32 as f32,
                    v.filter_env,
                    v.filter_attack,
                    v.filter_decay,
                    v.key_track,
                    v.lfo_pitch,
                    v.lfo_cutoff,
                    v.lfo_amp,
                    v.lfo_pwm,
                    v.unison,
                    v.detune,
                    channel.transpose,
                    channel.reverb_send,
                    channel.pan,
                ];
                for value in ints {
                    mix(value.to_bits());
                }
            }
        }
        hash
    }

    #[test]
    fn the_shipped_palette_still_makes_the_sounds_it_made_before_the_split() {
        assert_eq!(
            palette_fingerprint(),
            PALETTE_FINGERPRINT,
            "a shipped ensemble's composed channel changed"
        );
    }

    #[test]
    fn no_shipped_ensemble_uses_the_equaliser_yet() {
        // The other half of the fingerprint's promise, and the half it cannot
        // make: a curve is not one of the composed channel's numbers, so a
        // non-flat one would change a shipped sound while the fingerprint above
        // stayed exactly the same. Every shipped ensemble and every shipped
        // register is flat, which is what makes adding the equaliser a purely
        // additive change. Putting curves on the palette is a deliberate next
        // step, and it is the point at which this test should be deleted and the
        // fingerprint re-recorded.
        for ensemble in builtin_ensembles() {
            for (register, placement) in Ensemble::REGISTERS.iter().zip(ensemble.placements()) {
                assert!(
                    placement.eq.is_flat(),
                    "{} {} has a curve: {:?}",
                    ensemble.name,
                    register,
                    placement.eq.gains
                );
            }
            assert!(
                ensemble.mixer.master_eq.is_flat(),
                "{} has a master curve: {:?}",
                ensemble.name,
                ensemble.mixer.master_eq.gains
            );
        }
    }

    #[test]
    fn no_shipped_ensemble_uses_the_effect_chain_yet() {
        // The other half of the fingerprint's promise, and the half it cannot
        // make: an insert chain is not one of a composed channel's numbers, so a
        // chain with something in it would change a shipped sound while the
        // fingerprint above stayed exactly the same. Every shipped register has
        // an empty rack and no delay send, and every shipped mixer has the delay
        // return at zero — which is what makes the whole effects system an
        // additive change rather than a rewrite of the palette.
        for ensemble in builtin_ensembles() {
            for (register, placement) in Ensemble::REGISTERS.iter().zip(ensemble.placements()) {
                assert!(
                    placement.chain.is_empty(),
                    "{} {} has a chain: {:?}",
                    ensemble.name,
                    register,
                    placement.chain
                );
                assert_eq!(
                    placement.delay_send, 0.0,
                    "{} {} sends to the delay",
                    ensemble.name, register
                );
            }
            assert_eq!(
                ensemble.mixer.delay_mix, 0.0,
                "{} returns the delay",
                ensemble.name
            );
            // And the two aux units are the ones a mixer with no `reverb` table
            // describes — the tank that was already here, with the *shipped*
            // size in it, and a delay nobody has turned up. The size is the one
            // parameter the old file carried, so it is the one that differs from
            // the default unit.
            let default = crate::voice::default_reverb_unit();
            assert_eq!(ensemble.mixer.reverb.kind, default.kind);
            assert_eq!(ensemble.mixer.reverb.subtype, default.subtype);
            for index in 1..FX_PARAMS {
                assert_eq!(
                    ensemble.mixer.reverb.param(index),
                    default.param(index),
                    "{} has a reverb setting the old file could not have carried",
                    ensemble.name
                );
            }
            assert_eq!(ensemble.mixer.delay, crate::voice::default_delay_unit());
        }
    }

    #[test]
    fn every_shipped_reverb_size_lands_in_the_effect_that_replaced_it() {
        // `reverb_size` was a mixer key and is now the reverb unit's first
        // parameter. The tracked file still says `reverb_size`, so this is the
        // shim doing its job on real data rather than on a fixture: the shipped
        // sizes run from 0.25 to 0.92 and every one of them has to arrive.
        let sizes: Vec<f32> = builtin_ensembles()
            .iter()
            .map(|e| e.mixer.reverb.param(crate::fx::P0))
            .collect();
        assert!(
            sizes.iter().any(|s| *s > 0.9) && sizes.iter().any(|s| *s < 0.3),
            "the shipped sizes should span the range: {:?}",
            sizes
        );
        assert!(
            sizes
                .iter()
                .all(|s| (0.0..=1.0).contains(s) && *s != 0.5 || *s == 0.5),
            "a size arrived out of range: {:?}",
            sizes
        );
    }

    #[test]
    fn every_shipped_ensemble_parses() {
        // `builtin_ensembles` panics on a malformed file, so this is what turns
        // that panic into a failing test.
        let ensembles = builtin_ensembles();
        assert!(ensembles.len() >= 40, "only {} ensembles", ensembles.len());
    }

    #[test]
    fn every_shipped_placement_finds_its_instrument() {
        // The shipped palette has to resolve completely: a name in
        // `ensembles.toml` with no entry in `instruments.toml` is a broken
        // reference, and the fallback would hide it.
        let library = library();
        for ensemble in builtin_ensembles() {
            let (_, missing) = ensemble.resolve(|name| library.voice_of(name));
            assert!(
                missing.is_empty(),
                "{} names instruments that do not exist: {:?}",
                ensemble.name,
                missing
            );
        }
    }

    #[test]
    fn an_unresolved_instrument_degrades_instead_of_failing() {
        // A user file can name anything. One bad name must not stop the palette
        // loading, and must be reported rather than silently swallowed.
        let ensemble = Ensemble {
            name: "Broken".to_string(),
            low: Placement {
                instrument: "Nonexistent".to_string(),
                volume: 4.0,
                transpose: 0.0,
                reverb_send: 0.0,
                delay_send: 0.0,
                pan: 0.0,
                eq: EqCurve::flat(),
                chain: Vec::new(),
            },
            mid: builtin_ensembles()[0].mid.clone(),
            high: builtin_ensembles()[0].high.clone(),
            mixer: builtin_ensembles()[0].mixer.clone(),
        };
        let library = library();
        let (channels, missing) = ensemble.resolve(|name| library.voice_of(name));
        assert_eq!(missing, vec!["Nonexistent".to_string()]);
        // The failed register is still a usable channel rather than a panic.
        assert!(channels[0].voice.cutoff > 0.0);
        // And the registers that did resolve are untouched.
        assert_eq!(channels[1], {
            let (ok, _) = builtin_ensembles()[0].resolve(|n| library.voice_of(n));
            ok[1].clone()
        });
    }

    #[test]
    fn the_shipped_ensemble_names_are_distinct() {
        let ensembles = builtin_ensembles();
        let mut names: Vec<&str> = ensembles.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), ensembles.len());
    }

    #[test]
    fn ensembles_round_trip_through_toml() {
        let store = EnsembleStore::from_ensembles(builtin_ensembles());
        let text = to_toml(&store.ensembles).unwrap();
        assert_eq!(from_toml(&text).unwrap(), store.ensembles);
    }

    #[test]
    fn load_creates_the_user_file() {
        let user = temp_path("ensemble-user");
        let _ = fs::remove_file(&user);

        let store = EnsembleStore::load(&user).unwrap();
        assert!(user.exists());
        assert_eq!(store.ensembles.len(), builtin_ensembles().len());
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
