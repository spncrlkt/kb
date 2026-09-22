//! Physical home-row key positions and keyboard layouts.
//!
//! All grammar in this crate operates on `KeyPosition`, never on the
//! characters a layout happens to produce. The QWERTY names are labels,
//! not a claim about the active layout.
//!
//! Three groups of positions exist:
//!
//! - Home-row positions (ten variants, `LeftPinky`..`RightPinky`). These
//!   feed the chord grammar. `is_left` / `is_right` return true only for
//!   these.
//! - Below-home-row positions (ten variants, `LeftPinkyBelow`..
//!   `RightPinkyBelow`). These are hotkeys and lock keys, never inserted
//!   into a `PositionSet`. `lock_target` returns `Some` for the two pinky
//!   positions used to lock registers.

use std::collections::BTreeSet;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyPosition {
    // Home row, left hand, outer -> inner
    LeftPinky,
    LeftRing,
    LeftMiddle,
    LeftIndex,
    LeftInner,
    // Home row, right hand, inner -> outer
    RightInner,
    RightIndex,
    RightMiddle,
    RightRing,
    RightPinky,
    // Below home row, left hand, outer -> inner
    LeftPinkyBelow,
    LeftRingBelow,
    LeftMiddleBelow,
    LeftIndexBelow,
    LeftInnerBelow,
    // Below home row, right hand, inner -> outer
    RightInnerBelow,
    RightIndexBelow,
    RightMiddleBelow,
    RightRingBelow,
    RightPinkyBelow,
}

/// Which register a lock key targets.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LockTarget {
    LeftRegister,
    RightRegister,
}

impl KeyPosition {
    /// True if this is a home-row left-hand position (chord grammar).
    pub fn is_left(self) -> bool {
        use KeyPosition::*;
        matches!(
            self,
            LeftPinky | LeftRing | LeftMiddle | LeftIndex | LeftInner
        )
    }

    /// True if this is a home-row right-hand position (chord grammar).
    pub fn is_right(self) -> bool {
        use KeyPosition::*;
        matches!(
            self,
            RightInner | RightIndex | RightMiddle | RightRing | RightPinky
        )
    }

    /// True if this is a home-row position of either hand.
    pub fn is_home_row(self) -> bool {
        self.is_left() || self.is_right()
    }

    /// True if this is a below-home-row position.
    pub fn is_below_home_row(self) -> bool {
        !self.is_home_row()
    }

    /// If this position is a lock key, which register it locks.
    ///
    /// The left pinky below home row locks the *right* register, and vice
    /// versa: the gesture mirrors the register being targeted.
    pub fn lock_target(self) -> Option<LockTarget> {
        match self {
            KeyPosition::LeftPinkyBelow => Some(LockTarget::RightRegister),
            KeyPosition::RightPinkyBelow => Some(LockTarget::LeftRegister),
            _ => None,
        }
    }

