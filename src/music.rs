//! Music theory core: notes, scale degrees, keys, and chord voicing.

use serde::{Deserialize, Serialize};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Semitone offsets of the major scale from the tonic.
pub const MAJOR_SCALE: [u8; 7] = [0, 2, 4, 5, 7, 9, 11];

/// Semitone offsets of the natural minor scale from the tonic.
pub const MINOR_SCALE: [u8; 7] = [0, 2, 3, 5, 7, 8, 10];

/// Tonic clamping range. Chosen so the widest voicing fits within MIDI 0-127.
/// The widest case is vii° with a 13 chord: root = tonic + 11,
/// top note = root + 21, so tonic + 32 <= 127, giving TONIC_MAX = 95.
pub const TONIC_MIN: u8 = 24; // C1
pub const TONIC_MAX: u8 = 95; // B6

/// Pitch-class names, indexed by `midi % 12`.
pub const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

// -----------------------------------------------------------------------------
// Scale
// -----------------------------------------------------------------------------

/// Serialised into the MIDI export payload, so the variant names are part of
/// the file format; rename them only with a `project::VERSION` bump.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scale {
    Major,
    Minor,
}

impl Scale {
    /// The seven semitone offsets of this scale from the tonic.
    pub fn intervals(self) -> [u8; 7] {
        match self {
            Scale::Major => MAJOR_SCALE,
            Scale::Minor => MINOR_SCALE,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Scale::Major => "major",
            Scale::Minor => "minor",
        }
    }
}

// -----------------------------------------------------------------------------
// Enums
// -----------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleDegree {
    I,
    II,
    III,
    IV,
    V,
    VI,
    VII,
}

impl ScaleDegree {
    pub fn index(self) -> usize {
        match self {
            ScaleDegree::I => 0,
            ScaleDegree::II => 1,
            ScaleDegree::III => 2,
            ScaleDegree::IV => 3,
            ScaleDegree::V => 4,
            ScaleDegree::VI => 5,
            ScaleDegree::VII => 6,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ScaleDegree::I => "I",
            ScaleDegree::II => "ii",
            ScaleDegree::III => "iii",
            ScaleDegree::IV => "IV",
            ScaleDegree::V => "V",
            ScaleDegree::VI => "vi",
            ScaleDegree::VII => "vii",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    J,
    H,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transformation {
    // j-mode: absolute chord qualities
    Dom7,
    Dom7b9,
    Dom9,
    Min7b5,
    Sus2,
    SixNine,
    Dim7,
    Dom7s9,
    Min9,
    Aug,
    Maj7s11,
    Dom7s11,
    MinMaj7,
    Eleven,
    Thirteen,

    // h-mode: diatonic additions
    Diatonic7,
    Diatonic9,
    Sus4,
    Diatonic6,
    Diatonic7_9,
    Diatonic7_13,
    Sus4_7,
    DiatonicFull,
}

impl Transformation {
    pub fn mode(self) -> Mode {
        use Transformation::*;
        match self {
            Dom7 | Dom7b9 | Dom9 | Min7b5 | Sus2 | SixNine | Dim7 | Dom7s9 | Min9 | Aug
            | Maj7s11 | Dom7s11 | MinMaj7 | Eleven | Thirteen => Mode::J,
            Diatonic7 | Diatonic9 | Sus4 | Diatonic6 | Diatonic7_9 | Diatonic7_13 | Sus4_7
            | DiatonicFull => Mode::H,
        }
    }

    pub fn label(self) -> &'static str {
        use Transformation::*;
        match self {
            Dom7 => "7",
            Dom7b9 => "7b9",
            Dom9 => "9",
            Min7b5 => "m7b5",
            Sus2 => "sus2",
            SixNine => "6/9",
            Dim7 => "dim7",
            Dom7s9 => "7#9",
            Min9 => "m9",
            Aug => "aug",
            Maj7s11 => "maj7#11",
            Dom7s11 => "7#11",
            MinMaj7 => "mMaj7",
            Eleven => "11",
            Thirteen => "13",
            Diatonic7 => "maj7",
            Diatonic9 => "add9",
            Sus4 => "sus4",
            Diatonic6 => "6",
            Diatonic7_9 => "maj9",
            Diatonic7_13 => "13",
            Sus4_7 => "7sus4",
            DiatonicFull => "13(9)",
        }
    }
}

// -----------------------------------------------------------------------------
// Key
// -----------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Key {
    /// MIDI note number of the tonic. Clamped to `[TONIC_MIN, TONIC_MAX]`.
    pub tonic: u8,
    /// Major or natural minor.
    pub scale: Scale,
}

impl Key {
    pub fn new(tonic: u8, scale: Scale) -> Self {
        Key {
            tonic: tonic.clamp(TONIC_MIN, TONIC_MAX),
            scale,
        }
    }

