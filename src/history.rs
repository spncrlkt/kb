//! What has been played this run, and how often.
//!
//! Two views of one log, which is the whole point of keeping it:
//!
//! - the **exact history**, every play in order and duplicates included, so a
//!   session reads back as the sequence of chords it was; and
//! - the **top list**, the same plays deduped by chord and ranked by how often it
//!   came up, which is the view that answers "what am I actually working on".
//!
//! Both are derived from one `Vec` of plays. The counts are not recomputed when
//! they are read — [`History::record`] keeps the ranking in step — so a panel can
//! draw either view every frame for nothing.
//!
//! # A chord, not a performance
//!
//! [`Chord`] is the *identity*: a scale degree and an optional transformation,
//! which is exactly what the progression stores and exactly what "played six
//! times" counts. A [`Play`] adds the register gesture that produced it, so a
//! history row can be put back in the hands or sounded again — the same two
//! things a progression row can do.
//!
//! # The counts outlive the log
//!
//! [`LOG_LIMIT`] caps how many plays are kept, because a session can run for
//! hours and a log that grows for ever is a leak with a nice name. The *tallies*
//! are not capped: they are what the run actually played, which is what the top
//! list is for, and it would be strange for the ranking to forget a chord merely
//! because the last hour of the log had scrolled away.

use crate::music::{
    chord_label, diatonic_triad_label, ChordSpec, Key, ScaleDegree, Transformation,
};
use crate::progression::Registers;

/// How many plays the exact history keeps.
///
/// Generous — an hour of steady playing is a few hundred — and only reached by
/// leaving the app running all day. Past it the oldest plays fall off the log
/// while their counts stay.
pub const LOG_LIMIT: usize = 5000;

/// A chord's identity: what it is, without the performance that produced it.
///
/// The same pair the progression stores, so "the chords in the progression" is a
/// comparison of these and nothing else.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Chord {
    pub degree: ScaleDegree,
    pub transformation: Option<Transformation>,
}

impl Chord {
    pub fn new(degree: ScaleDegree, transformation: Option<Transformation>) -> Self {
        Chord {
            degree,
            transformation,
        }
    }

    /// The chord's name, written the way the progression panel writes it.
    pub fn label(&self, key: &Key) -> String {
        match self.transformation {
            Some(t) => chord_label(key, &ChordSpec::new(self.degree, t)),
            None => diatonic_triad_label(key, self.degree),
        }
    }

    /// The chord's notes, voiced in the track key.
    pub fn notes(&self, key: &Key) -> Vec<u8> {
        match self.transformation {
            Some(t) => ChordSpec::new(self.degree, t).voice(key),
            None => crate::music::diatonic_triad(key, self.degree),
        }
    }

    /// The degree on its own, for a row that has no name column.
    pub fn degree_label(&self) -> String {
        format!("{:?}", self.degree)
    }
}

/// One play: the chord, and the register gesture that produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct Play {
    pub chord: Chord,
    pub registers: Registers,
}

impl Play {
    pub fn new(chord: Chord, registers: Registers) -> Self {
        Play { chord, registers }
    }

    /// The notes to sound for this play, in the track key.
    pub fn notes(&self, key: &Key) -> Vec<u8> {
        self.chord.notes(key)
    }
}

/// A chord and how many times it has been played.
#[derive(Clone, Debug, PartialEq)]
pub struct Tally {
    /// The chord, at the registers it was played at most recently.
    pub play: Play,
    pub count: usize,
}

/// The run's log.
#[derive(Clone, Debug, Default)]
pub struct History {
    /// Every play, oldest first. Duplicates are the point.
    plays: Vec<Play>,
    /// The same plays, deduped by chord and ranked.
    top: Vec<Tally>,
    /// Monotonic, so recency survives the log being trimmed.
    sequence: u64,
    /// Each tally's most recent `sequence`, kept beside it for the sort.
    recency: Vec<u64>,
}

