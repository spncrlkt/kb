//! Idle chimes: short portamento gestures that fire every 4 bars when the
//! transport is idle. Each chime is three voices (low, mid, high) gliding
//! from a start pitch to an end pitch over a fixed duration.

#[derive(Copy, Clone, Debug)]
pub struct ChimePair {
    pub name: &'static str,
    pub start: [u8; 3], // low, mid, high
    pub end: [u8; 3],
}

pub const CHIMES: [ChimePair; 5] = [
    ChimePair {
        name: "Rise",
        start: [53, 60, 65], // F3 C4 F4 -> C4 G4 C5
        end: [60, 67, 72],
    },
    ChimePair {
        name: "Fall",
        start: [65, 72, 77], // F4 C5 F5 -> C4 G4 C5
        end: [60, 67, 72],
    },
    ChimePair {
        name: "Converge",
        start: [48, 76, 84], // C3 E5 C6 -> C4 G4 C5
        end: [60, 67, 72],
    },
    ChimePair {
        name: "Creep",
        start: [59, 66, 71], // B3 F#4 B4 -> C4 G4 C5
        end: [60, 67, 72],
    },
    ChimePair {
        name: "Octave",
        start: [48, 55, 64], // C3 G3 E4 -> C4 G4 C5
        end: [60, 67, 72],
    },
];

/// Total duration of one chime in seconds. The glide takes this long; a
/// short hold follows before the release envelope takes over.
pub const CHIME_GLIDE_SECS: f32 = 1.8;

/// How long to hold at the target pitch before releasing.
pub const CHIME_HOLD_SECS: f32 = 0.2;

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_chimes_have_three_voices() {
        for c in &CHIMES {
            assert_eq!(c.start.len(), 3);
            assert_eq!(c.end.len(), 3);
        }
    }

    #[test]
    fn all_chimes_resolve_to_c_major_shell() {
        for c in &CHIMES {
            assert_eq!(c.end, [60, 67, 72], "chime '{}' target wrong", c.name);
        }
    }

    #[test]
    fn all_chime_notes_are_in_midi_range() {
        for c in &CHIMES {
            for &n in c.start.iter().chain(c.end.iter()) {
                assert!(n <= 127, "chime '{}' out of range", c.name);
            }
        }
    }
}
