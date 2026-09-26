//! The rhythm pattern *palette*: named patterns in `rhythms.toml`.
//!
//! Mirrors `presets.rs` deliberately — same file-when-missing behaviour, same
//! `add`-replaces-by-name rule, same TOML-first testing style. The file is user
//! state that "Save Pattern As..." rewrites.
//!
//! Nothing plays from here. A progression entry *owns* its rhythm, so assigning
//! one of these clones it into the entry; the library is only the set of
//! starting points the `pattern` row offers.
//!
//! This is the only filesystem code in the rhythm feature; the model itself
//! stays pure in `crate::rhythm`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::rhythm::{builtin_patterns, RhythmPattern};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RhythmFile {
    rhythms: Vec<RhythmPattern>,
}

/// Every pattern this install knows, in file order.
#[derive(Clone, Debug, Default)]
pub struct RhythmStore {
    pub patterns: Vec<RhythmPattern>,
}

impl RhythmStore {
    /// Load the library, writing the built-ins first if the file is missing.
    pub fn load(path: &Path) -> io::Result<Self> {
        if !path.exists() {
            let store = RhythmStore {
                patterns: builtin_patterns(),
            };
            store.save(path)?;
            return Ok(store);
        }
        let text = fs::read_to_string(path)?;
        Self::from_toml(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = self
            .to_toml()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        fs::write(path, text)
    }

    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        let file = RhythmFile {
            rhythms: self.patterns.clone(),
        };
        toml::to_string_pretty(&file)
    }

    /// Parse and validate a library document.
    ///
    /// Validation lives in `RhythmPattern`'s own `Deserialize`, so a malformed
    /// step grid, a wrong resolution or layers that disagree on their grid all
    /// fail here rather than at play time.
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        let file: RhythmFile = toml::from_str(text)?;
        Ok(RhythmStore {
            patterns: file.rhythms,
        })
    }

    /// Add a pattern, replacing any existing one with the same name.
    pub fn add(&mut self, pattern: RhythmPattern) {
        if let Some(existing) = self.patterns.iter_mut().find(|p| p.name == pattern.name) {
            *existing = pattern;
        } else {
            self.patterns.push(pattern);
        }
    }

    pub fn find(&self, name: &str) -> Option<&RhythmPattern> {
        self.patterns.iter().find(|p| p.name == name)
    }

    /// A unique name derived from `base`, so saving over an existing pattern
    /// never silently merges two takes.
    pub fn unique_name(&self, base: &str) -> String {
        if self.find(base).is_none() {
            return base.to_string();
        }
        let mut n = 2;
        loop {
            let candidate = format!("{} {}", base, n);
            if self.find(&candidate).is_none() {
                return candidate;
            }
            n += 1;
        }
    }
}

/// Where the library lives, relative to the working directory.
pub fn default_path() -> PathBuf {
    PathBuf::from("rhythms.toml")
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "chord-tool-rhythms-{}-{}.toml",
            name,
            std::process::id()
        ));
        p
    }

    fn store() -> RhythmStore {
        RhythmStore {
            patterns: builtin_patterns(),
        }
    }

    #[test]
    fn the_built_ins_are_the_model_built_ins() {
        let store = RhythmStore {
            patterns: builtin_patterns(),
        };
        assert_eq!(store.patterns.len(), 7);
        assert!(store.find("Offbeat Eighths").is_some());
        assert!(store.find("Held Half").is_some());
    }

    #[test]
    fn toml_round_trip_preserves_every_pattern() {
        let store = store();
        let text = store.to_toml().unwrap();
        let back = RhythmStore::from_toml(&text).unwrap();
        assert_eq!(store.patterns, back.patterns);
    }

    #[test]
    fn an_empty_store_round_trips() {
        let store = RhythmStore::default();
        assert!(store.patterns.is_empty());
        let text = store.to_toml().unwrap();
        assert_eq!(RhythmStore::from_toml(&text).unwrap().patterns, Vec::new());
    }

    #[test]
    fn load_writes_the_built_ins_when_the_file_is_missing() {
        let path = temp_path("create");
        let _ = fs::remove_file(&path);
        assert!(!path.exists());

        let store = RhythmStore::load(&path).unwrap();
        assert!(path.exists(), "load must create the file");
        assert_eq!(store.patterns.len(), 7);

        // And the file it wrote can be read straight back.
        let again = RhythmStore::load(&path).unwrap();
        assert_eq!(again.patterns, store.patterns);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_then_load_keeps_a_multi_layer_pattern() {
        let path = temp_path("stack");
        let mut store = RhythmStore::default();
        let mut pattern = RhythmPattern::from_step_string("Stack", 0.4, "x--x--x-").unwrap();
        pattern.layers.push(
            crate::rhythm::RhythmLayer::from_step_string(0.7, "--------").unwrap(),
        );
        store.add(pattern.clone());

        store.save(&path).unwrap();
        let back = RhythmStore::load(&path).unwrap();
        assert_eq!(back.patterns, vec![pattern]);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_malformed_step_grid_is_reported_on_load() {
        let path = temp_path("malformed");
        fs::write(
            &path,
            "[[rhythms]]\nname = \"Bad\"\ngate = 0.5\n\n[[rhythms.layers]]\nsteps = \"xxo-\"\n",
        )
        .unwrap();

        let err = RhythmStore::load(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains('o'), "message was: {}", err);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn add_appends_a_new_pattern() {
        let mut store = store();
        let before = store.patterns.len();
        let p = RhythmPattern::from_step_string("Mine", 0.5, "xxxxxxxx").unwrap();
        store.add(p);
        assert_eq!(store.patterns.len(), before + 1);
    }

    #[test]
    fn add_replaces_an_existing_pattern_in_place() {
        let mut store = store();
        let before = store.patterns.len();
        let mut updated = store.find("Offbeat Eighths").unwrap().clone();
        updated.hold = 864;
        store.add(updated);
        assert_eq!(store.patterns.len(), before, "no new pattern");
        assert_eq!(store.find("Offbeat Eighths").unwrap().hold, 864);
    }

    #[test]
    fn the_built_ins_are_written_in_playing_order() {
        let store = store();
        let names: Vec<&str> = store.patterns.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Quarters",
                "Eighths",
                "Offbeat Eighths",
                "Syncopated 16ths",
                "Sixteenth Pulse",
                "Held Half",
                "Held 3/4"
            ]
        );
    }

    #[test]
    fn find_reaches_a_pattern_by_name() {
        let store = store();
        assert!(store.find("Not Here").is_none());
        assert_eq!(store.find("Quarters").unwrap().steps_per_bar(), 4);
    }

    #[test]
    fn unique_name_never_collides_with_a_saved_pattern() {
        let store = store();
        assert_eq!(store.unique_name("New"), "New");
        assert_eq!(store.unique_name("Eighths"), "Eighths 2");

        let mut with_clash = store.clone();
        let mut second = with_clash.find("Eighths").unwrap().clone();
        second.name = "Eighths 2".to_string();
        with_clash.add(second);
        assert_eq!(with_clash.unique_name("Eighths"), "Eighths 3");
    }
}
