//! Progression data model: entries, clipboard, and edit operations.

use std::collections::BTreeSet;

use crate::keyboard::{KeyPosition, PositionSet};
use crate::music::{
    chord_label, diatonic_triad, diatonic_triad_label, ChordSpec, Key, ScaleDegree,
    Transformation,
};

// -----------------------------------------------------------------------------
// Registers
// -----------------------------------------------------------------------------

/// Per-hand locked position sets.
///
/// `None` means "never set". `Some(empty set)` means "explicitly cleared".
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Registers {
    pub left: Option<PositionSet>,
    pub right: Option<PositionSet>,
}

impl Registers {
    pub fn lock_right(&mut self, held: &PositionSet) {
        let captured: PositionSet = held.iter().filter(|p| p.is_right()).copied().collect();
        self.right = Some(captured);
    }

    pub fn lock_left(&mut self, held: &PositionSet) {
        let captured: PositionSet = held.iter().filter(|p| p.is_left()).copied().collect();
        self.left = Some(captured);
    }

    /// Combine live input with the registers. Live input wins per side.
    pub fn resolve(&self, live: &PositionSet) -> PositionSet {
        let mut out = PositionSet::new();

        let live_left: PositionSet = live.iter().filter(|p| p.is_left()).copied().collect();
        let live_right: PositionSet = live.iter().filter(|p| p.is_right()).copied().collect();

        if !live_left.is_empty() {
            out.extend(live_left);
        } else if let Some(ref r) = self.left {
            out.extend(r.iter().copied());
        }

        if !live_right.is_empty() {
            out.extend(live_right);
        } else if let Some(ref r) = self.right {
            out.extend(r.iter().copied());
        }

        out
    }

    pub fn is_empty(&self) -> bool {
        self.left.is_none() && self.right.is_none()
    }
}

// -----------------------------------------------------------------------------
// Entries
// -----------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct ProgressionEntry {
    pub degree: ScaleDegree,
    pub transformation: Option<Transformation>,
    pub registers: Registers,
}

impl ProgressionEntry {
    pub fn notes(&self, key: &Key) -> Vec<u8> {
        match self.transformation {
            Some(t) => ChordSpec::new(self.degree, t).voice(key),
            None => diatonic_triad(key, self.degree),
        }
    }

