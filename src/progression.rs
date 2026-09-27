//! Progression data model: entries, clipboard, and edit operations.

use crate::keyboard::PositionSet;
use crate::music::{
    chord_label, diatonic_triad, diatonic_triad_label, ChordSpec, Key, ScaleDegree,
    Transformation, BAR_TICKS,
};
use crate::rhythm::RhythmPattern;

/// The furthest a chord may be moved off its own downbeat.
///
/// A whole note equals one 4/4 bar, so this is exactly "up to a whole note
/// before or after the beginning of the measure". It is a hard bound rather than
/// a convention: `set_offset` clamps to it, and `arrangement` relies on the
/// value being sane when it wraps events around the loop.
pub const MAX_OFFSET_TICKS: i32 = BAR_TICKS as i32;

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

    /// Latch both hands at once.
    ///
    /// The one-key version of the two register locks: hold a two-handed chord,
    /// press once, and both hands are free. Held over an empty set it captures
    /// `Some(empty)` for both sides — the same "explicitly cleared" state the
    /// per-side locks produce — so pressing it twice in a row clears the
    /// registers rather than leaving them at `None`.
    pub fn lock_both(&mut self, held: &PositionSet) {
        self.lock_right(held);
        self.lock_left(held);
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
}

// -----------------------------------------------------------------------------
// Entries
// -----------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct ProgressionEntry {
    pub degree: ScaleDegree,
    pub transformation: Option<Transformation>,
    pub registers: Registers,
    /// The rhythm this entry *owns*, or `None` for the whole-bar default.
    ///
    /// An entry owns a **copy**, not a library name. Two chords may both have
    /// been given `Quarters`, but each holds its own instance, so editing the
    /// hits on one cannot reach the other — a shared name made that bug
    /// unavoidable. The library is a palette of starting points: assigning one
    /// clones it here, and `[Save Pattern As...]` copies an entry's rhythm back
    /// out under a new name.
    ///
    /// The cost is deliberate: a rhythm edited here lives in the *session*
    /// document, so it travels with an export rather than reaching
    /// `rhythms.toml` on its own.
    pub pattern: Option<RhythmPattern>,
    /// Signed ticks, clamped to `-MAX_OFFSET_TICKS..=MAX_OFFSET_TICKS`.
    ///
    /// Negative anticipates the chord across the bar line, positive delays it;
    /// events that spill past the loop wrap around.
    pub offset_ticks: i32,
}

impl ProgressionEntry {
    /// A chord with no pattern and no offset — the behaviour this tool had
    /// before rhythm patterns existed.
    pub fn new(degree: ScaleDegree, transformation: Option<Transformation>) -> Self {
        ProgressionEntry {
            degree,
            transformation,
            registers: Registers::default(),
            pattern: None,
            offset_ticks: 0,
        }
    }

    /// Clamp an offset into the playable range.
    pub fn clamp_offset(ticks: i32) -> i32 {
        ticks.clamp(-MAX_OFFSET_TICKS, MAX_OFFSET_TICKS)
    }

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

}

// -----------------------------------------------------------------------------
// Progression
// -----------------------------------------------------------------------------

/// How many edits deep undo goes before the oldest is discarded.
const HISTORY_LIMIT: usize = 128;

#[derive(Default)]
pub struct Progression {
    pub slots: Vec<Slot>,
    /// A run of slots waiting to be pasted — one entry is a single chord, several
    /// are a phrase. `None` means nothing has been copied.
    pub clipboard: Option<Vec<Slot>>,
    /// Snapshots of `slots` taken immediately before each change.
    undo_stack: Vec<Vec<Slot>>,
    /// Snapshots discarded by `undo`, available to `redo`.
    redo_stack: Vec<Vec<Slot>>,
}

/// A range the caller may have given either way round.
fn ordered(start: usize, end: usize) -> (usize, usize) {
    if start <= end {
        (start, end)
    } else {
        (end, start)
    }
}

impl Progression {
    pub fn new() -> Self {
        Progression::default()
    }

    /// Record the current state so the edit about to happen can be undone.
    ///
    /// Every mutating method calls this, and only once it knows the edit is
    /// real, so no-ops never pollute the history. Recording also discards the
    /// redo stack: history becomes a straight line again after a fresh edit.
    fn record(&mut self) {
        self.undo_stack.push(self.slots.clone());
        if self.undo_stack.len() > HISTORY_LIMIT {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Step back one edit. Returns true if the progression changed.
    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo_stack.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.slots, previous);
        self.redo_stack.push(current);
        true
    }

    /// Step forward one undone edit. Returns true if the progression changed.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo_stack.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.slots, next);
        self.undo_stack.push(current);
        true
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Append a slot to the end.
    pub fn append(&mut self, slot: Slot) -> usize {
        self.record();
        self.slots.push(slot);
        self.slots.len() - 1
    }

    /// Insert a slot at a specific index. If index >= len, appends.
    pub fn insert_at(&mut self, index: usize, slot: Slot) -> usize {
        self.record();
        let idx = index.min(self.slots.len());
        self.slots.insert(idx, slot);
        idx
    }

    /// Delete the slot at `index`. The one-row spelling of
    /// [`Self::delete_range`], for tests. Returns true if anything was removed.
    #[cfg(test)]
    pub fn delete(&mut self, index: usize) -> bool {
        if index < self.slots.len() {
            self.record();
            self.slots.remove(index);
            true
        } else {
            false
        }
    }

