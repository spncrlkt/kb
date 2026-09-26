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
use crate::rhythm::RhythmPattern;
use crate::rhythm_store::RhythmStore;

/// Bumped whenever the encoded shape changes incompatibly.
///
/// The enum variant names below are part of the format (they derive `serde`),
/// so renaming a Rust variant is a breaking format change and must bump this.
///
/// Version 2 added `slots[].pattern`, `slots[].offset_ticks` and the embedded
/// `rhythms` list. In version 2 `slots[].pattern` was the *name* of an entry in
/// `rhythms`, so two chords could share one pattern.
///
/// Version 3 makes `slots[].pattern` the pattern itself, because an entry owns
/// its rhythm: two chords given `Quarters` each hold a copy, and editing one
/// cannot be heard on the other. A version 2 name is resolved against the
/// document's own `rhythms` on import, so an older export restores to the copy
/// each slot used to share.
///
/// Version 1 documents still import: the new fields all default, so an older
/// export restores to a progression with no patterns and no offsets, which is
/// exactly what it sounded like.
pub const VERSION: u32 = 3;

/// The whole session context needed to restore a progression exactly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub key_tonic: u8,
    pub key_scale: Scale,
    pub bpm: u16,
    pub note_length: f32,
    pub slots: Vec<SlotDoc>,
    /// The pattern *library* as it stood when the file was written, snapshotted
    /// so an import lands with the palette the export had.
    ///
    /// Since version 3 the slots do not reference these: each slot carries its
    /// own copy in `SlotDoc::pattern`, which is what the notes were built from.
    /// A version 2 file is the other way round — the names here are what its
    /// slots resolve against.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rhythms: Vec<RhythmPattern>,
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
    /// The slot's own rhythm, or the name of one in `rhythms` when the document
    /// is older than version 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<PatternDoc>,
    /// Signed ticks off the slot's downbeat. Absent in a v1 document.
    #[serde(default, skip_serializing_if = "is_zero_offset")]
    pub offset_ticks: i32,
    /// `None` means the register was never set; `Some([])` means it was
    /// explicitly cleared. The distinction is what makes `g`-to-recall behave
    /// identically after a round trip, so it is preserved rather than
    /// flattened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<Vec<KeyPosition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<Vec<KeyPosition>>,
}

fn is_zero_offset(v: &i32) -> bool {
    *v == 0
}

/// A slot's rhythm as it appears in a document.
///
/// Untagged, because the two shapes are both TOML-idiomatic and the field means
/// the same thing in each: a bare string is a version 2 *reference* into
/// `rhythms`, a table is a version 3 *owned copy*. Reading both here keeps the
/// version check in one place instead of in every consumer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PatternDoc {
    /// A library name, as version 2 wrote it.
    Name(String),
    /// The pattern itself, as version 3 writes it.
    Inline(RhythmPattern),
}