    /// Root note of the chord built on `degree`.
    pub fn degree_root(&self, degree: ScaleDegree) -> u8 {
        (self.tonic as i16 + self.scale.intervals()[degree.index()] as i16) as u8
    }

    /// The `n`-th scale step above `degree`, wrapping octaves.
    ///
    /// `n = 0` is the degree itself. `n = 2` is a third above. `n = 6` is a
    /// seventh. `n = 8` is a ninth. `n = 12` is a thirteenth.
    pub fn diatonic_nth(&self, degree: ScaleDegree, n: usize) -> u8 {
        let sum = degree.index() + n;
        let octaves = (sum / 7) as i16;
        let idx = sum % 7;
        let intervals = self.scale.intervals();
        let raw = self.tonic as i16 + 12 * octaves + intervals[idx] as i16;
        raw.clamp(0, 127) as u8
    }

    /// Display string, e.g. "C major", "A minor".
    pub fn name(&self) -> String {
        let pc = NOTE_NAMES[(self.tonic % 12) as usize];
        format!("{} {}", pc, self.scale.name())
    }
}

// -----------------------------------------------------------------------------
// ChordSpec
// -----------------------------------------------------------------------------

#[derive(Copy, Clone, Debug)]
pub struct ChordSpec {
    pub degree: ScaleDegree,
    pub transformation: Transformation,
}

impl ChordSpec {
    pub fn new(degree: ScaleDegree, transformation: Transformation) -> Self {
        ChordSpec {
            degree,
            transformation,
        }
    }

