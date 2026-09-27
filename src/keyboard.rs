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

use serde::{Deserialize, Serialize};

/// Serialised into the MIDI export payload, so the variant names are part of
/// the file format; rename them only with a `project::VERSION` bump.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    // Above the home row, right hand, inner -> outer.
    //
    // The row the chord grammar never reaches and nothing else wanted: five
    // keys that are free, on the hand that is not holding a chord. On
    // Programmer Dvorak these are `f`, `g`, `c`, `r` and `l`.
    RightInnerAbove,
    RightIndexAbove,
    RightMiddleAbove,
    RightRingAbove,
    RightPinkyAbove,
    /// The very top-left key, left of `1`.
    ///
    /// Not part of either hand and never inserted into a `PositionSet`: it is
    /// the sinko tap key, so it has to stay usable while both hands hold a
    /// chord. On Programmer Dvorak this key is `$`.
    TopLeft,
    /// The next key along: the number row's `1`, right of `TopLeft`.
    ///
    /// The metronome toggle. Like `TopLeft` it is a performance control rather
    /// than part of the chord grammar. On Programmer Dvorak this key is `&`.
    TopRow1,
    /// Two keys further along again: the number row's `3`.
    ///
    /// The transport tap — play/pause, and the tap counts that restart or seek.
    /// On Programmer Dvorak this key is `{`.
    TopRow3,
    /// The key right of `TopRow3`: the number row's `4`.
    ///
    /// On Programmer Dvorak this key is `}` — the neighbour of the transport
    /// tap and the shape of a mistyped `{`. It does the same thing, because a
    /// performance key that punishes a one-key miss is a performance key that
    /// stops the music.
    TopRow4,
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
    /// Copy the *rhythm* of the chord under the cursor, for pasting onto other
    /// chords.
    ///
    /// Shares its position with [`Hotkey::CopyChord`] and is selected with
    /// Shift, exactly as [`Hotkey::Redo`] rides on [`Hotkey::Undo`] — so `q`
    /// copies the whole entry (chord, registers, rhythm and offset) and
    /// `Shift+Q` copies only the rhythm. Never returned by `hotkey()`.
    CopySinko,
    /// Paste the clipboard after the progression cursor.
    PasteChord,
    /// Give the chord under the cursor a copy of the rhythm clipboard.
    ///
    /// Shares its position with [`Hotkey::PasteChord`] and is selected with
    /// Shift. The difference is the whole point of having both: `j` inserts a
    /// new slot, while `Shift+J` changes the rhythm of the slot you are on,
    /// leaving its chord and its offset alone. Never returned by `hotkey()`.
    PasteSinko,
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
    /// Put the selected history chord back into the registers.
    ///
    /// The same gesture as recalling a progression row, on a chord that is not
    /// in the progression — which is how a chord you played by accident becomes
    /// one you can commit.
    HistoryRecall,
    /// Step back through the history, sounding each chord on the way.
    HistoryBack,
    /// Step forward through the history, the same way.
    HistoryForward,
    /// Sound the selected chord for as long as the key is held, and let it fade
    /// when it is released.
    HistoryPlay,
    /// Cycle the log: away, the exact history, the top list, away.
    HistoryView,
    /// Tap one beat of the rhythm being recorded in the Sinko panel.
    ///
    /// Bound to `TopLeft` (`$` on Programmer Dvorak). Global rather than scoped
    /// to the panel: the point is to tap while watching the progression, and the
    /// key is reachable without moving either hand off the home row.
    SinkoTap,
    /// Start or stop the metronome click.
    ///
    /// Bound to `TopRow1` (`&` on Programmer Dvorak), the next key along from
    /// the tap key.
    MetronomeToggle,
    /// Tap the transport: once plays or pauses, twice restarts from bar 1,
    /// three times seeks to the middle.
    ///
    /// Bound to `TopRow3` (`{` on Programmer Dvorak). It used to be the space
    /// bar, which is now the both-hands chord latch.
    TransportTap,
}

