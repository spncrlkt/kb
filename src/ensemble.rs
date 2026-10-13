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

    /// A fingerprint of everything a shipped ensemble contributes to the sound.
    ///
    /// A golden master, so any change to a shipped voice, placement, rack or
    /// mixer has to be deliberate rather than accidental. The value was computed
    /// from the **single-level** palette, before the split — which is what proves
    /// that moving from one fused channel per register to an instrument plus a
    /// placement changed no sound at all.
    ///
    /// It covers every composed channel's voice and placement numbers, each
    /// register's six chain slots, and the mixer's levels and its two aux units.
    /// The one thing it still cannot see is the equaliser, which is checked
    /// separately and more strongly by `no_shipped_ensemble_uses_the_equaliser_yet`.
    ///
    /// The chain and the mixer were added to it when the showcase ensembles gave
    /// the palette its first non-empty racks. Before then a chain was provably
    /// empty and the mixer's only load-bearing field was the level, so a
    /// fingerprint that skipped both was complete; now that a rack is part of a
    /// shipped sound, leaving it out would make this test a promise with a hole
    /// in it.
    ///
    /// The waveform goes in as `Waveform::stable_id` rather than as its
    /// discriminant, because the two are not the same promise. `pluck` was
    /// inserted into the computed group, which renumbered every table-backed
    /// variant by one and changed not a single sample — and a golden master that
    /// cannot tell those two apart is one that gets re-recorded for the wrong
    /// reason sooner or later. An effect's subtype goes in as its *slot within
    /// its kind* for the same reason: the enum is shared by every kind, so its
    /// discriminant renumbers when an unrelated family gains a variant.
    ///
    /// The value has moved twice. The first was that encoding change, where
    /// recomputing the old hash with the pre-`pluck` numbering reproduced
    /// `0x57c44c21beafc252` exactly, which is what establishes that the sounds
    /// were the same and only the way they are written down had changed. The
    /// second is the one described above, and it is not a claim about the sounds
    /// at all: the field list grew, so the number it lands on answers a different
    /// question rather than giving a different answer.
    pub const PALETTE_FINGERPRINT: u64 = 0x0874_53ef_70c3_9454;

    /// Where a subtype sits in its own kind's list, which is the numbering that
    /// does not move when a different family gains a variant.
    fn subtype_slot(kind: crate::fx::FxKind, subtype: crate::fx::FxSubtype) -> f32 {
        kind.subtypes()
            .iter()
            .position(|s| *s == subtype)
            .unwrap_or(0) as f32
    }

    /// FNV-1a over the bit patterns of everything above, in a fixed field order.
    fn palette_fingerprint() -> u64 {
        /// One field, in the order it is fed in.
        fn mix(hash: &mut u64, bits: u32) {
            *hash ^= bits as u64;
            *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        /// A whole slot: what it is, what variant of it, and its six numbers.
        fn mix_fx(hash: &mut u64, fx: &Fx) {
            mix(hash, (fx.kind.index() as f32).to_bits());
            mix(hash, subtype_slot(fx.kind, fx.subtype).to_bits());
            for param in fx.params {
                mix(hash, param.to_bits());
            }
        }
        let library = library();
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        // **By name, not by file order.** The list order is a curation — which
        // ensemble is near the top of the picker — and moving one up or down
        // changes no sound at all. Hashing the file order would make a reorder
        // indistinguishable from a re-voice, and would force a re-record for a
        // change this test has no business having an opinion about.
        let mut ensembles = builtin_ensembles();
        ensembles.sort_by(|a, b| a.name.cmp(&b.name));
        for ensemble in ensembles {
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
                    channel.delay_send,
                    channel.pan,
                ];
                for value in ints {
                    mix(&mut hash, value.to_bits());
                }
                for fx in &channel.chain {
                    mix_fx(&mut hash, fx);
                }
            }
            let mixer = &ensemble.mixer;
            for value in [
                mixer.reverb_mix,
                mixer.delay_mix,
                mixer.master_volume,
                mixer.lfo_rate,
                mixer.note_length,
            ] {
                mix(&mut hash, value.to_bits());
            }
            mix(&mut hash, (mixer.master_mute as i32 as f32).to_bits());
            mix(&mut hash, (mixer.lfo_wave as i32 as f32).to_bits());
            mix_fx(&mut hash, &mixer.reverb);
            mix_fx(&mut hash, &mixer.delay);
        }
        hash
    }

    #[test]
    fn the_shipped_palette_still_makes_the_sounds_it_made_before_the_split() {
        assert_eq!(
            palette_fingerprint(),
            PALETTE_FINGERPRINT,
            "a shipped ensemble's voice, placement, rack or mixer changed"
        );
    }

    /// The showcase ensembles, by name.
    ///
    /// By name rather than by position, because the list order is a curation the
    /// player is meant to rearrange — what is near the top of the picker is a
    /// judgement about the palette, not a fact about these thirty-two. This is the
    /// one place the set is written down, and
    /// `every_showcase_ensemble_is_still_shipped` is what keeps it honest.
    const SHOWCASE: [&str; 32] = [
        "X-Mod Bell",
        "Glass Cathedral",
        "Growl Stack",
        "Through Zero",
        "Ring Bells",
        "Ring Clav",
        "CZ Lead",
        "CZ Organ",
        "Steel And Sitar",
        "Electric Bass",
        "Harpsichord",
        "Jazz Guitar",
        "Folded Lead",
        "Chaos Kit",
        "Breath Choir",
        "Morph Choir",
        "Glass Morph",
        "Twin Drawbars",
        "Supersaw Lead",
        "Wide Strings",
        "Phase Pad",
        "Celesta Bells",
        "Dirty Tine",
        "Honky Tonk",
        "FM Tines",
        "FM Brass",
        "FM Marimba",
        "Bell Choir",
        "FM Bass",
        "Overdriven Organ",
        "Sine Pad",
        "Funk Clav",
    ];

    #[test]
    fn default_is_the_first_ensemble() {
        // The one position that is load-bearing rather than curation. `Default` is
        // what a fresh launch loads and what the picker opens on, and it is what
        // the reference describes `reverb_size` and the other starting points
        // against. Everything else about the order is the player's to move.
        assert_eq!(builtin_ensembles()[0].name, "Default");
    }

    #[test]
    fn every_showcase_ensemble_is_still_shipped() {
        let ensembles = builtin_ensembles();
        for name in SHOWCASE {
            assert!(
                ensembles.iter().any(|ensemble| ensemble.name == name),
                "the showcase lists {name}, which is not shipped"
            );
        }
    }

    #[test]
    fn the_showcase_ensembles_layer_three_different_instruments() {
        // The showcase is one ensemble per instrument design, built as three
        // *different* designs stacked, with the low as the foundation, the mid as
        // the identity and the high as the sparkle. The rule is what makes them a
        // demonstration of the synth rather than the same instrument at three
        // levels, which is what the original palette is and is meant to be.
        let ensembles = builtin_ensembles();
        for name in SHOWCASE {
            let ensemble = ensembles
                .iter()
                .find(|ensemble| ensemble.name == name)
                .unwrap_or_else(|| panic!("{name} is not shipped"));
            let designs: Vec<&str> = Ensemble::REGISTERS
                .iter()
                .zip(ensemble.placements())
                .map(|(_, placement)| design_of(&placement.instrument))
                .collect();
            let mut unique = designs.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(
                unique.len(),
                3,
                "{} is {} at three levels: {:?}",
                ensemble.name,
                if unique.len() == 1 {
                    "one instrument"
                } else {
                    "only two"
                },
                designs
            );
        }
    }

    /// The designs that are a bass, which is what a `low` register has to be.
    const BASS_DESIGNS: [&str; 10] = [
        "Sub Bass",
        "Upright Bass",
        "Electric Bass",
        "Acid Bass",
        "Bassy",
        "FM Bass",
        "Through-Zero Bass",
        "Growl Bass",
        "Bass Guitar",
        "Deep Sine",
    ];

    /// The drums. A drum has no register, so these are the one exception.
    const DRUM_DESIGNS: [&str; 4] = ["Kick", "Snare", "Hi-Hat", "Chaos Perc"];

    #[test]
    fn every_shipped_ensemble_has_a_bass_a_mid_and_a_lead() {
        // The rule the palette is built on: the chord is voiced as the root an
        // octave below, the chord itself, and the top an octave above — so the
        // three registers are three *parts*, and each has a job. `low` is always a
        // bass, `high` is always something that can carry a line above the chord,
        // and no ensemble is one instrument at three pitch levels.
        //
        // It is worth asserting because the palette spent a long time being
        // exactly that: forty-two of the seventy-four were the same design in all
        // three registers, so "Drawbar Organ" was an organ, an organ and an organ,
        // which is one instrument with an octave doubler rather than a voicing.
        //
        // The middle is deliberately *not* constrained to a family. It is the
        // ensemble's identity — the name describes it — and it is the one slot
        // with no headroom: eighty-one designs, seventy-four ensembles, so no two
        // may share one. Constraining it here would make the palette unbuildable.
        for ensemble in builtin_ensembles() {
            let low = design_of(&ensemble.low.instrument);
            let mid = design_of(&ensemble.mid.instrument);
            let high = design_of(&ensemble.high.instrument);

            let mut designs = [low, mid, high];
            designs.sort_unstable();

            if DRUM_DESIGNS.contains(&mid) {
                // The documented exception: a drum is at every pitch at once, and
                // a kit is the only way to have three of them.
                assert!(
                    DRUM_DESIGNS.contains(&low) && DRUM_DESIGNS.contains(&high),
                    "{} is a drum in the middle with {low} and {high} around it",
                    ensemble.name
                );
                continue;
            }

            assert!(
                designs[0] != designs[1] && designs[1] != designs[2],
                "{} is {mid} at all three levels",
                ensemble.name
            );
            assert!(
                BASS_DESIGNS.contains(&low),
                "{} has {low} in the low register, which is not a bass",
                ensemble.name
            );
            assert!(
                !BASS_DESIGNS.contains(&high) && !DRUM_DESIGNS.contains(&high),
                "{} has {high} in the high register, which cannot lead",
                ensemble.name
            );
        }
    }

    #[test]
    fn every_shipped_ensemble_has_its_own_mid_instrument() {
        // The mid register plays most of the chord, so it carries an ensemble's
        // identity: two ensembles sharing one is two names for one sound. This is
        // the cheap, readable half of
        // `synth::tests::no_two_shipped_ensembles_are_the_same_sound`, which
        // catches the case a name cannot — two *different* designs that render
        // nearly the same.
        let ensembles = builtin_ensembles();
        let mut seen: Vec<(&str, &str)> = ensembles
            .iter()
            .map(|ensemble| (design_of(&ensemble.mid.instrument), ensemble.name.as_str()))
            .collect();
        seen.sort_unstable();
        for pair in seen.windows(2) {
            assert_ne!(
                pair[0].0, pair[1].0,
                "{} and {} are both a {} in the middle",
                pair[0].1, pair[1].1, pair[0].0
            );
        }
    }

    /// An instrument's design: its name with the register hint taken off.
    ///
    /// `Plucky low` and `Plucky high` are one design rather than two, which is
    /// what makes them interchangeable parts of a stack. A voicing *is* a design
    /// of its own: `Rhodes Bright` and `Rhodes Dark` share a name and count as
    /// two, which is why neither can be the other's mid.
    fn design_of(name: &str) -> &str {
        name.trim_end_matches(" low")
            .trim_end_matches(" mid")
            .trim_end_matches(" high")
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
    fn the_shipped_mixers_use_the_two_aux_units_and_nothing_else() {
        // The half of the fingerprint's promise it can still not make. A chain is
        // one of a composed channel's fields now, so the fingerprint sees it and
        // this test does not have to; the two *aux* units are mixer fields the
        // fingerprint does see but whose identity is the interesting part, and a
        // shipped ensemble turning either of them into something other than the
        // reverb and the delay the old file implied would be a change nobody
        // asked for. The delay send is shipped at zero on every register and the
        // delay return at zero on every mixer, which is what makes the delay
        // aux a feature a user can reach rather than one the palette already
        // uses.
        for ensemble in builtin_ensembles() {
            for (register, placement) in Ensemble::REGISTERS.iter().zip(ensemble.placements()) {
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
            // The reverb is the tank that was already here, with the *shipped*
            // size in it. The size is the one parameter the old file carried, so
            // it is the one that differs from the default unit.
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
