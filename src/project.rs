//! The progression document: the *semantic* content of a session, encoded so
//! it can travel inside an exported MIDI file and be restored later.
//!
//! A MIDI file on its own only holds absolute notes, and the original degrees,
//! transformations and register gestures cannot be recovered from them — C-E-G
//! is I in C, IV in G, and V in F, and all of those voicings are identical.
//! So the exporter embeds this document alongside the notes, and the importer
//! reads it back. A file without it is refused rather than guessed at.
//!
//! The document is TOML (the crate already depends on `toml`), versioned, and
//! wrapped by `smf` in a sequencer-specific meta event so it is invisible in a
//! DAW.

use serde::{Deserialize, Serialize};

use crate::keyboard::KeyPosition;
use crate::music::{Key, Scale, ScaleDegree, Transformation};
use crate::progression::{Progression, ProgressionEntry, Registers, Slot};

/// Bumped whenever the encoded shape changes incompatibly.
///
/// The enum variant names below are part of the format (they derive `serde`),
/// so renaming a Rust variant is a breaking format change and must bump this.
pub const VERSION: u32 = 1;

/// The whole session context needed to restore a progression exactly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub key_tonic: u8,
    pub key_scale: Scale,
    pub bpm: u16,
    pub note_length: f32,
    pub slots: Vec<SlotDoc>,
}

/// One progression slot as it appears in the document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlotDoc {
    /// `"chord"` or `"rest"`. A plain string rather than a serde-tagged enum,
    /// because TOML handles flat tables far more predictably.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degree: Option<ScaleDegree>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transformation: Option<Transformation>,
    /// `None` means the register was never set; `Some([])` means it was
    /// explicitly cleared. The distinction is what makes `g`-to-recall behave
    /// identically after a round trip, so it is preserved rather than
    /// flattened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<Vec<KeyPosition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<Vec<KeyPosition>>,
}

/// What a document restores to: domain values ready to install in `AppState`.
#[derive(Clone, Debug, PartialEq)]
pub struct Restored {
    pub slots: Vec<Slot>,
    pub key: Key,
    pub bpm: u16,
    pub note_length: f32,
}

#[derive(Debug)]
pub enum ProjectError {
    /// The payload was not valid UTF-8.
    Encoding,
    /// A document from a newer (or older) format than this build understands.
    UnsupportedVersion(u32),
    /// The document could not be parsed at all.
    Malformed(String),
    /// The document parsed but does not describe a valid session.
    Invalid(String),
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectError::Encoding => write!(f, "project payload is not valid UTF-8"),
            ProjectError::UnsupportedVersion(v) => write!(
                f,
                "project format v{} is not supported (this build reads v{})",
                v, VERSION
            ),
            ProjectError::Malformed(msg) => write!(f, "project payload is malformed: {}", msg),
            ProjectError::Invalid(msg) => write!(f, "project payload is invalid: {}", msg),
        }
    }
}

impl std::error::Error for ProjectError {}

/// Encode a progression and its context as a project document.
pub fn encode(
    prog: &Progression,
    key: Key,
    bpm: u16,
    note_length: f32,
) -> Result<Vec<u8>, ProjectError> {
    let project = Project {
        version: VERSION,
        key_tonic: key.tonic,
        key_scale: key.scale,
        bpm,
        note_length,
        slots: prog.slots.iter().map(slot_to_doc).collect(),
    };
    let text = toml::to_string(&project).map_err(|e| ProjectError::Malformed(e.to_string()))?;
    Ok(text.into_bytes())
}

/// Decode a project document. This checks the version but not the contents.
pub fn decode(bytes: &[u8]) -> Result<Project, ProjectError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ProjectError::Encoding)?;
    let project: Project =
        toml::from_str(text).map_err(|e| ProjectError::Malformed(e.to_string()))?;
    if project.version != VERSION {
        return Err(ProjectError::UnsupportedVersion(project.version));
    }
    Ok(project)
}

/// Turn a decoded document back into domain values.
pub fn restore(project: &Project) -> Result<Restored, ProjectError> {
    if project.version != VERSION {
        return Err(ProjectError::UnsupportedVersion(project.version));
    }

    let mut slots = Vec::with_capacity(project.slots.len());
    for doc in &project.slots {
        match doc.kind.as_str() {
            "rest" => slots.push(Slot::Rest),
            "chord" => {
                let degree = doc.degree.ok_or_else(|| {
                    ProjectError::Invalid("a chord slot has no scale degree".to_string())
                })?;
                slots.push(Slot::Chord(ProgressionEntry {
                    degree,
                    transformation: doc.transformation,
                    registers: Registers {
                        left: doc.left.as_ref().map(|v| v.iter().copied().collect()),
                        right: doc.right.as_ref().map(|v| v.iter().copied().collect()),
                    },
                }));
            }
            other => {
                return Err(ProjectError::Invalid(format!(
                    "unknown slot kind {:?}",
                    other
                )))
            }
        }
    }

    Ok(Restored {
        slots,
        // `Key::new` clamps the tonic into the playable range.
        key: Key::new(project.key_tonic, project.key_scale),
        bpm: project.bpm,
        note_length: project.note_length,
    })
}

