//! Stage 4: the audio path, measured.
//!
//! What `PERFORMANCE.md` calls the real path — [`Engine::process`], the same
//! call the cpal callback makes, driven with the same `SynthParams` an ensemble
//! produces — at a handful of pool sizes and rack loadings.
//!
//! Three rules from that document, kept here:
//!
//! - **Release only.** `cargo bench` is a release build; a debug build's DSP runs
//!   several times slower than the audio it produces, which is a fact about
//!   debug builds rather than about the engine.
//! - **Warm up, then measure.** Criterion does that for us. Its warm-up also
//!   covers the other transient this file would otherwise have to handle: the
//!   notes are struck *once*, before the measurement, and held, so the attack and
//!   the filter's settling are over before the first measured buffer and every
//!   measured buffer is a sustaining one.
//! - **Compare, do not threshold.** There is no pass or fail here.
//!   `--save-baseline` before, `critcmp` after; `scripts/bench.sh` is the
//!   wrapper.
//!
//! The throughput is reported in *frames per second*, which is the number that
//! means something for an audio engine: the device needs 48,000 of them a
//! second, so the ratio between what criterion prints and 48,000 is how much
//! headroom there is before the callback cannot keep up.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

#[path = "../tests/harness/mod.rs"]
mod harness;

use chord_tool::eq::EqCurve;
use chord_tool::fx::FxKind;
use chord_tool::voice::VoicePatch;

use harness::{
    fill_the_pool, maxed, prepare, render, worst_case_ensemble, Config, Event, MAX_BLOCK,
};

/// The rate and buffer size the engine and rack groups are measured at.
///
/// 48 kHz and 512 frames is the common case on this machine and the deadline the
/// numbers in `PERFORMANCE.md` are quoted against: 10.7 ms.
const RATE: f32 = 48_000.0;
const BLOCK: usize = 512;

/// Build a case and strike it once.
///
/// The notes are held, not released: `trigger_everything` applies every event
/// the config holds, and none of these configs holds a release, so the voices sit
/// in the sustain part of their envelope for the whole measurement.
fn case(name: &str, config: Config) -> (String, harness::Prepared) {
    let mut prepared = prepare(config);
    prepared.trigger_everything();
    (name.to_string(), prepared)
}

/// One note at a given unison, which is the only honest way to ask this engine
/// for an exact number of voices.
///
/// A single note is not one voice: `allocate` spreads it over the registers as
/// itself, an octave below and an octave above, so it is three. Adding notes to a
/// chord does not scale it either — a triad is still three, because the low and
/// high registers each take one and the middle takes the rest. Unison is the
/// multiplication that is actually under the player's control: three registers
/// times `unison` voices each.
fn note_at_unison(unison: f32) -> Config {
    Config::voice(VoicePatch::neutral(12_000.0))
        .rate(RATE)
        .block(BLOCK)
        .frames(BLOCK)
        .at(
            0,
            Event::Tweak(Box::new(move |p| {
                for channel in [&p.low, &p.mid, &p.high] {
                    channel.unison.set(unison);
                }
            })),
        )
        .at(
            0,
            Event::Stab {
                group: 0,
                notes: vec![60],
                gain: 1.0,
                velocity: 1.0,
            },
        )
}

