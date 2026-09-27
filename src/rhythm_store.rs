//! The rhythm pattern *palette*: named patterns in `rhythms.toml`.
//!
//! The same two-file arrangement the other palettes use — see
//! [`crate::instrument`] and [`crate::eq`] — with the one difference that these
//! defaults keep Rust as their source and regenerate `rhythms.toml`, while the
//! instrument and EQ libraries are authored directly in their files. The
//! behaviour is otherwise identical: created when missing, `add` replaces by
//! name, the tracked file is never written.
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

/// The palette: the shipped defaults with the user's own layered over them.
///
/// Two files, for the same reason the other palettes have two: the defaults are
/// tracked in the repository, so a pattern added or changed by a later build
/// reaches an install that has saved patterns of its own. The user's file is
/// gitignored and the only one `[Save Pattern As...]` writes.
#[derive(Clone, Debug, Default)]
pub struct RhythmStore {
    /// What the UI offers, in order: the defaults, with a user entry of the same
    /// name replacing one and a user entry with a new name appended.
    pub patterns: Vec<RhythmPattern>,
    /// Just the user's own entries. This is what [`Self::save`] writes.
    user: Vec<RhythmPattern>,
}

impl RhythmStore {
    /// Load the defaults and the user's own file.
    ///
    /// The user's file is *created* when it is missing, so the path exists to be
    /// edited and a later save has somewhere to land. The defaults file is never
    /// written: it belongs to the repository.
    ///
    /// Validation lives in `RhythmPattern`'s own `Deserialize`, so a malformed
    /// step grid, a wrong resolution or layers that disagree on their grid all
    /// fail at load rather than at play time.
    pub fn load(defaults_path: &Path, user_path: &Path) -> io::Result<Self> {
        // A checkout that lost the tracked file still starts: the built-ins are
        // compiled in as well, and a test keeps the two identical.
        let defaults = if defaults_path.exists() {
            read(defaults_path)?
        } else {
            builtin_patterns()
        };

        let mut store = RhythmStore {
            patterns: defaults,
            user: Vec::new(),
        };
        if user_path.exists() {
            for pattern in read(user_path)? {
                store.add(pattern);
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

    /// Add a pattern, replacing any existing one with the same name.
    ///
    /// Both lists move together: the palette the UI reads, and the user's own,
    /// which is what a save writes.
    pub fn add(&mut self, pattern: RhythmPattern) {
        upsert(&mut self.patterns, pattern.clone());
        upsert(&mut self.user, pattern);
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

    /// A store whose defaults are the built-ins and whose user file is empty, for
    /// a test that wants the shipped palette without touching a file.
    #[cfg(test)]
    pub fn with_builtins() -> Self {
        RhythmStore {
            patterns: builtin_patterns(),
            user: Vec::new(),
        }
    }
}

/// Replace the entry of this name, or append it.
fn upsert(patterns: &mut Vec<RhythmPattern>, pattern: RhythmPattern) {
    match patterns.iter_mut().find(|p| p.name == pattern.name) {
        Some(existing) => *existing = pattern,
        None => patterns.push(pattern),
    }
}

fn read(path: &Path) -> io::Result<Vec<RhythmPattern>> {
    let text = fs::read_to_string(path)?;
    from_toml(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

/// Parse and validate a library document.
pub fn from_toml(text: &str) -> Result<Vec<RhythmPattern>, toml::de::Error> {
    let file: RhythmFile = toml::from_str(text)?;
    Ok(file.rhythms)
}

/// Render a library document.
pub fn to_toml(patterns: &[RhythmPattern]) -> Result<String, toml::ser::Error> {
    toml::to_string_pretty(&RhythmFile {
        rhythms: patterns.to_vec(),
    })
}

/// The shipped defaults, tracked in the repository.
pub fn default_path() -> PathBuf {
    PathBuf::from("rhythms.toml")
}

/// The user's own patterns. Gitignored, and created empty when missing.
pub fn user_path() -> PathBuf {
    PathBuf::from("rhythms.user.toml")
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
        RhythmStore::with_builtins()
    }

    #[test]
    fn the_built_ins_are_the_model_built_ins() {
        let store = RhythmStore::with_builtins();
        assert_eq!(store.patterns.len(), 23);
        assert!(store.find("Offbeat Eighths").is_some());
        assert!(store.find("Held Half").is_some());
        assert!(store.find("Swung Eighths").is_some());
        assert!(store.find("Jazz Chorus 4/4").is_some());
    }

    #[test]
    fn toml_round_trip_preserves_every_pattern() {
        let store = store();
        let text = to_toml(&store.patterns).unwrap();
        assert_eq!(from_toml(&text).unwrap(), store.patterns);
    }

    #[test]
    fn an_empty_store_round_trips() {
        let store = RhythmStore::default();
        assert!(store.patterns.is_empty());
        let text = to_toml(&store.patterns).unwrap();
        assert_eq!(from_toml(&text).unwrap(), Vec::new());
    }

    #[test]
    fn load_creates_the_user_file_when_missing_and_never_the_defaults() {
        let defaults = temp_path("create-defaults");
        let user = temp_path("create-user");
        let _ = fs::remove_file(&defaults);
        let _ = fs::remove_file(&user);

        let store = RhythmStore::load(&defaults, &user).unwrap();
        assert!(user.exists(), "the user's file is created to be edited");
        assert!(!defaults.exists(), "the tracked defaults are never written");
        assert_eq!(store.patterns.len(), 23, "the built-ins stood in for them");

        let again = RhythmStore::load(&defaults, &user).unwrap();
        assert_eq!(again.patterns, store.patterns);
        let _ = fs::remove_file(&user);
    }

    #[test]
    fn the_tracked_defaults_match_the_compiled_in_builtins() {
        let text = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/rhythms.toml"))
            .expect("rhythms.toml is tracked and must be there");
        assert_eq!(from_toml(&text).unwrap(), builtin_patterns());
    }

    /// Rewrite the tracked palette from the compiled-in built-ins.
    ///
    /// This is the one thing the pattern palette does that the instrument and EQ
    /// libraries do not need: those are authored in their files, so there is no
    /// second copy to regenerate from.
    #[test]
    #[ignore = "rewrites rhythms.toml"]
    fn regenerate_the_tracked_patterns() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/rhythms.toml");
        let text = to_toml(&builtin_patterns()).unwrap();
        fs::write(path, text).expect("rhythms.toml is writable from the checkout");
    }

    #[test]
    fn save_then_load_keeps_a_multi_layer_pattern() {
        let defaults = temp_path("stack-defaults");
        let user = temp_path("stack-user");
        let _ = fs::remove_file(&defaults);
        let _ = fs::remove_file(&user);

        let mut store = RhythmStore::default();
        let mut pattern = RhythmPattern::from_step_string("Stack", 0.4, "x--x--x-").unwrap();
        pattern
            .layers
            .push(crate::rhythm::RhythmLayer::from_step_string(0.7, "--------").unwrap());
        store.add(pattern.clone());
        store.save(&user).unwrap();

        // The defaults supply the palette; the user's file supplies the rest.
        let back = RhythmStore::load(&defaults, &user).unwrap();
        assert_eq!(back.find("Stack"), Some(&pattern));
        assert_eq!(back.patterns.len(), 24, "23 built-ins plus mine");

        let _ = fs::remove_file(&user);
    }

    #[test]
    fn the_users_file_is_layered_over_the_defaults() {
        // The split's whole point: the palettes ship in the tracked file, and the
        // user's file holds only what is theirs. A pattern they edited under a
        // built-in's name wins; a built-in they never touched keeps coming from
        // the defaults, so a later build can still change it under them.
        let defaults = temp_path("layer-defaults");
        let user = temp_path("layer-user");
        fs::write(&defaults, to_toml(&builtin_patterns()).unwrap()).unwrap();

        let mut mine = RhythmStore::default();
        mine.add(RhythmPattern::from_step_string("Mine", 0.5, "xxxxxxxx").unwrap());
        let mut edited = RhythmPattern::from_step_string("Quarters", 0.9, "xx--").unwrap();
        edited.hold = 864;
        mine.add(edited);
        mine.save(&user).unwrap();

        let merged = RhythmStore::load(&defaults, &user).unwrap();
        assert_eq!(merged.patterns.len(), 24, "23 defaults plus one of mine");
        assert!(merged.find("Mine").is_some());
        assert!(
            merged.find("Swung Eighths").is_some(),
            "the default is still there"
        );
        assert_eq!(
            merged.find("Quarters").unwrap().hold,
            864,
            "my Quarters wins over the shipped one"
        );
        // And the shipped order survives: mine is appended, not interleaved.
        assert_eq!(merged.patterns[0].name, "Held Whole");
        assert_eq!(merged.patterns[23].name, "Mine");

        let _ = fs::remove_file(&user);
        let _ = fs::remove_file(&defaults);
    }

    #[test]
    fn a_malformed_step_grid_is_reported_on_load() {
        let path = temp_path("malformed");
        fs::write(
            &path,
            "[[rhythms]]\nname = \"Bad\"\ngate = 0.5\n\n[[rhythms.layers]]\nsteps = \"xxo-\"\n",
        )
        .unwrap();

        let err = RhythmStore::load(&path, &temp_path("malformed-user")).unwrap_err();
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
        // Sustained, straight, sixteenths, triplets, texture — then the phrase
        // sets, each numbered set kept together so the run reads as one figure.
        let store = store();
        let names: Vec<&str> = store.patterns.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Held Whole",
                "Held 3/4",
                "Held Half",
                "Two Feel",
                "Quarters",
                "Eighths",
                "Offbeat Eighths",
                "Offbeat 16ths",
                "Dembow",
                "Charleston",
                "Tresillo",
                "Syncopated 16ths",
                "Sixteenth Pulse",
                "Swung Eighths",
                "Damped Quarters",
                "Accented Eighths",
                "32nd Roll",
                "Jazz Chorus 1/4",
                "Jazz Chorus 2/4",
                "Jazz Chorus 3/4",
                "Jazz Chorus 4/4",
                "Son Clave 1/2",
                "Son Clave 2/2",
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