    /// Replace every slot in a single undoable edit. Returns true if changed.
    ///
    /// MIDI import installs a whole session at once; recording once means one
    /// undo returns to the previous progression instead of unwinding the
    /// import slot by slot.
    pub fn replace(&mut self, slots: Vec<Slot>) -> bool {
        if self.slots == slots {
            return false;
        }
        self.record();
        self.slots = slots;
        true
    }

    /// Copy the entry at `index` into the clipboard.
    ///
    /// The one-row spelling of [`Self::copy_range`], which is what the panel
    /// calls: every group action is the range form, so there is one code path to
    /// keep correct. This stays because a test reads better saying `copy(1)`.
    #[cfg(test)]
    pub fn copy(&mut self, index: usize) -> bool {
        self.copy_range(index, index)
    }

    /// Copy a run of slots — rests included — into the clipboard.
    ///
    /// The clipboard is a *list* rather than one entry because a selection is a
    /// phrase: copying four chords and pasting them somewhere else has to keep
    /// their order, and a rest in the middle is part of the figure. A one-row
    /// copy is just the one-element case, so there is no second clipboard.
    pub fn copy_range(&mut self, start: usize, end: usize) -> bool {
        let (start, end) = ordered(start, end);
        if start >= self.slots.len() {
            return false;
        }
        let end = end.min(self.slots.len() - 1);
        self.clipboard = Some(self.slots[start..=end].to_vec());
        true
    }

    /// Paste the clipboard at `index` — before the slot that is there now.
    ///
    /// Returns how many slots landed, so the caller can put the cursor on them.
    /// `index` at or past the end appends, which is how a paste at the end of the
    /// list and a paste into the gap above position 1 are the same call.
    pub fn paste_at(&mut self, index: usize) -> usize {
        let Some(entries) = self.clipboard.clone() else {
            return 0;
        };
        if entries.is_empty() {
            return 0;
        }
        self.record();
        let at = index.min(self.slots.len());
        let count = entries.len();
        for (offset, slot) in entries.into_iter().enumerate() {
            self.slots.insert(at + offset, slot);
        }
        count
    }

    /// Paste the clipboard after `index`. If `index` is None, append.
    ///
    /// The positional spelling of [`Self::paste_at`], for tests.
    #[cfg(test)]
    pub fn paste_after(&mut self, index: Option<usize>) -> usize {
        match index {
            Some(i) => self.paste_at(i + 1),
            None => self.paste_at(self.slots.len()),
        }
    }

    /// Delete a run of slots in one undoable edit.
    pub fn delete_range(&mut self, start: usize, end: usize) -> bool {
        let (start, end) = ordered(start, end);
        if start >= self.slots.len() {
            return false;
        }
        let end = end.min(self.slots.len() - 1);
        self.record();
        self.slots.drain(start..=end);
        true
    }

    /// Turn a run of slots around in place.
    pub fn reverse_range(&mut self, start: usize, end: usize) -> bool {
        let (start, end) = ordered(start, end);
        if start >= self.slots.len() {
            return false;
        }
        let end = end.min(self.slots.len() - 1);
        if start == end {
            return false;
        }
        self.record();
        self.slots[start..=end].reverse();
        true
    }

    /// Roll a run of slots along by `by` places, wrapping inside the run.
    ///
    /// `by = 1` moves every chord one place later and brings the last to the
    /// front, which is the turnaround a four-chord loop usually wants; `-1` goes
    /// the other way. The run's *length* is fixed, so rotating a selection never
    /// changes the progression's shape.
    pub fn rotate_range(&mut self, start: usize, end: usize, by: i32) -> bool {
        let (start, end) = ordered(start, end);
        if start >= self.slots.len() {
            return false;
        }
        let end = end.min(self.slots.len() - 1);
        let len = end - start + 1;
        if len < 2 {
            return false;
        }
        let by = by.rem_euclid(len as i32) as usize;
        if by == 0 {
            return false;
        }
        self.record();
        self.slots[start..=end].rotate_right(by);
        true
    }

    /// Take the rhythms and offsets off a run of slots, leaving the chords.
    ///
    /// "Clear sinko" as one edit: the chords keep their registers and their
    /// place in the loop, and go back to a plain whole-bar stab. A rest is
    /// already unstyled, so it is left alone.
    pub fn strip_rhythms(&mut self, start: usize, end: usize) -> bool {
        let (start, end) = ordered(start, end);
        if start >= self.slots.len() {
            return false;
        }
        let end = end.min(self.slots.len() - 1);
        let changes = self.slots[start..=end].iter().any(|slot| match slot {
            Slot::Chord(entry) => entry.pattern.is_some() || entry.offset_ticks != 0,
            Slot::Rest => false,
        });
        if !changes {
            return false;
        }
        self.record();
        for slot in &mut self.slots[start..=end] {
            if let Slot::Chord(entry) = slot {
                entry.pattern = None;
                entry.offset_ticks = 0;
            }
        }
        true
    }

    /// Lay a run of rhythms across a run of slots, in order, repeating.
    ///
    /// One entry per target slot is the phrase case; a single entry is "put this
    /// rhythm on all of them", which is why the clipboard is cycled rather than
    /// requiring a length match. An entry of `None` clears that slot's rhythm, so
    /// a copy of "no rhythm here" pastes as one.
    pub fn assign_patterns(
        &mut self,
        start: usize,
        end: usize,
        patterns: &[Option<RhythmPattern>],
    ) -> bool {
        let (start, end) = ordered(start, end);
        if patterns.is_empty() || start >= self.slots.len() {
            return false;
        }
        let end = end.min(self.slots.len() - 1);
        let changes = (start..=end).any(|index| match self.slots.get(index) {
            Some(Slot::Chord(entry)) => {
                entry.pattern != patterns[(index - start) % patterns.len()]
            }
            _ => false,
        });
        if !changes {
            return false;
        }
        self.record();
        for index in start..=end {
            let pattern = patterns[(index - start) % patterns.len()].clone();
            if let Some(Slot::Chord(entry)) = self.slots.get_mut(index) {
                entry.pattern = pattern;
            }
        }
        true
    }

