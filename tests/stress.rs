//! Stage 2: load and stress.
//!
//! See `PERFORMANCE.md`. Stage 1 asks whether the sound is *right*; this asks
//! whether the program survives being asked for everything at once. Every
//! dimension here is a value a user can reach from a panel — a tempo, a grid, a
//! rack, an ensemble — so the matrix is not a hypothetical worst case, it is the
//! corner of the reachable space.
//!
//! Three of these run on every `cargo test`. The soak does not: a minute of audio
//! is a minute of audio, and it is `#[ignore]`d so that it is asked for
//! explicitly.
//!
//! ```text
//! cargo test --release --test stress -- --ignored --nocapture
//! ```

mod harness;

use chord_tool::arrangement::{arrangement_with_swing, assign_groups, bar_events, BarEvent};
use chord_tool::music::{Key, Scale, BAR_TICKS};
use chord_tool::progression::{Progression, ProgressionEntry, Slot};
use chord_tool::rhythm::{RhythmLayer, RhythmPattern};
use chord_tool::rhythm_store::RhythmStore;
use chord_tool::smf::SmfOptions;
use chord_tool::synth::{AUDITION_GROUP, STAB_GROUPS};
use harness::{
    count_allocations, fill_the_pool, prepare, ticks_to_frames, worst_case_ensemble, Config, Event,
    MAX_BLOCK, WIDE,
};

/// The rates a device can hand us.
const RATES: [f32; 3] = [44_100.0, 48_000.0, 96_000.0];

/// The block sizes worth checking: the smallest a backend might ask for and the
/// largest the harness will schedule against.
const BLOCKS: [usize; 3] = [16, 512, MAX_BLOCK];

// -----------------------------------------------------------------------------
// The matrix
// -----------------------------------------------------------------------------

#[test]
fn the_worst_case_survives_every_rate_and_block_size() {
    // Everything at once: 120 voices of the pool sounding at four voices of
    // unison each, eighteen inserts across the three registers, every EQ band at
    // an end of its range, both aux units running, and the master fader at the
    // top. Nine combinations of rate and block size, all of which are reachable
    // from the panels and from whatever audio device the machine has.
    for rate in RATES {
        for block in BLOCKS {
            let mut prepared = prepare(
                fill_the_pool(
                    Config::built(worst_case_ensemble())
                        .rate(rate)
                        .block(block)
                        .frames(2 * MAX_BLOCK),
                )
                .chord(&WIDE),
            );
            prepared.trigger_everything();

            let mut buffer = vec![0.0f32; prepared.block * prepared.channels];
            let mut loudest = 0.0f32;
            let mut worst = 0.0f32;
            for _ in 0..prepared.frames / prepared.block {
                prepared.engine.process(&mut buffer, &prepared.params, None);
                for sample in &buffer {
                    assert!(
                        sample.is_finite(),
                        "{rate} Hz at a block of {block} produced a non-finite sample"
                    );
                    loudest = loudest.max(sample.abs());
                    worst = worst.max(sample.abs());
                }
            }
            assert!(
                worst <= 1.0,
                "{rate} Hz at a block of {block} left the engine at {worst}"
            );
            assert!(
                loudest > 0.0,
                "{rate} Hz at a block of {block} rendered silence"
            );
        }
    }
}

#[test]
fn the_worst_case_allocates_nothing_once_the_pool_is_full() {
    // The allocation test in its own binary proves the callback is clean with a
    // plain ensemble; this is the same claim with the rack, the curves and the
    // aux units in the path, over a longer span than that test runs.
    let mut prepared = prepare(fill_the_pool(
        Config::built(worst_case_ensemble())
            .block(512)
            .frames(4 * MAX_BLOCK),
    ));
    prepared.trigger_everything();

    let mut buffer = vec![0.0f32; prepared.block * prepared.channels];
    let (_, passes) = count_allocations(|| {
        for _ in 0..prepared.frames / prepared.block {
            prepared.engine.process(&mut buffer, &prepared.params, None);
        }
    });
    assert_eq!(
        passes, 0,
        "{passes} allocator passes with the racks running"
    );
}