fn slot_to_doc(slot: &Slot) -> SlotDoc {
    match slot {
        Slot::Rest => SlotDoc {
            kind: "rest".to_string(),
            degree: None,
            transformation: None,
            left: None,
            right: None,
        },
        Slot::Chord(entry) => SlotDoc {
            kind: "chord".to_string(),
            degree: Some(entry.degree),
            transformation: entry.transformation,
            left: entry
                .registers
                .left
                .as_ref()
                .map(|set| set.iter().copied().collect()),
            right: entry
                .registers
                .right
                .as_ref()
                .map(|set| set.iter().copied().collect()),
        },
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::Scale;

    fn c_major() -> Key {
        Key::new(60, Scale::Major)
    }

    fn entry(
        degree: ScaleDegree,
        transformation: Option<Transformation>,
        left: Option<&[KeyPosition]>,
        right: Option<&[KeyPosition]>,
    ) -> Slot {
        Slot::Chord(ProgressionEntry {
            degree,
            transformation,
            registers: Registers {
                left: left.map(|v| v.iter().copied().collect()),
                right: right.map(|v| v.iter().copied().collect()),
            },
        })
    }

    fn progression(slots: Vec<Slot>) -> Progression {
        let mut p = Progression::new();
        p.slots = slots;
        p
    }

    fn round_trip(prog: &Progression, key: Key, bpm: u16, note_length: f32) -> Restored {
        let bytes = encode(prog, key, bpm, note_length).unwrap();
        let project = decode(&bytes).unwrap();
        restore(&project).unwrap()
    }

    #[test]
    fn a_progression_round_trips_exactly() {
        let prog = progression(vec![
            entry(
                ScaleDegree::I,
                Some(Transformation::Diatonic7),
                Some(&[KeyPosition::LeftIndex]),
                None,
            ),
            Slot::Rest,
            entry(
                ScaleDegree::V,
                Some(Transformation::Dom7),
                Some(&[KeyPosition::LeftPinky]),
                Some(&[KeyPosition::RightIndex, KeyPosition::RightMiddle]),
            ),
        ]);

        let restored = round_trip(&prog, c_major(), 128, 0.75);
        assert_eq!(restored.slots, prog.slots);
        assert_eq!(restored.key, c_major());
        assert_eq!(restored.bpm, 128);
        assert_eq!(restored.note_length, 0.75);
    }

    #[test]
    fn an_empty_progression_round_trips() {
        let restored = round_trip(&Progression::new(), c_major(), 120, 1.0);
        assert!(restored.slots.is_empty());
    }

    #[test]
    fn a_never_set_register_stays_distinct_from_an_explicitly_empty_one() {
        // `None` and `Some(empty)` resolve differently, so the document has to
        // keep them apart rather than collapsing both to "no keys".
        let prog = progression(vec![
            entry(ScaleDegree::I, None, None, None),
            entry(ScaleDegree::I, None, Some(&[]), Some(&[])),
        ]);
        let restored = round_trip(&prog, c_major(), 120, 1.0);

        let registers = |i: usize| match &restored.slots[i] {
            Slot::Chord(e) => e.registers.clone(),
            other => panic!("expected a chord, got {:?}", other),
        };
        assert_eq!(registers(0).left, None);
        assert_eq!(registers(0).right, None);
        assert_eq!(registers(1).left, Some(Default::default()));
        assert_eq!(registers(1).right, Some(Default::default()));
    }

    #[test]
    fn both_scales_round_trip() {
        for scale in [Scale::Major, Scale::Minor] {
            let restored = round_trip(&Progression::new(), Key::new(57, scale), 100, 0.5);
            assert_eq!(restored.key.scale, scale);
            assert_eq!(restored.key.tonic, 57);
        }
    }

    /// Serialise a single value inside a one-field table, because a TOML
    /// document must have a table at the root and a bare enum is just a string.
    /// Returns the encoded text, which is the value's wire name in context.
    fn wire_name<T>(value: T) -> String
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug + Copy,
    {
        #[derive(Serialize, Deserialize)]
        struct One<T> {
            value: T,
        }
        let text = toml::to_string(&One { value }).unwrap();
        let back: One<T> = toml::from_str(&text).unwrap();
        assert_eq!(back.value, value, "{} did not round trip", text);
        text
    }

    #[test]
    fn every_transformation_has_a_unique_wire_name() {
        // A duplicate or missing `serde` name would silently corrupt a
        // reimported progression, so check the whole set explicitly.
        let all = [
            Transformation::Dom7,
            Transformation::Dom7b9,
            Transformation::Dom9,
            Transformation::Min7b5,
            Transformation::Sus2,
            Transformation::SixNine,
            Transformation::Dim7,
            Transformation::Dom7s9,
            Transformation::Min9,
            Transformation::Aug,
            Transformation::Maj7s11,
            Transformation::Dom7s11,
            Transformation::MinMaj7,
            Transformation::Eleven,
            Transformation::Thirteen,
            Transformation::Diatonic7,
            Transformation::Diatonic9,
            Transformation::Sus4,
            Transformation::Diatonic6,
            Transformation::Diatonic7_9,
            Transformation::Diatonic7_13,
            Transformation::Sus4_7,
            Transformation::DiatonicFull,
        ];

        let mut names = std::collections::BTreeSet::new();
        for t in all {
            let text = wire_name(t);
            assert!(names.insert(text.clone()), "duplicate wire name {}", text);
        }
        assert_eq!(names.len(), all.len());
    }

    #[test]
    fn every_key_position_has_a_unique_wire_name() {
        let all = [
            KeyPosition::LeftPinky,
            KeyPosition::LeftRing,
            KeyPosition::LeftMiddle,
            KeyPosition::LeftIndex,
            KeyPosition::LeftInner,
            KeyPosition::RightInner,
            KeyPosition::RightIndex,
            KeyPosition::RightMiddle,
            KeyPosition::RightRing,
            KeyPosition::RightPinky,
            KeyPosition::LeftPinkyBelow,
            KeyPosition::LeftRingBelow,
            KeyPosition::LeftMiddleBelow,
            KeyPosition::LeftIndexBelow,
            KeyPosition::LeftInnerBelow,
            KeyPosition::RightInnerBelow,
            KeyPosition::RightIndexBelow,
            KeyPosition::RightMiddleBelow,
            KeyPosition::RightRingBelow,
            KeyPosition::RightPinkyBelow,
        ];

        let mut names = std::collections::BTreeSet::new();
        for p in all {
            let text = wire_name(p);
            assert!(names.insert(text.clone()), "duplicate wire name {}", text);
        }
        assert_eq!(names.len(), all.len());
    }

    #[test]
    fn the_document_is_readable_toml() {
        let prog = progression(vec![entry(
            ScaleDegree::I,
            Some(Transformation::Diatonic7),
            None,
            None,
        )]);
        let text = String::from_utf8(encode(&prog, c_major(), 120, 0.5).unwrap()).unwrap();
        assert!(text.contains("version = 1"));
        assert!(text.contains("kind = \"chord\""));
        assert!(text.contains("degree = \"i\""));
        assert!(text.contains("transformation = \"diatonic7\""));
    }

    #[test]
    fn a_newer_version_is_refused_with_a_clear_message() {
        let text = "version = 99\nkey_tonic = 60\nkey_scale = \"major\"\nbpm = 120\nnote_length = 1.0\nslots = []\n";
        match decode(text.as_bytes()) {
            Err(ProjectError::UnsupportedVersion(99)) => {}
            other => panic!("expected an unsupported-version error, got {:?}", other),
        }
    }

    #[test]
    fn malformed_payloads_are_refused() {
        assert!(matches!(
            decode(b"this is not toml"),
            Err(ProjectError::Malformed(_))
        ));
        assert!(matches!(
            decode(&[0xFF, 0xFE, 0xFD]),
            Err(ProjectError::Encoding)
        ));
    }

    #[test]
    fn an_unknown_slot_kind_is_refused() {
        let text = "version = 1\nkey_tonic = 60\nkey_scale = \"major\"\nbpm = 120\nnote_length = 1.0\n\n[[slots]]\nkind = \"arpeggio\"\n";
        let project = decode(text.as_bytes()).unwrap();
        assert!(matches!(
            restore(&project),
            Err(ProjectError::Invalid(_))
        ));
    }

    #[test]
    fn a_chord_without_a_degree_is_refused() {
        let text = "version = 1\nkey_tonic = 60\nkey_scale = \"major\"\nbpm = 120\nnote_length = 1.0\n\n[[slots]]\nkind = \"chord\"\n";
        let project = decode(text.as_bytes()).unwrap();
        assert!(matches!(
            restore(&project),
            Err(ProjectError::Invalid(_))
        ));
    }

    #[test]
    fn an_out_of_range_tonic_is_clamped_on_restore() {
        let text = "version = 1\nkey_tonic = 250\nkey_scale = \"major\"\nbpm = 120\nnote_length = 1.0\nslots = []\n";
        let project = decode(text.as_bytes()).unwrap();
        let restored = restore(&project).unwrap();
        assert_eq!(restored.key.tonic, crate::music::TONIC_MAX);
    }
}