impl Hotkey {
    /// The action `Shift` selects on this key, or the action itself.
    ///
    /// One place, because two things depend on it: the input layer, and the test
    /// that resolves `REFERENCE.md`'s key table through the real keyboard map.
    /// `Shift` never introduces a new physical key — it only picks the second
    /// action on one — so this is the whole of the modifier's meaning.
    pub fn shifted(self, shift: bool) -> Hotkey {
        if !shift {
            return self;
        }
        match self {
            Hotkey::Undo => Hotkey::Redo,
            Hotkey::CopyChord => Hotkey::CopySinko,
            Hotkey::PasteChord => Hotkey::PasteSinko,
            other => other,
        }
    }
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
    /// Covers the whole below-home-row row, the right hand's row above the home
    /// row, and `LeftInner` — the single home-row position the chord grammar
    /// never uses.
    ///
    /// The left pinky below home row locks the *right* register, and vice
    /// versa: the gesture mirrors the register being targeted.
    ///
    /// `Redo`, `CopySinko` and `PasteSinko` are never returned: they share a
    /// position with another action and are selected with Shift by the input
    /// layer, so this stays a pure function of the physical key.
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
            KeyPosition::RightInnerAbove => Some(HistoryRecall),
            KeyPosition::RightIndexAbove => Some(HistoryBack),
            KeyPosition::RightMiddleAbove => Some(HistoryForward),
            KeyPosition::RightRingAbove => Some(HistoryPlay),
            KeyPosition::RightPinkyAbove => Some(HistoryView),
            KeyPosition::TopLeft => Some(SinkoTap),
            KeyPosition::TopRow1 => Some(MetronomeToggle),
            KeyPosition::TopRow3 | KeyPosition::TopRow4 => Some(TransportTap),
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
            RightInnerAbove => 'y',
            RightIndexAbove => 'u',
            RightMiddleAbove => 'i',
            RightRingAbove => 'o',
            RightPinkyAbove => 'p',
            // The QWERTY label is the keycap, not the character P.D. types.
            TopLeft => '`',
            TopRow1 => '1',
            TopRow3 => '3',
            TopRow4 => '4',
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Layout {
    /// The layout most terminals arrive in. Fully mapped, and selected by
    /// editing [`ACTIVE_LAYOUT`] rather than at runtime, so a build that has not
    /// switched to it never constructs the variant — hence the allow, not a
    /// deletion: it is one line away from being the active layout.
    #[allow(dead_code)]
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
            // Top-left, above the home row. Both shifted and unshifted reach
            // the same physical key, as everywhere else.
            // Above home row
            (Layout::Qwerty, 'y') => RightInnerAbove,
            (Layout::Qwerty, 'u') => RightIndexAbove,
            (Layout::Qwerty, 'i') => RightMiddleAbove,
            (Layout::Qwerty, 'o') => RightRingAbove,
            (Layout::Qwerty, 'p') => RightPinkyAbove,
            (Layout::Qwerty, '`') => TopLeft,
            (Layout::Qwerty, '~') => TopLeft,
            (Layout::Qwerty, '1') => TopRow1,
            (Layout::Qwerty, '!') => TopRow1,
            (Layout::Qwerty, '3') => TopRow3,
            (Layout::Qwerty, '#') => TopRow3,
            (Layout::Qwerty, '4') => TopRow4,
            (Layout::Qwerty, '$') => TopRow4,

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
            // Above home row. P.D. moves the whole row right by two: the
            // QWERTY `y u i o p` positions carry `f g c r l`, which is where
            // the history keys come from.
            (Layout::ProgrammerDvorak, 'f') => RightInnerAbove,
            (Layout::ProgrammerDvorak, 'g') => RightIndexAbove,
            (Layout::ProgrammerDvorak, 'c') => RightMiddleAbove,
            (Layout::ProgrammerDvorak, 'r') => RightRingAbove,
            (Layout::ProgrammerDvorak, 'l') => RightPinkyAbove,
            // P.D. puts `$` on the top-left key, with `~` shifted.
            (Layout::ProgrammerDvorak, '$') => TopLeft,
            (Layout::ProgrammerDvorak, '~') => TopLeft,
            // And `&` on the `1` key next to it, with `1` shifted.
            (Layout::ProgrammerDvorak, '&') => TopRow1,
            (Layout::ProgrammerDvorak, '1') => TopRow1,
            // And `{` two keys along on the `3` key, with `3` shifted.
            (Layout::ProgrammerDvorak, '{') => TopRow3,
            (Layout::ProgrammerDvorak, '3') => TopRow3,
            (Layout::ProgrammerDvorak, '}') => TopRow4,
            (Layout::ProgrammerDvorak, '4') => TopRow4,

            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Layout::Qwerty => "QWERTY",
            Layout::ProgrammerDvorak => "Programmer Dvorak",
        }
    }

    /// The register-lock keys as `(keycap, character typed)`, for the right and
    /// left registers.
    ///
    /// The keycap and the typed character for the two register locks.
    ///
    /// Test-only. It fed the `Lock:` line on screen until that line was removed
    /// for costing a row; what is worth keeping is the check it enables —
    /// `the_lock_hint_names_keys_that_really_work` asserts each *typed*
    /// character resolves to the lock it claims, so the README's table cannot
    /// drift from [`Layout::position`].
    #[cfg(test)]
    pub fn register_lock_keys(self) -> ((char, char), (char, char)) {
        match self {
            Layout::ProgrammerDvorak => (('z', '\''), ('/', 'z')),
            Layout::Qwerty => (('z', 'z'), ('/', '/')),
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
        // Only the number-row keys the app actually binds are mapped; the rest
        // of the row is not part of any gesture.
        // `3` is deliberately absent: it is the shifted half of the transport
        // tap key and must keep reaching it.
        for c in ['2', '9', '0', '-', '=', ' '] {
            assert_eq!(Layout::Qwerty.position(c), None, "QWERTY {:?}", c);
            assert_eq!(
                Layout::ProgrammerDvorak.position(c),
                None,
                "Programmer Dvorak {:?}",
                c
            );
        }
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
    fn the_group_actions_have_no_key_of_their_own() {
        // Replace, reverse, rotate and clear-sinko live in the Progression
        // panel's menu, so the below-home-row slots they briefly held are free
        // again and every one of them is inert rather than a chord key.
        for p in [
            KeyPosition::RightInnerBelow,
            KeyPosition::RightIndexBelow,
            KeyPosition::RightMiddleBelow,
            KeyPosition::RightRingBelow,
        ] {
            assert_eq!(p.hotkey(), None, "{:?} should be inert", p);
        }
    }

    /// The rows of a fenced hotkey table: keycap, the character that reaches the
    /// app, the `Hotkey` variant the row claims (empty where the document does not
    /// name one), and what the row says the key does.
    ///
    /// Columns are found **by header name**, not by position, because the two
    /// documents that carry this table have different numbers of them:
    /// `REFERENCE.md` names the variant and the panel scope, and `CHEATSHEET.md`
    /// drops both, because a card a player reads does not need to know what the
    /// code calls the command.
    ///
    /// Parsing the documents is the point: they are what a player reads, and the
    /// only way to keep them true is to make the build fail when they are not.
    fn fenced_hotkeys(doc: &str, begin: &str, end: &str) -> Vec<(String, char, String, String)> {
        let table = doc
            .split(begin)
            .nth(1)
            .and_then(|rest| rest.split(end).next())
            .unwrap_or_else(|| panic!("{begin} … {end} must fence a table of hotkeys"));

        // Trimmed but otherwise left as written: a keycap cell is markdown
        // (`` `c` + Shift ``), and it only ever reaches a failure message, where
        // the markup reads better than a half-stripped backtick.
        let cells = |line: &str| -> Vec<String> {
            line.split('|')
                .map(|cell| cell.trim().to_string())
                .collect()
        };
        let mut lines = table.lines().filter(|line| line.starts_with('|'));
        let header = cells(lines.next().expect("the table must have a header row"));
        let column = |name: &str| {
            header
                .iter()
                .position(|cell| cell == name)
                .unwrap_or_else(|| panic!("no `{name}` column in the table under {begin}"))
        };
        let (keycap_at, typed_at, does_at) =
            (column("Keycap"), column("You type"), column("What it does"));
        let variant_at = header.iter().position(|cell| cell == "Command");

        let rows: Vec<(String, char, String, String)> = lines
            .filter(|line| !line.contains("---"))
            .map(|line| {
                // The cell before the leading `|` is empty, so every index here is
                // one past what the header's own numbering says.
                let cells = cells(line);
                // Only the two cells that are *compared* have to be unwrapped —
                // the character, which is mapped through the layout, and the
                // variant name, which is matched against the real `Hotkey`. The
                // keycap and the description are read, not parsed.
                let typed: Vec<char> = cells[typed_at].trim_matches('`').trim().chars().collect();
                assert_eq!(
                    typed.len(),
                    1,
                    "the \"you type\" cell must be one character: {line:?}"
                );
                let variant = variant_at
                    .map(|at| cells[at].trim_matches('`').trim().to_string())
                    .unwrap_or_default();
                (
                    cells[keycap_at].clone(),
                    typed[0],
                    variant,
                    cells[does_at].clone(),
                )
            })
            .collect();
        assert!(rows.len() >= 10, "the table looks empty: {rows:?}");
        rows
    }

    /// The rows of `REFERENCE.md`'s hotkey table.
    fn documented_hotkeys() -> Vec<(String, char, String)> {
        fenced_hotkeys(
            include_str!("../REFERENCE.md"),
            "<!-- hotkeys:begin -->",
            "<!-- hotkeys:end -->",
        )
        .into_iter()
        .map(|(keycap, typed, variant, _)| (keycap, typed, variant))
        .collect()
    }

    #[test]
    fn the_reference_documents_keys_that_do_what_they_claim() {
        // Direction one: every documented row resolves, through the real map,
        // to the command it names. A renamed variant or a rebound key fails here.
        for (keycap, typed, command) in documented_hotkeys() {
            let position = Layout::ProgrammerDvorak
                .position(typed)
                .unwrap_or_else(|| panic!("{} is unmapped on this layout", keycap));
            let hotkey = position
                .hotkey()
                .unwrap_or_else(|| panic!("{} carries no hotkey", keycap));
            // An uppercase "you type" is how the table spells `Shift`.
            let actual = hotkey.shifted(typed.is_ascii_uppercase());
            assert_eq!(
                format!("{:?}", actual),
                command,
                "{} ({:?}) does not run {}",
                keycap,
                typed,
                command
            );
        }
    }

    #[test]
    fn the_reference_documents_every_key_the_keyboard_offers() {
        // Direction two: no binding is missing from the table. A new hotkey on a
        // position with no row here fails, so the document cannot go quietly out
        // of date as the keyboard grows.
        //
        // Compared by *position*, not by character: one physical key answers to
        // several characters (`1` and `&`, `$` and `~`), and the table names the
        // key once.
        let documented: Vec<KeyPosition> = documented_hotkeys()
            .into_iter()
            .filter(|(_, typed, _)| !typed.is_ascii_uppercase())
            .map(|(keycap, typed, _)| {
                Layout::ProgrammerDvorak
                    .position(typed)
                    .unwrap_or_else(|| panic!("{} is unmapped", keycap))
            })
            .collect();

        let offered: Vec<KeyPosition> = (0x20u8..0x7f)
            .map(|b| b as char)
            .filter_map(|c| Layout::ProgrammerDvorak.position(c))
            .filter(|p| p.hotkey().is_some())
            .fold(Vec::new(), |mut seen, p| {
                if !seen.contains(&p) {
                    seen.push(p);
                }
                seen
            });

        for p in &offered {
            assert!(
                documented.contains(p),
                "{:?} carries a hotkey but has no row in REFERENCE.md",
                p
            );
        }
        for p in &documented {
            assert!(
                offered.contains(p),
                "{:?} is in REFERENCE.md's table but carries no hotkey",
                p
            );
        }
    }

    #[test]
    fn the_cheatsheet_says_what_the_reference_says() {
        // The cheat sheet is the card a player keeps beside the app and trusts
        // without checking, so it has to agree with the reference it summarises:
        // the same keys, and the same words for what each one does. A rebound key,
        // a missing row or a reworded action fails here rather than in somebody's
        // hands.
        //
        // Compared as rows of (keycap, character, wording), sorted by character so
        // the cheat sheet is free to group them for reading rather than follow the
        // reference's order. The `Hotkey` variant is dropped from both: it is the
        // reference's business, not a card's, and the wording is what ties the two
        // together.
        let rows = |doc, begin, end| {
            let mut rows: Vec<(String, char, String)> = fenced_hotkeys(doc, begin, end)
                .into_iter()
                .map(|(keycap, typed, _, does)| (keycap, typed, does))
                .collect();
            rows.sort_by_key(|(_, typed, _)| *typed);
            rows
        };

        assert_eq!(
            rows(
                include_str!("../CHEATSHEET.md"),
                "<!-- cheatsheet-hotkeys:begin -->",
                "<!-- cheatsheet-hotkeys:end -->",
            ),
            rows(
                include_str!("../REFERENCE.md"),
                "<!-- hotkeys:begin -->",
                "<!-- hotkeys:end -->",
            ),
            "CHEATSHEET.md and REFERENCE.md disagree about the hotkeys"
        );
    }

    #[test]
    fn shift_only_ever_picks_a_second_action_on_a_real_key() {
        // The invariant the table relies on: `Shift` never invents a key.
        use Hotkey::*;
        for (plain, shifted) in [
            (Undo, Redo),
            (CopyChord, CopySinko),
            (PasteChord, PasteSinko),
        ] {
            assert_eq!(plain.shifted(false), plain, "{:?}", plain);
            assert_eq!(plain.shifted(true), shifted, "{:?}", plain);
        }
        // And an action with no second meaning is itself under `Shift`.
        assert_eq!(SinkoTap.shifted(true), SinkoTap);
        assert_eq!(TransportTap.shifted(true), TransportTap);
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
    fn the_rhythm_clipboard_rides_the_chord_clipboard_positions() {
        // Shift selects a second action on one position, so the physical key
        // stays a pure function: `q`/`j` move whole entries, `Shift+Q`/`Shift+J`
        // move rhythms. The typed characters are the P.D. ones, shifted.
        let l = Layout::ProgrammerDvorak;
        for (plain, shifted, plain_action, shifted_action) in [
            ('q', 'Q', Hotkey::CopyChord, Hotkey::CopySinko),
            ('j', 'J', Hotkey::PasteChord, Hotkey::PasteSinko),
        ] {
            let plain_pos = l.position(plain).expect("the unshifted key");
            let shifted_pos = l.position(shifted).expect("the shifted key");
            assert_eq!(plain_pos, shifted_pos, "{:?} and {:?}", plain, shifted);
            assert_eq!(plain_pos.hotkey(), Some(plain_action));
            // The position reports the unshifted action; the input layer turns
            // it into the shifted one, so `hotkey()` never returns either.
            assert_eq!(plain_pos.hotkey(), Some(plain_action));
            assert_ne!(plain_pos.hotkey(), Some(shifted_action));
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
    fn the_top_left_key_is_the_sinko_tap_on_both_layouts() {
        // The character P.D. actually produces, and the QWERTY keycap.
        assert_eq!(
            Layout::ProgrammerDvorak.position('$'),
            Some(KeyPosition::TopLeft)
        );
        assert_eq!(
            Layout::ProgrammerDvorak.position('~'),
            Some(KeyPosition::TopLeft),
            "the shifted half of the same physical key"
        );
        assert_eq!(Layout::Qwerty.position('`'), Some(KeyPosition::TopLeft));
        assert_eq!(KeyPosition::TopLeft.qwerty_label(), '`');
    }

    #[test]
    fn the_tap_key_is_a_hotkey_and_never_a_chord_key() {
        // It has to stay usable while both hands hold a chord, which means it
        // must never reach the held `PositionSet`.
        assert_eq!(KeyPosition::TopLeft.hotkey(), Some(Hotkey::SinkoTap));
        assert!(!KeyPosition::TopLeft.is_left());
        assert!(!KeyPosition::TopLeft.is_right());
        assert!(!KeyPosition::TopLeft.is_home_row());
    }

    #[test]
    fn the_key_next_to_the_transport_tap_does_the_same_thing() {
        // `{` and `}` are neighbours on Programmer Dvorak, and a one-key miss at
        // a performance control is worth tolerating: both tap the transport.
        let l = Layout::ProgrammerDvorak;
        assert_eq!(l.position('{'), Some(KeyPosition::TopRow3));
        assert_eq!(l.position('}'), Some(KeyPosition::TopRow4));
        assert_eq!(l.position('3'), Some(KeyPosition::TopRow3), "the shift pair");
        assert_eq!(l.position('4'), Some(KeyPosition::TopRow4), "the shift pair");

        assert_eq!(KeyPosition::TopRow3.hotkey(), Some(Hotkey::TransportTap));
        assert_eq!(KeyPosition::TopRow4.hotkey(), Some(Hotkey::TransportTap));

        // Neither is a chord key, so both stay usable mid-performance.
        for p in [KeyPosition::TopRow3, KeyPosition::TopRow4] {
            assert!(!p.is_left() && !p.is_right() && !p.is_home_row());
        }
        assert_eq!(KeyPosition::TopRow4.qwerty_label(), '4');
        assert_eq!(Layout::Qwerty.position('4'), Some(KeyPosition::TopRow4));
    }

    #[test]
    fn the_metronome_key_is_next_to_the_tap_key() {
        // `&` sits immediately right of `$` on Programmer Dvorak, which is why
        // both are reachable without leaving the home row.
        assert_eq!(
            Layout::ProgrammerDvorak.position('&'),
            Some(KeyPosition::TopRow1)
        );
        assert_eq!(
            Layout::ProgrammerDvorak.position('1'),
            Some(KeyPosition::TopRow1),
            "the shifted half of the same physical key"
        );
        assert_eq!(Layout::Qwerty.position('1'), Some(KeyPosition::TopRow1));
        assert_eq!(KeyPosition::TopRow1.qwerty_label(), '1');

        assert_eq!(KeyPosition::TopRow1.hotkey(), Some(Hotkey::MetronomeToggle));
        assert!(!KeyPosition::TopRow1.is_left());
        assert!(!KeyPosition::TopRow1.is_right());
        assert!(!KeyPosition::TopRow1.is_home_row());
    }

    #[test]
    fn the_transport_key_is_the_third_number_row_key() {
        // The three performance controls sit in a row along the top: `$`, `&`,
        // `{` on Programmer Dvorak.
        assert_eq!(
            Layout::ProgrammerDvorak.position('{'),
            Some(KeyPosition::TopRow3)
        );
        assert_eq!(
            Layout::ProgrammerDvorak.position('3'),
            Some(KeyPosition::TopRow3),
            "the shifted half of the same physical key"
        );
        assert_eq!(Layout::Qwerty.position('3'), Some(KeyPosition::TopRow3));
        assert_eq!(KeyPosition::TopRow3.qwerty_label(), '3');

        assert_eq!(KeyPosition::TopRow3.hotkey(), Some(Hotkey::TransportTap));
        assert!(!KeyPosition::TopRow3.is_left());
        assert!(!KeyPosition::TopRow3.is_right());
        assert!(!KeyPosition::TopRow3.is_home_row());
    }

    #[test]
    fn the_three_performance_keys_are_three_distinct_positions() {
        let keys = [
            ('$', Hotkey::SinkoTap),
            ('&', Hotkey::MetronomeToggle),
            ('{', Hotkey::TransportTap),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for (c, expected) in keys {
            let pos = Layout::ProgrammerDvorak
                .position(c)
                .unwrap_or_else(|| panic!("{:?} is unmapped", c));
            assert_eq!(pos.hotkey(), Some(expected), "char {:?}", c);
            assert!(seen.insert(pos), "{:?} is shared by two keys", pos);
        }
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn the_lock_hint_names_keys_that_really_work() {
        // The regression: this line hardcoded the QWERTY names, which under
        // Programmer Dvorak listed the registers backwards.
        let ((right_cap, right_typed), (left_cap, left_typed)) =
            Layout::ProgrammerDvorak.register_lock_keys();
        assert_eq!((right_cap, right_typed), ('z', '\''));
        assert_eq!((left_cap, left_typed), ('/', 'z'));

        // Each typed character must resolve to the lock it claims.
        assert_eq!(
            Layout::ProgrammerDvorak
                .position(right_typed)
                .and_then(|p| p.hotkey()),
            Some(Hotkey::LockRightRegister)
        );
        assert_eq!(
            Layout::ProgrammerDvorak
                .position(left_typed)
                .and_then(|p| p.hotkey()),
            Some(Hotkey::LockLeftRegister)
        );

        // QWERTY types what it draws.
        let ((rc, rt), (lc, lt)) = Layout::Qwerty.register_lock_keys();
        assert_eq!((rc, rt, lc, lt), ('z', 'z', '/', '/'));
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