#[test]
fn a_parameter_moved_mid_render_does_not_break_anything() {
    // Every row of every synth page, swept while the sound is playing. A
    // parameter that is read once per buffer, once per voice or once per sample
    // is a different thing in each case, and a filter that recomputes its
    // coefficients on the wrong granularity is a click or a blow-up rather than
    // a wrong number.
    /// One synth row: its name, and how to move it.
    type Row = (&'static str, fn(&chord_tool::synth::SynthParams, f32));
    let rows: [Row; 8] = [
        ("cutoff", |p, v| p.mid.cutoff.set(v)),
        ("resonance", |p, v| p.mid.resonance.set(v)),
        ("position", |p, v| p.mid.position.set(v)),
        ("phase_dist", |p, v| p.mid.phase_dist.set(v)),
        ("osc2_fm", |p, v| p.mid.osc2_fm.set(v)),
        ("feedback", |p, v| p.mid.feedback.set(v)),
        ("osc2_level", |p, v| p.mid.osc2_level.set(v)),
        ("drive", |p, v| p.mid.drive.set(v)),
    ];
    for (name, set) in rows {
        for (low, high) in [(0.0f32, 1.0f32), (1.0, 0.0)] {
            let mut prepared = prepare(
                Config::built(worst_case_ensemble())
                    .frames(2 * MAX_BLOCK)
                    .block(MAX_BLOCK),
            );
            prepared
                .voices
                .play_stab(0, &WIDE, 1.0, 1.0, &prepared.params);
            let mut buffer = vec![0.0f32; prepared.block * prepared.channels];

            prepared.engine.process(&mut buffer, &prepared.params, None);
            set(&prepared.params, low);
            prepared.engine.process(&mut buffer, &prepared.params, None);
            set(&prepared.params, high);
            prepared.engine.process(&mut buffer, &prepared.params, None);

            for sample in &buffer {
                assert!(
                    sample.is_finite() && sample.abs() <= 1.0,
                    "{name} swept {low} -> {high} left the engine at {sample}"
                );
            }
        }
    }
}

#[test]
fn every_voice_of_the_pool_survives_every_waveform() {
    // 120 voices at once is the pool's own worst case; a waveform that keeps
    // state — a plucked string, a folded saw, a table being morphed — is the
    // worst case for one voice. Together they are the only configuration where
    // every `Voice` in the engine is doing the most expensive thing it can do.
    for waveform in chord_tool::synth::Waveform::ALL {
        let mut prepared = prepare(fill_the_pool(
            Config::built(worst_case_ensemble())
                .block(MAX_BLOCK)
                .frames(MAX_BLOCK),
        ));
        prepared.trigger_everything();
        // After the stabs, so the waveform is the one the notes start on.
        for channel in [
            &prepared.params.low,
            &prepared.params.mid,
            &prepared.params.high,
        ] {
            channel.waveform.set(waveform as i32 as f32);
        }
        prepared.trigger_everything();

        let mut buffer = vec![0.0f32; prepared.block * prepared.channels];
        for _ in 0..prepared.frames / prepared.block {
            prepared.engine.process(&mut buffer, &prepared.params, None);
            for sample in &buffer {
                assert!(
                    sample.is_finite() && sample.abs() <= 1.0,
                    "{waveform:?} with the whole pool sounding left the engine at {sample}"
                );
            }
        }
    }
}

// -----------------------------------------------------------------------------
// The planner, end to end
// -----------------------------------------------------------------------------

/// A progression of `bars` chords on the densest grid the tool offers.
fn dense_progression(bars: usize, steps: usize, layers: usize) -> Vec<Slot> {
    let mut pattern =
        RhythmPattern::from_step_string("Dense", 0.25, &"x".repeat(steps)).expect("a valid grid");
    // Four takes, decaying, all hitting every cell: the densest onset rate and
    // the deepest stack the arrangement will ever have to assign groups to.
    while pattern.layers.len() < layers {
        let gain = 1.0 / (pattern.layers.len() as f32 + 1.0);
        pattern
            .layers
            .push(RhythmLayer::new(gain, vec![true; steps]));
    }

    let degrees = [
        chord_tool::music::ScaleDegree::I,
        chord_tool::music::ScaleDegree::VI,
        chord_tool::music::ScaleDegree::IV,
        chord_tool::music::ScaleDegree::V,
    ];
    (0..bars)
        .map(|bar| {
            let mut entry = ProgressionEntry::new(degrees[bar % degrees.len()], None);
            entry.pattern = Some(pattern.clone());
            Slot::Chord(entry)
        })
        .collect()
}

#[test]
fn a_dense_plan_plays_through_the_engine() {
    // The planner and the engine together, which is the pairing no unit test
    // covers: a plan of timed stabs, sliced into bars, assigned to voice groups
    // and played at a tempo. Sixty-four cells of four takes at 240 bpm is 256
    // onsets a bar, which is more than the four stab groups can hold without
    // stealing from each other — deliberately, because that is the case where
    // the group assignment matters.
    let key = Key::new(60, Scale::Major);
    // One bar each, because a bar at 40 bpm is six seconds of audio and the
    // point of the low tempo is how *long* each note is, not how many there are.
    for (tempo, bars) in [(40.0f32, 1usize), (120.0, 1), (240.0, 1)] {
        let slots = dense_progression(bars, 64, 4);
        let plan = arrangement_with_swing(&slots, &key, 1.0, 0.5);
        let groups = assign_groups(&plan, STAB_GROUPS);
        assert_eq!(groups.len(), plan.len());

        // The whole loop, as one bar's worth of events at a time.
        let mut events: Vec<(usize, Event)> = Vec::new();
        for bar in 0..bars {
            let mut bar_plan = bar_events(&plan, bar, BAR_TICKS, STAB_GROUPS);
            for event in bar_plan.drain(..) {
                let at = bar as u64 * BAR_TICKS + event.at();
                let frame = ticks_to_frames(at, tempo, 48_000.0);
                events.push((
                    frame,
                    match event {
                        BarEvent::On {
                            group,
                            notes,
                            gain,
                            velocity,
                            ..
                        } => Event::Stab {
                            group,
                            notes,
                            gain,
                            velocity,
                        },
                        BarEvent::Off { group, .. } => Event::Release { group },
                        BarEvent::Click { strong, .. } => Event::Click {
                            strong,
                            sound: 0,
                            volume: 0.5,
                        },
                    },
                ));
            }
        }
        assert!(
            !events.is_empty(),
            "{bars} bars at {tempo} bpm planned nothing"
        );

        let frames = ticks_to_frames(bars as u64 * BAR_TICKS, tempo, 48_000.0) + 2 * MAX_BLOCK;
        let mut config = Config::built(worst_case_ensemble())
            .block(MAX_BLOCK)
            .frames(frames);
        for (frame, event) in events {
            config = config.at(frame, event);
        }

        // The events are applied at their own frames, which is what the
        // scheduler does between two buffers. Applying them all up front — as
        // the tests that want the pool at its loudest do — would release every
        // note before the first sample was asked for and render silence.
        let render = harness::render(config);
        assert_eq!(
            render.non_finite(),
            0,
            "{tempo} bpm produced a non-finite sample"
        );
        assert!(
            render.peak() <= 1.0,
            "{tempo} bpm left the engine at {}",
            render.peak()
        );
        assert!(
            render.rms() > 1e-5,
            "{tempo} bpm rendered {} frames at an RMS of {}",
            render.frames(),
            render.rms()
        );
    }
}

#[test]
fn a_long_progression_plans_in_reasonable_time() {
    // 256 chords is the progression panel's limit, and it is the scale at which
    // anything quadratic in the plan shows up. The planner is a pure function,
    // so this measures only the planner — but the planner is what the engine's
    // onsets come from, and a plan that takes a second to build is a bar of
    // silence on the first loop.
    let key = Key::new(60, Scale::Major);
    let slots = dense_progression(256, 16, 2);
    let start = std::time::Instant::now();
    let plan = arrangement_with_swing(&slots, &key, 1.0, 0.0);
    let groups = assign_groups(&plan, STAB_GROUPS);
    let elapsed = start.elapsed();

    assert_eq!(groups.len(), plan.len());
    assert!(!plan.is_empty());
    println!(
        "256 bars of a 16-cell two-take pattern: {} stabs in {elapsed:?}",
        plan.len()
    );
    assert!(
        elapsed.as_millis() < 2_000,
        "planning 256 bars took {elapsed:?}"
    );
}

// -----------------------------------------------------------------------------
// The soak
// -----------------------------------------------------------------------------

#[test]
#[ignore = "a minute of audio; run it in release, with --ignored"]
fn a_minute_of_the_worst_case_does_not_drift() {
    // Growth, drift and monotonic accumulation. Sixty seconds at the top of the
    // matrix, with the whole pool struck at the start and a long tail behind it,
    // watching for the things that only show up over minutes: an allocation in a
    // buffer that grows on first use, a level that climbs, and — the one a
    // clipped output cannot show on its own — a cost per sample that climbs with
    // it, which is what a tail decaying into the subnormal range does on a CPU
    // that has not been told to flush them.
    //
    // What this test deliberately does *not* assert is that the tail reaches
    // silence. The maximised aux delay has its feedback at 0.95 with a two
    // second line and the aux reverb has its size at the top of its range, so
    // the wash is *supposed* to still be audible a minute later; the levels are
    // reported rather than asserted. What does assert is that the machinery is
    // in the same state at the end as at the start.
    let seconds = 60usize;
    let rate = 48_000.0f32;
    let frames = (seconds as f32 * rate) as usize;
    let mut prepared = prepare(
        Config::built(worst_case_ensemble())
            .rate(rate)
            .block(512)
            .frames(frames)
            .at(
                0,
                Event::Stab {
                    group: 0,
                    notes: WIDE.to_vec(),
                    gain: 1.0,
                    velocity: 1.0,
                },
            )
            .at(
                ticks_to_frames(BAR_TICKS * 4, 120.0, rate),
                Event::Release { group: 0 },
            ),
    );
    for group in 1..=AUDITION_GROUP {
        prepared
            .voices
            .play_stab(group, &WIDE, 0.8, 1.0, &prepared.params);
    }

    // The first ten seconds and the last ten, which is the comparison that says
    // whether anything grew.
    let ten = 10 * rate as usize;
    let head_to = ten.min(frames);
    let mid_to = (2 * ten).min(frames);
    let tail_from = frames.saturating_sub(ten);

    let mut buffer = vec![0.0f32; prepared.block * prepared.channels];
    let mut loudest = 0.0f32;
    let mut head_peak = 0.0f32;
    let mut tail_peak = 0.0f32;
    let mut tail_sum = 0.0f64;
    let mut tail_count = 0u64;
    let mut mid_sum = 0.0f64;
    let mut mid_count = 0u64;
    let mut head_time = std::time::Duration::ZERO;
    let mut tail_time = std::time::Duration::ZERO;

    let (_, passes) = count_allocations(|| {
        for start in (0..prepared.frames).step_by(prepared.block) {
            let began = std::time::Instant::now();
            prepared.engine.process(&mut buffer, &prepared.params, None);
            let took = began.elapsed();
            if start < head_to {
                head_time += took;
            } else if start >= tail_from {
                tail_time += took;
            }
            for sample in &buffer {
                assert!(
                    sample.is_finite(),
                    "a non-finite sample after {start} frames"
                );
                let level = sample.abs();
                loudest = loudest.max(level);
                if start < head_to {
                    head_peak = head_peak.max(level);
                }
                if start >= tail_from {
                    tail_peak = tail_peak.max(level);
                    tail_sum += level as f64;
                    tail_count += 1;
                } else if start >= head_to && start < mid_to {
                    mid_sum += level as f64;
                    mid_count += 1;
                }
            }
        }
    });

    let tail_mean = if tail_count == 0 {
        0.0
    } else {
        tail_sum / tail_count as f64
    };
    let mid_mean = if mid_count == 0 {
        0.0
    } else {
        mid_sum / mid_count as f64
    };
    let head_us = head_time.as_secs_f64() / head_to as f64 * 1e6;
    let tail_us = tail_time.as_secs_f64() / (frames - tail_from) as f64 * 1e6;
    println!(
        "over {seconds} s: loudest {loudest:.6}, first ten seconds peak {head_peak:.3e} \
         (cost {head_us:.3} us/frame), last ten seconds peak {tail_peak:.3e} \
         (cost {tail_us:.3} us/frame), mean |sample| there {tail_mean:.3e}, \
         {passes} allocator passes"
    );

    assert_eq!(
        passes, 0,
        "{passes} allocator passes over a minute of audio"
    );
    assert!(loudest > 0.0, "the soak rendered silence");
    assert!(loudest <= 1.0, "the soak left the engine at {loudest}");
    // Whether anything is *accumulating*, measured as a level rather than as a
    // peak: the master clip puts a ceiling of exactly one on the peak, so a
    // system that is quietly charging up looks identical to a steady one from
    // there. Between ten and twenty seconds and the last ten, it does not.
    assert!(
        tail_mean <= mid_mean * 1.2,
        "the last ten seconds average {tail_mean:.3e} against {mid_mean:.3e} ten seconds in: \
         something is accumulating"
    );
    // The wall-clock half of the same claim, and the half a clipped output
    // cannot make on its own: whatever the tail is doing, it must not have
    // become dramatically more expensive to compute. A tail that has decayed
    // into the subnormal range is the usual reason this moves, and it is worth
    // knowing about even when the sound is identical.
    assert!(
        tail_us <= head_us * 2.0,
        "the last ten seconds cost {tail_us:.3} us a frame against {head_us:.3} at the start"
    );
}

/// What every effect's tail does, twelve seconds after the note is released.
///
/// The first row is the control: `none` leaves the rack empty, so whatever is in
/// its tail is the instrument's own rather than any effect's.
///
/// This is the measurement that found the subnormal tails — five maximised
/// effects settled between `1e-45` and `1e-42` instead of reaching zero, where
/// every operation costs tens to hundreds of times more than the same operation
/// on a normal float, for as long as the effect stays loaded. `settle` in
/// `fx_dsp.rs` is the fix, and it is a flush to zero below the level that file
/// already treats as silence.
///
/// It stays a diagnostic rather than a gate because of the numbers on the right:
/// what each effect leaves behind is a *rate* of decay, not a threshold. What is
/// worth asserting is that none of them parks in the subnormal range, and that
/// is what the assertion at the bottom of the loop does.
///
/// ```text
/// cargo test --release --test stress where_the_effect_tails_settle -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a diagnostic; it prints, it does not gate"]
fn where_the_effect_tails_settle() {
    const FLOAT_MIN: f32 = f32::MIN_POSITIVE;
    let rate = 48_000.0f32;
    let frames = (14.0 * rate) as usize;
    let release = 2 * rate as usize;

    println!("{:<12} {:>10} {:>12}", "effect", "head", "tail");
    for kind in chord_tool::fx::FxKind::ALL {
        let mut ensemble = chord_tool::ensemble::builtin_ensembles()[0].clone();
        // `None` is in the list on purpose: it is the control. Whatever is left
        // in its tail is the instrument's own, and any effect whose tail matches
        // it is passing the dry signal rather than ringing.
        let chain = if kind.is_none() {
            Vec::new()
        } else {
            vec![harness::maxed(kind)]
        };
        for placement in [&mut ensemble.low, &mut ensemble.mid, &mut ensemble.high] {
            placement.chain = chain.clone();
        }
        ensemble.name = format!("{kind:?}");
        let render = harness::render(
            Config::built(ensemble)
                .rate(rate)
                .block(512)
                .frames(frames)
                .at(
                    0,
                    Event::Stab {
                        group: 0,
                        notes: vec![60],
                        gain: 1.0,
                        velocity: 1.0,
                    },
                )
                .at(release, Event::Release { group: 0 }),
        );
        let head = render.peak_between(0, release);
        let tail = render.peak_between(frames - rate as usize, frames);
        let note = if tail == 0.0 {
            ""
        } else if tail < FLOAT_MIN * 1024.0 {
            "  <- SUBNORMAL"
        } else {
            "  <- still ringing, in the normal range"
        };
        println!("{kind:?} {head:>10.4} {tail:>12.3e}{note}");
        assert_eq!(render.non_finite(), 0, "{kind:?}");
        // The assertion the measurement earned: a tail may still be ringing at
        // twelve seconds — a filter at 99 % resonance legitimately is — but
        // nothing may be *parked* below the normal range, where the decay stops
        // and the cost per operation multiplies.
        assert!(
            tail == 0.0 || tail >= FLOAT_MIN * 1024.0,
            "{kind:?} has parked at {tail:.3e}, in the subnormal range"
        );
    }
}

// -----------------------------------------------------------------------------
// Hostile files
// -----------------------------------------------------------------------------

/// Every parser that reads a file a user can edit or hand-write.
///
/// The crate promises that a wrong-shaped file reads as a default rather than a
/// panic — `from_toml` in each library, the settings and project readers, and the
/// MIDI reader. The promise is worth more than the tests that assert it on
/// well-shaped input, because the input that breaks it is the input nobody
/// thought of.
/// A named parser: what it reads, and whether the read succeeded.
type Parser = (&'static str, fn(&str) -> bool);

fn parsers() -> Vec<Parser> {
    vec![
        ("instruments", |t| {
            chord_tool::instrument::from_toml(t).is_ok()
        }),
        ("ensembles", |t| chord_tool::ensemble::from_toml(t).is_ok()),
        ("fx_presets", |t| chord_tool::fx::from_toml(t).is_ok()),
        ("eq_presets", |t| chord_tool::eq::from_toml(t).is_ok()),
        ("rhythms", |t| {
            chord_tool::rhythm_store::from_toml(t).is_ok()
        }),
        ("steps", |t| chord_tool::rhythm::parse_steps(t).is_ok()),
    ]
}

/// The shipped files, which are the shape every mutant is a mutation of.
const SHIPPED: [(&str, &str); 5] = [
    ("instruments", include_str!("../instruments.toml")),
    ("ensembles", include_str!("../ensembles.toml")),
    ("fx_presets", include_str!("../fx_presets.toml")),
    ("eq_presets", include_str!("../eq_presets.toml")),
    ("rhythms", include_str!("../rhythms.toml")),
];

/// A deterministic little generator, so a failure is reproducible from its seed
/// rather than from a saved corpus.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*, which is enough to shuffle bytes and has no dependency.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// Mutate one file in one of several ways, in place.
fn mutate(text: &str, rng: &mut Rng) -> String {
    let mut bytes = text.as_bytes().to_vec();
    if bytes.is_empty() {
        return String::new();
    }
    match rng.below(8) {
        // Truncate somewhere.
        0 => bytes.truncate(rng.below(bytes.len())),
        // Cut a run out of the middle.
        1 => {
            let at = rng.below(bytes.len());
            let len = rng.below(bytes.len() - at).max(1);
            bytes.drain(at..at + len);
        }
        // Flip a byte to something structurally interesting.
        2 => {
            let at = rng.below(bytes.len());
            const INTERESTING: &[u8] = b"\"'=[]{}#\n\r\x00\xff0123456789.-+eE";
            bytes[at] = INTERESTING[rng.below(INTERESTING.len())];
        }
        // Duplicate a run, which is how a key ends up defined twice.
        3 => {
            let at = rng.below(bytes.len());
            let len = rng.below(64).max(1).min(bytes.len() - at);
            let run = bytes[at..at + len].to_vec();
            bytes.splice(at..at, run);
        }
        // Repeat a whole file, so tables collide.
        4 => {
            let copy = bytes.clone();
            bytes.extend_from_slice(&copy);
        }
        // Replace a number with something a parser should refuse or clamp.
        5 => {
            let at = rng.below(bytes.len());
            let replacement =
                ["nan", "inf", "-inf", "1e999", "-1e999", "0x10", "1e-999"][rng.below(7)];
            let end = (at + 3).min(bytes.len());
            bytes.splice(at..end, replacement.bytes());
        }
        // Truncate a string's closing quote.
        6 => {
            if let Some(at) = bytes.iter().position(|b| *b == b'"') {
                bytes.remove(at);
            }
        }
        // Chop the very end, which is the most common way a file is half-written.
        _ => {
            let cut = rng.below(16).min(bytes.len());
            bytes.truncate(bytes.len() - cut);
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[test]
fn every_shipped_file_parses_before_it_is_mutated() {
    // The oracle for the fuzz below: a mutation that "fails to parse" only means
    // something if the file it came from parses. Include the check here so that
    // a broken shipped file is reported as itself rather than as a fuzz failure.
    for (name, text) in SHIPPED {
        let parser = parsers()
            .into_iter()
            .find(|(n, _)| *n == name)
            .map(|(_, p)| p)
            .unwrap_or_else(|| panic!("no parser for {name}"));
        assert!(parser(text), "{name}.toml does not parse");
    }
}

#[test]
fn hostile_files_never_panic() {
    // Truncations, duplicated keys, wrong types, wrong array lengths, `nan`,
    // infinities, huge numbers, removed quotes and doubled files, over every
    // shipped file, against every parser that reads one. Nothing here asserts
    // that a mutant is *rejected* — a mutation that happens to still be valid
    // TOML should load. What is asserted is that every parser returns, in either
    // direction, and never panics and never hangs.
    let mut rng = Rng(0x5EED_1234_5678_9ABC);
    let mut accepted = 0usize;
    let mut rejected = 0usize;

    // Two hundred rounds, which is a smoke test rather than a fuzz: `deep_fuzz`
    // below is the same generator with a corpus a hundred times the size, and it
    // is `#[ignore]`d because parsing a hundred and fifty instruments six times
    // over is not something to do on every `cargo test`.
    for round in 0..200 {
        let (name, text) = SHIPPED[rng.below(SHIPPED.len())];
        let mutant = mutate(text, &mut rng);
        // Every parser reads every mutant: a file of the wrong shape for a
        // parser is exactly the case where the "reads as a default" promise has
        // to hold, and the shipped files share the `name = "..."` shape that
        // makes cross-parsing reachable rather than absurd.
        for (parser_name, parse) in parsers() {
            let ok = std::panic::catch_unwind(|| parse(&mutant));
            match ok {
                Ok(true) => accepted += 1,
                Ok(false) => rejected += 1,
                Err(_) => panic!(
                    "round {round}: the {parser_name} parser panicked on a mutated {name}.toml\n\
                     --- the mutant was ---\n{mutant}\n--- end ---"
                ),
            }
        }
    }

    println!("{accepted} accepted, {rejected} rejected, no panics");
    assert!(
        accepted > 0,
        "every mutant was rejected, which cannot be right"
    );
}

/// The same generator, with a corpus worth calling a fuzz.
///
/// `#[ignore]`d because it is minutes of TOML parsing in a debug build. Run it
/// after touching anything that reads a file:
///
/// ```text
/// cargo test --release --test stress deep_fuzz -- --ignored
/// ```
#[test]
#[ignore = "tens of thousands of parses; run it in release"]
fn deep_fuzz() {
    let mut rng = Rng(0xDEAD_BEEF_CAFE_1234);
    let (mut accepted, mut rejected) = (0usize, 0usize);
    for round in 0..60_000 {
        let (name, text) = SHIPPED[rng.below(SHIPPED.len())];
        let mutant = mutate(text, &mut rng);
        for (parser_name, parse) in parsers() {
            match std::panic::catch_unwind(|| parse(&mutant)) {
                Ok(true) => accepted += 1,
                Ok(false) => rejected += 1,
                Err(_) => panic!(
                    "round {round}: the {parser_name} parser panicked on a mutated {name}.toml\n\
                     --- the mutant was ---\n{mutant}\n--- end ---"
                ),
            }
        }
    }
    println!("{accepted} accepted, {rejected} rejected, no panics");
}

#[test]
fn a_binary_file_never_panics_the_midi_reader() {
    // The MIDI side reads bytes rather than TOML, so bytes are what it gets:
    // random ones, truncated ones, and the shipped export with holes punched in
    // it. `smf::read_project` is the reader the import panel calls.
    let mut rng = Rng(0x1357_9BDF_2468_ACE0);
    let mut valid = Vec::new();
    for round in 0..2_000 {
        let len = rng.below(512);
        let mut bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        if round % 2 == 0 && !bytes.is_empty() {
            bytes.truncate(rng.below(bytes.len()));
        }
        valid.push(bytes);
    }
    // A real file, chopped, is the mutation most likely to walk off the end of a
    // length field.
    let score = chord_tool::midi::render_progression_with_swing(
        &dense_progression(2, 16, 1),
        &Key::new(60, Scale::Major),
        120,
        1.0,
        0.0,
    );
    let real = chord_tool::smf::write(&score, &SmfOptions::single("fuzz"));
    for cut in 0..real.len().min(512) {
        valid.push(real[..cut].to_vec());
    }

    for (index, bytes) in valid.iter().enumerate() {
        let result = std::panic::catch_unwind(|| chord_tool::smf::read_project(bytes));
        assert!(
            result.is_ok(),
            "the MIDI reader panicked on case {index} ({} bytes)",
            bytes.len()
        );
    }
}

#[test]
fn a_corrupt_project_file_never_panics() {
    let mut rng = Rng(0x0F0F_1234_5678_9999);
    let mut cases: Vec<Vec<u8>> = Vec::new();
    for _ in 0..1_000 {
        let len = rng.below(256);
        cases.push((0..len).map(|_| rng.next() as u8).collect());
    }
    // And a valid one, truncated at every length: the encoding has a header and
    // length-prefixed sections, so a cut anywhere is a cut inside a field.
    let mut progression = Progression::default();
    progression.slots = dense_progression(1, 16, 1);
    let encoded = chord_tool::project::encode(
        &progression,
        &RhythmStore::default(),
        Key::new(60, Scale::Major),
        120,
        1.0,
    )
    .expect("a progression with a valid pattern encodes");
    for cut in 0..encoded.len().min(256) {
        cases.push(encoded[..cut].to_vec());
    }

    for (index, bytes) in cases.iter().enumerate() {
        let result = std::panic::catch_unwind(|| chord_tool::project::decode(bytes));
        assert!(
            result.is_ok(),
            "the project decoder panicked on case {index} ({} bytes)",
            bytes.len()
        );
    }
}
