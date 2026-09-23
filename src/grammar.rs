//! Position sets -> musical intent.
//!
//! The two hands are disjoint, so each is extracted independently from the
//! same set. This module knows nothing about keyboard layouts.

use crate::keyboard::{KeyPosition, PositionSet};
use crate::music::{ScaleDegree, Transformation};

/// Map the left-hand positions in a set to a scale degree.
pub fn left_hand_degree(positions: &PositionSet) -> Option<ScaleDegree> {
    use KeyPosition::*;
    use ScaleDegree::*;

    let left: Vec<KeyPosition> = positions.iter().filter(|p| p.is_left()).copied().collect();

    match left.as_slice() {
        [LeftIndex] => Some(I),
        [LeftPinky] => Some(II),
        [LeftPinky, LeftRing] => Some(III),
        [LeftMiddle, LeftIndex] => Some(IV),
        [LeftMiddle] => Some(V),
        [LeftRing] => Some(VI),
        [LeftPinky, LeftRing, LeftMiddle] => Some(VII),
        _ => None,
    }
}

/// Map the right-hand positions in a set to a chord transformation.
pub fn right_hand_transformation(positions: &PositionSet) -> Option<Transformation> {
    use KeyPosition::*;
    use Transformation::*;

    let h = positions.contains(&RightInner);
    let j = positions.contains(&RightIndex);
    let k = positions.contains(&RightMiddle);
    let l = positions.contains(&RightRing);
    let p = positions.contains(&RightPinky);

    if h {
        if p {
            return None;
        }
        return match (j, k, l) {
            (false, false, false) => Some(Diatonic7),
            (true, false, false) => Some(Diatonic9),
            (false, true, false) => Some(Sus4),
            (false, false, true) => Some(Diatonic6),
            (true, true, false) => Some(Diatonic7_9),
            (true, false, true) => Some(Diatonic7_13),
            (false, true, true) => Some(Sus4_7),
            (true, true, true) => Some(DiatonicFull),
        };
    }

    match (j, k, l, p) {
        (true, false, false, false) => Some(Dom7),
        (false, true, false, false) => Some(Dom7b9),
        (false, false, true, false) => Some(Dom9),
        (false, false, false, true) => Some(Min7b5),
        (true, true, false, false) => Some(Sus2),
        (false, false, true, true) => Some(SixNine),
        (true, false, true, false) => Some(Dim7),
        (true, false, false, true) => Some(Dom7s9),
        (false, true, true, false) => Some(Min9),
        (false, true, false, true) => Some(Aug),
        (true, true, true, false) => Some(Maj7s11),
        (false, true, true, true) => Some(Dom7s11),
        (true, true, false, true) => Some(MinMaj7),
        (true, false, true, true) => Some(Eleven),
        (true, true, true, true) => Some(Thirteen),
        (false, false, false, false) => None,
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::Layout;
    use crate::music::{ChordSpec, Key, Scale};

    fn c_major() -> Key {
        Key::new(60, Scale::Major)
    }

    fn positions(chars: &[char], layout: Layout) -> PositionSet {
        chars
            .iter()
            .filter_map(|c| layout.position(*c))
            .collect::<PositionSet>()
    }

    #[test]
    fn all_left_hand_degrees_resolve_in_qwerty() {
        let l = Layout::Qwerty;
        let cases = [
            (vec!['f'], ScaleDegree::I),
            (vec!['a'], ScaleDegree::II),
            (vec!['a', 's'], ScaleDegree::III),
            (vec!['d', 'f'], ScaleDegree::IV),
            (vec!['d'], ScaleDegree::V),
            (vec!['s'], ScaleDegree::VI),
            (vec!['a', 's', 'd'], ScaleDegree::VII),
        ];
        for (chars, expected) in cases {
            let p = positions(&chars, l);
            assert_eq!(left_hand_degree(&p), Some(expected), "chars={:?}", chars);
        }
    }

    #[test]
    fn all_j_mode_transformations_resolve_in_qwerty() {
        let l = Layout::Qwerty;
        let cases = [
            (vec!['j'], Transformation::Dom7),
            (vec!['k'], Transformation::Dom7b9),
            (vec!['l'], Transformation::Dom9),
            (vec![';'], Transformation::Min7b5),
            (vec!['j', 'k'], Transformation::Sus2),
            (vec!['l', ';'], Transformation::SixNine),
            (vec!['j', 'l'], Transformation::Dim7),
            (vec!['j', ';'], Transformation::Dom7s9),
            (vec!['k', 'l'], Transformation::Min9),
            (vec!['k', ';'], Transformation::Aug),
            (vec!['j', 'k', 'l'], Transformation::Maj7s11),
            (vec!['k', 'l', ';'], Transformation::Dom7s11),
            (vec!['j', 'k', ';'], Transformation::MinMaj7),
            (vec!['j', 'l', ';'], Transformation::Eleven),
            (vec!['j', 'k', 'l', ';'], Transformation::Thirteen),
        ];
        for (chars, expected) in cases {
            let p = positions(&chars, l);
            assert_eq!(
                right_hand_transformation(&p),
                Some(expected),
                "chars={:?}",
                chars
            );
        }
    }

    #[test]
    fn all_h_mode_transformations_resolve_in_qwerty() {
        let l = Layout::Qwerty;
        let cases = [
            (vec!['h'], Transformation::Diatonic7),
            (vec!['h', 'j'], Transformation::Diatonic9),
            (vec!['h', 'k'], Transformation::Sus4),
            (vec!['h', 'l'], Transformation::Diatonic6),
            (vec!['h', 'j', 'k'], Transformation::Diatonic7_9),
            (vec!['h', 'j', 'l'], Transformation::Diatonic7_13),
            (vec!['h', 'k', 'l'], Transformation::Sus4_7),
            (vec!['h', 'j', 'k', 'l'], Transformation::DiatonicFull),
        ];
        for (chars, expected) in cases {
            let p = positions(&chars, l);
            assert_eq!(
                right_hand_transformation(&p),
                Some(expected),
                "chars={:?}",
                chars
            );
        }
    }

    #[test]
    fn h_mode_with_pinky_is_invalid() {
        let p = positions(&['h', ';'], Layout::Qwerty);
        assert_eq!(right_hand_transformation(&p), None);
    }

    #[test]
    fn left_inner_is_not_part_of_the_grammar() {
        // `LeftInner` (physical `g`) carries the recall hotkey rather than a
        // chord meaning, so the grammar must ignore it...
        let l = Layout::Qwerty;
        assert_eq!(left_hand_degree(&positions(&['g'], l)), None);

        // ...and this is exactly why hotkeys must never be inserted into the
        // held set: the arms match exact shapes, so one extra position
        // silently destroys an otherwise valid chord.
        let with_chord = positions(&['f', 'g'], l);
        assert_eq!(with_chord.len(), 2);
        assert_eq!(left_hand_degree(&with_chord), None);
        // The same shape without it still resolves.
        assert_eq!(
            left_hand_degree(&positions(&['f'], l)),
            Some(ScaleDegree::I)
        );
    }

    #[test]
    fn empty_right_hand_is_none() {
        let p = PositionSet::new();
        assert_eq!(right_hand_transformation(&p), None);
    }

    #[test]
    fn programmer_dvorak_input_is_indistinguishable_from_qwerty() {
        let qwerty_positions = positions(&['f', 'h'], Layout::Qwerty);
        let pd_positions = positions(&['u', 'd'], Layout::ProgrammerDvorak);
        assert_eq!(qwerty_positions, pd_positions);
        assert_eq!(
            right_hand_transformation(&qwerty_positions),
            right_hand_transformation(&pd_positions)
        );
        assert_eq!(
            left_hand_degree(&qwerty_positions),
            left_hand_degree(&pd_positions)
        );
    }

    #[test]
    fn full_spec_from_qwerty_keys() {
        let p = positions(&['d', 'j'], Layout::Qwerty);
        let degree = left_hand_degree(&p).unwrap();
        let transformation = right_hand_transformation(&p).unwrap();
        let spec = ChordSpec::new(degree, transformation);
        assert_eq!(spec.voice(&c_major()), vec![67, 71, 74, 77]);
    }

    #[test]
    fn full_spec_from_programmer_dvorak_keys() {
        // Same physical gesture as above: V position + j position.
        let p = positions(&['e', 'h'], Layout::ProgrammerDvorak);
        let degree = left_hand_degree(&p).unwrap();
        let transformation = right_hand_transformation(&p).unwrap();
        let spec = ChordSpec::new(degree, transformation);
        assert_eq!(spec.voice(&c_major()), vec![67, 71, 74, 77]);
    }
}
