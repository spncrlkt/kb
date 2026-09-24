//! Neutral MIDI rendering: a progression becomes timed notes.
//!
//! This module is deliberately pure — no audio, no terminal, no filesystem.
//! Both the Standard MIDI File exporter (`crate::smf`) and, later, a live MIDI
//! sink consume the [`Score`] produced here, so offline export and live output
//! cannot drift apart in what they play.

use crate::music::Key;
use crate::progression::Progression;

/// Ticks per quarter note. 960 divides cleanly by 2, 3, 4 and 5, so straight
/// and triplet subdivisions stay exact.
pub const PPQ: u16 = 960;

/// The transport hardcodes 4/4 today, so every bar is four quarter notes.
pub const BEATS_PER_BAR: u64 = 4;

/// Length of one progression slot.
pub const BAR_TICKS: u64 = PPQ as u64 * BEATS_PER_BAR;

/// Velocity written for every note.
///
/// The synth has no velocity, so there is nothing to derive this from yet; the
/// field exists on [`Note`] so it can be driven later without a model change.
pub const DEFAULT_VELOCITY: u8 = 100;

/// Which register a note belongs to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Layer {
    Low,
    Mid,
    High,
}

/// Maps layers onto MIDI channels.
///
/// This is the seam for "send the low, mid and high notes to different MIDI
/// channels": today the file writer may collapse every layer onto one channel,
/// but the mapping is already a value rather than a hardcoded constant.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ChannelMap {
    pub low: u8,
    pub mid: u8,
    pub high: u8,
}

impl Default for ChannelMap {
    fn default() -> Self {
        ChannelMap {
            low: 0,
            mid: 1,
            high: 2,
        }
    }
}

impl ChannelMap {
    pub fn channel(self, layer: Layer) -> u8 {
        match layer {
            Layer::Low => self.low,
            Layer::Mid => self.mid,
            Layer::High => self.high,
        }
    }
}

/// One note in the exported score, in ticks.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub start: u64,
    pub duration: u64,
    pub note: u8,
    pub velocity: u8,
    pub layer: Layer,
}

impl Note {
    /// Tick at which this note is released.
    pub fn end(&self) -> u64 {
        self.start + self.duration
    }
}

/// The whole progression as timed notes, plus the metadata a file needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Score {
    pub ppq: u16,
    pub bpm: u16,
    pub beats_per_bar: u8,
    pub key: Key,
    /// Total loop length, including any trailing rest bars.
    ///
    /// A rest at the end of the progression still occupies a bar, so this is
    /// the slot count rather than the end of the last note.
    pub length_ticks: u64,
    pub notes: Vec<Note>,
}

/// Split a chord into low / mid / high layers.
///
/// This is the layer *concept* with no sound design attached: the lowest note
/// is low, the highest is high, everything between is mid, and no octaves are
/// invented. `synth::allocate` deliberately doubles sparse chords an octave
/// out for the audio voice pools; that is a timbre choice, not musical
/// content, so it must not leak into an exported file.
pub fn split_layers(notes: &[u8]) -> [Vec<u8>; 3] {
    let mut sorted: Vec<u8> = notes.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    match sorted.len() {
        0 => [vec![], vec![], vec![]],
        1 => [vec![sorted[0]], vec![], vec![]],
        2 => [vec![sorted[0]], vec![], vec![sorted[1]]],
        _ => {
            let low = vec![sorted[0]];
            let mid = sorted[1..sorted.len() - 1].to_vec();
            let high = vec![sorted[sorted.len() - 1]];
            [low, mid, high]
        }
    }
}

