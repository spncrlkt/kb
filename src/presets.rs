//! Patches: named synth configurations, loadable from and saveable to TOML.
//!
//! A patch captures the full sound-design state: three channels (low, mid,
//! high) plus a mixer section. It does not capture transport state (BPM,
//! key, loop) or the progression.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::synth::Waveform;

// -----------------------------------------------------------------------------
// Patch data
// -----------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChannelPatch {
    pub volume: f32,
    pub waveform: Waveform,
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
    pub cutoff: f32,
    pub resonance: f32,
    pub transpose: f32,
    pub reverb_send: f32,
    pub pan: f32,
}

impl ChannelPatch {
    pub fn neutral(volume: f32, cutoff: f32) -> Self {
        ChannelPatch {
            volume,
            waveform: Waveform::Sine,
            attack: 0.005,
            decay: 0.05,
            sustain: 0.7,
            release: 0.1,
            cutoff,
            resonance: 0.2,
            transpose: 0.0,
            reverb_send: 0.0,
            pan: 0.0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MixerPatch {
    pub reverb_mix: f32,
    pub reverb_size: f32,
    pub master_volume: f32,
    pub master_mute: bool,
    pub preview_fade: f32,
    /// Fraction of one bar that a chord sustains before releasing.
    /// 0.25 = quarter, 0.5 = half, 0.75 = dotted half, 1.0 = whole.
    #[serde(default = "default_note_length")]
    pub note_length: f32,
}

fn default_note_length() -> f32 {
    1.0
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Patch {
    pub name: String,
    pub low: ChannelPatch,
    pub mid: ChannelPatch,
    pub high: ChannelPatch,
    pub mixer: MixerPatch,
}

// -----------------------------------------------------------------------------
// Store
// -----------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PatchFile {
    patches: Vec<Patch>,
}

#[derive(Clone)]
pub struct PatchStore {
    pub patches: Vec<Patch>,
}

impl PatchStore {
    pub fn load(path: &Path) -> io::Result<Self> {
        if !path.exists() {
            let store = PatchStore {
                patches: builtin_patches(),
            };
            store.save(path)?;
            return Ok(store);
        }
        let text = fs::read_to_string(path)?;
        let store = Self::from_toml(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        Ok(store)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = self
            .to_toml()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        fs::write(path, text)
    }

    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        let file = PatchFile {
            patches: self.patches.clone(),
        };
        toml::to_string_pretty(&file)
    }

    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        let file: PatchFile = toml::from_str(text)?;
        Ok(PatchStore {
            patches: file.patches,
        })
    }

    pub fn add(&mut self, patch: Patch) {
        if let Some(existing) = self.patches.iter_mut().find(|p| p.name == patch.name) {
            *existing = patch;
        } else {
            self.patches.push(patch);
        }
    }

    pub fn find(&self, name: &str) -> Option<&Patch> {
        self.patches.iter().find(|p| p.name == name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.patches.iter().map(|p| p.name.as_str()).collect()
    }
}

pub fn default_path() -> PathBuf {
    PathBuf::from("patches.toml")
}

// -----------------------------------------------------------------------------
// Built-in patches
// -----------------------------------------------------------------------------

pub fn builtin_patches() -> Vec<Patch> {
    vec![
        default_patch(),
        warm_pad_patch(),
        plucky_patch(),
        bassy_patch(),
        glass_patch(),
    ]
}

fn default_patch() -> Patch {
    Patch {
        name: "Default".to_string(),
        low: ChannelPatch::neutral(4.0, 4000.0),
        mid: ChannelPatch::neutral(4.0, 4000.0),
        high: ChannelPatch::neutral(4.0, 4000.0),
        mixer: MixerPatch {
            reverb_mix: 0.0,
            reverb_size: 0.5,
            master_volume: 5.0,
            master_mute: false,
            preview_fade: 0.03,
            note_length: 1.0,
        },
    }
}

fn warm_pad_patch() -> Patch {
    Patch {
        name: "Warm Pad".to_string(),
        low: ChannelPatch {
            volume: 3.5,
            waveform: Waveform::Triangle,
            attack: 0.20,
            decay: 0.30,
            sustain: 0.80,
            release: 0.80,
            cutoff: 1500.0,
            resonance: 0.30,
            transpose: 0.0,
            reverb_send: 0.10,
            pan: -0.20,
        },
        mid: ChannelPatch {
            volume: 3.0,
            waveform: Waveform::Saw,
            attack: 0.25,
            decay: 0.30,
            sustain: 0.75,
            release: 0.90,
            cutoff: 2500.0,
            resonance: 0.25,
            transpose: 0.0,
            reverb_send: 0.40,
            pan: 0.0,
        },
        high: ChannelPatch {
            volume: 3.0,
            waveform: Waveform::Sine,
            attack: 0.30,
            decay: 0.30,
            sustain: 0.70,
            release: 1.00,
            cutoff: 4000.0,
            resonance: 0.20,
            transpose: 0.0,
            reverb_send: 0.50,
            pan: 0.20,
        },
        mixer: MixerPatch {
            reverb_mix: 0.40,
            reverb_size: 0.70,
            master_volume: 5.0,
            master_mute: false,
            preview_fade: 0.05,
            note_length: 1.0,
        },
    }
}

fn plucky_patch() -> Patch {
    Patch {
        name: "Plucky".to_string(),
        low: ChannelPatch {
            volume: 4.0,
            waveform: Waveform::Square,
            attack: 0.002,
            decay: 0.10,
            sustain: 0.20,
            release: 0.15,
            cutoff: 2500.0,
            resonance: 0.40,
            transpose: 0.0,
            reverb_send: 0.05,
            pan: -0.15,
        },
        mid: ChannelPatch {
            volume: 3.5,
            waveform: Waveform::Square,
            attack: 0.002,
            decay: 0.08,
            sustain: 0.15,
            release: 0.12,
            cutoff: 3000.0,
            resonance: 0.40,
            transpose: 0.0,
            reverb_send: 0.15,
            pan: 0.0,
        },
        high: ChannelPatch {
            volume: 3.0,
            waveform: Waveform::Triangle,
            attack: 0.002,
            decay: 0.06,
            sustain: 0.10,
            release: 0.10,
            cutoff: 4000.0,
            resonance: 0.30,
            transpose: 0.0,
            reverb_send: 0.25,
            pan: 0.15,
        },
        mixer: MixerPatch {
            reverb_mix: 0.15,
            reverb_size: 0.40,
            master_volume: 5.0,
            master_mute: false,
            preview_fade: 0.02,
            note_length: 0.25,
        },
    }
}

fn bassy_patch() -> Patch {
    Patch {
        name: "Bassy".to_string(),
        low: ChannelPatch {
            volume: 6.0,
            waveform: Waveform::Sine,
            attack: 0.005,
            decay: 0.05,
            sustain: 0.90,
            release: 0.15,
            cutoff: 800.0,
            resonance: 0.10,
            transpose: 0.0,
            reverb_send: 0.0,
            pan: 0.0,
        },
        mid: ChannelPatch {
            volume: 2.5,
            waveform: Waveform::Saw,
            attack: 0.010,
            decay: 0.10,
            sustain: 0.60,
            release: 0.20,
            cutoff: 1800.0,
            resonance: 0.30,
            transpose: 0.0,
            reverb_send: 0.15,
            pan: 0.0,
        },
        high: ChannelPatch {
            volume: 2.0,
            waveform: Waveform::Sine,
            attack: 0.010,
            decay: 0.10,
            sustain: 0.50,
            release: 0.25,
            cutoff: 5000.0,
            resonance: 0.20,
            transpose: 0.0,
            reverb_send: 0.30,
            pan: 0.15,
        },
        mixer: MixerPatch {
            reverb_mix: 0.15,
            reverb_size: 0.40,
            master_volume: 5.0,
            master_mute: false,
            preview_fade: 0.03,
            note_length: 0.5,
        },
    }
}

fn glass_patch() -> Patch {
    Patch {
        name: "Glass".to_string(),
        low: ChannelPatch {
            volume: 3.0,
            waveform: Waveform::Sine,
            attack: 0.10,
            decay: 0.20,
            sustain: 0.60,
            release: 1.20,
            cutoff: 2000.0,
            resonance: 0.15,
            transpose: 0.0,
            reverb_send: 0.30,
            pan: -0.40,
        },
        mid: ChannelPatch {
            volume: 3.0,
            waveform: Waveform::Sine,
            attack: 0.15,
            decay: 0.20,
            sustain: 0.60,
            release: 1.50,
            cutoff: 5000.0,
            resonance: 0.20,
            transpose: 0.0,
            reverb_send: 0.50,
            pan: 0.0,
        },
        high: ChannelPatch {
            volume: 3.0,
            waveform: Waveform::Sine,
            attack: 0.20,
            decay: 0.20,
            sustain: 0.60,
            release: 2.00,
            cutoff: 8000.0,
            resonance: 0.25,
            transpose: 0.0,
            reverb_send: 0.70,
            pan: 0.40,
        },
        mixer: MixerPatch {
            reverb_mix: 0.60,
            reverb_size: 0.85,
            master_volume: 5.0,
            master_mute: false,
            preview_fade: 0.06,
            note_length: 1.0,
        },
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_have_distinct_names() {
        let patches = builtin_patches();
        assert_eq!(patches.len(), 5);
        let mut names: Vec<&str> = patches.iter().map(|p| p.name.as_str()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 5);
    }

    #[test]
    fn toml_round_trip_preserves_all_fields() {
        let store = PatchStore {
            patches: builtin_patches(),
        };
        let text = store.to_toml().unwrap();
        let back = PatchStore::from_toml(&text).unwrap();
        assert_eq!(store.patches.len(), back.patches.len());
        for (a, b) in store.patches.iter().zip(back.patches.iter()) {
            assert_eq!(a, b);
        }
    }

    #[test]
    fn waveform_serializes_as_lowercase_name() {
        let store = PatchStore {
            patches: vec![warm_pad_patch()],
        };
        let text = store.to_toml().unwrap();
        assert!(text.contains("waveform = \"triangle\""));
        assert!(text.contains("waveform = \"saw\""));
        assert!(text.contains("waveform = \"sine\""));
    }

    #[test]
    fn note_length_serializes_and_round_trips() {
        let store = PatchStore {
            patches: builtin_patches(),
        };
        let text = store.to_toml().unwrap();
        assert!(text.contains("note_length"));
        let back = PatchStore::from_toml(&text).unwrap();
        assert_eq!(back.patches[2].mixer.note_length, 0.25); // Plucky
        assert_eq!(back.patches[3].mixer.note_length, 0.5); // Bassy
    }

    #[test]
    fn old_patches_without_note_length_default_to_whole() {
        // A patch file from before note_length existed should still load.
        let text = r#"
[[patches]]
name = "Legacy"

[patches.low]
volume = 4.0
waveform = "sine"
attack = 0.005
decay = 0.05
sustain = 0.7
release = 0.1
cutoff = 4000.0
resonance = 0.2
transpose = 0.0
reverb_send = 0.0
pan = 0.0

[patches.mid]
volume = 4.0
waveform = "sine"
attack = 0.005
decay = 0.05
sustain = 0.7
release = 0.1
cutoff = 4000.0
resonance = 0.2
transpose = 0.0
reverb_send = 0.0
pan = 0.0

[patches.high]
volume = 4.0
waveform = "sine"
attack = 0.005
decay = 0.05
sustain = 0.7
release = 0.1
cutoff = 4000.0
resonance = 0.2
transpose = 0.0
reverb_send = 0.0
pan = 0.0

[patches.mixer]
reverb_mix = 0.0
reverb_size = 0.5
master_volume = 5.0
master_mute = false
preview_fade = 0.03
"#;
        let store = PatchStore::from_toml(text).unwrap();
        assert_eq!(store.patches[0].mixer.note_length, 1.0);
    }

    #[test]
    fn add_appends_new_patch() {
        let mut store = PatchStore {
            patches: builtin_patches(),
        };
        let before = store.patches.len();
        let mut p = default_patch();
        p.name = "Test".to_string();
        store.add(p);
        assert_eq!(store.patches.len(), before + 1);
    }

    #[test]
    fn add_replaces_existing_patch_in_place() {
        let mut store = PatchStore {
            patches: builtin_patches(),
        };
        let before = store.patches.len();
        let mut p = default_patch();
        p.name = "Default".to_string();
        p.mixer.master_volume = 3.0;
        store.add(p);
        assert_eq!(store.patches.len(), before);
        assert_eq!(store.patches[0].mixer.master_volume, 3.0);
    }

    #[test]
    fn find_returns_matching_patch() {
        let store = PatchStore {
            patches: builtin_patches(),
        };
        assert!(store.find("Warm Pad").is_some());
        assert!(store.find("Nonexistent").is_none());
    }

    #[test]
    fn load_creates_file_when_missing() {
        let path = temp_path("create");
        let _ = fs::remove_file(&path);
        assert!(!path.exists());
        let store = PatchStore::load(&path).unwrap();
        assert!(path.exists());
        assert_eq!(store.patches.len(), 5);
        let _ = fs::remove_file(&path);
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
