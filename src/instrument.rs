//! A library of named single-register instrument designs.
//!
//! This exists for one workflow: putting a different instrument into one of the
//! three registers *while the loop is playing*, and hearing it. The Synth panel
//! already changes a sound live — its arrows write straight through the
//! lock-free parameters the audio callback reads — so swapping a whole voice is
//! the same mechanism applied to a couple of dozen fields at once.
//!
//! An instrument is a [`VoicePatch`] and nothing else. It has no level, no pan
//! and no transpose, because those are decisions about where a sound *sits*
//! rather than what it *is*, and they belong to the `Placement` that puts it in
//! an ensemble. That is what makes an instrument mean the same thing in the low
//! register as in the high one, and what makes auditioning one leave the mix
//! alone.
//!
//! # The shipped library is the file
//!
//! `instruments.toml` is compiled in with `include_str!` and parsed at start-up.
//! It is *authored data*: editing it is how the shipped library is changed, with
//! no Rust to keep in step and no regeneration step. The other palettes in this
//! crate keep Rust as the source and regenerate their file; this one is the
//! other way round on purpose, because a library of sounds is content rather
//! than code.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::voice::VoicePatch;

/// One named voice.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Instrument {
    pub name: String,
    pub voice: VoicePatch,
}

/// The shipped library, compiled in from the tracked file.
///
/// Panics only on a malformed file, which is a repository bug that
/// `every_shipped_instrument_parses` catches before it can be committed.
pub fn builtin_instruments() -> Vec<Instrument> {
    from_toml(include_str!("../instruments.toml")).expect("instruments.toml is valid TOML")
}

// -----------------------------------------------------------------------------
// Store
// -----------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
struct InstrumentFile {
    instruments: Vec<Instrument>,
}

/// The library: the shipped instruments with the user's own layered over them.
#[derive(Clone, Debug, Default)]
pub struct InstrumentStore {
    /// What the UI offers, in order.
    pub instruments: Vec<Instrument>,
    /// Just the user's own entries. This is what [`Self::save`] writes.
    user: Vec<Instrument>,
}

