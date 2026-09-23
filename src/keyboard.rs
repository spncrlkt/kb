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
//!   these. `LeftInner` is the one home-row position the grammar never
//!   uses, so it carries a hotkey instead of a chord meaning.
//! - Below-home-row positions (ten variants, `LeftPinkyBelow`..
//!   `RightPinkyBelow`). These are hotkeys and lock keys, never inserted
//!   into a `PositionSet`. `hotkey` returns `Some` for the six positions
//!   that are bound; the rest are deliberately inert. The two register
//!   locks are simply two of those hotkeys.
//!
//! No position that `hotkey` returns `Some` for is ever inserted into a
//! `PositionSet`. That matters beyond tidiness: the chord grammar matches
//! exact shapes, so an extra position in the held set silently breaks the
//! chord it was added to.

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

/// An action bound to a below-home-row key.
///
/// The row below the home row is the hotkey row. Its keys never take part in
/// the chord grammar and never enter a `PositionSet`, so they stay usable for
/// editing even while both hands are holding a chord.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Hotkey {
    /// Latch the right register (left pinky below home row).
    LockRightRegister,
    /// Latch the left register (right pinky below home row).
    LockLeftRegister,
    /// Copy the chord under the progression cursor.
    CopyChord,
    /// Paste the clipboard after the progression cursor.
    PasteChord,
    /// Delete the chord under the progression cursor.
    DeleteChord,
    /// Undo the last progression edit. Shift selects `Redo` instead.
    Undo,
    /// Redo the last undone progression edit.
    Redo,
    /// Recall the chord under the progression cursor into the registers.
    ///
    /// Bound to `LeftInner`, the one home-row position the chord grammar
    /// ignores. Deliberately explicit: selecting a row must not clobber a
    /// latched register mid-performance.
    LoadSelectedChord,
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

    /// If this position is bound to a hotkey, which one.
    ///
    /// Covers the whole below-home-row row plus `LeftInner`, the single
    /// home-row position the chord grammar never uses.
    ///
    /// The left pinky below home row locks the *right* register, and vice
    /// versa: the gesture mirrors the register being targeted.
    ///
    /// `Redo` is never returned: it shares a position with `Undo` and is
    /// selected with Shift by the input layer, so this stays a pure function
    /// of the physical key.
    pub fn hotkey(self) -> Option<Hotkey> {
        use Hotkey::*;
        match self {
            KeyPosition::LeftInner => Some(LoadSelectedChord),
            KeyPosition::LeftPinkyBelow => Some(LockRightRegister),
            KeyPosition::RightPinkyBelow => Some(LockLeftRegister),
            KeyPosition::LeftRingBelow => Some(CopyChord),
            KeyPosition::LeftMiddleBelow => Some(PasteChord),
            KeyPosition::LeftIndexBelow => Some(DeleteChord),
            KeyPosition::LeftInnerBelow => Some(Undo),
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
    fn lock_hotkeys_are_cross_handed() {
        assert_eq!(
            KeyPosition::LeftPinkyBelow.hotkey(),
            Some(Hotkey::LockRightRegister)
        );
        assert_eq!(
            KeyPosition::RightPinkyBelow.hotkey(),
            Some(Hotkey::LockLeftRegister)
        );
    }

    #[test]
    fn grammar_home_row_positions_have_no_hotkey() {
        // Every home-row position the grammar actually uses must stay a
        // chord key. `LeftInner` is excluded on purpose: see the next test.
        for p in [
            KeyPosition::LeftPinky,
            KeyPosition::LeftRing,
            KeyPosition::LeftMiddle,
            KeyPosition::LeftIndex,
            KeyPosition::RightInner,
            KeyPosition::RightIndex,
            KeyPosition::RightMiddle,
            KeyPosition::RightRing,
            KeyPosition::RightPinky,
        ] {
            assert_eq!(p.hotkey(), None, "{:?} must stay a chord key", p);
        }
    }

    #[test]
    fn left_inner_recalls_instead_of_sounding_and_is_never_is_right() {
        assert_eq!(
            KeyPosition::LeftInner.hotkey(),
            Some(Hotkey::LoadSelectedChord)
        );
        // It reports as a left position, so the input layer must route it by
        // `hotkey` first or it would land in the held set and break shapes.
        assert!(KeyPosition::LeftInner.is_left());
        assert!(!KeyPosition::LeftInner.is_right());
    }

    #[test]
    fn unassigned_below_home_row_positions_are_inert() {
        // These four right-hand slots are deliberately unbound, reserved for
        // later progression operations.
        for p in [
            KeyPosition::RightInnerBelow,
            KeyPosition::RightIndexBelow,
            KeyPosition::RightMiddleBelow,
            KeyPosition::RightRingBelow,
        ] {
            assert_eq!(p.hotkey(), None, "{:?} should be inert", p);
        }
    }

    #[test]
    fn hotkeys_map_to_the_documented_programmer_dvorak_keys() {
        // The characters the user actually types on the active layout.
        let l = Layout::ProgrammerDvorak;
        let cases = [
            ('i', Hotkey::LoadSelectedChord), // physical `g`
            ('\'', Hotkey::LockRightRegister),
            ('q', Hotkey::CopyChord),
            ('j', Hotkey::PasteChord),
            ('k', Hotkey::DeleteChord),
            ('x', Hotkey::Undo),
            ('z', Hotkey::LockLeftRegister),
        ];
        for (c, expected) in cases {
            let pos = l.position(c).unwrap_or_else(|| panic!("{:?} is unmapped", c));
            assert_eq!(pos.hotkey(), Some(expected), "char {:?}", c);
        }
    }

    #[test]
    fn below_home_row_hotkeys_never_resolve_as_hands() {
        // A hotkey position must not double as a hand position, or the key
        // would do two things at once.
        for p in [
            KeyPosition::LeftPinkyBelow,
            KeyPosition::LeftRingBelow,
            KeyPosition::LeftMiddleBelow,
            KeyPosition::LeftIndexBelow,
            KeyPosition::LeftInnerBelow,
            KeyPosition::RightPinkyBelow,
        ] {
            assert!(p.hotkey().is_some());
            assert!(!p.is_left() && !p.is_right());
            assert!(!p.is_home_row());
        }
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