/// What a document restores to: domain values ready to install in `AppState`.
#[derive(Clone, Debug, PartialEq)]
pub struct Restored {
    pub slots: Vec<Slot>,
    pub key: Key,
    pub bpm: u16,
    pub note_length: f32,
    /// The library snapshot to merge into the palette, so an import lands with
    /// the starting points the export had.
    pub rhythms: Vec<RhythmPattern>,
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
///
/// Each slot writes the rhythm it owns, so the document holds exactly what the
/// notes were built from. `rhythms` is still taken as the pattern *library*
/// snapshot, so an import lands with the palette the export had — but no slot
/// depends on it any more, which is why a slot can no longer go dangling.
pub fn encode(
    prog: &Progression,
    rhythms: &RhythmStore,
    key: Key,
    bpm: u16,
    note_length: f32,
) -> Result<Vec<u8>, ProjectError> {
    let mut slots = Vec::with_capacity(prog.slots.len());

    for slot in &prog.slots {
        match slot {
            Slot::Rest => slots.push(SlotDoc {
                kind: "rest".to_string(),
                degree: None,
                transformation: None,
                pattern: None,
                offset_ticks: 0,
                left: None,
                right: None,
            }),
            Slot::Chord(entry) => {
                slots.push(SlotDoc {
                    kind: "chord".to_string(),
                    degree: Some(entry.degree),
                    transformation: entry.transformation,
                    pattern: entry.pattern.clone().map(PatternDoc::Inline),
                    offset_ticks: ProgressionEntry::clamp_offset(entry.offset_ticks),
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
                });
            }
        }
    }

    let project = Project {
        version: VERSION,
        key_tonic: key.tonic,
        key_scale: key.scale,
        bpm,
        note_length,
        slots,
        // The library snapshot: the palette, never the slots' own rhythms.
        rhythms: rhythms.patterns.clone(),
    };
    let text = toml::to_string(&project).map_err(|e| ProjectError::Malformed(e.to_string()))?;
    Ok(text.into_bytes())
}

/// Decode a project document. This checks the version but not the contents.
///
/// Every version from 1 up to [`VERSION`] is accepted: the new fields all
/// default, so an older export restores to the progression it described.
pub fn decode(bytes: &[u8]) -> Result<Project, ProjectError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ProjectError::Encoding)?;
    let project: Project =
        toml::from_str(text).map_err(|e| ProjectError::Malformed(e.to_string()))?;
    if project.version == 0 || project.version > VERSION {
        return Err(ProjectError::UnsupportedVersion(project.version));
    }
    Ok(project)
}

