//! The arrangement seam: one timings plan that playback and export both read.
//!
//! Before rhythm patterns, the scheduler (`transport`) and the exporter
//! (`midi`) each decided independently what a bar contained. With sub-bar
//! onsets, holds that cross a bar line, and chords offset across the loop
//! boundary, two implementations would inevitably disagree — the README already
//! warned that offline export and live playback "cannot drift apart".
//!
//! So this module is the single source of truth. It is pure: no audio, no
//! terminal, no filesystem. [`arrangement`] lays the whole loop out as timed
//! [`Stab`]s; [`window`] slices that plan for one bar, which is what lets the
//! scheduler play a note whose release lands in the next bar; and
//! [`assign_groups`] decides which voice pool each stab uses.

use crate::music::{Key, BEATS_PER_BAR, BAR_TICKS};
use crate::progression::Slot;

// -----------------------------------------------------------------------------
// Stab
// -----------------------------------------------------------------------------

/// One chord sounding for a span, in absolute loop ticks.
#[derive(Clone, Debug, PartialEq)]
pub struct Stab {
    pub start: u64,
    pub duration: u64,
    /// The chord tones, exactly as the entry voices them — never the synth's
    /// own allocation, so the audio path's octave doubling stays sound design
    /// and does not leak into an exported file.
    pub notes: Vec<u8>,
    /// 0..1. One per take, so a decayed layer is a quieter stab.
    pub gain: f32,
}

impl Stab {
    /// The tick this stab is released.
    pub fn end(&self) -> u64 {
        self.start + self.duration
    }
}

// -----------------------------------------------------------------------------
// The whole loop
// -----------------------------------------------------------------------------

/// Lay a progression out as timed stabs, in start order.
///
/// A slot with no pattern assigned behaves exactly as it did before this feature
/// existed: one stab at its downbeat, holding for the transport's note length.
/// That is what keeps `midi::render_progression` byte-identical for existing
/// progressions.
///
/// `note_length` is the fraction of a bar a patternless chord sustains; a slot
/// with a pattern uses the pattern's own hold instead.
pub fn arrangement(slots: &[Slot], key: &Key, note_length: f32) -> Vec<Stab> {
    let length = slots.len() as u64 * BAR_TICKS;
    if length == 0 {
        return Vec::new();
    }

    // Clamp so a wild note length can never produce a zero-length note or one
    // that outlives its bar — the same rule `midi::render_progression` applies.
    let fallback = ((BAR_TICKS as f64) * (note_length.clamp(0.0, 1.0) as f64)).round() as u64;
    let fallback = fallback.clamp(1, BAR_TICKS);

    let mut plan = Vec::new();
    for (index, slot) in slots.iter().enumerate() {
        let Slot::Chord(entry) = slot else {
            continue;
        };
        let notes = entry.notes(key);
        if notes.is_empty() {
            continue;
        }
        let base = index as i64 * BAR_TICKS as i64 + entry.offset_ticks as i64;

        // A rhythm the entry owns plays exactly its hits — including none at
        // all, which is how a deliberately silent pattern stays silent. An entry
        // with no rhythm of its own falls back to the whole-bar default, the
        // behaviour this tool had before rhythm patterns existed.
        //
        // There is no library lookup and so no dangling name to report: the
        // entry carries its own pattern, which is what stops one chord's edit
        // from reaching another's.
        match entry.pattern.as_ref() {
            Some(pattern) => {
                let step_ticks = pattern.step_ticks() as i64;
                let hold = pattern.hold_ticks();
                // A muted tail is a *bar position*, so it lands wherever the
                // chord's offset puts it rather than where the slot's downbeat
                // is. `None` at zero mute, so a hold still crosses the bar line
                // when nothing is muted.
                let mute_at = pattern.mute_boundary();

                for (step, gain) in pattern.hits() {
                    let start = base + step as i64 * step_ticks;
                    let mut duration = hold;

                    if let Some(mute_at) = mute_at {
                        let bar_pos = start.rem_euclid(BAR_TICKS as i64) as u64;
                        if bar_pos >= mute_at {
                            // Begins inside the silent tail: it never sounds.
                            continue;
                        }
                        // Cut at this bar's mute boundary.
                        let boundary = start - bar_pos as i64 + mute_at as i64;
                        duration = duration.min((boundary - start).max(0) as u64);
                        if duration == 0 {
                            continue;
                        }
                    }

                    push_wrapped(&mut plan, start, duration, &notes, gain, length);
                }
            }
            None => push_wrapped(&mut plan, base, fallback, &notes, 1.0, length),
        }
    }

    // Stable, so stabs sharing a tick keep slot-then-step order.
    plan.sort_by_key(|s| s.start);
    plan
}