    pub fn label(&self, key: &Key) -> String {
        match self.transformation {
            Some(t) => chord_label(key, &ChordSpec::new(self.degree, t)),
            None => diatonic_triad_label(key, self.degree),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Slot {
    Chord(ProgressionEntry),
    Rest,
}

impl Slot {
    pub fn label(&self, key: &Key) -> String {
        match self {
            Slot::Chord(e) => e.label(key),
            Slot::Rest => "—".to_string(),
        }
    }

    pub fn notes(&self, key: &Key) -> Option<Vec<u8>> {
        match self {
            Slot::Chord(e) => Some(e.notes(key)),
            Slot::Rest => None,
        }
    }
}

// -----------------------------------------------------------------------------
// Progression
// -----------------------------------------------------------------------------

#[derive(Default)]
pub struct Progression {
    pub slots: Vec<Slot>,
    pub clipboard: Option<ProgressionEntry>,
}

impl Progression {
    pub fn new() -> Self {
        Progression::default()
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Append a slot to the end.
    pub fn append(&mut self, slot: Slot) -> usize {
        self.slots.push(slot);
        self.slots.len() - 1
    }

    /// Insert a slot at a specific index. If index >= len, appends.
    pub fn insert_at(&mut self, index: usize, slot: Slot) -> usize {
        let idx = index.min(self.slots.len());
        self.slots.insert(idx, slot);
        idx
    }

    /// Delete the slot at `index`. Returns true if anything was removed.
    pub fn delete(&mut self, index: usize) -> bool {
        if index < self.slots.len() {
            self.slots.remove(index);
            true
        } else {
            false
        }
    }

    /// Delete all slots.
    pub fn delete_all(&mut self) {
        self.slots.clear();
    }

    /// Copy the entry at `index` into the clipboard.
    pub fn copy(&mut self, index: usize) -> bool {
        match self.slots.get(index) {
            Some(Slot::Chord(e)) => {
                self.clipboard = Some(e.clone());
                true
            }
            _ => false,
        }
    }

    /// Paste the clipboard entry after `index`. If `index` is None, append.
    /// Returns true if anything was pasted.
    pub fn paste_after(&mut self, index: Option<usize>) -> bool {
        let Some(entry) = self.clipboard.clone() else {
            return false;
        };
        match index {
            Some(i) => {
                self.insert_at(i + 1, Slot::Chord(entry));
                true
            }
            None => {
                self.append(Slot::Chord(entry));
                true
            }
        }
    }

    /// Move the slot at `index` up one position. Returns true if moved.
    pub fn move_up(&mut self, index: usize) -> bool {
        if index == 0 || index >= self.slots.len() {
            return false;
        }
        self.slots.swap(index - 1, index);
        true
    }

    /// Move the slot at `index` down one position. Returns true if moved.
    pub fn move_down(&mut self, index: usize) -> bool {
        if index + 1 >= self.slots.len() {
            return false;
        }
        self.slots.swap(index, index + 1);
        true
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::Scale;

    fn entry(degree: ScaleDegree, t: Option<Transformation>) -> ProgressionEntry {
        ProgressionEntry {
            degree,
            transformation: t,
            registers: Registers::default(),
        }
    }

    fn c_major() -> Key {
        Key::new(60, Scale::Major)
    }

    #[test]
    fn new_progression_is_empty() {
        let p = Progression::new();
        assert!(p.is_empty());
        assert_eq!(p.len(), 0);
    }

    #[test]
    fn append_grows_progression() {
        let mut p = Progression::new();
        p.append(Slot::Chord(entry(ScaleDegree::I, None)));
        assert_eq!(p.len(), 1);
        p.append(Slot::Chord(entry(ScaleDegree::V, None)));
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn insert_at_before_end() {
        let mut p = Progression::new();
        p.append(Slot::Chord(entry(ScaleDegree::I, None)));
        p.append(Slot::Chord(entry(ScaleDegree::V, None)));
        p.insert_at(1, Slot::Chord(entry(ScaleDegree::IV, None)));
        assert_eq!(p.len(), 3);
        match &p.slots[1] {
            Slot::Chord(e) => assert_eq!(e.degree, ScaleDegree::IV),
            _ => panic!(),
        }
    }

    #[test]
    fn insert_at_past_end_appends() {
        let mut p = Progression::new();
        p.append(Slot::Chord(entry(ScaleDegree::I, None)));
        p.insert_at(99, Slot::Chord(entry(ScaleDegree::V, None)));
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn delete_removes_slot() {
        let mut p = Progression::new();
        p.append(Slot::Chord(entry(ScaleDegree::I, None)));
        p.append(Slot::Chord(entry(ScaleDegree::II, None)));
        p.append(Slot::Chord(entry(ScaleDegree::V, None)));
        assert!(p.delete(1));
        assert_eq!(p.len(), 2);
        match &p.slots[1] {
            Slot::Chord(e) => assert_eq!(e.degree, ScaleDegree::V),
            _ => panic!(),
        }
    }

    #[test]
    fn delete_out_of_range_is_noop() {
        let mut p = Progression::new();
        p.append(Slot::Chord(entry(ScaleDegree::I, None)));
        assert!(!p.delete(5));
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn delete_all_clears_progression() {
        let mut p = Progression::new();
        for d in [ScaleDegree::I, ScaleDegree::V, ScaleDegree::VI] {
            p.append(Slot::Chord(entry(d, None)));
        }
        p.delete_all();
        assert!(p.is_empty());
    }

    #[test]
    fn copy_and_paste() {
        let mut p = Progression::new();
        p.append(Slot::Chord(entry(ScaleDegree::I, None)));
        p.append(Slot::Chord(entry(ScaleDegree::V, Some(Transformation::Dom7))));
        assert!(p.copy(1));
        assert!(p.paste_after(Some(0)));
        assert_eq!(p.len(), 3);
        match &p.slots[1] {
            Slot::Chord(e) => {
                assert_eq!(e.degree, ScaleDegree::V);
                assert_eq!(e.transformation, Some(Transformation::Dom7));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn paste_without_copy_is_noop() {
        let mut p = Progression::new();
        p.append(Slot::Chord(entry(ScaleDegree::I, None)));
        assert!(!p.paste_after(Some(0)));
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn paste_at_end_when_index_none() {
        let mut p = Progression::new();
        p.append(Slot::Chord(entry(ScaleDegree::I, None)));
        p.copy(0);
        assert!(p.paste_after(None));
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn copy_of_rest_does_nothing() {
        let mut p = Progression::new();
        p.append(Slot::Rest);
        assert!(!p.copy(0));
    }

    #[test]
    fn move_up_and_down() {
        let mut p = Progression::new();
        p.append(Slot::Chord(entry(ScaleDegree::I, None)));
        p.append(Slot::Chord(entry(ScaleDegree::II, None)));
        p.append(Slot::Chord(entry(ScaleDegree::V, None)));
        assert!(p.move_down(0));
        match &p.slots[0] {
            Slot::Chord(e) => assert_eq!(e.degree, ScaleDegree::II),
            _ => panic!(),
        }
        assert!(p.move_up(1));
        match &p.slots[0] {
            Slot::Chord(e) => assert_eq!(e.degree, ScaleDegree::I),
            _ => panic!(),
        }
    }

    #[test]
    fn move_at_boundaries_is_noop() {
        let mut p = Progression::new();
        p.append(Slot::Chord(entry(ScaleDegree::I, None)));
        p.append(Slot::Chord(entry(ScaleDegree::II, None)));
        assert!(!p.move_up(0));
        assert!(!p.move_down(1));
    }

    #[test]
    fn entry_notes_for_plain_triad() {
        let k = c_major();
        let e = entry(ScaleDegree::I, None);
        assert_eq!(e.notes(&k), vec![60, 64, 67]);
    }

    #[test]
    fn entry_notes_with_transformation() {
        let k = c_major();
        let e = entry(ScaleDegree::V, Some(Transformation::Dom7));
        assert_eq!(e.notes(&k), vec![67, 71, 74, 77]);
    }

    #[test]
    fn entry_label_for_plain_triad() {
        let k = c_major();
        let e = entry(ScaleDegree::II, None);
        assert_eq!(e.label(&k), "Dm");
    }

    #[test]
    fn entry_label_with_transformation() {
        let k = c_major();
        let e = entry(ScaleDegree::I, Some(Transformation::Diatonic7));
        assert_eq!(e.label(&k), "Cmaj7");
    }

    #[test]
    fn rest_slot_has_no_notes() {
        let k = c_major();
        assert!(Slot::Rest.notes(&k).is_none());
        assert_eq!(Slot::Rest.label(&k), "—");
    }
}