/// Render a progression to a [`Score`].
///
/// One slot is one bar, exactly as the scheduler plays it. The live "+1 bar"
/// the scheduler appends while playing is intentionally *not* included: an
/// export has to be a deterministic function of the progression alone.
pub fn render_progression(prog: &Progression, key: &Key, bpm: u16, note_length: f32) -> Score {
    // Clamp so a wild `note_length` can never produce a zero-length note or
    // one that outlives its bar.
    let duration = ((BAR_TICKS as f64) * (note_length.clamp(0.0, 1.0) as f64)).round() as u64;
    let duration = duration.clamp(1, BAR_TICKS);

    let mut notes = Vec::new();
    for (index, slot) in prog.slots.iter().enumerate() {
        let Some(pitches) = slot.notes(key) else {
            continue;
        };
        let start = index as u64 * BAR_TICKS;
        let [low, mid, high] = split_layers(&pitches);
        for (layer, pitches) in [(Layer::Low, low), (Layer::Mid, mid), (Layer::High, high)] {
            for note in pitches {
                notes.push(Note {
                    start,
                    duration,
                    note,
                    velocity: DEFAULT_VELOCITY,
                    layer,
                });
            }
        }
    }

    Score {
        ppq: PPQ,
        bpm: bpm.max(1),
        beats_per_bar: BEATS_PER_BAR as u8,
        key: *key,
        length_ticks: prog.slots.len() as u64 * BAR_TICKS,
        notes,
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::{Scale, ScaleDegree, Transformation};
    use crate::progression::{Progression, ProgressionEntry, Registers, Slot};

    fn c_major() -> Key {
        Key::new(60, Scale::Major)
    }

    fn chord(degree: ScaleDegree, transformation: Option<Transformation>) -> Slot {
        Slot::Chord(ProgressionEntry {
            degree,
            transformation,
            registers: Registers::default(),
        })
    }

    fn progression(slots: Vec<Slot>) -> Progression {
        let mut p = Progression::new();
        p.slots = slots;
        p
    }

    // ---- split_layers ----

    #[test]
    fn split_of_a_triad_gives_one_note_per_layer() {
        let [low, mid, high] = split_layers(&[60, 64, 67]);
        assert_eq!(low, vec![60]);
        assert_eq!(mid, vec![64]);
        assert_eq!(high, vec![67]);
    }

    #[test]
    fn split_of_a_seventh_fills_the_middle() {
        let [low, mid, high] = split_layers(&[60, 64, 67, 70]);
        assert_eq!(low, vec![60]);
        assert_eq!(mid, vec![64, 67]);
        assert_eq!(high, vec![70]);
    }

    #[test]
    fn split_of_one_note_does_not_invent_octaves() {
        // The synth doubles a lone note an octave either way; the export must
        // not, or a single note would arrive as a three-note chord.
        let [low, mid, high] = split_layers(&[60]);
        assert_eq!(low, vec![60]);
        assert_eq!(mid, Vec::<u8>::new());
        assert_eq!(high, Vec::<u8>::new());
    }

    #[test]
    fn split_of_two_notes_uses_low_and_high() {
        let [low, mid, high] = split_layers(&[60, 67]);
        assert_eq!(low, vec![60]);
        assert_eq!(mid, Vec::<u8>::new());
        assert_eq!(high, vec![67]);
    }

    #[test]
    fn split_sorts_and_dedups() {
        let [low, mid, high] = split_layers(&[67, 60, 64, 60]);
        assert_eq!(low, vec![60]);
        assert_eq!(mid, vec![64]);
        assert_eq!(high, vec![67]);
    }

    #[test]
    fn split_of_nothing_is_empty() {
        assert_eq!(split_layers(&[]), [vec![], vec![], vec![]]);
    }

    // ---- geometry ----

    #[test]
    fn one_bar_is_four_quarters() {
        assert_eq!(PPQ, 960);
        assert_eq!(BEATS_PER_BAR, 4);
        assert_eq!(BAR_TICKS, 3840);
    }

    #[test]
    fn note_length_scales_the_duration() {
        let key = c_major();
        let prog = progression(vec![chord(ScaleDegree::I, None)]);
        for (length, expected) in [(0.25, 960), (0.5, 1920), (0.75, 2880), (1.0, 3840)] {
            let score = render_progression(&prog, &key, 120, length);
            assert!(
                score.notes.iter().all(|n| n.duration == expected),
                "note_length {} should give {} ticks",
                length,
                expected
            );
        }
    }

    #[test]
    fn slots_are_laid_out_one_bar_apart() {
        let key = c_major();
        let prog = progression(vec![
            chord(ScaleDegree::I, None),
            chord(ScaleDegree::V, None),
        ]);
        let score = render_progression(&prog, &key, 120, 0.5);
        let starts: Vec<u64> = score
            .notes
            .iter()
            .map(|n| n.start)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(starts, vec![0, BAR_TICKS]);
    }

    #[test]
    fn a_rest_contributes_no_notes_but_keeps_the_bar() {
        let key = c_major();
        let prog = progression(vec![
            chord(ScaleDegree::I, None),
            Slot::Rest,
            chord(ScaleDegree::V, None),
        ]);
        let score = render_progression(&prog, &key, 120, 1.0);
        // Three bars of loop...
        assert_eq!(score.length_ticks, 3 * BAR_TICKS);
        // ...but nothing sounds in bar two.
        assert!(score.notes.iter().all(|n| n.start != BAR_TICKS));
    }

    #[test]
    fn a_trailing_rest_still_lengthens_the_loop() {
        let key = c_major();
        let prog = progression(vec![chord(ScaleDegree::I, None), Slot::Rest]);
        let score = render_progression(&prog, &key, 120, 1.0);
        assert_eq!(score.length_ticks, 2 * BAR_TICKS);
    }

    #[test]
    fn an_empty_progression_renders_an_empty_score() {
        let score = render_progression(&Progression::new(), &c_major(), 120, 1.0);
        assert!(score.notes.is_empty());
        assert_eq!(score.length_ticks, 0);
    }

    #[test]
    fn render_assigns_layers() {
        let key = c_major();
        let prog = progression(vec![chord(ScaleDegree::I, Some(Transformation::Dom7))]);
        let score = render_progression(&prog, &key, 120, 1.0);
        // C E G Bb -> low C, mid E+G, high Bb.
        let by_layer = |layer: Layer| -> Vec<u8> {
            let mut notes: Vec<u8> = score
                .notes
                .iter()
                .filter(|n| n.layer == layer)
                .map(|n| n.note)
                .collect();
            notes.sort_unstable();
            notes
        };
        assert_eq!(by_layer(Layer::Low), vec![60]);
        assert_eq!(by_layer(Layer::Mid), vec![64, 67]);
        assert_eq!(by_layer(Layer::High), vec![70]);
    }

    #[test]
    fn render_carries_the_transport_metadata() {
        let key = Key::new(57, Scale::Minor);
        let prog = progression(vec![chord(ScaleDegree::I, None)]);
        let score = render_progression(&prog, &key, 90, 1.0);
        assert_eq!(score.bpm, 90);
        assert_eq!(score.ppq, PPQ);
        assert_eq!(score.beats_per_bar, 4);
        assert_eq!(score.key, key);
        assert!(score.notes.iter().all(|n| n.velocity == DEFAULT_VELOCITY));
    }

    #[test]
    fn a_zero_bpm_is_clamped_so_tempo_math_stays_sane() {
        let prog = progression(vec![chord(ScaleDegree::I, None)]);
        let score = render_progression(&prog, &c_major(), 0, 1.0);
        assert_eq!(score.bpm, 1);
    }

    #[test]
    fn note_length_is_clamped_into_the_bar() {
        let prog = progression(vec![chord(ScaleDegree::I, None)]);
        let score = render_progression(&prog, &c_major(), 120, 4.0);
        assert!(score.notes.iter().all(|n| n.duration == BAR_TICKS));
        let score = render_progression(&prog, &c_major(), 120, -1.0);
        assert!(score.notes.iter().all(|n| n.duration == 1));
    }

    // ---- channel map ----

    #[test]
    fn default_channel_map_splits_the_layers() {
        let map = ChannelMap::default();
        assert_eq!(map.channel(Layer::Low), 0);
        assert_eq!(map.channel(Layer::Mid), 1);
        assert_eq!(map.channel(Layer::High), 2);
    }
}