/// Place one stab, wrapping it around the loop end and splitting it if its hold
/// crosses the bar line.
///
/// Splitting is the honest representation: a MIDI note cannot wrap, and a
/// looped note does retrigger at the loop point, so one stab becomes a head at
/// the end of the loop plus a tail at the start. It also keeps every tick inside
/// `0..length`, which is what makes [`window`] and the SMF writer simple.
fn push_wrapped(
    plan: &mut Vec<Stab>,
    start: i64,
    duration: u64,
    notes: &[u8],
    gain: f32,
    length: u64,
) {
    let duration = duration.max(1).min(length);
    let start = start.rem_euclid(length as i64) as u64;
    let head = duration.min(length - start);
    plan.push(Stab {
        start,
        duration: head,
        notes: notes.to_vec(),
        gain,
    });
    let tail = duration - head;
    if tail > 0 {
        plan.push(Stab {
            start: 0,
            duration: tail,
            notes: notes.to_vec(),
            gain,
        });
    }
}

// -----------------------------------------------------------------------------
// One bar
// -----------------------------------------------------------------------------

/// How many stab groups the synth gives this feature.
///
/// A hit retriggers only its own group, so this is the deepest a stack of
/// overlapping takes can sound before something has to be cut. Four is the take
/// limit the UI works to as well.
pub const RHYTHM_LAYERS: usize = 4;

/// One scheduled action inside a bar, at a tick offset from the bar line.
#[derive(Clone, Debug, PartialEq)]
pub enum BarEvent {
    /// Start a chord on a voice group.
    On {
        at: u64,
        group: usize,
        notes: Vec<u8>,
        gain: f32,
    },
    /// Release a voice group.
    Off { at: u64, group: usize },
    /// One metronome tick, audible only while a rhythm is being recorded.
    Click { at: u64, strong: bool },
}

impl BarEvent {
    /// Tick offset from the bar line.
    pub fn at(&self) -> u64 {
        match self {
            BarEvent::On { at, .. } | BarEvent::Off { at, .. } | BarEvent::Click { at, .. } => *at,
        }
    }
}

/// Order events for playback: by time, and at the same tick a release before a
/// start, so a group reused on the same beat is not cut by its own release.
pub fn sort_events(events: &mut [BarEvent]) {
    events.sort_by_key(|e| {
        let rank = match e {
            BarEvent::Off { .. } => 0,
            BarEvent::On { .. } => 1,
            BarEvent::Click { .. } => 2,
        };
        (e.at(), rank)
    });
}

/// Everything the scheduler does in one bar, in due order, with `at` measured
/// from the bar line.
///
/// This is the single input the playback thread walks, and it is what makes a
/// hold that crosses the bar line work: the bar that *ends* a note carries its
/// release, which is the same place the exporter puts the note-off.
pub fn bar_events(plan: &[Stab], bar: usize, bar_ticks: u64, groups: usize) -> Vec<BarEvent> {
    let assigned = assign_groups(plan, groups);
    let start = bar as u64 * bar_ticks;
    let end = start + bar_ticks;

    let mut events = Vec::new();
    for (index, stab) in plan.iter().enumerate() {
        let group = assigned[index];
        // An onset is half-open on the start: a stab beginning exactly on a bar
        // line belongs to the next bar. A release is closed on the end, so a
        // chord that ends exactly at the loop boundary is still released.
        if stab.start >= start && stab.start < end {
            events.push(BarEvent::On {
                at: stab.start - start,
                group,
                notes: stab.notes.clone(),
                gain: stab.gain,
            });
        }
        if stab.end() > start && stab.end() <= end {
            events.push(BarEvent::Off {
                at: stab.end() - start,
                group,
            });
        }
    }
    sort_events(&mut events);
    events
}

/// The metronome for one bar: a click on every beat, the downbeat stronger.
pub fn metronome_events(bar_ticks: u64) -> Vec<BarEvent> {
    let beats = BEATS_PER_BAR.max(1);
    let step = bar_ticks / beats;
    (0..beats)
        .map(|beat| BarEvent::Click {
            at: beat * step,
            strong: beat == 0,
        })
        .collect()
}

// -----------------------------------------------------------------------------
// Voice groups
// -----------------------------------------------------------------------------