/// Turn a decoded document back into domain values.
pub fn restore(project: &Project) -> Result<Restored, ProjectError> {
    if project.version == 0 || project.version > VERSION {
        return Err(ProjectError::UnsupportedVersion(project.version));
    }

    // A version 2 slot *names* a pattern in `rhythms`, and a name the document
    // does not carry would import as something other than what was exported, so
    // it is refused rather than guessed at — the same rule that refuses a file
    // with no document at all. A version 3 slot carries its own copy, which only
    // has to be a pattern at all.
    let mut slots = Vec::with_capacity(project.slots.len());
    for doc in &project.slots {
        match doc.kind.as_str() {
            "rest" => slots.push(Slot::Rest),
            "chord" => {
                let degree = doc.degree.ok_or_else(|| {
                    ProjectError::Invalid("a chord slot has no scale degree".to_string())
                })?;
                let pattern = match doc.pattern.as_ref() {
                    None => None,
                    Some(PatternDoc::Inline(pattern)) => {
                        pattern.validate().map_err(|err| {
                            ProjectError::Invalid(format!(
                                "a slot's rhythm {:?} is invalid: {}",
                                pattern.name, err
                            ))
                        })?;
                        Some(pattern.clone())
                    }
                    Some(PatternDoc::Name(name)) => {
                        let found = project
                            .rhythms
                            .iter()
                            .find(|p| p.name == *name)
                            .ok_or_else(|| {
                                ProjectError::Invalid(format!(
                                    "a slot uses the rhythm pattern {:?}, which the file does not contain",
                                    name
                                ))
                            })?;
                        // The copy each slot used to share becomes its own, so a
                        // version 2 file opens with the isolation version 3 has.
                        Some(found.clone())
                    }
                };
                let mut entry = ProgressionEntry::new(degree, doc.transformation);
                entry.registers = Registers {
                    left: doc.left.as_ref().map(|v| v.iter().copied().collect()),
                    right: doc.right.as_ref().map(|v| v.iter().copied().collect()),
                };
                entry.pattern = pattern;
                entry.offset_ticks = ProgressionEntry::clamp_offset(doc.offset_ticks);
                slots.push(Slot::Chord(entry));
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
        rhythms: project.rhythms.clone(),
    })
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
            registers: Registers {
                left: left.map(|v| v.iter().copied().collect()),
                right: right.map(|v| v.iter().copied().collect()),
            },
            ..ProgressionEntry::new(degree, transformation)
        })
    }

    fn progression(slots: Vec<Slot>) -> Progression {
        let mut p = Progression::new();
        p.slots = slots;
        p
    }

    fn round_trip(prog: &Progression, key: Key, bpm: u16, note_length: f32) -> Restored {
        round_trip_with(prog, &RhythmStore::default(), key, bpm, note_length)
    }

    fn round_trip_with(
        prog: &Progression,
        rhythms: &RhythmStore,
        key: Key,
        bpm: u16,
        note_length: f32,
    ) -> Restored {
        let bytes = encode(prog, rhythms, key, bpm, note_length).unwrap();
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
            KeyPosition::TopLeft,
            KeyPosition::TopRow1,
            KeyPosition::TopRow3,
            KeyPosition::TopRow4,
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
        let text = String::from_utf8(
            encode(&prog, &RhythmStore::default(), c_major(), 120, 0.5).unwrap(),
        )
        .unwrap();
        assert!(text.contains("version = 3"));
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

    // ---- rhythm patterns and offsets ----

    /// The palette an export snapshots: two named patterns.
    fn library() -> RhythmStore {
        let mut store = RhythmStore::default();
        store.add(pattern("Offbeat"));
        store.add(pattern("Quarters"));
        store
    }

    /// A pattern a slot can *own*, matching the ones `library()` offers.
    fn pattern(name: &str) -> RhythmPattern {
        match name {
            "Offbeat" => RhythmPattern::from_step_string("Offbeat", 0.5, "-x-x-x-x").unwrap(),
            "Quarters" => RhythmPattern::from_step_string("Quarters", 0.8, "x---").unwrap(),
            other => RhythmPattern::from_step_string(other, 0.5, "x---").unwrap(),
        }
    }

    fn assigned(degree: ScaleDegree, pattern: Option<RhythmPattern>, offset: i32) -> Slot {
        let mut entry = ProgressionEntry::new(degree, None);
        entry.pattern = pattern;
        entry.offset_ticks = offset;
        Slot::Chord(entry)
    }

    fn pattern_of(slot: &Slot) -> Option<RhythmPattern> {
        match slot {
            Slot::Chord(e) => e.pattern.clone(),
            other => panic!("expected a chord, got {:?}", other),
        }
    }

    fn offset_of(slot: &Slot) -> i32 {
        match slot {
            Slot::Chord(e) => e.offset_ticks,
            other => panic!("expected a chord, got {:?}", other),
        }
    }

    #[test]
    fn patterns_and_offsets_round_trip() {
        let prog = progression(vec![
            assigned(ScaleDegree::I, Some(pattern("Offbeat")), 0),
            Slot::Rest,
            assigned(ScaleDegree::V, None, -240),
            assigned(ScaleDegree::VI, Some(pattern("Quarters")), 480),
        ]);

        let restored = round_trip_with(&prog, &library(), c_major(), 120, 0.5);
        assert_eq!(restored.slots, prog.slots);
        assert_eq!(restored.rhythms.len(), 2, "the palette came along");
    }

    #[test]
    fn the_whole_library_travels_as_a_palette() {
        // Slots no longer reference the library, so what is embedded is the
        // palette itself rather than only the entries in use.
        let prog = progression(vec![assigned(ScaleDegree::I, Some(pattern("Offbeat")), 0)]);
        let restored = round_trip_with(&prog, &library(), c_major(), 120, 0.5);
        let names: Vec<&str> = restored.rhythms.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Offbeat", "Quarters"]);
    }

    #[test]
    fn a_slots_own_pattern_keeps_its_layers_and_its_step_string() {
        let mut own = RhythmPattern::from_step_string("Stack", 0.4, "x--x--x-").unwrap();
        own.layers
            .push(crate::rhythm::RhythmLayer::from_step_string(0.7, "--------").unwrap());

        let prog = progression(vec![assigned(ScaleDegree::I, Some(own.clone()), 0)]);
        let bytes = encode(&prog, &RhythmStore::default(), c_major(), 120, 1.0).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains(r#"steps = "x--x--x-""#), "rendered:\n{}", text);

        let restored = round_trip_with(&prog, &RhythmStore::default(), c_major(), 120, 1.0);
        assert_eq!(pattern_of(&restored.slots[0]), Some(own));
        assert!(
            restored.rhythms.is_empty(),
            "the palette was empty, so nothing was embedded"
        );
    }

    #[test]
    fn two_slots_that_started_alike_keep_their_own_hits() {
        // The whole point of version 3: two chords both given `Quarters`, with
        // one of them edited. The file has to bring back two different rhythms,
        // not one shared name.
        let mut edited = pattern("Quarters");
        for layer in edited.layers.iter_mut() {
            layer.steps[1] = true;
        }
        let prog = progression(vec![
            assigned(ScaleDegree::I, Some(pattern("Quarters")), 0),
            assigned(ScaleDegree::V, Some(edited.clone()), 0),
        ]);

        let restored = round_trip_with(&prog, &library(), c_major(), 120, 0.5);
        assert_eq!(pattern_of(&restored.slots[0]), Some(pattern("Quarters")));
        assert_eq!(pattern_of(&restored.slots[1]), Some(edited));
        assert_ne!(
            pattern_of(&restored.slots[0]),
            pattern_of(&restored.slots[1]),
            "the imported copies are independent"
        );
    }

    #[test]
    fn a_slot_with_no_pattern_writes_no_pattern() {
        let prog = progression(vec![assigned(ScaleDegree::I, None, 0)]);
        let restored = round_trip_with(&prog, &library(), c_major(), 120, 0.5);
        assert_eq!(pattern_of(&restored.slots[0]), None);
        assert_eq!(restored.rhythms.len(), 2, "the palette is still snapshotted");
    }

    #[test]
    fn a_slot_naming_a_pattern_the_file_lacks_is_refused() {
        // Hand-edited or corrupted: the reference must not silently become a
        // whole-bar chord.
        let text = "version = 2\nkey_tonic = 60\nkey_scale = \"major\"\nbpm = 120\n\
                    note_length = 1.0\n\n[[slots]]\nkind = \"chord\"\ndegree = \"i\"\n\
                    pattern = \"Missing\"\n";
        let project = decode(text.as_bytes()).unwrap();
        match restore(&project) {
            Err(ProjectError::Invalid(msg)) => {
                assert!(msg.contains("Missing"), "message was: {}", msg)
            }
            other => panic!("expected an invalid-document error, got {:?}", other),
        }
    }

    #[test]
    fn a_version_1_document_still_restores() {
        // Every export made before rhythm patterns existed is a v1 document, and
        // all of them must keep importing.
        let text = "version = 1\nkey_tonic = 60\nkey_scale = \"major\"\nbpm = 128\n\
                    note_length = 0.75\n\n[[slots]]\nkind = \"chord\"\ndegree = \"v\"\n\
                    transformation = \"dom7\"\nleft = [\"left_index\"]\n\n\
                    [[slots]]\nkind = \"rest\"\n";
        let project = decode(text.as_bytes()).unwrap();
        let restored = restore(&project).unwrap();

        assert_eq!(restored.slots.len(), 2);
        assert_eq!(pattern_of(&restored.slots[0]), None, "no pattern in v1");
        assert_eq!(offset_of(&restored.slots[0]), 0, "no offset in v1");
        assert!(restored.rhythms.is_empty());
        assert_eq!(restored.bpm, 128);
        assert_eq!(restored.note_length, 0.75);
        assert_eq!(restored.key, c_major());
        match &restored.slots[0] {
            Slot::Chord(e) => {
                assert_eq!(e.degree, ScaleDegree::V);
                assert_eq!(e.transformation, Some(Transformation::Dom7));
                assert_eq!(
                    e.registers.left,
                    Some([KeyPosition::LeftIndex].into()),
                    "the register snapshot survived"
                );
            }
            other => panic!("expected a chord, got {:?}", other),
        }
    }

    #[test]
    fn output_is_written_as_the_current_version_with_the_pattern_inline() {
        let prog = progression(vec![assigned(ScaleDegree::I, Some(pattern("Quarters")), 120)]);
        let bytes = encode(&prog, &library(), c_major(), 120, 1.0).unwrap();
        let project = decode(&bytes).unwrap();
        assert_eq!(project.version, VERSION);
        assert_eq!(project.version, 3);
        assert_eq!(project.slots[0].offset_ticks, 120);
        assert_eq!(
            project.slots[0].pattern,
            Some(PatternDoc::Inline(pattern("Quarters")))
        );
        assert_eq!(project.rhythms.len(), 2, "the palette");
    }

    #[test]
    fn a_version_2_document_gives_each_slot_its_own_copy() {
        // Two slots naming one pattern was the version 2 shape, and the reason
        // version 3 exists: reading it must produce two independent copies, so
        // editing one chord after an import cannot reach the other.
        let text = "version = 2\nkey_tonic = 60\nkey_scale = \"major\"\nbpm = 120\n\
                    note_length = 1.0\n\n\
                    [[rhythms]]\nname = \"Quarters\"\nhold = 240\n\
                    [[rhythms.layers]]\ngain = 1.0\nsteps = \"x---\"\n\n\
                    [[slots]]\nkind = \"chord\"\ndegree = \"i\"\npattern = \"Quarters\"\n\n\
                    [[slots]]\nkind = \"chord\"\ndegree = \"v\"\npattern = \"Quarters\"\n";
        let project = decode(text.as_bytes()).unwrap();
        let mut restored = restore(&project).unwrap();

        assert_eq!(
            pattern_of(&restored.slots[0]),
            pattern_of(&restored.slots[1]),
            "identical to start with"
        );
        // Edit one copy the way the `hits` row would, and check the other is
        // untouched.
        match &mut restored.slots[1] {
            Slot::Chord(entry) => {
                let own = entry.pattern.as_mut().expect("its own copy");
                for layer in own.layers.iter_mut() {
                    layer.steps[0] = false;
                }
            }
            other => panic!("expected a chord, got {:?}", other),
        }
        assert_ne!(pattern_of(&restored.slots[0]), pattern_of(&restored.slots[1]));
    }

    #[test]
    fn a_zero_offset_is_left_out_of_the_document() {
        // Absent means zero, so writing it would only be noise in every slot of
        // every file.
        let prog = progression(vec![assigned(ScaleDegree::I, None, 0)]);
        let text = String::from_utf8(
            encode(&prog, &RhythmStore::default(), c_major(), 120, 1.0).unwrap(),
        )
        .unwrap();
        assert!(!text.contains("offset_ticks"), "rendered:\n{}", text);
    }

    #[test]
    fn an_offset_is_clamped_when_it_is_written_and_when_it_is_read() {
        let mut prog = Progression::new();
        prog.slots = vec![assigned(ScaleDegree::I, None, 999_999)];
        let bytes = encode(&prog, &RhythmStore::default(), c_major(), 120, 1.0).unwrap();
        let project = decode(&bytes).unwrap();
        assert_eq!(project.slots[0].offset_ticks, crate::progression::MAX_OFFSET_TICKS);

        let text = "version = 2\nkey_tonic = 60\nkey_scale = \"major\"\nbpm = 120\n\
                    note_length = 1.0\n\n[[slots]]\nkind = \"chord\"\ndegree = \"i\"\n\
                    offset_ticks = -99999\n";
        let project = decode(text.as_bytes()).unwrap();
        let restored = restore(&project).unwrap();
        assert_eq!(offset_of(&restored.slots[0]), -crate::progression::MAX_OFFSET_TICKS);
    }
}