    /// Give every slot in a run the chord in the registers, keeping each one's
    /// rhythm and offset.
    ///
    /// The range form of [`Self::replace_chord`], and one undoable edit for the
    /// same reason `replace` is: retargeting a phrase is one decision, not four.
    pub fn replace_chord_range(
        &mut self,
        start: usize,
        end: usize,
        degree: ScaleDegree,
        transformation: Option<Transformation>,
        registers: Registers,
    ) -> bool {
        let (start, end) = ordered(start, end);
        if start >= self.slots.len() {
            return false;
        }
        let end = end.min(self.slots.len() - 1);
        let changes = (start..=end).any(|index| match self.slots.get(index) {
            Some(Slot::Chord(entry)) => {
                entry.degree != degree
                    || entry.transformation != transformation
                    || entry.registers != registers
            }
            Some(Slot::Rest) => true,
            None => false,
        });
        if !changes {
            return false;
        }
        self.record();
        for index in start..=end {
            let rhythms = match self.slots.get(index) {
                Some(Slot::Chord(entry)) => (entry.pattern.clone(), entry.offset_ticks),
                _ => (None, 0),
            };
            self.slots[index] = Slot::Chord(ProgressionEntry {
                degree,
                transformation,
                registers: registers.clone(),
                pattern: rhythms.0,
                offset_ticks: rhythms.1,
            });
        }
        true
    }

    /// Give a slot a rhythm, or take its rhythm away.
    ///
    /// One undoable edit, and a no-op for a rest, an out-of-range index, or a
    /// value that is already set — so scrolling a pattern list past the current
    /// one never pollutes the history.
    ///
    /// The caller passes a *copy*: assigning a library pattern clones it, which
    /// is what keeps two chords from sharing one instance. Returns whether
    /// anything changed, so a caller can flash instead of recording an edit that
    /// would do nothing.
    pub fn assign_pattern(&mut self, index: usize, pattern: Option<RhythmPattern>) -> bool {
        let unchanged = match self.slots.get(index) {
            Some(Slot::Chord(entry)) => entry.pattern == pattern,
            _ => return false,
        };
        if unchanged {
            return false;
        }
        self.record();
        if let Some(Slot::Chord(entry)) = self.slots.get_mut(index) {
            entry.pattern = pattern;
        }
        true
    }

    /// Move a slot off its downbeat, clamping to the playable range.
    ///
    /// The caller decides the step: the UI nudges by the assigned pattern's grid
    /// so a nudge always lands on a cell. Clamping here means no caller can push
    /// a chord further than a whole note.
    pub fn set_offset(&mut self, index: usize, offset_ticks: i32) -> bool {
        let clamped = ProgressionEntry::clamp_offset(offset_ticks);
        let unchanged = match self.slots.get(index) {
            Some(Slot::Chord(entry)) => entry.offset_ticks == clamped,
            _ => return false,
        };
        if unchanged {
            return false;
        }
        self.record();
        if let Some(Slot::Chord(entry)) = self.slots.get_mut(index) {
            entry.offset_ticks = clamped;
        }
        true
    }