impl History {
    /// Write a play down.
    pub fn record(&mut self, play: Play) {
        self.sequence += 1;
        let sequence = self.sequence;
        self.plays.push(play.clone());
        if self.plays.len() > LOG_LIMIT {
            // The oldest falls off the *log*; its count stays in the tally,
            // because the tally is what the run played rather than what is still
            // on screen.
            let excess = self.plays.len() - LOG_LIMIT;
            self.plays.drain(0..excess);
        }

        match self.top.iter_mut().position(|t| t.play.chord == play.chord) {
            Some(index) => {
                let tally = &mut self.top[index];
                tally.count += 1;
                // The newest registers win: a top row sent back to the hands
                // should be the way it was last played.
                tally.play = play;
                self.recency[index] = sequence;
            }
            None => {
                self.top.push(Tally { play, count: 1 });
                self.recency.push(sequence);
            }
        }
        self.sort();
    }

    /// Rank the top list: most played first, most recent breaking the tie.
    ///
    /// The tie-break is what makes the list feel ordered rather than arbitrary —
    /// two chords you have played once each should read newest first, and the
    /// list should not reshuffle under the cursor for no reason.
    fn sort(&mut self) {
        let mut order: Vec<usize> = (0..self.top.len()).collect();
        order.sort_by(|a, b| {
            self.top[*b]
                .count
                .cmp(&self.top[*a].count)
                .then(self.recency[*b].cmp(&self.recency[*a]))
        });
        self.top = order.iter().map(|i| self.top[*i].clone()).collect();
        self.recency = order.iter().map(|i| self.recency[*i]).collect();
    }

    /// Every play, oldest first.
    pub fn plays(&self) -> &[Play] {
        &self.plays
    }

    /// The chords, deduped and ranked.
    pub fn top(&self) -> &[Tally] {
        &self.top
    }

    /// How many plays have been logged.
    pub fn len(&self) -> usize {
        self.plays.len()
    }

    /// How many distinct chords have been played.
    pub fn chords(&self) -> usize {
        self.top.len()
    }

    /// Whether anything has been played yet.
    pub fn is_empty(&self) -> bool {
        self.plays.is_empty()
    }

    /// Forget everything.
    #[cfg(test)]
    pub fn clear(&mut self) {
        self.plays.clear();
        self.top.clear();
        self.recency.clear();
        self.sequence = 0;
    }

