//! The handful of settings that survive a restart.
//!
//! Deliberately not the progression. A progression is a *document* — it has
//! structure, rhythms and an offset per chord — and there is already a way to
//! keep one: export it. What is here is the state of the instrument rather than
//! the music in it: the key you were playing in, the tempo, and how loud the
//! master was.
//!
//! Three settings, one table, and no shipped defaults to layer over — which is
//! why this is the one tracked-looking file in the crate that is simply
//! gitignored rather than split in two. There is nothing to merge with: whatever
//! is in the file is what the last run left behind.
//!
//! # A bad file is never fatal
//!
//! `load` returns a `Result` like every other file in this crate, but the caller
//! falls back to the defaults rather than refusing to start: a settings file is
//! a convenience, and losing it should cost a tempo, not a session. `#[serde(default)]`
//! on the struct means a file with two of the three keys still gives the third,
//! and `Key::new` clamps a tonic that is out of range, so a hand-edited file
//! degrades the same way an out-of-date one does.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::music::{Key, Scale};
use crate::synth::range;

/// What a fresh install opens with: C major, 120 bpm, master at five.
pub const DEFAULT_BPM: u16 = 120;
pub const DEFAULT_TONIC: u8 = 60;
pub const DEFAULT_MASTER_VOLUME: f32 = 5.0;

/// What the app remembers between runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub key: Key,
    pub bpm: u16,
    pub master_volume: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            key: Key::new(DEFAULT_TONIC, Scale::Major),
            bpm: DEFAULT_BPM,
            master_volume: DEFAULT_MASTER_VOLUME,
        }
    }
}

impl Settings {
    /// The settings as they stand, from a transport and the live master volume.
    pub fn capture(transport: &crate::transport::Transport, master_volume: f32) -> Self {
        Settings {
            key: transport.key(),
            bpm: transport.bpm(),
            master_volume,
        }
    }

    /// Read the file, or fail with the reason.
    ///
    /// A missing file is *not* an error — it is a first run — so it is a
    /// `NotFound` the caller can distinguish from a file that would not parse.
    pub fn load(path: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
    }

    /// Write the file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(|e| io::Error::other(e.to_string()))?;
        fs::write(path, text)
    }

    /// Pull every value inside the range the UI can reach.
    ///
    /// A file can say anything: a tempo of 4000, a master volume of 900. The
    /// arrows could not reach either, so a value from a hand-edited file has to
    /// be pulled back to something the transport and the mixer understand.
    pub fn clamped(&self) -> Self {
        Settings {
            key: Key::new(self.key.tonic, self.key.scale),
            bpm: self
                .bpm
                .clamp(crate::transport::BPM_MIN, crate::transport::BPM_MAX),
            master_volume: if self.master_volume.is_finite() {
                self.master_volume
                    .clamp(range::MASTER_VOLUME.0, range::MASTER_VOLUME.1)
            } else {
                DEFAULT_MASTER_VOLUME
            },
        }
    }
}

pub fn path() -> PathBuf {
    PathBuf::from("settings.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "chord-tool-test-{}-{}.toml",
            name,
            std::process::id()
        ));
        p
    }

    #[test]
    fn the_defaults_are_what_a_fresh_install_opens_with() {
        let s = Settings::default();
        assert_eq!(s.key.tonic, 60);
        assert_eq!(s.key.scale, Scale::Major);
        assert_eq!(s.bpm, 120);
        assert_eq!(s.master_volume, 5.0);
    }

    #[test]
    fn settings_round_trip_through_toml() {
        let s = Settings {
            key: Key::new(63, Scale::Minor),
            bpm: 96,
            master_volume: 3.5,
        };
        let text = toml::to_string_pretty(&s).unwrap();
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), s);
    }

    #[test]
    fn a_file_missing_a_key_still_gives_the_others() {
        // An older build wrote two of the three, or a hand edit removed one.
        let s: Settings = toml::from_str("bpm = 88").unwrap();
        assert_eq!(s.bpm, 88);
        assert_eq!(s.key, Settings::default().key);
        assert_eq!(s.master_volume, Settings::default().master_volume);
    }

    #[test]
    fn a_load_that_cannot_find_the_file_says_so_rather_than_guessing() {
        let path = temp_path("settings-missing");
        let _ = fs::remove_file(&path);
        let error = Settings::load(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        // A missing file is a first run, so the caller's fallback is the defaults.
        assert_eq!(Settings::default().bpm, DEFAULT_BPM);
    }

    #[test]
    fn a_malformed_file_is_an_error_rather_than_a_silent_default() {
        let path = temp_path("settings-broken");
        fs::write(&path, "bpm = \"not a number\"").unwrap();
        let error = Settings::load(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_saved_file_reloads_exactly() {
        let path = temp_path("settings-round");
        let _ = fs::remove_file(&path);
        let s = Settings {
            key: Key::new(58, Scale::Minor),
            bpm: 143,
            master_volume: 6.5,
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_hand_edited_file_is_pulled_inside_the_ui_range() {
        let s = Settings {
            key: Key::new(200, Scale::Major),
            bpm: 4000,
            master_volume: 900.0,
        }
        .clamped();
        assert!(s.key.tonic <= crate::music::TONIC_MAX);
        assert!(s.key.tonic >= crate::music::TONIC_MIN);
        assert!((crate::transport::BPM_MIN..=crate::transport::BPM_MAX).contains(&s.bpm));
        assert!(s.master_volume <= range::MASTER_VOLUME.1);
    }

    #[test]
    fn a_nan_master_volume_becomes_the_default_rather_than_a_silent_mix() {
        let s = Settings {
            master_volume: f32::NAN,
            ..Settings::default()
        }
        .clamped();
        assert_eq!(s.master_volume, DEFAULT_MASTER_VOLUME);
    }

    #[test]
    fn capturing_reads_the_transport_and_the_live_volume() {
        let transport = crate::transport::Transport::new(Key::new(65, Scale::Minor));
        transport.set_bpm(101);
        let s = Settings::capture(&transport, 2.5);
        assert_eq!(s.key, Key::new(65, Scale::Minor));
        assert_eq!(s.bpm, 101);
        assert_eq!(s.master_volume, 2.5);
    }
}