    /// Set a slot to a chord, keeping the rhythm it already had.
    ///
    /// The point of the operation: change what a slot *plays* without losing the
    /// pattern and offset assigned to it. Those belong to the entry, not to the
    /// chord, so a slot that stabs on the offbeat keeps stabbing on the offbeat
    /// when its chord is swapped out. Delete-and-insert would lose them, which is
    /// what this exists to avoid.
    ///
    /// A rest becomes a chord, since a rest has no rhythm to keep.
    ///
    /// Only the degree, transformation and register snapshot are taken from the
    /// caller, so there is no way to pass a pattern here by accident. One
    /// undoable edit; a no-op for an out-of-range index or an unchanged chord,
    /// which keeps scrolling-and-pressing out of the history.
    #[cfg(test)]
    pub fn replace_chord(
        &mut self,
        index: usize,
        degree: ScaleDegree,
        transformation: Option<Transformation>,
        registers: Registers,
    ) -> bool {
        let rhythm = match self.slots.get(index) {
            Some(Slot::Chord(entry)) => {
                if entry.degree == degree
                    && entry.transformation == transformation
                    && entry.registers == registers
                {
                    return false;
                }
                (entry.pattern.clone(), entry.offset_ticks)
            }
            Some(Slot::Rest) => (None, 0),
            None => return false,
        };

        self.record();
        self.slots[index] = Slot::Chord(ProgressionEntry {
            degree,
            transformation,
            registers,
            pattern: rhythm.0,
            offset_ticks: rhythm.1,
        });
        true
    }

}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::KeyPosition;
    use crate::music::Scale;

    fn entry(degree: ScaleDegree, t: Option<Transformation>) -> ProgressionEntry {
        ProgressionEntry::new(degree, t)
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
    fn deleting_the_whole_range_clears_the_progression() {
        // The model has one delete: a range. "Clear all" is `Cmd+A` then delete,
        // so there is no second method that could drift from it.
        let mut p = Progression::new();
        for d in [ScaleDegree::I, ScaleDegree::V, ScaleDegree::VI] {
            p.append(Slot::Chord(entry(d, None)));
        }
        assert!(p.delete_range(0, 2));
        assert!(p.is_empty());
    }

    #[test]
    fn copy_and_paste() {
        let mut p = Progression::new();
        p.append(Slot::Chord(entry(ScaleDegree::I, None)));
        p.append(Slot::Chord(entry(ScaleDegree::V, Some(Transformation::Dom7))));
        assert!(p.copy(1));
        assert_eq!(p.paste_after(Some(0)), 1);
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
        assert_eq!(p.paste_after(Some(0)), 0);
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn paste_at_end_when_index_none() {
        let mut p = Progression::new();
        p.append(Slot::Chord(entry(ScaleDegree::I, None)));
        p.copy(0);
        assert_eq!(p.paste_after(None), 1);
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn a_rest_copies_and_pastes_like_any_other_slot() {
        // A rest is part of a phrase's shape, so a range copy has to carry it —
        // otherwise pasting four bars would close the gap where the silence was.
        let mut p = Progression::new();
        p.append(Slot::Chord(entry(ScaleDegree::I, None)));
        p.append(Slot::Rest);
        assert!(p.copy_range(0, 1));
        assert_eq!(p.paste_at(2), 2);
        assert_eq!(p.len(), 4);
        assert!(matches!(p.slots[2], Slot::Chord(_)));
        assert!(matches!(p.slots[3], Slot::Rest));
    }

    #[test]
    fn reordering_a_pair_moves_one_past_the_other() {
        // Single-step movement is the two-slot rotate: the model has one way to
        // reorder, not two.
        let mut p = Progression::new();
        p.append(Slot::Chord(entry(ScaleDegree::I, None)));
        p.append(Slot::Chord(entry(ScaleDegree::II, None)));
        assert!(p.rotate_range(0, 1, 1));
        assert_eq!(
            degrees(&p),
            vec![Some(ScaleDegree::II), Some(ScaleDegree::I)]
        );
        assert!(p.rotate_range(0, 1, 1));
        assert_eq!(
            degrees(&p),
            vec![Some(ScaleDegree::I), Some(ScaleDegree::II)]
        );
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
    fn a_rest_is_labelled_with_a_dash() {
        // A rest has no voicing at all — `arrangement::a_rest_contributes_nothing`
        // pins that at the seam the scheduler and the exporter both read — so all
        // that is left here is the label the chord list shows.
        assert_eq!(Slot::Rest.label(&c_major()), "—");
    }

    // ---- undo / redo ----

    fn degrees(p: &Progression) -> Vec<Option<ScaleDegree>> {
        p.slots
            .iter()
            .map(|s| match s {
                Slot::Chord(e) => Some(e.degree),
                Slot::Rest => None,
            })
            .collect()
    }

    fn filled(degrees: &[ScaleDegree]) -> Progression {
        let mut p = Progression::new();
        for d in degrees {
            p.append(Slot::Chord(entry(*d, None)));
        }
        p
    }

    #[test]
    fn fresh_progression_has_no_history() {
        let p = Progression::new();
        assert!(!p.can_undo());
        assert!(!p.can_redo());
    }

    #[test]
    fn undo_reverses_an_append() {
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V]);
        assert!(p.can_undo());
        assert!(p.undo());
        assert_eq!(degrees(&p), vec![Some(ScaleDegree::I)]);
        assert!(p.can_redo());
    }

    #[test]
    fn redo_reapplies_an_undone_append() {
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V]);
        p.undo();
        assert!(p.redo());
        assert_eq!(degrees(&p), vec![Some(ScaleDegree::I), Some(ScaleDegree::V)]);
        assert!(!p.can_redo());
    }

    #[test]
    fn undo_unwinds_multiple_edits_in_order() {
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::IV, ScaleDegree::V]);
        assert!(p.undo());
        assert!(p.undo());
        assert_eq!(degrees(&p), vec![Some(ScaleDegree::I)]);
        assert!(p.undo());
        assert!(p.is_empty());
        assert!(!p.undo());
        assert!(!p.can_undo());
    }

    #[test]
    fn a_new_edit_discards_the_redo_stack() {
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V]);
        p.undo();
        assert!(p.can_redo());
        p.append(Slot::Chord(entry(ScaleDegree::II, None)));
        assert!(!p.can_redo());
        assert!(!p.redo());
        assert_eq!(degrees(&p), vec![Some(ScaleDegree::I), Some(ScaleDegree::II)]);
    }

    #[test]
    fn undo_reverses_delete_and_a_range_delete() {
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::IV, ScaleDegree::V]);
        assert!(p.delete(1));
        assert_eq!(
            degrees(&p),
            vec![Some(ScaleDegree::I), Some(ScaleDegree::V)]
        );
        assert!(p.delete_range(0, 1));
        assert!(p.is_empty());
        assert!(p.undo());
        assert_eq!(
            degrees(&p),
            vec![Some(ScaleDegree::I), Some(ScaleDegree::V)]
        );
        assert!(p.undo());
        assert_eq!(
            degrees(&p),
            vec![Some(ScaleDegree::I), Some(ScaleDegree::IV), Some(ScaleDegree::V)]
        );
    }

    #[test]
    fn undo_reverses_reorder_and_paste() {
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V]);
        assert!(p.rotate_range(0, 1, 1));
        assert_eq!(
            degrees(&p),
            vec![Some(ScaleDegree::V), Some(ScaleDegree::I)]
        );
        assert!(p.undo());
        assert_eq!(
            degrees(&p),
            vec![Some(ScaleDegree::I), Some(ScaleDegree::V)]
        );

        assert!(p.copy(0));
        assert_eq!(p.paste_after(Some(0)), 1);
        assert_eq!(
            degrees(&p),
            vec![Some(ScaleDegree::I), Some(ScaleDegree::I), Some(ScaleDegree::V)]
        );
        assert!(p.undo());
        assert_eq!(degrees(&p), vec![Some(ScaleDegree::I), Some(ScaleDegree::V)]);
    }

    #[test]
    fn no_op_edits_do_not_touch_history() {
        let mut p = filled(&[ScaleDegree::I]);
        // Out of range / boundary operations change nothing, so there must be
        // nothing extra to undo afterwards.
        assert!(!p.delete(9));
        assert!(!p.rotate_range(0, 0, 1));
        // Nothing on the clipboard, so nothing to paste.
        assert_eq!(p.paste_after(None), 0);
        // Only the original append is undoable, so one undo empties it.
        assert!(p.undo());
        assert!(p.is_empty());
        assert!(!p.undo());
    }

    #[test]
    fn deleting_an_empty_range_records_nothing() {
        let mut p = Progression::new();
        assert!(!p.delete_range(0, 0));
        assert!(!p.can_undo());
    }

    #[test]
    fn copy_alone_is_not_an_undoable_edit() {
        let mut p = filled(&[ScaleDegree::I]);
        assert!(p.copy(0));
        assert!(p.can_undo());
        // Undo removes the append that created the chord, not the copy.
        assert!(p.undo());
        assert!(p.is_empty());
        assert!(!p.undo());
    }

    #[test]
    fn history_is_bounded() {
        let mut p = Progression::new();
        for _ in 0..(HISTORY_LIMIT + 20) {
            p.append(Slot::Rest);
        }
        // Undo can step back the whole retained window, but no further.
        for _ in 0..HISTORY_LIMIT {
            assert!(p.undo());
        }
        assert!(!p.undo());
    }

    #[test]
    fn undo_restores_chords_with_their_transformations() {
        let mut p = Progression::new();
        p.append(Slot::Chord(entry(ScaleDegree::V, Some(Transformation::Dom7))));
        p.append(Slot::Chord(entry(ScaleDegree::I, None)));
        p.delete(0);
        p.undo();
        match &p.slots[0] {
            Slot::Chord(e) => {
                assert_eq!(e.degree, ScaleDegree::V);
                assert_eq!(e.transformation, Some(Transformation::Dom7));
            }
            other => panic!("expected the restored chord, got {:?}", other),
        }
    }

    // ---- replace (used by MIDI import) ----

    #[test]
    fn replace_swaps_every_slot_in_one_undo_step() {
        // Start with no history so the assertion is about `replace` alone.
        let mut p = Progression::new();
        p.slots = vec![Slot::Chord(entry(ScaleDegree::I, None))];

        assert!(p.replace(vec![Slot::Rest, Slot::Rest]));
        assert_eq!(p.len(), 2);

        // One undo returns the whole previous progression, rather than
        // unwinding the import slot by slot.
        assert!(p.undo());
        assert_eq!(degrees(&p), vec![Some(ScaleDegree::I)]);
        assert!(!p.undo());
    }

    #[test]
    fn replace_with_identical_slots_changes_nothing() {
        let mut p = filled(&[ScaleDegree::I]);
        let same = p.slots.clone();
        assert!(!p.replace(same));
        // Only the original append remains undoable.
        assert!(p.undo());
        assert!(p.is_empty());
    }

    #[test]
    fn replace_with_an_empty_list_clears_the_progression() {
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V]);
        assert!(p.replace(Vec::new()));
        assert!(p.is_empty());
        assert!(p.undo());
        assert_eq!(p.len(), 2);
    }

    // ---- register locks ----

    fn held(positions: &[KeyPosition]) -> PositionSet {
        positions.iter().copied().collect()
    }

    #[test]
    fn locking_both_captures_both_hands_from_one_gesture() {
        let mut registers = Registers::default();
        registers.lock_both(&held(&[
            KeyPosition::LeftIndex,
            KeyPosition::RightIndex,
            KeyPosition::RightMiddle,
        ]));

        assert_eq!(registers.left, Some(held(&[KeyPosition::LeftIndex])));
        assert_eq!(
            registers.right,
            Some(held(&[KeyPosition::RightIndex, KeyPosition::RightMiddle]))
        );
    }

    #[test]
    fn locking_both_over_nothing_clears_rather_than_forgets() {
        // `Some(empty)` is the explicitly-cleared state, so the gesture is a
        // toggle: press it twice and the registers are empty, not `None`. The
        // distinction survives a project round trip.
        let mut registers = Registers::default();
        registers.lock_both(&held(&[KeyPosition::LeftIndex]));
        registers.lock_both(&PositionSet::new());

        assert_eq!(registers.left, Some(PositionSet::new()));
        assert_eq!(registers.right, Some(PositionSet::new()));
        assert_eq!(registers.resolve(&PositionSet::new()), PositionSet::new());
    }

    #[test]
    fn a_second_lock_both_replaces_the_first() {
        let mut registers = Registers::default();
        registers.lock_both(&held(&[KeyPosition::LeftIndex]));
        registers.lock_both(&held(&[KeyPosition::LeftMiddle, KeyPosition::RightInner]));

        assert_eq!(registers.left, Some(held(&[KeyPosition::LeftMiddle])));
        assert_eq!(registers.right, Some(held(&[KeyPosition::RightInner])));
    }

    #[test]
    fn live_input_still_wins_per_side_after_a_both_hands_lock() {
        let mut registers = Registers::default();
        registers.lock_both(&held(&[KeyPosition::LeftIndex, KeyPosition::RightIndex]));

        // A live left-hand key overrides only the left side.
        let live = held(&[KeyPosition::LeftMiddle]);
        let resolved = registers.resolve(&live);
        assert!(resolved.contains(&KeyPosition::LeftMiddle));
        assert!(resolved.contains(&KeyPosition::RightIndex), "the right stays latched");
        assert!(!resolved.contains(&KeyPosition::LeftIndex));
    }

    // ---- replacing a slot's chord ----

    /// A throwaway rhythm, named so a test can tell two apart.
    fn pattern(name: &str) -> RhythmPattern {
        RhythmPattern::from_step_string(name, 0.5, "x---").unwrap()
    }

    fn pattern_and_offset(p: &Progression, i: usize) -> (Option<RhythmPattern>, i32) {
        match &p.slots[i] {
            Slot::Chord(e) => (e.pattern.clone(), e.offset_ticks),
            other => panic!("expected a chord, got {:?}", other),
        }
    }

    fn chord_of(p: &Progression, i: usize) -> (ScaleDegree, Option<Transformation>, Registers) {
        match &p.slots[i] {
            Slot::Chord(e) => (e.degree, e.transformation, e.registers.clone()),
            other => panic!("expected a chord, got {:?}", other),
        }
    }

    #[test]
    fn replacing_a_chord_keeps_its_rhythm() {
        // The whole point: swap what the slot plays, keep how it plays it.
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V]);
        p.assign_pattern(1, Some(pattern("Offbeat Eighths")));
        p.set_offset(1, -480);
        let before = pattern_and_offset(&p, 1);

        assert!(p.replace_chord(
            1,
            ScaleDegree::IV,
            Some(Transformation::Dom7),
            Registers::default()
        ));

        assert_eq!(
            chord_of(&p, 1),
            (
                ScaleDegree::IV,
                Some(Transformation::Dom7),
                Registers::default()
            )
        );
        assert_eq!(
            pattern_and_offset(&p, 1),
            before,
            "the pattern and offset must survive the swap"
        );
        assert_eq!(pattern_and_offset(&p, 0), (None, 0), "other slots untouched");
    }

    #[test]
    fn replacing_a_chord_keeps_the_register_snapshot_it_is_given() {
        let mut p = filled(&[ScaleDegree::I]);
        let registers = Registers {
            left: Some([crate::keyboard::KeyPosition::LeftMiddle].into()),
            right: Some(Default::default()),
        };
        assert!(p.replace_chord(0, ScaleDegree::V, None, registers.clone()));
        assert_eq!(chord_of(&p, 0).2, registers);
    }

    #[test]
    fn replacing_a_chord_is_undoable() {
        let mut p = Progression::new();
        p.slots = vec![Slot::Chord(entry(ScaleDegree::I, None))];
        p.slots[0] = Slot::Chord(ProgressionEntry {
            pattern: Some(pattern("Quarters")),
            offset_ticks: 240,
            ..entry(ScaleDegree::I, None)
        });

        assert!(p.replace_chord(0, ScaleDegree::VI, None, Registers::default()));
        assert_eq!(chord_of(&p, 0).0, ScaleDegree::VI);

        assert!(p.undo());
        assert_eq!(
            chord_of(&p, 0),
            (ScaleDegree::I, None, Registers::default()),
            "one undo restores the chord"
        );
        assert_eq!(
            pattern_and_offset(&p, 0),
            (Some(pattern("Quarters")), 240),
            "and the rhythm it had"
        );
    }

    #[test]
    fn replacing_with_the_same_chord_records_nothing() {
        let mut p = Progression::new();
        p.slots = vec![Slot::Chord(entry(ScaleDegree::I, None))];

        assert!(!p.replace_chord(0, ScaleDegree::I, None, Registers::default()));
        assert!(!p.can_undo(), "an unchanged chord is not an edit");
    }

    #[test]
    fn replacing_a_rest_makes_it_a_chord() {
        let mut p = Progression::new();
        p.slots = vec![Slot::Rest, Slot::Chord(entry(ScaleDegree::I, None))];

        assert!(p.replace_chord(0, ScaleDegree::V, None, Registers::default()));
        assert_eq!(chord_of(&p, 0).0, ScaleDegree::V);
        assert_eq!(
            pattern_and_offset(&p, 0),
            (None, 0),
            "a rest had no rhythm to keep"
        );
    }

    #[test]
    fn replacing_past_the_end_does_nothing() {
        let mut p = Progression::new();
        assert!(!p.replace_chord(0, ScaleDegree::I, None, Registers::default()));
        assert!(!p.can_undo());
    }

    // ---- rhythm assignment and offset ----

    fn pattern_of(p: &Progression, i: usize) -> Option<RhythmPattern> {
        match &p.slots[i] {
            Slot::Chord(e) => e.pattern.clone(),
            other => panic!("expected a chord, got {:?}", other),
        }
    }

    fn offset_of(p: &Progression, i: usize) -> i32 {
        match &p.slots[i] {
            Slot::Chord(e) => e.offset_ticks,
            other => panic!("expected a chord, got {:?}", other),
        }
    }

    #[test]
    fn a_new_entry_has_no_pattern_and_no_offset() {
        let e = ProgressionEntry::new(ScaleDegree::I, None);
        assert_eq!(e.pattern, None);
        assert_eq!(e.offset_ticks, 0);
    }

    #[test]
    fn assigning_a_pattern_is_undoable() {
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V]);
        assert!(p.assign_pattern(1, Some(pattern("Offbeat Eighths"))));
        assert_eq!(pattern_of(&p, 1), Some(pattern("Offbeat Eighths")));
        assert_eq!(pattern_of(&p, 0), None, "only the targeted slot changed");

        assert!(p.undo());
        assert_eq!(pattern_of(&p, 1), None);
        assert!(p.redo());
        assert_eq!(pattern_of(&p, 1), Some(pattern("Offbeat Eighths")));
    }

    #[test]
    fn clearing_a_pattern_is_undoable_too() {
        let mut p = filled(&[ScaleDegree::I]);
        p.assign_pattern(0, Some(pattern("Eighths")));
        assert!(p.assign_pattern(0, None));
        assert_eq!(pattern_of(&p, 0), None);
        assert!(p.undo());
        assert_eq!(pattern_of(&p, 0), Some(pattern("Eighths")));
    }

    #[test]
    fn assigning_to_a_rest_or_past_the_end_does_nothing() {
        let mut p = Progression::new();
        p.slots = vec![Slot::Rest];
        assert!(!p.assign_pattern(0, Some(pattern("Eighths"))));
        assert!(!p.assign_pattern(9, Some(pattern("Eighths"))));
        assert!(!p.can_undo(), "a refused assign must not touch history");
    }

    #[test]
    fn assigning_the_same_pattern_again_records_nothing() {
        let mut p = filled(&[ScaleDegree::I]);
        p.assign_pattern(0, Some(pattern("Eighths")));
        assert!(!p.assign_pattern(0, Some(pattern("Eighths"))));
        // Only the first assign and the original append are undoable.
        assert!(p.undo());
        assert_eq!(pattern_of(&p, 0), None);
        assert!(p.undo());
        assert!(p.is_empty());
        assert!(!p.undo());
    }

    #[test]
    fn setting_an_offset_is_undoable() {
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V]);
        assert!(p.set_offset(0, -240));
        assert_eq!(offset_of(&p, 0), -240);
        assert_eq!(offset_of(&p, 1), 0);
        assert!(p.undo());
        assert_eq!(offset_of(&p, 0), 0);
    }

    #[test]
    fn setting_an_offset_clamps_to_a_whole_note() {
        let mut p = filled(&[ScaleDegree::I]);
        assert!(p.set_offset(0, 99_999));
        assert_eq!(offset_of(&p, 0), MAX_OFFSET_TICKS);
        assert!(p.set_offset(0, -99_999));
        assert_eq!(offset_of(&p, 0), -MAX_OFFSET_TICKS);
    }

    #[test]
    fn the_offset_bound_is_exactly_one_whole_note() {
        assert_eq!(MAX_OFFSET_TICKS, 3840);
        assert_eq!(ProgressionEntry::clamp_offset(3841), 3840);
        assert_eq!(ProgressionEntry::clamp_offset(-3841), -3840);
        assert_eq!(ProgressionEntry::clamp_offset(120), 120);
    }

    #[test]
    fn setting_the_same_offset_again_records_nothing() {
        // Seed the slot directly so the history starts empty and the assertion
        // is about `set_offset` alone.
        let mut p = Progression::new();
        p.slots = vec![Slot::Chord(entry(ScaleDegree::I, None))];

        assert!(!p.set_offset(0, 0));
        assert!(!p.can_undo(), "an unchanged offset must not record");

        assert!(p.set_offset(0, MAX_OFFSET_TICKS));
        assert!(!p.set_offset(0, MAX_OFFSET_TICKS + 500), "clamped to the same");
        assert_eq!(p.undo_stack.len(), 1, "only the real move is undoable");
    }

    // ---- ranges ----

    #[test]
    fn a_range_copies_and_pastes_in_order() {
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V, ScaleDegree::VI]);
        assert!(p.copy_range(0, 1));
        assert_eq!(p.paste_at(3), 2, "two slots landed");
        assert_eq!(
            degrees(&p),
            vec![
                Some(ScaleDegree::I),
                Some(ScaleDegree::V),
                Some(ScaleDegree::VI),
                Some(ScaleDegree::I),
                Some(ScaleDegree::V),
            ],
            "the phrase keeps its order"
        );
    }

    #[test]
    fn pasting_at_zero_is_how_a_chord_reaches_position_one() {
        // The gap above the first chord: `paste_after` cannot reach it, which is
        // what `paste_at` exists for.
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V]);
        p.copy(1);
        assert_eq!(p.paste_at(0), 1);
        assert_eq!(
            degrees(&p),
            vec![
                Some(ScaleDegree::V),
                Some(ScaleDegree::I),
                Some(ScaleDegree::V)
            ]
        );
    }

    #[test]
    fn pasting_a_range_is_one_undoable_edit() {
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V, ScaleDegree::VI]);
        p.copy_range(0, 2);
        p.paste_at(0);
        assert_eq!(p.len(), 6);
        assert!(p.undo());
        assert_eq!(p.len(), 3, "one undo takes the whole paste back");
    }

    #[test]
    fn deleting_a_range_takes_exactly_those_slots() {
        let mut p = filled(&[
            ScaleDegree::I,
            ScaleDegree::V,
            ScaleDegree::VI,
            ScaleDegree::IV,
        ]);
        assert!(p.delete_range(1, 2));
        assert_eq!(
            degrees(&p),
            vec![Some(ScaleDegree::I), Some(ScaleDegree::IV)]
        );
        assert!(p.undo());
        assert_eq!(p.len(), 4, "and it is one undoable edit");
    }

    #[test]
    fn reversing_a_run_leaves_its_ends_and_its_length_alone() {
        let mut p = filled(&[
            ScaleDegree::I,
            ScaleDegree::V,
            ScaleDegree::VI,
            ScaleDegree::IV,
        ]);
        assert!(p.reverse_range(1, 2));
        assert_eq!(
            degrees(&p),
            vec![
                Some(ScaleDegree::I),
                Some(ScaleDegree::VI),
                Some(ScaleDegree::V),
                Some(ScaleDegree::IV),
            ]
        );
        assert_eq!(p.len(), 4);
        assert!(!p.reverse_range(0, 0), "one chord has nothing to reverse");
    }

    #[test]
    fn rotating_a_run_wraps_inside_it() {
        let mut p = filled(&[
            ScaleDegree::I,
            ScaleDegree::V,
            ScaleDegree::VI,
            ScaleDegree::IV,
        ]);
        assert!(p.rotate_range(0, 3, 1));
        assert_eq!(
            degrees(&p),
            vec![
                Some(ScaleDegree::IV),
                Some(ScaleDegree::I),
                Some(ScaleDegree::V),
                Some(ScaleDegree::VI),
            ],
            "the last chord comes round to the front"
        );
        assert!(p.rotate_range(0, 3, -1));
        assert_eq!(degrees(&p)[0], Some(ScaleDegree::I), "and back");

        // The run's length never changes, whichever way it goes.
        assert!(p.rotate_range(1, 2, 1));
        assert_eq!(p.len(), 4);
        assert!(!p.rotate_range(2, 2, 1), "one chord cannot rotate");
    }

    #[test]
    fn clearing_rhythms_keeps_the_chords_and_the_offsets_go_too() {
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V]);
        p.assign_pattern(0, Some(pattern("Syncopated 16ths")));
        p.set_offset(1, 480);

        assert!(p.strip_rhythms(0, 1));
        assert_eq!(pattern_of(&p, 0), None);
        assert_eq!(offset_of(&p, 1), 0);
        assert_eq!(
            degrees(&p),
            vec![Some(ScaleDegree::I), Some(ScaleDegree::V)],
            "the chords are untouched"
        );

        // Nothing left to clear: the second call is a no-op, not an edit.
        assert!(!p.strip_rhythms(0, 1));
    }

    #[test]
    fn one_rhythm_lays_across_a_whole_selection() {
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V, ScaleDegree::VI]);
        let patterns = vec![Some(pattern("Quarters"))];
        assert!(p.assign_patterns(0, 2, &patterns));
        for i in 0..3 {
            assert_eq!(pattern_of(&p, i), Some(pattern("Quarters")), "at {}", i);
        }
    }

    #[test]
    fn a_phrase_of_rhythms_pastes_in_order_and_repeats() {
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V, ScaleDegree::VI]);
        let patterns = vec![Some(pattern("A")), Some(pattern("B"))];
        assert!(p.assign_patterns(0, 2, &patterns));
        assert_eq!(pattern_of(&p, 0), Some(pattern("A")));
        assert_eq!(pattern_of(&p, 1), Some(pattern("B")));
        assert_eq!(pattern_of(&p, 2), Some(pattern("A")), "and it repeats");
    }

    #[test]
    fn a_phrase_of_rhythms_can_clear_as_well_as_set() {
        // A copied chord with no rhythm pastes as "no rhythm", so a phrase's shape
        // survives the round trip.
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V]);
        p.assign_pattern(0, Some(pattern("A")));
        p.assign_pattern(1, Some(pattern("B")));
        let patterns = vec![None, Some(pattern("B"))];
        assert!(p.assign_patterns(0, 1, &patterns));
        assert_eq!(pattern_of(&p, 0), None);
        assert_eq!(pattern_of(&p, 1), Some(pattern("B")));
    }

    #[test]
    fn replacing_a_range_keeps_each_slots_own_rhythm() {
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V]);
        p.assign_pattern(1, Some(pattern("Quarters")));
        p.set_offset(1, 240);

        assert!(p.replace_chord_range(
            0,
            1,
            ScaleDegree::IV,
            Some(Transformation::Dom7),
            Registers::default()
        ));
        assert_eq!(degrees(&p), vec![Some(ScaleDegree::IV), Some(ScaleDegree::IV)]);
        assert_eq!(pattern_of(&p, 0), None, "the first had none");
        assert_eq!(
            pattern_of(&p, 1),
            Some(pattern("Quarters")),
            "the second kept its own"
        );
        assert_eq!(offset_of(&p, 1), 240);
    }

    #[test]
    fn copy_and_paste_carry_the_pattern_and_the_offset() {
        let mut p = filled(&[ScaleDegree::I, ScaleDegree::V]);
        p.assign_pattern(1, Some(pattern("Syncopated 16ths")));
        p.set_offset(1, 480);

        assert!(p.copy(1));
        assert_eq!(p.paste_after(Some(1)), 1);
        assert_eq!(p.len(), 3);
        assert_eq!(pattern_of(&p, 2), Some(pattern("Syncopated 16ths")));
        assert_eq!(offset_of(&p, 2), 480);
    }
}