    /// The QWERTY character at this physical position. Used for all display.
    pub fn qwerty_label(self) -> char {
        use KeyPosition::*;
        match self {
            LeftPinky => 'a',
            LeftRing => 's',
            LeftMiddle => 'd',
            LeftIndex => 'f',
            LeftInner => 'g',
            RightInner => 'h',
            RightIndex => 'j',
            RightMiddle => 'k',
            RightRing => 'l',
            RightPinky => ';',
            LeftPinkyBelow => 'z',
            LeftRingBelow => 'x',
            LeftMiddleBelow => 'c',
            LeftIndexBelow => 'v',
            LeftInnerBelow => 'b',
            RightInnerBelow => 'n',
            RightIndexBelow => 'm',
            RightMiddleBelow => ',',
            RightRingBelow => '.',
            RightPinkyBelow => '/',
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Layout {
    Qwerty,
    ProgrammerDvorak,
}

/// The active input layout. Change this line to switch.
pub const ACTIVE_LAYOUT: Layout = Layout::ProgrammerDvorak;

impl Layout {
    /// Translate a character from the terminal into a physical position.
    /// The only place the active layout matters.
    ///
    /// Both shifted and unshifted variants are accepted where they map to
    /// the same physical key. Lock and hotkey positions are included.
    pub fn position(self, c: char) -> Option<KeyPosition> {
        use KeyPosition::*;
        let c = c.to_ascii_lowercase();
        Some(match (self, c) {
            // ---- QWERTY ----
            // Home row
            (Layout::Qwerty, 'a') => LeftPinky,
            (Layout::Qwerty, 's') => LeftRing,
            (Layout::Qwerty, 'd') => LeftMiddle,
            (Layout::Qwerty, 'f') => LeftIndex,
            (Layout::Qwerty, 'g') => LeftInner,
            (Layout::Qwerty, 'h') => RightInner,
            (Layout::Qwerty, 'j') => RightIndex,
            (Layout::Qwerty, 'k') => RightMiddle,
            (Layout::Qwerty, 'l') => RightRing,
            (Layout::Qwerty, ';') => RightPinky,
            // Below home row
            (Layout::Qwerty, 'z') => LeftPinkyBelow,
            (Layout::Qwerty, 'x') => LeftRingBelow,
            (Layout::Qwerty, 'c') => LeftMiddleBelow,
            (Layout::Qwerty, 'v') => LeftIndexBelow,
            (Layout::Qwerty, 'b') => LeftInnerBelow,
            (Layout::Qwerty, 'n') => RightInnerBelow,
            (Layout::Qwerty, 'm') => RightIndexBelow,
            (Layout::Qwerty, ',') => RightMiddleBelow,
            (Layout::Qwerty, '.') => RightRingBelow,
            (Layout::Qwerty, '/') => RightPinkyBelow,

            // ---- Programmer Dvorak ----
            // Home row
            (Layout::ProgrammerDvorak, 'a') => LeftPinky,
            (Layout::ProgrammerDvorak, 'o') => LeftRing,
            (Layout::ProgrammerDvorak, 'e') => LeftMiddle,
            (Layout::ProgrammerDvorak, 'u') => LeftIndex,
            (Layout::ProgrammerDvorak, 'i') => LeftInner,
            (Layout::ProgrammerDvorak, 'd') => RightInner,
            (Layout::ProgrammerDvorak, 'h') => RightIndex,
            (Layout::ProgrammerDvorak, 't') => RightMiddle,
            (Layout::ProgrammerDvorak, 'n') => RightRing,
            (Layout::ProgrammerDvorak, 's') => RightPinky,
            // Below home row
            (Layout::ProgrammerDvorak, '\'') => LeftPinkyBelow,
            (Layout::ProgrammerDvorak, 'q') => LeftRingBelow,
            (Layout::ProgrammerDvorak, 'j') => LeftMiddleBelow,
            (Layout::ProgrammerDvorak, 'k') => LeftIndexBelow,
            (Layout::ProgrammerDvorak, 'x') => LeftInnerBelow,
            (Layout::ProgrammerDvorak, 'b') => RightInnerBelow,
            (Layout::ProgrammerDvorak, 'm') => RightIndexBelow,
            (Layout::ProgrammerDvorak, 'w') => RightMiddleBelow,
            (Layout::ProgrammerDvorak, 'v') => RightRingBelow,
            (Layout::ProgrammerDvorak, 'z') => RightPinkyBelow,

            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Layout::Qwerty => "QWERTY",
            Layout::ProgrammerDvorak => "Programmer Dvorak",
        }
    }
}

/// A set of physical home-row positions held together as one chord.
/// Below-home-row positions are never inserted into this set.
pub type PositionSet = BTreeSet<KeyPosition>;

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qwerty_home_row_maps_to_positions() {
        let l = Layout::Qwerty;
        assert_eq!(l.position('a'), Some(KeyPosition::LeftPinky));
        assert_eq!(l.position('s'), Some(KeyPosition::LeftRing));
        assert_eq!(l.position('d'), Some(KeyPosition::LeftMiddle));
        assert_eq!(l.position('f'), Some(KeyPosition::LeftIndex));
        assert_eq!(l.position('g'), Some(KeyPosition::LeftInner));
        assert_eq!(l.position('h'), Some(KeyPosition::RightInner));
        assert_eq!(l.position('j'), Some(KeyPosition::RightIndex));
        assert_eq!(l.position('k'), Some(KeyPosition::RightMiddle));
        assert_eq!(l.position('l'), Some(KeyPosition::RightRing));
        assert_eq!(l.position(';'), Some(KeyPosition::RightPinky));
    }

    #[test]
    fn qwerty_below_home_row_maps_to_positions() {
        let l = Layout::Qwerty;
        assert_eq!(l.position('z'), Some(KeyPosition::LeftPinkyBelow));
        assert_eq!(l.position('x'), Some(KeyPosition::LeftRingBelow));
        assert_eq!(l.position('c'), Some(KeyPosition::LeftMiddleBelow));
        assert_eq!(l.position('v'), Some(KeyPosition::LeftIndexBelow));
        assert_eq!(l.position('b'), Some(KeyPosition::LeftInnerBelow));
        assert_eq!(l.position('n'), Some(KeyPosition::RightInnerBelow));
        assert_eq!(l.position('m'), Some(KeyPosition::RightIndexBelow));
        assert_eq!(l.position(','), Some(KeyPosition::RightMiddleBelow));
        assert_eq!(l.position('.'), Some(KeyPosition::RightRingBelow));
        assert_eq!(l.position('/'), Some(KeyPosition::RightPinkyBelow));
    }

    #[test]
    fn programmer_dvorak_home_row_maps_to_positions() {
        let l = Layout::ProgrammerDvorak;
        assert_eq!(l.position('a'), Some(KeyPosition::LeftPinky));
        assert_eq!(l.position('o'), Some(KeyPosition::LeftRing));
        assert_eq!(l.position('e'), Some(KeyPosition::LeftMiddle));
        assert_eq!(l.position('u'), Some(KeyPosition::LeftIndex));
        assert_eq!(l.position('i'), Some(KeyPosition::LeftInner));
        assert_eq!(l.position('d'), Some(KeyPosition::RightInner));
        assert_eq!(l.position('h'), Some(KeyPosition::RightIndex));
        assert_eq!(l.position('t'), Some(KeyPosition::RightMiddle));
        assert_eq!(l.position('n'), Some(KeyPosition::RightRing));
        assert_eq!(l.position('s'), Some(KeyPosition::RightPinky));
    }

    #[test]
    fn programmer_dvorak_below_home_row_maps_to_positions() {
        let l = Layout::ProgrammerDvorak;
        assert_eq!(l.position('\''), Some(KeyPosition::LeftPinkyBelow));
        assert_eq!(l.position('q'), Some(KeyPosition::LeftRingBelow));
        assert_eq!(l.position('j'), Some(KeyPosition::LeftMiddleBelow));
        assert_eq!(l.position('k'), Some(KeyPosition::LeftIndexBelow));
        assert_eq!(l.position('x'), Some(KeyPosition::LeftInnerBelow));
        assert_eq!(l.position('b'), Some(KeyPosition::RightInnerBelow));
        assert_eq!(l.position('m'), Some(KeyPosition::RightIndexBelow));
        assert_eq!(l.position('w'), Some(KeyPosition::RightMiddleBelow));
        assert_eq!(l.position('v'), Some(KeyPosition::RightRingBelow));
        assert_eq!(l.position('z'), Some(KeyPosition::RightPinkyBelow));
    }

    #[test]
    fn off_keyboard_characters_are_ignored() {
        assert_eq!(Layout::Qwerty.position('1'), None);
        assert_eq!(Layout::ProgrammerDvorak.position('1'), None);
        assert_eq!(Layout::Qwerty.position(' '), None);
        assert_eq!(Layout::ProgrammerDvorak.position(' '), None);
    }

    #[test]
    fn shifted_characters_map_to_same_position() {
        assert_eq!(Layout::Qwerty.position('A'), Some(KeyPosition::LeftPinky));
        assert_eq!(Layout::Qwerty.position('Z'), Some(KeyPosition::LeftPinkyBelow));
        assert_eq!(
            Layout::ProgrammerDvorak.position('Q'),
            Some(KeyPosition::LeftRingBelow)
        );
        assert_eq!(
            Layout::ProgrammerDvorak.position('X'),
            Some(KeyPosition::LeftInnerBelow)
        );
    }

    #[test]
    fn same_character_means_different_positions_across_layouts() {
        // 's' is left-ring in QWERTY but right-pinky in P.D.
        assert_eq!(Layout::Qwerty.position('s'), Some(KeyPosition::LeftRing));
        assert_eq!(
            Layout::ProgrammerDvorak.position('s'),
            Some(KeyPosition::RightPinky)
        );
        // 'd' is left-middle in QWERTY but right-inner in P.D.
        assert_eq!(Layout::Qwerty.position('d'), Some(KeyPosition::LeftMiddle));
        assert_eq!(
            Layout::ProgrammerDvorak.position('d'),
            Some(KeyPosition::RightInner)
        );
        // 'z' is a lock key in QWERTY but a lock key too in P.D. (different side).
        assert_eq!(Layout::Qwerty.position('z'), Some(KeyPosition::LeftPinkyBelow));
        assert_eq!(
            Layout::ProgrammerDvorak.position('z'),
            Some(KeyPosition::RightPinkyBelow)
        );
    }

    #[test]
    fn lock_targets_are_cross_handed() {
        assert_eq!(
            KeyPosition::LeftPinkyBelow.lock_target(),
            Some(LockTarget::RightRegister)
        );
        assert_eq!(
            KeyPosition::RightPinkyBelow.lock_target(),
            Some(LockTarget::LeftRegister)
        );
    }

    #[test]
    fn non_lock_keys_have_no_lock_target() {
        assert_eq!(KeyPosition::LeftPinky.lock_target(), None);
        assert_eq!(KeyPosition::RightIndex.lock_target(), None);
        assert_eq!(KeyPosition::LeftInnerBelow.lock_target(), None);
        assert_eq!(KeyPosition::RightMiddleBelow.lock_target(), None);
    }

    #[test]
    fn below_home_row_is_not_left_or_right() {
        // Register resolution uses is_left/is_right, so below-home-row
        // keys must never satisfy either.
        for p in [
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
        ] {
            assert!(!p.is_left(), "{:?} should not be is_left", p);
            assert!(!p.is_right(), "{:?} should not be is_right", p);
            assert!(!p.is_home_row(), "{:?} should not be is_home_row", p);
            assert!(p.is_below_home_row(), "{:?} should be is_below_home_row", p);
        }
    }

    #[test]
    fn qwerty_labels_are_layout_independent() {
        assert_eq!(KeyPosition::RightInner.qwerty_label(), 'h');
        assert_eq!(KeyPosition::RightPinky.qwerty_label(), ';');
        assert_eq!(KeyPosition::LeftPinky.qwerty_label(), 'a');
        assert_eq!(KeyPosition::LeftPinkyBelow.qwerty_label(), 'z');
        assert_eq!(KeyPosition::RightPinkyBelow.qwerty_label(), '/');
        assert_eq!(KeyPosition::LeftInnerBelow.qwerty_label(), 'b');
        assert_eq!(KeyPosition::RightIndexBelow.qwerty_label(), 'm');
    }
}