/// The engine, with nothing in the rack, at every pool size worth distinguishing.
///
/// One variable: the number of voices. Everything else — the bus, the curves, the
/// master, the analyser — is a default ensemble with no inserts and no sends, so
/// the difference between two rows is the difference between zero, one, three and
/// a hundred and twenty voices and nothing else. The rack's own cost is the
/// `rack` group below; the two multiplied together is `bus`.
///
/// "no voices" is the floor: a buffer of silence still goes through four EQ
/// stages, the master curve, the clip and the analyser.
fn engine(c: &mut Criterion) {
    let mut group = c.benchmark_group("engine");
    group.throughput(Throughput::Elements(BLOCK as u64));

    let bare = || {
        Config::voice(VoicePatch::neutral(12_000.0))
            .rate(RATE)
            .block(BLOCK)
            .frames(BLOCK)
    };

    // The last two are the same pool playing patches that use the *second*
    // oscillator, which is the other half of the frequency arithmetic: an `expo`
    // cross-modulation and a ring modulation. They exist because a patch that
    // runs the second oscillator is the one that pays for it, so a row built from
    // a neutral sine cannot see a change aimed at them.
    let oscillator = |name: &'static str| {
        fill_the_pool(
            Config::instruments([name, name, name])
                .rate(RATE)
                .block(BLOCK)
                .frames(BLOCK),
        )
    };

    let cases = vec![
        ("no voices", bare()),
        ("3 voices (a triad, unison 1)", note_at_unison(1.0)),
        ("6 voices (unison 2)", note_at_unison(2.0)),
        ("12 voices (unison 4)", note_at_unison(4.0)),
        // `fill_the_pool` asks for every group at four voices of unison, which is
        // 120 of the pool's 121 voices.
        ("120 voices (the whole pool)", fill_the_pool(bare())),
        ("120 voices (cross-modulated)", oscillator("X-Mod Clang")),
        ("120 voices (ring modulated)", oscillator("Ring Bell")),
    ];

    for (name, config) in cases {
        let (name, mut prepared) = case(name, config);
        let mut buffer = vec![0.0f32; prepared.block * prepared.channels];
        group.bench_with_input(BenchmarkId::from_parameter(name), &(), |b, _| {
            b.iter(|| {
                prepared
                    .engine
                    .process(black_box(&mut buffer), &prepared.params, None);
            });
        });
    }

    group.finish();
}