    /// The `index`-th play from the end, `0` being the most recent.
    #[cfg(test)]
    pub fn recent(&self, index: usize) -> Option<&Play> {
        self.plays.iter().rev().nth(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::ScaleDegree::{I, IV, V, VI};

    fn chord(degree: ScaleDegree) -> Chord {
        Chord::new(degree, None)
    }

    fn play(degree: ScaleDegree) -> Play {
        Play::new(chord(degree), Registers::default())
    }

    #[test]
    fn the_exact_history_keeps_every_play_in_order() {
        let mut h = History::default();
        for degree in [I, V, I, IV, I] {
            h.record(play(degree));
        }
        let degrees: Vec<ScaleDegree> = h.plays().iter().map(|p| p.chord.degree).collect();
        assert_eq!(degrees, vec![I, V, I, IV, I], "duplicates and order both");
        assert_eq!(h.len(), 5);
    }

    #[test]
    fn the_top_list_dedupes_and_ranks_by_how_often() {
        let mut h = History::default();
        for degree in [I, V, I, IV, I, V] {
            h.record(play(degree));
        }
        let top: Vec<(ScaleDegree, usize)> = h
            .top()
            .iter()
            .map(|t| (t.play.chord.degree, t.count))
            .collect();
        assert_eq!(top, vec![(I, 3), (V, 2), (IV, 1)]);
        assert_eq!(h.chords(), 3);
        // The counts add up to the log, which is the invariant the two views
        // have to share.
        assert_eq!(h.top().iter().map(|t| t.count).sum::<usize>(), h.len());
    }

    #[test]
    fn a_tie_is_broken_by_which_was_played_most_recently() {
        // Two chords played once each: the newer one leads, and the order does
        // not shuffle for no reason when a third arrives.
        let mut h = History::default();
        h.record(play(I));
        h.record(play(V));
        assert_eq!(h.top()[0].play.chord.degree, V);
        h.record(play(IV));
        assert_eq!(
            h.top()
                .iter()
                .map(|t| t.play.chord.degree)
                .collect::<Vec<_>>(),
            vec![IV, V, I]
        );

        // And re-playing the oldest of the three does not demote it.
        h.record(play(IV));
        assert_eq!(h.top()[0].play.chord.degree, IV);
        assert_eq!(h.top()[0].count, 2);
    }

    #[test]
    fn a_tally_keeps_the_registers_it_was_last_played_at() {
        // So a top row sent back to the hands is the way it was last played.
        let mut h = History::default();
        let mut first = play(I);
        first.registers.left = Some(Default::default());
        h.record(first.clone());
        let mut second = play(I);
        second.registers.right = Some(Default::default());
        h.record(second.clone());

        assert_eq!(h.top().len(), 1);
        assert_eq!(h.top()[0].count, 2);
        assert_eq!(h.top()[0].play.registers, second.registers);
        // The log still has both, so the exact view is unchanged by the dedup.
        assert_eq!(h.plays()[0].registers, first.registers);
    }

    #[test]
    fn a_transformation_makes_a_different_chord() {
        let mut h = History::default();
        h.record(play(I));
        h.record(Play::new(
            Chord::new(I, Some(Transformation::Dom7)),
            Registers::default(),
        ));
        assert_eq!(h.chords(), 2, "a triad and its seventh are two chords");
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn an_empty_log_is_empty_in_both_views() {
        let h = History::default();
        assert!(h.is_empty());
        assert_eq!(h.len(), 0);
        assert_eq!(h.chords(), 0);
        assert!(h.plays().is_empty() && h.top().is_empty());
        assert!(h.recent(0).is_none());
    }

    #[test]
    fn clearing_forgets_the_counts_as_well_as_the_log() {
        let mut h = History::default();
        h.record(play(I));
        h.record(play(V));
        h.clear();
        assert!(h.is_empty() && h.top().is_empty());
        // And recording again starts from one, not from where it left off.
        h.record(play(I));
        assert_eq!(h.top()[0].count, 1);
    }

    #[test]
    fn the_most_recent_play_is_the_first_from_the_end() {
        let mut h = History::default();
        h.record(play(I));
        h.record(play(V));
        assert_eq!(h.recent(0).unwrap().chord.degree, V);
        assert_eq!(h.recent(1).unwrap().chord.degree, I);
        assert!(h.recent(2).is_none());
    }

    #[test]
    fn a_long_run_keeps_its_counts_while_the_log_is_capped() {
        // The tail of a very long session: the log is bounded, the ranking is
        // not, because the ranking is what the run played.
        let mut h = History::default();
        for i in 0..(LOG_LIMIT + 50) {
            h.record(play(if i % 2 == 0 { I } else { V }));
        }
        assert_eq!(h.len(), LOG_LIMIT, "the log is capped");
        assert_eq!(h.chords(), 2);
        assert_eq!(h.top()[0].count + h.top()[1].count, LOG_LIMIT + 50);
    }

    #[test]
    fn a_chord_is_named_the_way_the_progression_names_it() {
        let key = Key::new(60, crate::music::Scale::Major);
        assert_eq!(chord(I).label(&key), "C");
        assert_eq!(chord(VI).label(&key), "Am");
        assert_eq!(Chord::new(I, Some(Transformation::Dom7)).label(&key), "C7");
        assert_eq!(chord(I).degree_label(), "I");
    }

    #[test]
    fn a_play_sounds_the_chord_the_key_says() {
        let key = Key::new(60, crate::music::Scale::Major);
        assert_eq!(play(I).notes(&key), vec![60, 64, 67]);
        assert_eq!(chord(V).notes(&key), vec![67, 71, 74]);
    }
}