    pub fn voice(&self, key: &Key) -> Vec<u8> {
        voice(key, self)
    }
}

// -----------------------------------------------------------------------------
// Voicing
// -----------------------------------------------------------------------------

pub fn voice(key: &Key, spec: &ChordSpec) -> Vec<u8> {
    use Transformation::*;
    let d = spec.degree;
    let r = key.degree_root(d);
    match spec.transformation {
        // ---- j-mode: fixed semitone intervals from the degree's root ----
        Dom7 => stack(r, &[0, 4, 7, 10]),
        Dom7b9 => stack(r, &[0, 4, 7, 10, 13]),
        Dom9 => stack(r, &[0, 4, 7, 10, 14]),
        Min7b5 => stack(r, &[0, 3, 6, 10]),
        Sus2 => stack(r, &[0, 2, 7]),
        SixNine => stack(r, &[0, 4, 7, 9, 14]),
        Dim7 => stack(r, &[0, 3, 6, 9]),
        Dom7s9 => stack(r, &[0, 4, 7, 10, 15]),
        Min9 => stack(r, &[0, 3, 7, 10, 14]),
        Aug => stack(r, &[0, 4, 8]),
        Maj7s11 => stack(r, &[0, 4, 7, 11, 18]),
        Dom7s11 => stack(r, &[0, 4, 7, 10, 18]),
        MinMaj7 => stack(r, &[0, 3, 7, 11]),
        Eleven => stack(r, &[0, 7, 10, 14, 17]),
        Thirteen => stack(r, &[0, 4, 10, 14, 21]),

        // ---- h-mode: diatonic stacks relative to the key's scale ----
        Diatonic7 => diatonic(key, d, &[0, 2, 4, 6]),
        Diatonic9 => diatonic(key, d, &[0, 2, 4, 8]),
        Sus4 => diatonic(key, d, &[0, 3, 4]),
        Diatonic6 => diatonic(key, d, &[0, 2, 4, 5]),
        Diatonic7_9 => diatonic(key, d, &[0, 2, 4, 6, 8]),
        Diatonic7_13 => diatonic(key, d, &[0, 2, 4, 6, 12]),
        Sus4_7 => diatonic(key, d, &[0, 3, 4, 6]),
        DiatonicFull => diatonic(key, d, &[0, 2, 4, 6, 8, 12]),
    }
}

/// The plain diatonic triad on a degree.
pub fn diatonic_triad(key: &Key, degree: ScaleDegree) -> Vec<u8> {
    diatonic(key, degree, &[0, 2, 4])
}

fn stack(root: u8, intervals: &[u8]) -> Vec<u8> {
    intervals
        .iter()
        .map(|i| (root as i16 + *i as i16).clamp(0, 127) as u8)
        .collect()
}

fn diatonic(key: &Key, degree: ScaleDegree, steps: &[usize]) -> Vec<u8> {
    steps.iter().map(|n| key.diatonic_nth(degree, *n)).collect()
}

// -----------------------------------------------------------------------------
// Display helpers
// -----------------------------------------------------------------------------

pub fn note_name(midi: u8) -> String {
    let name = NOTE_NAMES[(midi % 12) as usize];
    let octave = (midi / 12) as i32 - 1;
    format!("{}{}", name, octave)
}

pub fn chord_label(key: &Key, spec: &ChordSpec) -> String {
    let root = key.degree_root(spec.degree);
    format!(
        "{}{}",
        NOTE_NAMES[(root % 12) as usize],
        spec.transformation.label()
    )
}

/// Name a plain diatonic triad: "C", "Dm", "Bdim", etc.
///
/// The suffix is derived from the intervals actually produced by the key and
/// degree, so it works for both major and minor scales without special cases.
pub fn diatonic_triad_label(key: &Key, degree: ScaleDegree) -> String {
    let root = key.degree_root(degree);
    let third = key.diatonic_nth(degree, 2);
    let fifth = key.diatonic_nth(degree, 4);
    let t = (third as i16 - root as i16).rem_euclid(12);
    let f = (fifth as i16 - root as i16).rem_euclid(12);
    let suffix = if f == 6 {
        "dim"
    } else if t == 3 {
        "m"
    } else {
        ""
    };
    format!("{}{}", NOTE_NAMES[(root % 12) as usize], suffix)
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn c_major() -> Key {
        Key::new(60, Scale::Major)
    }

    fn c_minor() -> Key {
        Key::new(60, Scale::Minor)
    }

    // ---- major scale ----

    #[test]
    fn degree_roots_in_c_major() {
        let k = c_major();
        assert_eq!(k.degree_root(ScaleDegree::I), 60);
        assert_eq!(k.degree_root(ScaleDegree::II), 62);
        assert_eq!(k.degree_root(ScaleDegree::III), 64);
        assert_eq!(k.degree_root(ScaleDegree::IV), 65);
        assert_eq!(k.degree_root(ScaleDegree::V), 67);
        assert_eq!(k.degree_root(ScaleDegree::VI), 69);
        assert_eq!(k.degree_root(ScaleDegree::VII), 71);
    }

    #[test]
    fn diatonic_sevenths_in_c_major() {
        let k = c_major();
        assert_eq!(k.diatonic_nth(ScaleDegree::I, 6), 71);   // B  -> maj7
        assert_eq!(k.diatonic_nth(ScaleDegree::V, 6), 77);   // F  -> dom7
        assert_eq!(k.diatonic_nth(ScaleDegree::VII, 6), 81); // A  -> m7b5
    }

    #[test]
    fn diatonic_wraps_octaves_in_c_major() {
        let k = c_major();
        assert_eq!(k.diatonic_nth(ScaleDegree::I, 8), 74);  // D5
        assert_eq!(k.diatonic_nth(ScaleDegree::I, 12), 81); // A5
    }

    // ---- minor scale ----

    #[test]
    fn degree_roots_in_c_minor() {
        let k = c_minor();
        assert_eq!(k.degree_root(ScaleDegree::I), 60);   // C
        assert_eq!(k.degree_root(ScaleDegree::II), 62);  // D
        assert_eq!(k.degree_root(ScaleDegree::III), 63); // Eb
        assert_eq!(k.degree_root(ScaleDegree::IV), 65);  // F
        assert_eq!(k.degree_root(ScaleDegree::V), 67);   // G
        assert_eq!(k.degree_root(ScaleDegree::VI), 68);  // Ab
        assert_eq!(k.degree_root(ScaleDegree::VII), 70); // Bb
    }

    #[test]
    fn diatonic_sevenths_in_c_minor() {
        let k = c_minor();
        // i: C Eb G Bb -> Cm7
        assert_eq!(k.diatonic_nth(ScaleDegree::I, 6), 70);
        // ii°: D F Ab C -> Dm7b5
        assert_eq!(k.diatonic_nth(ScaleDegree::II, 6), 72);
        // III: Eb G Bb D -> Ebmaj7
        assert_eq!(k.diatonic_nth(ScaleDegree::III, 6), 74);
        // v: G Bb D F -> Gm7
        assert_eq!(k.diatonic_nth(ScaleDegree::V, 6), 77);
        // VII: Bb D F Ab -> Bb7
        assert_eq!(k.diatonic_nth(ScaleDegree::VII, 6), 80);
    }

    #[test]
    fn diatonic_wraps_octaves_in_c_minor() {
        let k = c_minor();
        assert_eq!(k.diatonic_nth(ScaleDegree::I, 8), 74);  // D5
        assert_eq!(k.diatonic_nth(ScaleDegree::I, 12), 80); // Ab5
    }

    // ---- h-mode voicings ----

    #[test]
    fn h_mode_i_maj7_in_c_major() {
        let k = c_major();
        let spec = ChordSpec::new(ScaleDegree::I, Transformation::Diatonic7);
        assert_eq!(spec.voice(&k), vec![60, 64, 67, 71]);
    }

    #[test]
    fn h_mode_v_dominant_7_in_c_major() {
        let k = c_major();
        let spec = ChordSpec::new(ScaleDegree::V, Transformation::Diatonic7);
        assert_eq!(spec.voice(&k), vec![67, 71, 74, 77]);
    }

    #[test]
    fn h_mode_vii_half_diminished_in_c_major() {
        let k = c_major();
        let spec = ChordSpec::new(ScaleDegree::VII, Transformation::Diatonic7);
        assert_eq!(spec.voice(&k), vec![71, 74, 77, 81]);
    }

    #[test]
    fn h_mode_i_min7_in_c_minor() {
        let k = c_minor();
        let spec = ChordSpec::new(ScaleDegree::I, Transformation::Diatonic7);
        assert_eq!(spec.voice(&k), vec![60, 63, 67, 70]);
    }

    #[test]
    fn h_mode_vii_dominant_7_in_c_minor() {
        let k = c_minor();
        let spec = ChordSpec::new(ScaleDegree::VII, Transformation::Diatonic7);
        assert_eq!(spec.voice(&k), vec![70, 74, 77, 80]);
    }

    // ---- j-mode voicings ----

    #[test]
    fn j_mode_i_dom7() {
        let k = c_major();
        let spec = ChordSpec::new(ScaleDegree::I, Transformation::Dom7);
        assert_eq!(spec.voice(&k), vec![60, 64, 67, 70]);
    }

    #[test]
    fn j_mode_ii_dom7_is_out_of_key() {
        let k = c_major();
        let spec = ChordSpec::new(ScaleDegree::II, Transformation::Dom7);
        assert_eq!(spec.voice(&k), vec![62, 66, 69, 72]);
    }

    #[test]
    fn j_mode_ii_dom7_in_c_minor() {
        // Root of ii is still D (62), j-mode ignores scale.
        let k = c_minor();
        let spec = ChordSpec::new(ScaleDegree::II, Transformation::Dom7);
        assert_eq!(spec.voice(&k), vec![62, 66, 69, 72]);
    }

    #[test]
    fn j_mode_v_matches_h_mode_v_in_major() {
        // On V in C major, diatonic 7th and dominant 7th produce the same notes.
        let k = c_major();
        let h = ChordSpec::new(ScaleDegree::V, Transformation::Diatonic7).voice(&k);
        let j = ChordSpec::new(ScaleDegree::V, Transformation::Dom7).voice(&k);
        assert_eq!(h, j);
    }

    #[test]
    fn j_mode_v_differs_from_h_mode_v_in_minor() {
        // On v in C minor, diatonic 7th is Gm7 (Bb) but j-mode makes G7 (B).
        let k = c_minor();
        let h = ChordSpec::new(ScaleDegree::V, Transformation::Diatonic7).voice(&k);
        let j = ChordSpec::new(ScaleDegree::V, Transformation::Dom7).voice(&k);
        assert_eq!(h, vec![67, 70, 74, 77]);
        assert_eq!(j, vec![67, 71, 74, 77]);
        assert_ne!(h, j);
    }

    // ---- range ----

    #[test]
    fn widest_voicing_fits_in_midi_range_major() {
        let k = Key::new(TONIC_MAX, Scale::Major);
        for t in [
            Transformation::Thirteen,
            Transformation::Maj7s11,
            Transformation::Dom7s11,
            Transformation::DiatonicFull,
        ] {
            let spec = ChordSpec::new(ScaleDegree::VII, t);
            let notes = spec.voice(&k);
            assert!(
                notes.iter().all(|&n| n <= 127),
                "transformation {:?} overflowed on highest major tonic",
                t
            );
        }
    }

    #[test]
    fn widest_voicing_fits_in_midi_range_minor() {
        let k = Key::new(TONIC_MAX, Scale::Minor);
        for t in [
            Transformation::Thirteen,
            Transformation::Maj7s11,
            Transformation::Dom7s11,
            Transformation::DiatonicFull,
        ] {
            let spec = ChordSpec::new(ScaleDegree::VII, t);
            let notes = spec.voice(&k);
            assert!(
                notes.iter().all(|&n| n <= 127),
                "transformation {:?} overflowed on highest minor tonic",
                t
            );
        }
    }

    #[test]
    fn tonic_clamping() {
        assert_eq!(Key::new(0, Scale::Major).tonic, TONIC_MIN);
        assert_eq!(Key::new(200, Scale::Major).tonic, TONIC_MAX);
        assert_eq!(Key::new(60, Scale::Major).tonic, 60);
    }

    // ---- display ----

    #[test]
    fn note_names_use_standard_octave_numbering() {
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(69), "A4");
        assert_eq!(note_name(61), "C#4");
    }