/// The analyser switched on and off, in the same run, on three signals.
///
/// The pair *is* the measurement: nothing else differs between the two rows, and
/// criterion measures them a few seconds apart in one process, so this is the one
/// number here that does not have to fight the machine's five per cent run-to-run
/// spread. The analyser is a readout rather than a sound — `Analyzer::tick`
/// returns nothing and writes only its own banks — so switching it off cannot
/// change a sample, which `the_analyser_cannot_change_the_audio` in
/// `tests/render.rs` asserts rather than assumes.
///
/// It is also the answer to the open question in `PERFORMANCE.md` §10: what the
/// silent floor is made of. Four taps of thirteen bandpass biquads is 52 filters
/// a sample, run whether or not anybody is looking at the Spectrum panel, and
/// these three pairs say how much of the floor that is.
fn ablate(c: &mut Criterion) {
    let mut group = c.benchmark_group("ablate");
    group.throughput(Throughput::Elements(BLOCK as u64));

    /// One signal to ablate over: its name, and how to build it again.
    type Case = (&'static str, fn() -> Config);

    let cases: [Case; 3] = [
        ("silence, bare bus", || {
            Config::voice(VoicePatch::neutral(12_000.0))
                .rate(RATE)
                .block(BLOCK)
                .frames(BLOCK)
        }),
        ("silence, whole rack", || {
            Config::built(worst_case_ensemble())
                .rate(RATE)
                .block(BLOCK)
                .frames(BLOCK)
        }),
        ("120 voices, whole rack", || {
            fill_the_pool(
                Config::built(worst_case_ensemble())
                    .rate(RATE)
                    .block(BLOCK)
                    .frames(BLOCK),
            )
        }),
    ];

    for (name, build) in cases {
        for (label, enabled) in [("on", 1.0f32), ("off", 0.0)] {
            let (name, mut prepared) = case(&format!("{name}: analyser {label}"), build());
            prepared.params.analyzer.enabled.set(enabled);
            let mut buffer = vec![0.0f32; prepared.block * prepared.channels];
            group.bench_with_input(BenchmarkId::from_parameter(name), &(), |b, _| {
                b.iter(|| {
                    prepared
                        .engine
                        .process(black_box(&mut buffer), &prepared.params, None);
                });
            });
        }
    }

    group.finish();
}

/// The bus with the rack, the curves and the sends pulled out one at a time, so
/// the difference between two rows *is* the cost of one part.
///
/// **No voices.** The first version of this group ran at 120 voices, and the
/// differences it was built to show — 0.08 to 0.24 ms — were three to nine per
/// cent of the total it was measuring against, which is inside the noise of a
/// hundred and twenty voices and a settling delay line. On silence the rack is
/// the whole signal and the same differences are the whole number. The cost of
/// the rack *under* a full pool is then the two groups' floors subtracted, and
/// `bus` is the combined case.
///
/// A maximised effect on silence is not a maximised effect on a chord — a
/// compressor's envelope and a gate's threshold both behave differently — but the
/// arithmetic a slot does per sample is the same arithmetic, and this is the
/// version that can be read.
fn rack(c: &mut Criterion) {
    let mut group = c.benchmark_group("rack");
    group.throughput(Throughput::Elements(BLOCK as u64));

    let bare = |name: &str| {
        let mut ensemble = chord_tool::ensemble::builtin_ensembles()[0].clone();
        ensemble.name = name.to_string();
        for placement in [&mut ensemble.low, &mut ensemble.mid, &mut ensemble.high] {
            placement.chain = Vec::new();
            placement.eq = EqCurve::flat();
            placement.reverb_send = 0.0;
            placement.delay_send = 0.0;
        }
        ensemble.mixer.reverb_mix = 0.0;
        ensemble.mixer.delay_mix = 0.0;
        ensemble
    };

    let chained = |name: &str, kinds: &[FxKind]| {
        let chain: Vec<chord_tool::fx::Fx> = kinds.iter().map(|k| maxed(*k)).collect();
        let mut ensemble = bare(name);
        for placement in [&mut ensemble.low, &mut ensemble.mid, &mut ensemble.high] {
            placement.chain = chain.clone();
        }
        ensemble
    };

    let curved = {
        let mut ensemble = bare("curves");
        for placement in [&mut ensemble.low, &mut ensemble.mid, &mut ensemble.high] {
            placement.eq = EqCurve {
                gains: std::array::from_fn(|i| if i % 2 == 0 { 12.0 } else { -12.0 }),
            };
        }
        ensemble
    };

    let sent = {
        let mut ensemble = bare("sends");
        for placement in [&mut ensemble.low, &mut ensemble.mid, &mut ensemble.high] {
            placement.reverb_send = 1.0;
            placement.delay_send = 1.0;
        }
        ensemble.mixer.reverb = maxed(FxKind::Reverb);
        ensemble.mixer.delay = maxed(FxKind::Delay);
        ensemble.mixer.reverb_mix = 1.0;
        ensemble.mixer.delay_mix = 1.0;
        ensemble
    };

    let cases = vec![
        ("nothing", bare("nothing")),
        ("one insert", chained("one", &[FxKind::Chorus])),
        (
            "six inserts",
            chained(
                "six",
                &[
                    FxKind::Reverb,
                    FxKind::Delay,
                    FxKind::Chorus,
                    FxKind::Flanger,
                    FxKind::Phaser,
                    FxKind::Distortion,
                ],
            ),
        ),
        ("thirteen bands a register", curved),
        ("both aux units", sent),
        // And all of it at once, which is the configuration the stress suite
        // calls the worst case and the one `PERFORMANCE.md` quotes.
        ("everything", worst_case_ensemble()),
    ];

    for (name, ensemble) in cases {
        let (name, mut prepared) = case(
            name,
            Config::built(ensemble)
                .rate(RATE)
                .block(BLOCK)
                .frames(BLOCK),
        );
        let mut buffer = vec![0.0f32; prepared.block * prepared.channels];
        group.bench_with_input(BenchmarkId::from_parameter(name), &(), |b, _| {
            b.iter(|| {
                prepared
                    .engine
                    .process(black_box(&mut buffer), &prepared.params, None);
            });
        });
    }

    group.finish();
}

/// The headline: everything at once, which is the number the rest of the file is
/// a decomposition of.
///
/// One row, deliberately. Every other group varies one thing and holds the rest;
/// this is the product of all of them — 120 voices, eighteen insert slots, every
/// curve at a rail, both aux units wide open — and it is what `PERFORMANCE.md`
/// quotes as the share of the buffer deadline the callback uses. Its parts are
/// the `engine` and `rack` groups, and the sum of those two floors is within a
/// few per cent of it, which is the cross-check that the decomposition is real.
fn worst(c: &mut Criterion) {
    let mut group = c.benchmark_group("worst");
    group.throughput(Throughput::Elements(BLOCK as u64));

    let (name, mut prepared) = case(
        "120 voices, the whole rack",
        fill_the_pool(
            Config::built(worst_case_ensemble())
                .rate(RATE)
                .block(BLOCK)
                .frames(BLOCK),
        ),
    );
    let mut buffer = vec![0.0f32; prepared.block * prepared.channels];
    group.bench_with_input(BenchmarkId::from_parameter(name), &(), |b, _| {
        b.iter(|| {
            prepared
                .engine
                .process(black_box(&mut buffer), &prepared.params, None);
        });
    });

    group.finish();
}

/// The whole worst case with nothing playing, at every rate and buffer size the
/// coefficients are computed for.
///
/// No voices at all, so this is the floor a machine has to clear before a note is
/// played: the racks, the curves, the sends, the master, the clip and the
/// analyser, on silence. It is also the only case where a coefficient derived
/// from the sample rate can be seen without a voice's own coefficients on top of
/// it.
fn bus(c: &mut Criterion) {
    let mut group = c.benchmark_group("bus");
    for rate in [44_100.0f32, 48_000.0, 96_000.0] {
        for block in [64usize, 512, MAX_BLOCK] {
            group.throughput(Throughput::Elements(block as u64));
            let mut prepared = prepare(
                Config::built(worst_case_ensemble())
                    .rate(rate)
                    .block(block)
                    .frames(block),
            );
            let mut buffer = vec![0.0f32; prepared.block * prepared.channels];
            group.bench_with_input(
                BenchmarkId::new(format!("{}Hz", rate as u32), block),
                &(),
                |b, _| {
                    b.iter(|| {
                        prepared
                            .engine
                            .process(black_box(&mut buffer), &prepared.params, None);
                    });
                },
            );
        }
    }
    group.finish();
}

/// Off the audio path: the planner, and the harness's own render.
///
/// The planner is rebuilt on every bar, so its cost is multiplied by the tempo
/// rather than by the note count, and a plan that takes longer than a bar is a
/// bar of silence. The render is here because it is what every test in
/// `tests/render.rs` pays, and a change that makes the tests slower without
/// making the engine slower is worth noticing too.
fn planner(c: &mut Criterion) {
    use chord_tool::arrangement::{arrangement_with_swing, assign_groups, bar_events};
    use chord_tool::music::{Key, Scale, ScaleDegree, BAR_TICKS};
    use chord_tool::progression::{ProgressionEntry, Slot};
    use chord_tool::rhythm::RhythmPattern;

    let mut group = c.benchmark_group("planner");
    let key = Key::new(60, Scale::Major);

    let build = |bars: usize, steps: usize| {
        let pattern =
            RhythmPattern::from_step_string("Dense", 0.25, &"x".repeat(steps)).expect("a grid");
        let degrees = [
            ScaleDegree::I,
            ScaleDegree::VI,
            ScaleDegree::IV,
            ScaleDegree::V,
        ];
        (0..bars)
            .map(|bar| {
                let mut entry = ProgressionEntry::new(degrees[bar % degrees.len()], None);
                entry.pattern = Some(pattern.clone());
                Slot::Chord(entry)
            })
            .collect::<Vec<Slot>>()
    };

    for (bars, steps) in [(1usize, 16usize), (8, 16), (256, 16), (1, 64)] {
        let slots = build(bars, steps);
        group.throughput(Throughput::Elements((bars * steps) as u64));
        group.bench_with_input(
            BenchmarkId::new(format!("{steps} cells"), bars),
            &(),
            |b, _| {
                b.iter(|| {
                    let plan = arrangement_with_swing(black_box(&slots), &key, 1.0, 0.0);
                    let groups = assign_groups(&plan, 4);
                    let events = bar_events(&plan, 0, BAR_TICKS, 4);
                    black_box((groups.len(), events.len()))
                });
            },
        );
    }
    group.finish();

    let mut group = c.benchmark_group("render");
    group.throughput(Throughput::Elements((4 * MAX_BLOCK) as u64));
    for name in ["Default", "Drawbar Organ", "Bell", "Plucky"] {
        let ensemble = name.to_string();
        group.bench_with_input(BenchmarkId::from_parameter(name), &(), |b, _| {
            b.iter(|| {
                black_box(render(
                    Config::ensemble(&ensemble)
                        .rate(RATE)
                        .block(BLOCK)
                        .frames(4 * MAX_BLOCK)
                        .chord(&[60, 64, 67]),
                ))
            });
        });
    }
    group.finish();
}

criterion_group!(benches, engine, rack, worst, ablate, bus, planner);
criterion_main!(benches);