impl InstrumentStore {
    /// Load the shipped library and the user's own file, creating the latter if
    /// missing.
    ///
    /// There is no defaults *path* any more: the shipped library is compiled in,
    /// so a checkout cannot lose it and there is nothing to read from disk
    /// except the user's additions.
    pub fn load(user_path: &Path) -> io::Result<Self> {
        let mut store = InstrumentStore {
            instruments: builtin_instruments(),
            user: Vec::new(),
        };
        if user_path.exists() {
            for instrument in read(user_path)? {
                store.add(instrument);
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

    /// Add an instrument, replacing any entry of the same name.
    pub fn add(&mut self, instrument: Instrument) {
        upsert(&mut self.instruments, instrument.clone());
        upsert(&mut self.user, instrument);
    }

    /// The voice of a named instrument, for resolving a placement.
    pub fn voice_of(&self, name: &str) -> Option<VoicePatch> {
        self.instruments
            .iter()
            .find(|i| i.name == name)
            .map(|i| i.voice.clone())
    }

    /// The name of the instrument with this voice, if the library has it.
    ///
    /// Used by the migration, so that two registers sharing a voice share one
    /// library entry rather than each getting a copy.
    pub fn name_for_voice(&self, voice: &VoicePatch) -> Option<String> {
        self.instruments
            .iter()
            .find(|i| i.voice == *voice)
            .map(|i| i.name.clone())
    }

    /// Where `name` sits in the list, for a picker that opens on it.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.instruments.iter().position(|i| i.name == name)
    }

    /// Step `delta` places through the library, wrapping.
    pub fn step(&self, from: Option<usize>, delta: i32) -> Option<(usize, &Instrument)> {
        let len = self.instruments.len();
        if len == 0 {
            return None;
        }
        let index = match from {
            Some(i) => (i as i32 + delta).rem_euclid(len as i32) as usize,
            None if delta >= 0 => 0,
            None => len - 1,
        };
        Some((index, &self.instruments[index]))
    }

    #[cfg(test)]
    pub fn with_builtins() -> Self {
        InstrumentStore {
            instruments: builtin_instruments(),
            user: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn from_instruments(instruments: Vec<Instrument>) -> Self {
        InstrumentStore {
            instruments: instruments.clone(),
            user: instruments,
        }
    }
}

/// Replace the entry of this name, or append it.
fn upsert(instruments: &mut Vec<Instrument>, instrument: Instrument) {
    match instruments.iter_mut().find(|i| i.name == instrument.name) {
        Some(existing) => *existing = instrument,
        None => instruments.push(instrument),
    }
}

fn read(path: &Path) -> io::Result<Vec<Instrument>> {
    let text = fs::read_to_string(path)?;
    from_toml(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

/// Parse an instrument document. Unknown keys are ignored, so a file written by
/// a different build still loads.
pub fn from_toml(text: &str) -> Result<Vec<Instrument>, toml::de::Error> {
    let file: InstrumentFile = toml::from_str(text)?;
    Ok(file.instruments)
}

pub fn to_toml(instruments: &[Instrument]) -> Result<String, toml::ser::Error> {
    toml::to_string_pretty(&InstrumentFile {
        instruments: instruments.to_vec(),
    })
}

pub fn user_path() -> PathBuf {
    PathBuf::from("instruments.user.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{range, FilterType, Waveform};

    #[test]
    fn every_shipped_instrument_parses() {
        // `builtin_instruments` panics on a malformed file, so this is what turns
        // that panic into a failing test.
        let instruments = builtin_instruments();
        assert!(
            instruments.len() >= 30,
            "only {} instruments",
            instruments.len()
        );
    }

    #[test]
    fn the_library_has_distinct_names() {
        let instruments = builtin_instruments();
        let mut names: Vec<&str> = instruments.iter().map(|i| i.name.as_str()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), instruments.len());
    }

    #[test]
    fn no_two_instruments_are_the_same_voice() {
        // Two names for one voice makes the picker lie about how much choice
        // there is.
        let instruments = builtin_instruments();
        for i in 0..instruments.len() {
            for j in (i + 1)..instruments.len() {
                assert_ne!(
                    instruments[i].voice, instruments[j].voice,
                    "{} and {} are the same voice",
                    instruments[i].name, instruments[j].name
                );
            }
        }
    }

    #[test]
    fn every_voice_is_inside_the_range_the_panel_can_reach() {
        // An instrument whose values the arrows would snap is one that changes
        // the first time it is touched.
        for instrument in builtin_instruments() {
            let v = &instrument.voice;
            let checks: [(&str, f32, (f32, f32)); 31] = [
                ("noise level", v.noise_level, range::NOISE_LEVEL),
                ("pulse width", v.pulse_width, range::PULSE_WIDTH),
                ("attack", v.attack, range::ATTACK),
                ("decay", v.decay, range::DECAY),
                ("sustain", v.sustain, range::SUSTAIN),
                ("release", v.release, range::RELEASE),
                ("env curve", v.env_curve, range::ENV_CURVE),
                ("glide", v.glide, range::GLIDE),
                ("cutoff", v.cutoff, range::CUTOFF),
                ("resonance", v.resonance, range::RESONANCE),
                ("filter env", v.filter_env, range::FILTER_ENV),
                ("filter attack", v.filter_attack, range::FILTER_ATTACK),
                ("filter decay", v.filter_decay, range::FILTER_DECAY),
                ("key track", v.key_track, range::KEY_TRACK),
                ("lfo pitch", v.lfo_pitch, range::LFO_PITCH),
                ("lfo cutoff", v.lfo_cutoff, range::LFO_CUTOFF),
                ("lfo amp", v.lfo_amp, range::LFO_AMP),
                ("lfo pwm", v.lfo_pwm, range::LFO_PWM),
                ("unison", v.unison, range::UNISON),
                ("detune", v.detune, range::DETUNE),
                ("drive", v.drive, range::DRIVE),
                ("vel cutoff", v.vel_cutoff, range::VEL_CUTOFF),
                ("vel pwm", v.vel_pwm, range::VEL_PWM),
                ("position", v.position, range::POSITION),
                ("phase dist", v.phase_dist, range::PHASE_DIST),
                ("osc2 interval", v.osc2_interval, range::OSC2_INTERVAL),
                ("osc2 level", v.osc2_level, range::OSC2_LEVEL),
                ("osc2 fm", v.osc2_fm, range::OSC2_FM),
                ("pluck decay", v.pluck_decay, range::PLUCK_DECAY),
                ("pluck damp", v.pluck_damp, range::PLUCK_DAMP),
                ("pluck burst", v.pluck_burst, range::PLUCK_BURST),
            ];
            for (field, value, (lo, hi)) in checks {
                assert!(
                    (lo..=hi).contains(&value),
                    "{} {} is {}, outside {}..{}",
                    instrument.name,
                    field,
                    value,
                    lo,
                    hi
                );
            }
            assert!(
                (v.unison - v.unison.round()).abs() < 1e-6,
                "{} unison is fractional",
                instrument.name
            );
            assert_eq!(
                FilterType::from_f32(v.filter_type as i32 as f32),
                v.filter_type
            );
            assert_eq!(Waveform::from_f32(v.waveform as i32 as f32), v.waveform);
            // The second oscillator's row offers everything but the string, so a
            // shipped patch naming a pluck there would be one the panel could
            // never have produced.
            assert_ne!(
                v.osc2_waveform,
                Waveform::Pluck,
                "{} sets the second oscillator to a plucked string",
                instrument.name
            );
            assert_eq!(
                Waveform::from_f32(v.osc2_waveform as i32 as f32),
                v.osc2_waveform
            );
        }
    }

    #[test]
    fn toml_round_trip_preserves_every_field() {
        let store = InstrumentStore::from_instruments(builtin_instruments());
        let text = to_toml(&store.instruments).unwrap();
        assert_eq!(from_toml(&text).unwrap(), store.instruments);
    }

    #[test]
    fn a_voice_can_be_found_by_name_and_a_name_by_voice() {
        let store = InstrumentStore::with_builtins();
        for instrument in &store.instruments {
            assert_eq!(
                store.voice_of(&instrument.name),
                Some(instrument.voice.clone())
            );
            assert_eq!(
                store.name_for_voice(&instrument.voice),
                Some(instrument.name.clone())
            );
        }
        assert_eq!(store.voice_of("Nonexistent"), None);
    }

    #[test]
    fn load_creates_the_user_file() {
        let user = temp_path("instrument-user");
        let _ = fs::remove_file(&user);
        let store = InstrumentStore::load(&user).unwrap();
        assert!(user.exists(), "the user's file is created to be edited");
        assert_eq!(store.instruments.len(), builtin_instruments().len());
        let _ = fs::remove_file(&user);
    }

    #[test]
    fn a_saved_instrument_shadows_the_shipped_one_by_name() {
        let user = temp_path("instrument-shadow");
        let _ = fs::remove_file(&user);

        let mut store = InstrumentStore::load(&user).unwrap();
        let mut mine = builtin_instruments()[0].clone();
        let name = mine.name.clone();
        mine.voice.cutoff = 1234.0;
        store.add(mine);
        store.save(&user).unwrap();

        let again = InstrumentStore::load(&user).unwrap();
        assert_eq!(again.instruments.len(), builtin_instruments().len());
        assert_eq!(again.voice_of(&name).unwrap().cutoff, 1234.0);
        let _ = fs::remove_file(&user);
    }

    #[test]
    fn stepping_walks_the_library_and_wraps() {
        let store = InstrumentStore::with_builtins();
        let len = store.instruments.len();
        assert_eq!(store.step(None, 1).unwrap().0, 0);
        assert_eq!(store.step(None, -1).unwrap().0, len - 1);
        assert_eq!(store.step(Some(0), -1).unwrap().0, len - 1);
        assert_eq!(store.step(Some(len - 1), 1).unwrap().0, 0);
        assert_eq!(store.step(Some(3), 2).unwrap().0, 5);
        let (index, instrument) = store.step(Some(0), 1000).unwrap();
        assert!(index < len);
        assert!(!instrument.name.is_empty());
    }

    #[test]
    fn stepping_an_empty_library_is_nothing_rather_than_a_panic() {
        let store = InstrumentStore::from_instruments(Vec::new());
        assert!(store.step(None, 1).is_none());
        assert!(store.step(Some(0), 1).is_none());
    }

    #[test]
    fn a_name_finds_its_place_in_the_list() {
        let store = InstrumentStore::with_builtins();
        for (i, instrument) in store.instruments.iter().enumerate() {
            assert_eq!(store.index_of(&instrument.name), Some(i));
        }
        assert_eq!(store.index_of("Nonexistent"), None);
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