    #[test]
    fn chord_labels_are_readable() {
        let k = c_major();
        let spec = ChordSpec::new(ScaleDegree::I, Transformation::Diatonic7);
        assert_eq!(chord_label(&k, &spec), "Cmaj7");
        let spec = ChordSpec::new(ScaleDegree::V, Transformation::Dom7b9);
        assert_eq!(chord_label(&k, &spec), "G7b9");
    }

    #[test]
    fn diatonic_triads_have_correct_quality_labels_in_c_major() {
        let k = c_major();
        assert_eq!(diatonic_triad_label(&k, ScaleDegree::I), "C");
        assert_eq!(diatonic_triad_label(&k, ScaleDegree::II), "Dm");
        assert_eq!(diatonic_triad_label(&k, ScaleDegree::VII), "Bdim");
    }

    #[test]
    fn diatonic_triads_have_correct_quality_labels_in_c_minor() {
        let k = c_minor();
        assert_eq!(diatonic_triad_label(&k, ScaleDegree::I), "Cm");
        assert_eq!(diatonic_triad_label(&k, ScaleDegree::II), "Ddim");
        assert_eq!(diatonic_triad_label(&k, ScaleDegree::III), "D#");
        assert_eq!(diatonic_triad_label(&k, ScaleDegree::VII), "A#");
    }

    #[test]
    fn key_name_includes_scale() {
        assert_eq!(c_major().name(), "C major");
        assert_eq!(c_minor().name(), "C minor");
    }
}