/// Assign each stab to a voice pool, reusing a pool as soon as it is free.
///
/// The synth has a fixed number of stab groups, and a hit retriggers only its
/// own group. A group that is already free at the stab's start is preferred, and
/// the lowest such index wins — so a plan that never overlaps always lands on
/// group 0 and today's single-stab playback is untouched. When every group is
/// still sounding, the one that freed longest ago is stolen, which cuts the
/// shortest possible tail instead of an arbitrary one.
pub fn assign_groups(plan: &[Stab], groups: usize) -> Vec<usize> {
    if groups == 0 {
        return vec![0; plan.len()];
    }
    let mut free_at = vec![0u64; groups];
    let mut out = Vec::with_capacity(plan.len());
    for stab in plan {
        let free_now = free_at.iter().position(|at| *at <= stab.start);
        let index = free_now.unwrap_or_else(|| {
            free_at
                .iter()
                .enumerate()
                .min_by_key(|(_, at)| **at)
                .map(|(i, _)| i)
                .unwrap_or(0)
        });
        free_at[index] = stab.end();
        out.push(index);
    }
    out
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::{Scale, ScaleDegree};
    use crate::progression::ProgressionEntry;
    use crate::rhythm::{RhythmLayer, RhythmPattern};

    fn c_major() -> Key {
        Key::new(60, Scale::Major)
    }

    /// A pattern this build ships, by name.
    ///
    /// Tests ask for the shipped patterns by name so they keep reading like the
    /// UI; the entry stores the returned copy, exactly as assigning one does.
    fn builtin(name: &str) -> RhythmPattern {
        crate::rhythm::builtin_patterns()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no built-in pattern named {:?}", name))
    }

    fn chord(degree: ScaleDegree, pattern: Option<RhythmPattern>, offset: i32) -> Slot {
        let mut entry = ProgressionEntry::new(degree, None);
        entry.pattern = pattern;
        entry.offset_ticks = offset;
        Slot::Chord(entry)
    }

    fn starts(plan: &[Stab]) -> Vec<u64> {
        plan.iter().map(|s| s.start).collect()
    }

    // ---- the default, patternless path ----

    #[test]
    fn a_slot_without_a_pattern_is_one_whole_bar_stab() {
        let slots = vec![chord(ScaleDegree::I, None, 0)];
        let plan = arrangement(&slots, &c_major(), 0.5);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].start, 0);
        assert_eq!(plan[0].duration, BAR_TICKS / 2);
        assert_eq!(plan[0].gain, 1.0);
        assert_eq!(plan[0].notes, vec![60, 64, 67]);
    }

    #[test]
    fn a_whole_note_length_fills_its_bar() {
        let slots = vec![chord(ScaleDegree::I, None, 0)];
        let plan = arrangement(&slots, &c_major(), 1.0);
        assert_eq!(plan[0].duration, BAR_TICKS);
    }

    #[test]
    fn a_wild_note_length_is_clamped_into_the_bar() {
        let slots = vec![chord(ScaleDegree::I, None, 0)];
        let long = arrangement(&slots, &c_major(), 4.0);
        assert_eq!(long[0].duration, BAR_TICKS);
        let short = arrangement(&slots, &c_major(), -1.0);
        assert_eq!(short[0].duration, 1);
    }

    #[test]
    fn slots_without_patterns_are_laid_out_one_bar_apart() {
        let slots = vec![
            chord(ScaleDegree::I, None, 0),
            chord(ScaleDegree::V, None, 0),
        ];
        let plan = arrangement(&slots, &c_major(), 0.5);
        assert_eq!(starts(&plan), vec![0, BAR_TICKS]);
    }

    #[test]
    fn a_rest_contributes_nothing() {
        let slots = vec![chord(ScaleDegree::I, None, 0), Slot::Rest];
        let plan = arrangement(&slots, &c_major(), 1.0);
        assert_eq!(starts(&plan), vec![0]);
    }

    #[test]
    fn an_empty_progression_has_an_empty_plan() {
        let plan = arrangement(&[], &c_major(), 1.0);
        assert!(plan.is_empty());
    }

    // ---- patterns ----

    #[test]
    fn a_pattern_produces_one_stab_per_hit() {
        let slots = vec![chord(ScaleDegree::I, Some(builtin("Offbeat Eighths")), 0)];
        let plan = arrangement(&slots, &c_major(), 1.0);
        // Eight steps, hits on 1, 3, 5 and 7, at 480 ticks each.
        assert_eq!(starts(&plan), vec![480, 1440, 2400, 3360]);
    }

    #[test]
    fn a_pattern_hold_comes_from_its_gate() {
        let slots = vec![chord(ScaleDegree::I, Some(builtin("Offbeat Eighths")), 0)];
        let plan = arrangement(&slots, &c_major(), 1.0);
        // 480-tick step at gate 0.5.
        assert!(plan.iter().all(|s| s.duration == 240));
    }

    #[test]
    fn every_layer_of_a_pattern_sounds_with_its_own_gain() {
        let mut pattern = RhythmPattern::from_step_string("Stack", 0.5, "x---").unwrap();
        pattern
            .layers
            .push(RhythmLayer::from_step_string(0.7, "x---").unwrap());

        let slots = vec![chord(ScaleDegree::I, Some(pattern), 0)];
        let plan = arrangement(&slots, &c_major(), 1.0);
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].gain, 1.0);
        assert_eq!(plan[1].gain, 0.7);
        assert_eq!(plan[0].notes, plan[1].notes);
    }

    #[test]
    fn a_silent_pattern_plays_nothing_rather_than_falling_back() {
        // A deliberately empty grid is a choice; it must not switch itself to
        // the whole-bar default.
        let silent = RhythmPattern::blank("Silent", 16).unwrap();
        let slots = vec![chord(ScaleDegree::I, Some(silent), 0)];
        assert!(arrangement(&slots, &c_major(), 1.0).is_empty());
    }

    #[test]
    fn two_chords_named_the_same_play_the_hits_they_own() {
        // The regression this model exists for: two chords both given
        // `Quarters`, with the hits edited on one of them. Because each entry
        // owns its copy, the edit cannot reach the other chord — with a shared
        // library name it did.
        let mut edited = builtin("Quarters");
        // Keep only the first hit, as the `hits` row does cell by cell.
        for cell in 1..edited.steps_per_bar() {
            for layer in edited.layers.iter_mut() {
                layer.steps[cell] = false;
            }
        }
        assert_ne!(edited, builtin("Quarters"), "the copy really was edited");

        let slots = vec![
            chord(ScaleDegree::I, Some(builtin("Quarters")), 0),
            chord(ScaleDegree::V, Some(edited), 0),
        ];
        let plan = arrangement(&slots, &c_major(), 1.0);

        let i_starts: Vec<u64> = plan
            .iter()
            .filter(|stab| stab.notes == vec![60, 64, 67])
            .map(|stab| stab.start)
            .collect();
        let v_starts: Vec<u64> = plan
            .iter()
            .filter(|stab| stab.notes == vec![67, 71, 74])
            .map(|stab| stab.start)
            .collect();
        assert_eq!(i_starts, vec![0, 960, 1920, 2880], "untouched: four hits");
        assert_eq!(v_starts, vec![BAR_TICKS], "edited: the rest are gone");
    }

    #[test]
    fn a_pattern_is_voiced_in_the_key_it_is_given() {
        let slots = vec![chord(ScaleDegree::I, Some(builtin("Quarters")), 0)];
        let plan = arrangement(&slots, &c_major(), 1.0);
        assert_eq!(plan[0].notes, vec![60, 64, 67]);
    }

    // ---- offset ----

    #[test]
    fn a_positive_offset_delays_the_chord() {
        // Half a bar, so the hold stays inside the bar and nothing wraps.
        let slots = vec![chord(ScaleDegree::I, None, 240)];
        let plan = arrangement(&slots, &c_major(), 0.5);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].start, 240);
        assert_eq!(plan[0].duration, BAR_TICKS / 2);
    }

    #[test]
    fn a_negative_offset_anticipates_into_the_previous_bar() {
        let slots = vec![
            chord(ScaleDegree::I, None, 0),
            chord(ScaleDegree::V, None, -240),
        ];
        let plan = arrangement(&slots, &c_major(), 1.0);
        assert_eq!(starts(&plan), vec![0, BAR_TICKS - 240]);
    }

    #[test]
    fn an_offset_on_the_first_slot_wraps_to_the_end_of_the_loop() {
        // A chord pulled a quarter note early on the first slot has nowhere to go
        // but the end of the loop — which is the point of wrapping.
        let slots = vec![
            chord(ScaleDegree::I, None, -960),
            chord(ScaleDegree::V, None, 0),
        ];
        let plan = arrangement(&slots, &c_major(), 0.25);
        assert_eq!(starts(&plan), vec![BAR_TICKS, 2 * BAR_TICKS - 960]);
        assert_eq!(
            plan[1].notes,
            vec![60, 64, 67],
            "the wrapped chord is still the first slot's"
        );
        assert_eq!(plan[1].duration, 960, "and it keeps its own length");
    }

    #[test]
    fn a_full_bar_offset_wraps_onto_its_own_downbeat() {
        let slots = vec![chord(ScaleDegree::I, None, -(BAR_TICKS as i32))];
        let plan = arrangement(&slots, &c_major(), 1.0);
        assert_eq!(starts(&plan), vec![0]);
    }

    #[test]
    fn offset_and_pattern_apply_together() {
        let slots = vec![chord(ScaleDegree::I, Some(builtin("Quarters")), 480)];
        let plan = arrangement(&slots, &c_major(), 1.0);
        // Four quarter notes on a 4-cell grid, all pushed half a beat late: each
        // cell is 960 ticks, so the four hits land 480 ticks after their cells.
        // The last one holds 768 ticks from 3360, which runs past the loop end
        // and wraps to a 288-tick tail at tick 0 — the plan is ordered by start.
        assert_eq!(starts(&plan), vec![0, 480, 1440, 2400, 3360]);
        assert_eq!(plan[0].duration, 288, "the wrapped tail");
        assert_eq!(plan[1].notes, vec![60, 64, 67]);
    }

    // ---- wrapping and splitting ----

    #[test]
    fn a_hold_that_crosses_the_loop_end_splits_into_a_head_and_a_tail() {
        // A whole-bar chord starting three quarter-notes in.
        let slots = vec![chord(ScaleDegree::I, None, 3 * 960)];
        let plan = arrangement(&slots, &c_major(), 1.0);
        assert_eq!(plan.len(), 2, "expected a head and a tail: {:?}", plan);

        // The plan is ordered by start, so the wrapped tail comes first.
        let tail = &plan[0];
        assert_eq!(tail.start, 0);
        assert_eq!(tail.duration, 2880);
        let head = &plan[1];
        assert_eq!(head.start, 2880);
        assert_eq!(head.duration, 960);
        assert_eq!(head.notes, tail.notes, "the split keeps the same voicing");
    }

    #[test]
    fn a_split_keeps_every_tick_inside_the_loop() {
        let slots = vec![chord(ScaleDegree::I, None, 3 * 960)];
        let plan = arrangement(&slots, &c_major(), 1.0);
        let length = BAR_TICKS;
        assert!(plan.iter().all(|s| s.end() <= length), "plan: {:?}", plan);
    }

    #[test]
    fn a_split_in_a_longer_loop_lands_at_the_loop_start() {
        let slots = vec![
            chord(ScaleDegree::I, None, 0),
            chord(ScaleDegree::V, None, 0),
            chord(ScaleDegree::VI, None, 2 * 960),
        ];
        let length = 3 * BAR_TICKS;
        let plan = arrangement(&slots, &c_major(), 1.0);

        // The third chord starts at 9600 and holds a whole bar, so 1920 ticks of
        // it wrap to the loop start.
        let tail = plan
            .iter()
            .find(|s| s.start == 0 && s.duration == 1920)
            .expect("the wrapped tail at the loop start");
        assert_eq!(tail.notes, vec![69, 72, 76], "vi in C major");
        assert!(plan.iter().all(|s| s.end() <= length), "plan: {:?}", plan);
    }

    #[test]
    fn the_plan_is_ordered_by_start() {
        let slots = vec![
            chord(ScaleDegree::I, None, 0),
            chord(ScaleDegree::V, None, -480),
            chord(ScaleDegree::VI, None, 240),
        ];
        let plan = arrangement(&slots, &c_major(), 0.25);
        let mut sorted = starts(&plan);
        sorted.sort_unstable();
        assert_eq!(starts(&plan), sorted);
    }

    // ---- bar events ----

    fn ons(events: &[BarEvent]) -> Vec<u64> {
        events
            .iter()
            .filter_map(|e| match e {
                BarEvent::On { at, .. } => Some(*at),
                _ => None,
            })
            .collect()
    }

    fn offs(events: &[BarEvent]) -> Vec<u64> {
        events
            .iter()
            .filter_map(|e| match e {
                BarEvent::Off { at, .. } => Some(*at),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_bar_carries_the_ons_and_offs_of_its_own_stabs() {
        let slots = vec![
            chord(ScaleDegree::I, None, 0),
            chord(ScaleDegree::V, None, 0),
        ];
        let plan = arrangement(&slots, &c_major(), 0.5);
        let bar0 = bar_events(&plan, 0, BAR_TICKS, RHYTHM_LAYERS);
        assert_eq!(ons(&bar0), vec![0]);
        assert_eq!(offs(&bar0), vec![BAR_TICKS / 2]);
    }

    #[test]
    fn a_hold_crossing_the_bar_line_releases_in_a_later_bar() {
        // A chord pushed half a bar late holds from bar 1 into bar 2, so its
        // note-on and its note-off land in different bars. A scheduler that only
        // looked at the current bar could never release it.
        let slots = vec![
            chord(ScaleDegree::I, None, 0),
            chord(ScaleDegree::V, None, 1920),
            Slot::Rest,
        ];
        let plan = arrangement(&slots, &c_major(), 1.0);

        let bar1 = bar_events(&plan, 1, BAR_TICKS, RHYTHM_LAYERS);
        assert_eq!(ons(&bar1), vec![1920], "the late chord starts here");
        assert!(offs(&bar1).is_empty());

        let bar2 = bar_events(&plan, 2, BAR_TICKS, RHYTHM_LAYERS);
        assert!(ons(&bar2).is_empty());
        assert_eq!(offs(&bar2), vec![1920], "and releases here");
    }

    #[test]
    fn a_whole_bar_chord_in_a_one_bar_loop_is_still_released() {
        // The loop-end hole: this chord ends exactly on the loop boundary, which
        // is also the last bar's end. If a release had to fall strictly inside a
        // bar, the note would never stop.
        let slots = vec![chord(ScaleDegree::I, None, 0)];
        let plan = arrangement(&slots, &c_major(), 1.0);
        let bar0 = bar_events(&plan, 0, BAR_TICKS, RHYTHM_LAYERS);
        assert_eq!(ons(&bar0), vec![0]);
        assert_eq!(offs(&bar0), vec![BAR_TICKS]);
    }

    #[test]
    fn a_stab_on_the_bar_line_belongs_to_the_next_bar() {
        let plan = vec![Stab {
            start: BAR_TICKS,
            duration: 10,
            notes: vec![60],
            gain: 1.0,
        }];
        assert!(ons(&bar_events(&plan, 0, BAR_TICKS, RHYTHM_LAYERS)).is_empty());
        assert_eq!(ons(&bar_events(&plan, 1, BAR_TICKS, RHYTHM_LAYERS)), vec![0]);
    }

    #[test]
    fn a_release_exactly_on_the_bar_line_closes_the_earlier_bar() {
        let plan = vec![Stab {
            start: 0,
            duration: BAR_TICKS,
            notes: vec![60],
            gain: 1.0,
        }];
        assert_eq!(offs(&bar_events(&plan, 0, BAR_TICKS, RHYTHM_LAYERS)), vec![BAR_TICKS]);
        assert!(offs(&bar_events(&plan, 1, BAR_TICKS, RHYTHM_LAYERS)).is_empty());
    }

    #[test]
    fn every_stab_of_a_loop_is_started_exactly_once() {
        let slots = vec![
            chord(ScaleDegree::I, Some(builtin("Offbeat Eighths")), 0),
            chord(ScaleDegree::V, Some(builtin("Quarters")), 0),
        ];
        let plan = arrangement(&slots, &c_major(), 1.0);
        let mut seen = 0;
        for bar in 0..slots.len() {
            seen += ons(&bar_events(&plan, bar, BAR_TICKS, RHYTHM_LAYERS)).len();
        }
        assert_eq!(seen, plan.len(), "every stab must be scheduled once");
    }

    #[test]
    fn every_stab_of_a_loop_is_released_exactly_once() {
        let slots = vec![
            chord(ScaleDegree::I, Some(builtin("Offbeat Eighths")), 0),
            chord(ScaleDegree::V, None, 1920),
            chord(ScaleDegree::VI, Some(builtin("Quarters")), -240),
        ];
        let plan = arrangement(&slots, &c_major(), 1.0);
        let mut released = 0;
        for bar in 0..slots.len() {
            released += offs(&bar_events(&plan, bar, BAR_TICKS, RHYTHM_LAYERS)).len();
        }
        assert_eq!(
            released,
            plan.len(),
            "a note that is never released would hang"
        );
    }

    #[test]
    fn bar_events_are_ordered_by_time() {
        let slots = vec![
            chord(ScaleDegree::I, Some(builtin("Offbeat Eighths")), 0),
            chord(ScaleDegree::V, None, 0),
        ];
        let plan = arrangement(&slots, &c_major(), 0.25);
        let events = bar_events(&plan, 0, BAR_TICKS, RHYTHM_LAYERS);
        let times: Vec<u64> = events.iter().map(|e| e.at()).collect();
        let mut sorted = times.clone();
        sorted.sort_unstable();
        assert_eq!(times, sorted, "events: {:?}", events);
    }

    #[test]
    fn a_release_sorts_before_a_start_at_the_same_tick() {
        // Otherwise a group reused on the same beat would be cut by the release
        // of the note it is replacing.
        let mut events = vec![
            BarEvent::On {
                at: 100,
                group: 0,
                notes: vec![60],
                gain: 1.0,
            },
            BarEvent::Off { at: 100, group: 0 },
        ];
        sort_events(&mut events);
        assert!(matches!(events[0], BarEvent::Off { .. }), "{:?}", events);
        assert!(matches!(events[1], BarEvent::On { .. }), "{:?}", events);
    }

    #[test]
    fn a_click_sorts_after_a_note_at_the_same_tick() {
        let mut events = vec![
            BarEvent::Click {
                at: 0,
                strong: true,
            },
            BarEvent::On {
                at: 0,
                group: 0,
                notes: vec![60],
                gain: 1.0,
            },
        ];
        sort_events(&mut events);
        assert!(matches!(events[0], BarEvent::On { .. }), "{:?}", events);
    }

    #[test]
    fn an_onset_carries_its_gain_and_notes() {
        let slots = vec![chord(ScaleDegree::I, Some(builtin("Offbeat Eighths")), 0)];
        let plan = arrangement(&slots, &c_major(), 1.0);
        let events = bar_events(&plan, 0, BAR_TICKS, RHYTHM_LAYERS);
        match &events[0] {
            BarEvent::On { notes, gain, .. } => {
                assert_eq!(notes, &vec![60, 64, 67]);
                assert_eq!(*gain, 1.0);
            }
            other => panic!("expected an onset, got {:?}", other),
        }
    }

    #[test]
    fn a_bar_with_no_events_is_empty() {
        let slots = vec![chord(ScaleDegree::I, None, 0), Slot::Rest];
        let plan = arrangement(&slots, &c_major(), 1.0);
        assert!(bar_events(&plan, 1, BAR_TICKS, RHYTHM_LAYERS).is_empty());
    }

    // ---- metronome ----

    #[test]
    fn the_metronome_clicks_on_every_beat_with_a_strong_downbeat() {
        let clicks = metronome_events(BAR_TICKS);
        assert_eq!(clicks.len(), 4);
        let times: Vec<u64> = clicks.iter().map(|e| e.at()).collect();
        assert_eq!(times, vec![0, 960, 1920, 2880]);
        let strong: Vec<bool> = clicks
            .iter()
            .map(|e| match e {
                BarEvent::Click { strong, .. } => *strong,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(strong, vec![true, false, false, false]);
    }

    // ---- the muted tail ----

    /// A one-bar pattern: `gate` cells of hold per hit, and a muted tail.
    fn muted(gate: f32, steps: &str, mute: u32) -> RhythmPattern {
        let mut pattern = RhythmPattern::from_step_string("Muted", gate, steps).unwrap();
        pattern.mute_ticks = mute;
        pattern
    }

    fn slot_with(pattern: RhythmPattern, offset: i32) -> Slot {
        chord(ScaleDegree::I, Some(pattern), offset)
    }

    #[test]
    fn a_hit_inside_the_muted_tail_never_sounds() {
        // Quarter notes, but the last quarter of the bar is muted — so the hit
        // on beat four is dropped and the other three survive.
        let slots = vec![slot_with(muted(1.0, "xxxx", 960), 0)];
        let plan = arrangement(&slots, &c_major(), 1.0);
        assert_eq!(starts(&plan), vec![0, 960, 1920]);
    }

    #[test]
    fn a_hold_ringing_into_the_muted_tail_is_cut_at_the_boundary() {
        // A whole-bar hold with the last eighth muted: it stops at 3360 rather
        // than ringing to the bar line.
        let plan = arrangement(&[slot_with(muted(4.0, "x---", 480), 0)], &c_major(), 1.0);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].start, 0);
        assert_eq!(plan[0].end(), BAR_TICKS - 480);
    }

    #[test]
    fn a_muted_tail_shorter_than_the_hold_still_cuts_it() {
        // Only the hit that runs into the tail is shortened; the mute is an
        // articulation, not a gap.
        let plan = arrangement(&[slot_with(muted(1.0, "xxxx", 240), 0)], &c_major(), 1.0);
        assert_eq!(starts(&plan), vec![0, 960, 1920, 2880]);
        assert_eq!(plan[0].duration, 960, "the first three hold a full cell");
        assert_eq!(plan[3].duration, 720, "the last is cut short");
    }

    #[test]
    fn the_mute_is_a_bar_position_not_a_slot_position() {
        // A whole-bar hold pushed half a bar late is cut at the boundary of the
        // bar it sounds in, not 960 ticks after its own start.
        let slots = vec![
            chord(ScaleDegree::I, None, 0),
            chord(ScaleDegree::V, Some(muted(4.0, "x---", 960)), 1920),
        ];
        let plan = arrangement(&slots, &c_major(), 1.0);

        let late = plan
            .iter()
            .find(|stab| stab.notes == vec![67, 71, 74])
            .expect("the offset chord");
        assert_eq!(late.start, BAR_TICKS + 1920, "half a bar into bar 2");
        assert_eq!(late.end(), BAR_TICKS + 2880, "bar 2's own mute boundary");
    }

    #[test]
    fn an_offset_can_push_a_hit_into_a_muted_tail() {
        // The muted window is per bar, so an offset that moves a hit past
        // 2880 into its bar silences it, wherever the slot's downbeat is.
        let slots = vec![
            chord(ScaleDegree::I, None, 0),
            chord(ScaleDegree::V, Some(muted(1.0, "xxxx", 960)), 480),
            Slot::Rest,
        ];
        let plan = arrangement(&slots, &c_major(), 1.0);

        let v_starts: Vec<u64> = plan
            .iter()
            .filter(|stab| stab.notes == vec![67, 71, 74])
            .map(|stab| stab.start)
            .collect();
        // Cells at 480, 1440, 2400 and 3360 inside bar 2; the last is inside the
        // muted tail and never sounds.
        assert_eq!(
            v_starts,
            vec![BAR_TICKS + 480, BAR_TICKS + 1440, BAR_TICKS + 2400]
        );
        assert!(plan.iter().all(|stab| stab.end() <= 3 * BAR_TICKS));
    }

    #[test]
    fn zero_mute_leaves_a_cross_bar_hold_alone() {
        // The regression the `Option` guards: a boundary sitting on the bar line
        // would silently trim every hold that crosses it.
        let slots = vec![chord(ScaleDegree::I, None, 3 * 960)];
        let plan = arrangement(&slots, &c_major(), 1.0);
        assert_eq!(plan.len(), 2, "head and wrapped tail, as before");
        assert_eq!(plan[0].start, 0, "the tail");
        assert_eq!(plan[1].start, 2880, "the head");
        assert_eq!(plan[1].duration, 960);
    }

    #[test]
    fn a_patternless_slot_has_no_mute() {
        // There is no pattern to carry one.
        let slots = vec![chord(ScaleDegree::I, None, 0)];
        let plan = arrangement(&slots, &c_major(), 1.0);
        assert_eq!(plan[0].duration, BAR_TICKS, "still a whole-bar chord");
    }

    // ---- groups ----

    #[test]
    fn a_sequential_plan_stays_on_one_group() {
        let plan = vec![
            Stab {
                start: 0,
                duration: 100,
                notes: vec![60],
                gain: 1.0,
            },
            Stab {
                start: 200,
                duration: 100,
                notes: vec![60],
                gain: 1.0,
            },
        ];
        assert_eq!(assign_groups(&plan, 4), vec![0, 0]);
    }

    #[test]
    fn overlapping_stabs_take_different_groups() {
        let plan = vec![
            Stab {
                start: 0,
                duration: 500,
                notes: vec![60],
                gain: 1.0,
            },
            Stab {
                start: 100,
                duration: 500,
                notes: vec![64],
                gain: 1.0,
            },
        ];
        assert_eq!(assign_groups(&plan, 4), vec![0, 1]);
    }

    #[test]
    fn a_group_is_reused_the_moment_it_is_free() {
        let plan = vec![
            Stab {
                start: 0,
                duration: 100,
                notes: vec![60],
                gain: 1.0,
            },
            Stab {
                start: 100,
                duration: 100,
                notes: vec![64],
                gain: 1.0,
            },
            Stab {
                start: 200,
                duration: 100,
                notes: vec![67],
                gain: 1.0,
            },
        ];
        assert_eq!(assign_groups(&plan, 2), vec![0, 0, 0]);
    }

    #[test]
    fn more_overlaps_than_groups_steals_the_soonest_free_one() {
        let plan = vec![
            Stab {
                start: 0,
                duration: 100,
                notes: vec![60],
                gain: 1.0,
            },
            Stab {
                start: 10,
                duration: 500,
                notes: vec![62],
                gain: 1.0,
            },
            Stab {
                start: 20,
                duration: 50,
                notes: vec![64],
                gain: 1.0,
            },
        ];
        // Group 0 frees at 100, group 1 at 510; the third stab takes group 0,
        // cutting the shorter tail.
        assert_eq!(assign_groups(&plan, 2), vec![0, 1, 0]);
    }

    #[test]
    fn assigning_with_no_groups_does_not_panic() {
        let plan = vec![Stab {
            start: 0,
            duration: 10,
            notes: vec![60],
            gain: 1.0,
        }];
        assert_eq!(assign_groups(&plan, 0), vec![0]);
    }
}
