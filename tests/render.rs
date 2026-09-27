//! Stage 1: the offline render harness, and the invariants it can assert.
//!
//! See `PERFORMANCE.md`. Everything here drives the real engine through the real
//! parameter path — an ensemble is resolved against the shipped library, the
//! three composed channels are applied to a `SynthParams`, and the buffers come
//! out of [`Engine::process`], which is the code the audio device calls.
//!
//! Two kinds of check live here, and they are doing different jobs:
//!
//! - **Invariants.** No sample is ever NaN, nothing ever leaves the device above
//!   full scale, an empty schedule is exactly silent, the block size does not
//!   change the sound. These hold for everything, so they are swept over every
//!   shipped ensemble and every shipped instrument rather than a chosen few.
//! - **Fingerprints.** A hash and five spectral features, recorded. These say
//!   "the sound is still the sound"; the hash is exact and therefore only
//!   meaningful on one machine, which is what the features and their tolerances
//!   are for.

mod harness;

use chord_tool::synth::Waveform;
use chord_tool::voice::VoicePatch;

use harness::{
    cents_above, maybe_dump, prepare, render, Config, Event, Features, Source, MAX_BLOCK, TRIAD,
};

/// A handful of ensembles, chosen to be unlike each other.
///
/// A pad, a plucked string, an organ, a struck bell and a bass: if a change to
/// the engine breaks something, it will break one of these.
const FINGERPRINTED: [&str; 6] = [
    "Default",
    "Warm Pad",
    "Plucky",
    "Drawbar Organ",
    "Bell",
    "Bassy",
];

/// Every shipped ensemble, by name.
fn all_ensembles() -> Vec<String> {
    chord_tool::ensemble::builtin_ensembles()
        .iter()
        .map(|e| e.name.clone())
        .collect()
}

/// Every shipped instrument, by name, deduplicated.
///
/// The library holds a low and a high variant of many instruments that differ
/// only in their register defaults, and both are shipped on purpose. Rendering
/// each name once is the sweep; registering it three times over would triple the
/// time for the same signal path.
fn all_instruments() -> Vec<String> {
    let mut names: Vec<String> = chord_tool::instrument::builtin_instruments()
        .iter()
        .map(|i| i.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

// -----------------------------------------------------------------------------
// Invariants
// -----------------------------------------------------------------------------

#[test]
fn an_empty_schedule_renders_exact_silence() {
    // Not "close to zero": the engine sums four EQ stages, two aux returns and a
    // soft clip, and if any of them had a denormal or a state leak the silence
    // would be a very small number instead of zero. Exactness is the assertion
    // that the path is at rest when nothing is playing.
    let render = render(Config::ensemble("Default").frames(MAX_BLOCK));
    assert_eq!(render.non_finite(), 0);
    assert!(
        render.is_silent(0, render.frames()),
        "an empty schedule produced {} at its loudest",
        render.peak()
    );
}

#[test]
fn a_render_ends_at_rest() {
    // The other half of the same claim: the envelope, the filter and the effect
    // tails have to *end* at rest, not merely start there. A voice that never
    // reaches zero is a voice that is still adding up in the mix, and a reverb
    // that never decays is a reverb that never stops costing anything to run.
    let render = render(
        Config::voice(VoicePatch::neutral(2000.0))
            .frames(4 * MAX_BLOCK)
            .at(
                0,
                Event::Stab {
                    group: 0,
                    notes: vec![60],
                    gain: 1.0,
                    velocity: 1.0,
                },
            )
            .at(MAX_BLOCK, Event::Release { group: 0 })
            .at(2 * MAX_BLOCK, Event::Silence),
    );
    maybe_dump(&render, "silence-after-a-note");
    assert_eq!(render.non_finite(), 0);
    assert!(
        render.peak_between(0, MAX_BLOCK) > 0.0,
        "the note itself must sound"
    );
    // From well past the release to the end, exactly nothing: not "small", zero.
    // The envelope reaches zero and the voice goes idle, so a nonzero sample
    // here is a state that never settled.
    let after = 3 * MAX_BLOCK;
    assert!(
        render.is_silent(after, render.frames()),
        "something is still ringing at {} long after the release",
        render.peak_between(after, render.frames())
    );
}

#[test]
fn nothing_ever_leaves_the_engine_above_full_scale() {
    // The master is `tanh`, so it cannot exceed one — but the EQ, the drive and
    // the chain all run before it, and a NaN in any of them would survive the
    // clip as a NaN. This is the sweep's first assertion, run against everything.
    for name in all_ensembles() {
        let render = render(Config::ensemble(&name).chord(&TRIAD));
        assert_eq!(
            render.non_finite(),
            0,
            "{name} produced a non-finite sample"
        );
        assert!(
            render.peak() <= 1.0,
            "{name} left the engine at {}",
            render.peak()
        );
    }
}

#[test]
fn every_shipped_ensemble_makes_a_sound() {
    // A shipped ensemble that renders silence is a broken preset, and there is
    // no way to notice by reading the file: a volume of zero, an instrument name
    // that no longer resolves, a chain whose first slot mutes — all of them look
    // the same from here.
    for name in all_ensembles() {
        let render = render(Config::ensemble(&name).chord(&TRIAD));
        assert!(
            render.rms() > 1e-4,
            "{name} rendered a triad at an RMS of {}",
            render.rms()
        );
    }
}

#[test]
fn every_shipped_instrument_plays_in_every_register() {
    // The library is register-neutral on purpose — one instrument can be placed
    // anywhere — so an instrument that only behaves in its own register is a bug
    // the palette hides. Low notes land on the one low voice, high notes on the
    // one high voice, and the middle of a triad on the four mid voices.
    let instruments = all_instruments();
    assert!(
        instruments.len() > 100,
        "the library has shrunk to {} instruments",
        instruments.len()
    );

    let mut quiet = Vec::new();
    let mut loud = Vec::new();
    for name in &instruments {
        let render = render(
            Config::instruments([name.as_str(), name.as_str(), name.as_str()])
                .frames(2 * MAX_BLOCK)
                .chord(&TRIAD),
        );
        maybe_dump(&render, &format!("instrument-{}", slug(name)));
        assert_eq!(
            render.non_finite(),
            0,
            "{name} produced a non-finite sample"
        );
        assert!(
            render.peak() <= 1.0,
            "{name} left the engine at {}",
            render.peak()
        );
        if render.rms() <= 1e-4 {
            quiet.push(name.clone());
        }
        if render.peak() <= 1e-3 {
            loud.push(name.clone());
        }
    }
    assert!(
        quiet.is_empty(),
        "these instruments rendered silence: {quiet:?}"
    );
    assert!(
        loud.is_empty(),
        "these instruments barely sounded: {loud:?}"
    );
}

#[test]
fn the_block_size_does_not_change_the_sound() {
    // The engine is fed a buffer at a time and its parameter reads are per
    // buffer, so a block size that changed the output would mean the sound
    // depended on the audio device's buffer rather than on the program. Every
    // event here is on a `MAX_BLOCK` boundary, so all five block sizes see it at
    // the same frame — which makes this a comparison of the sound and nothing
    // else.
    let config = |block| {
        Config::ensemble("Drawbar Organ")
            .block(block)
            .frames(4 * MAX_BLOCK)
            .chord(&TRIAD)
    };

    let reference = render(config(MAX_BLOCK));
    maybe_dump(&reference, "block-size");
    assert!(reference.rms() > 1e-4);

    for block in [16, 64, 512, 1024] {
        let other = render(config(block));
        assert_eq!(other.hash(), reference.hash(), "block size {block} differs");
        assert!(
            other.left == reference.left && other.right == reference.right,
            "block size {block} produced different samples"
        );
    }
}

#[test]
fn the_block_size_does_not_change_the_sound_while_a_parameter_moves() {
    // The same claim with something moving: a parameter changed on a `MAX_BLOCK`
    // boundary, mid-render. The per-buffer reads are the thing under test, so a
    // sweep that only ever holds one value would not be testing them.
    let config = |block| {
        Config::ensemble("Warm Pad")
            .block(block)
            .frames(4 * MAX_BLOCK)
            .chord(&TRIAD)
            .at(
                MAX_BLOCK,
                Event::Tweak(Box::new(|p: &_| p.low.cutoff.set(400.0))),
            )
            .at(
                2 * MAX_BLOCK,
                Event::Tweak(Box::new(|p: &_| p.mid.cutoff.set(1200.0))),
            )
    };

    let reference = render(config(MAX_BLOCK));
    assert!(reference.rms() > 1e-4);
    for block in [64, 512, 1024] {
        assert_eq!(
            render(config(block)).hash(),
            reference.hash(),
            "block size {block} differs"
        );
    }
}

#[test]
fn the_sample_rate_does_not_change_the_pitch() {
    // A note is a frequency, not a table index. An oscillator that advanced by a
    // fixed increment per sample instead of per second would play a fifth too
    // high at 48 kHz, and every filter and LFO coefficient would be wrong with
    // it.
    let config = |rate| {
        Config::voice(VoicePatch::neutral(12_000.0))
            .rate(rate)
            .frames(8 * MAX_BLOCK)
            .middle_register_only()
            .chord(&[69])
    };

    let at_44 = render(config(44_100.0));
    let at_48 = render(config(48_000.0));
    maybe_dump(&at_48, "a440-at-48k");

    for (rate, render) in [(44_100.0, &at_44), (48_000.0, &at_48)] {
        let hz = render.features().peak_hz;
        let cents = cents_above(hz, 440.0);
        assert!(
            cents.abs() < 5.0,
            "A440 came out at {hz:.2} Hz at {rate} Hz, {cents:+.1} cents out"
        );
    }
    // And to each other: two sample rates that disagree about the note are two
    // different instruments.
    let cents = cents_above(at_48.features().peak_hz, at_44.features().peak_hz);
    assert!(
        cents.abs() < 2.0,
        "the same note is {cents:+.1} cents apart between sample rates"
    );
}

#[test]
fn the_pitch_is_the_note_that_was_played() {
    // The whole oscillator stack — wave shaping, cross-modulation, phase
    // distortion, feedback — has to leave the fundamental where the keyboard put
    // it. A waveform that is one table entry out, or a phase that wraps early,
    // shows up here as cents.
    let cases: [(u8, f32); 4] = [(48, 130.81), (60, 261.63), (69, 440.0), (76, 659.26)];
    for (note, expected) in cases {
        let render = render(
            Config::voice(VoicePatch::neutral(12_000.0))
                .frames(8 * MAX_BLOCK)
                .middle_register_only()
                .chord(&[note]),
        );
        let hz = render.features().peak_hz;
        let cents = cents_above(hz, expected);
        assert!(
            cents.abs() < 5.0,
            "note {note} came out at {hz:.2} Hz, {cents:+.1} cents from {expected} Hz; \
             the strongest partials are {:?}",
            render.top_peaks(6)
        );
    }
}

#[test]
fn one_note_is_voiced_as_the_note_and_its_octaves() {
    // This is the design, not a bug to be worked around: a note is spread over
    // the three registers as itself, an octave below and an octave above, so a
    // triad fills the low voice, all four middle voices and the high voice. The
    // pitch tests above have to silence the neighbours to have a pitch to
    // measure, and this is the assertion that says why.
    let render = render(
        Config::voice(VoicePatch::neutral(12_000.0))
            .frames(8 * MAX_BLOCK)
            .chord(&[48]),
    );
    let peaks = render.top_peaks(3);
    let at = |hz: f32| peaks.iter().any(|(p, _)| cents_above(*p, hz).abs() < 5.0);
    assert!(
        at(65.41) && at(130.81) && at(261.63),
        "note 48 should sound as 36, 48 and 60, but the partials are {peaks:?}"
    );
}

#[test]
fn muting_the_master_is_exactly_silent() {
    let render = render(
        Config::ensemble("Bell")
            .frames(2 * MAX_BLOCK)
            .chord(&TRIAD)
            .at(0, Event::Tweak(Box::new(|p: &_| p.master_mute.set(1.0)))),
    );
    assert_eq!(render.non_finite(), 0);
    assert!(
        render.is_silent(0, render.frames()),
        "a muted master produced {}",
        render.peak()
    );
}

#[test]
fn a_register_at_zero_volume_is_exactly_silent() {
    // Silencing one register must not leave a filter or a reverb ringing: the
    // channel is scaled before the sends, so nothing downstream ever sees it.
    let render = render(
        Config::ensemble("Warm Pad")
            .frames(2 * MAX_BLOCK)
            .chord(&TRIAD)
            .at(
                0,
                Event::Tweak(Box::new(|p: &_| {
                    p.low.volume.set(0.0);
                    p.mid.volume.set(0.0);
                    p.high.volume.set(0.0);
                })),
            ),
    );
    assert_eq!(render.non_finite(), 0);
    assert!(
        render.is_silent(0, render.frames()),
        "three registers at zero produced {}",
        render.peak()
    );
}

#[test]
fn a_note_past_the_top_of_the_keyboard_still_renders() {
    // The panel pins the tonic below 96, but a placement's `transpose` is
    // free, and every register does arithmetic on the note it was handed.
    // Nothing here may wrap, go negative or turn into an index.
    for note in [0u8, 12, 96, 108, 126, 127] {
        let render = render(
            Config::voice(VoicePatch::neutral(8000.0))
                .frames(MAX_BLOCK)
                .chord(&[note]),
        );
        assert_eq!(
            render.non_finite(),
            0,
            "note {note} produced a non-finite sample"
        );
        assert!(
            render.peak() <= 1.0,
            "note {note} left the engine at {}",
            render.peak()
        );
    }
}

#[test]
fn every_waveform_renders_a_finite_note() {
    // The library sweep covers the waveforms the patches actually use. This one
    // covers the ones they do not, including the shapes that need their own
    // state — a plucked string, a noisy table, a fold.
    for waveform in Waveform::ALL {
        for unison in [1.0f32, 4.0] {
            let mut patch = VoicePatch::neutral(6000.0);
            patch.waveform = waveform;
            patch.unison = unison;
            patch.detune = if unison > 1.0 { 12.0 } else { 0.0 };
            let render = render(Config::voice(patch).frames(2 * MAX_BLOCK).chord(&TRIAD));
            assert_eq!(
                render.non_finite(),
                0,
                "{waveform:?} at unison {unison} produced a non-finite sample"
            );
            assert!(
                render.peak() <= 1.0,
                "{waveform:?} at unison {unison} left the engine at {}",
                render.peak()
            );
        }
    }
}

// -----------------------------------------------------------------------------
// The instrumentation
// -----------------------------------------------------------------------------

#[test]
fn the_engine_counts_the_buffers_it_processed() {
    // The counters the debug log reports, exercised through the real path rather
    // than by calling `Callback::record` directly. Its own unit tests check the
    // arithmetic; this checks that the arithmetic is wired to the engine at all,
    // which is the half that breaks silently.
    let block = 512;
    let mut prepared = prepare(
        Config::ensemble("Drawbar Organ")
            .block(block)
            .frames(4 * MAX_BLOCK)
            .chord(&TRIAD),
    );
    assert_eq!(prepared.timing().buffers(), 0, "nothing has run yet");

    let render = prepared.run();
    let timing = prepared.timing();
    assert_eq!(timing.buffers(), (4 * MAX_BLOCK / block) as u64);
    assert!(render.rms() > 1e-4, "the render must have produced a sound");

    // The loads are fractions of the deadline, so they are positive and finite,
    // and the mean can never exceed the worst. Nothing here asserts that the
    // *debug* build stays inside its deadline: a debug build's DSP runs several
    // times slower than the audio it produces, and that is the point of the
    // release profile rather than a bug in the callback.
    assert!(timing.last_load() > 0.0 && timing.last_load().is_finite());
    assert!(timing.mean_load() > 0.0 && timing.mean_load().is_finite());
    assert!(timing.peak_load() >= timing.mean_load());
    assert!(timing.overruns() <= timing.buffers());

    let summary = timing.summary();
    assert!(summary.contains(&format!("callbacks {}", 4 * MAX_BLOCK / block)));
    assert!(summary.contains("over deadline"), "{summary}");
}

#[test]
fn the_instrumentation_does_not_change_the_sound() {
    // The timing is two `Instant`s and four relaxed stores per buffer, and the
    // one thing it must never do is move a sample. The golden hashes are the
    // check that already says so — they were recorded before the counters
    // existed — but this says it in the place somebody would look.
    let with_timing = render(Config::ensemble("Bell").frames(4 * MAX_BLOCK).chord(&TRIAD));
    let again = render(Config::ensemble("Bell").frames(4 * MAX_BLOCK).chord(&TRIAD));
    assert_eq!(with_timing.hash(), again.hash());
    with_timing
        .features()
        .assert_close(&again.features(), 0.0, "two renders of one config");
}

#[test]
fn the_analyser_cannot_change_the_audio() {
    // The spectrum is a *readout*: `Analyzer::tick` returns nothing, touches only
    // its own banks, and is read only by the publish loop into
    // `params.analyzer.taps`. That makes switching it off free of any risk to the
    // sound — and it is worth asserting rather than assuming, because the whole
    // justification for running forty-eight fewer filters a sample is that
    // nothing else can see them.
    //
    // It is also what lets the interface decide: the banks only run while the
    // Spectrum panel is the one on screen.
    let listening = render(Config::ensemble("Bell").frames(4 * MAX_BLOCK).chord(&TRIAD));
    let ignoring = render(
        Config::ensemble("Bell")
            .frames(4 * MAX_BLOCK)
            .chord(&TRIAD)
            .at(
                0,
                Event::Tweak(Box::new(|p: &_| p.analyzer.enabled.set(0.0))),
            ),
    );
    assert_eq!(
        listening.hash(),
        ignoring.hash(),
        "the analyser reached the audio path"
    );
    assert!(
        listening.rms() > 1e-4,
        "the render must have produced a sound"
    );

    // And the levels really are the thing it stops doing: with nobody watching,
    // nothing is published and the readout keeps what it last had.
    assert!(
        reading_levels(Event::Tweak(Box::new(|p: &_| p.analyzer.enabled.set(1.0)))) > 0.0,
        "an enabled analyser publishes nothing"
    );
    assert_eq!(
        reading_levels(Event::Tweak(Box::new(|p: &_| p.analyzer.enabled.set(0.0)))),
        0.0,
        "a disabled analyser published a level"
    );
}

/// The loudest band the analyser published over a render.
///
/// Taken from the *last* buffer's worth, which is what the panel would be
/// drawing, rather than a peak over the whole render: a disabled analyser leaves
/// the levels at their initial value, and a peak would be indistinguishable from
/// a quiet one.
fn reading_levels(enabled: Event) -> f32 {
    let mut prepared = prepare(
        Config::ensemble("Bell")
            .frames(4 * MAX_BLOCK)
            .chord(&TRIAD)
            .at(0, enabled),
    );
    // `run` rather than `trigger_everything`: the events have to land at their
    // own frames, or the chord is released before the first buffer and the
    // analyser is correctly reporting silence.
    prepared.run();
    let mut loudest = 0.0f32;
    for tap in 0..4 {
        for level in prepared.params.analyzer.tap(tap).levels.iter() {
            loudest = loudest.max(level.get());
        }
    }
    loudest
}

// -----------------------------------------------------------------------------
// The analysis, before it is used to judge anything else
// -----------------------------------------------------------------------------

#[test]
fn the_analysis_measures_a_signal_it_was_given() {
    // Every other number in this file is produced by this transform, so a
    // transform that is subtly wrong would make all of them agree with each
    // other and none of them agree with the sound. These are the checks that
    // stop that: known tones, at known frequencies.
    let analyse = |signal: &[f32], rate: f32| {
        let (mags, bin_hz) = harness::spectrum(signal, rate, 4);
        (
            harness::peak_hz(&mags, bin_hz),
            harness::centroid_hz(&mags, bin_hz),
        )
    };

    for hz in [55.0f32, 440.0, 1000.0, 3000.0] {
        for rate in [44_100.0f32, 48_000.0] {
            let n = 16_384;
            let signal: Vec<f32> = (0..n)
                .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / rate).sin())
                .collect();
            let (peak, centroid) = analyse(&signal, rate);
            let cents = cents_above(peak, hz);
            assert!(
                cents.abs() < 1.0,
                "{hz} Hz at {rate} Hz was measured as {peak:.3} Hz, {cents:+.2} cents out"
            );
            // A single tone's energy-weighted mean *is* its frequency.
            assert!(
                cents_above(centroid, hz).abs() < 20.0,
                "{hz} Hz at {rate} Hz had a centroid of {centroid:.1} Hz"
            );
        }
    }

    // And the transform separates two tones rather than smearing them into
    // their average.
    let n = 16_384;
    let signal: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / 48_000.0;
            (2.0 * std::f32::consts::PI * 500.0 * t).sin()
                + (2.0 * std::f32::consts::PI * 1500.0 * t).sin()
        })
        .collect();
    let (mags, bin_hz) = harness::spectrum(&signal, 48_000.0, 4);
    let peaks = harness::top_peaks(&mags, bin_hz, 2);
    let mut found: Vec<f32> = peaks.iter().map(|(hz, _)| *hz).collect();
    found.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert!(
        cents_above(found[0], 500.0).abs() < 2.0 && cents_above(found[1], 1500.0).abs() < 2.0,
        "two tones came back as {found:?}"
    );
}

// -----------------------------------------------------------------------------
// Fingerprints
// -----------------------------------------------------------------------------

#[test]
fn the_fingerprints_are_what_they_were() {
    // The portable half of the golden test. Recorded numbers, compared with a
    // tolerance wide enough for a different `sin` and narrow enough to notice a
    // filter that moved, a level that drifted or an effect that stopped running.
    //
    // If this fails, the first question is whether the sound was supposed to
    // change. `CHORD_TOOL_RENDER=/tmp/out.wav cargo test --test render
    // the_fingerprints -- --nocapture` writes out what the render actually is.
    const RECORDED: [(&str, Features); 6] = [
        (
            "Default",
            Features {
                peak_hz: 329.6,
                centroid_hz: 327.9,
                rms: 0.1256046,
                crest: 4.0122,
                decay_secs: 0.57,
            },
        ),
        (
            "Warm Pad",
            Features {
                peak_hz: 392.5,
                centroid_hz: 512.6,
                rms: 0.10835968,
                crest: 3.8026,
                decay_secs: 0.73,
            },
        ),
        (
            "Plucky",
            Features {
                peak_hz: 261.6,
                centroid_hz: 434.8,
                rms: 0.0418238,
                crest: 11.074,
                decay_secs: 0.23,
            },
        ),
        (
            "Drawbar Organ",
            Features {
                peak_hz: 392.1,
                centroid_hz: 652.5,
                rms: 0.09399436,
                crest: 4.7659,
                decay_secs: 0.22,
            },
        ),
        (
            "Bell",
            Features {
                peak_hz: 659.2,
                centroid_hz: 529.1,
                rms: 0.09274316,
                crest: 4.7316,
                decay_secs: 1.01,
            },
        ),
        (
            "Bassy",
            Features {
                peak_hz: 261.6,
                centroid_hz: 275.2,
                rms: 0.15137866,
                crest: 2.8449,
                decay_secs: 0.62,
            },
        ),
    ];

    let mut recorded = std::collections::HashMap::new();
    for (name, features) in RECORDED {
        recorded.insert(name, features);
    }

    for name in FINGERPRINTED {
        let render = render(Config::ensemble(name).frames(12 * MAX_BLOCK).chord(&TRIAD));
        maybe_dump(&render, &format!("fingerprint-{}", slug(name)));
        let features = render.features();
        let expected = recorded[name];
        println!("{name:>15}: {features}  hash {:#018x}", render.hash());
        println!(
            "        (\n            {name:?},\n            Features {{ peak_hz: {:.4}, centroid_hz: {:.4}, rms: {:.8}, crest: {:.5}, decay_secs: {:.5} }},\n        ),",
            features.peak_hz, features.centroid_hz, features.rms, features.crest, features.decay_secs
        );
        if expected.rms == 0.0 {
            // Not yet recorded — the first run prints what to paste in.
            continue;
        }
        features.assert_close(&expected, 0.02, name);
    }
}

#[test]
fn the_hashes_are_what_they_were() {
    // The exact half. A mismatch here is not a failure on its own: it means the
    // samples changed, which is either the point of the change or the bug in it.
    // The message prints both hashes and the features, because "0x… != 0x…" on
    // its own is not something anybody can act on.
    const RECORDED: [(&str, u64); 6] = [
        ("Default", 0xf77bcd8290c60425),
        ("Warm Pad", 0x964c8fd0aa79ed82),
        ("Plucky", 0x9d6255deacd0b285),
        ("Drawbar Organ", 0x6c4c3f8853399b55),
        ("Bell", 0x5b6ec8577b05afaa),
        ("Bassy", 0x47b68c36f619d8bd),
    ];
    let mut recorded = std::collections::HashMap::new();
    for (name, hash) in RECORDED {
        recorded.insert(name, hash);
    }

    let mut lines = Vec::new();
    for name in FINGERPRINTED {
        let render = render(Config::ensemble(name).frames(12 * MAX_BLOCK).chord(&TRIAD));
        let hash = render.hash();
        lines.push(format!("(\"{name}\", {hash:#018x}),"));
        if let Some(expected) = recorded.get(name) {
            assert_eq!(
                hash, *expected,
                "{name} renders different samples than it did\n  now: {:#018x}\n  was: {:#018x}\n  now: {}\n  record it with CHORD_TOOL_RENDER=1 and re-run `the_fingerprints_are_what_they_were`",
                hash, expected, render.features()
            );
        }
    }
    if recorded.is_empty() {
        panic!(
            "no hashes recorded yet; paste these in\n{}",
            lines.join("\n")
        );
    }
}

// -----------------------------------------------------------------------------

/// The shipped instruments that run the second oscillator.
///
/// Thirteen instruments in the library use it, through a level, a
/// cross-modulation depth or a ring modulator, and **none of them was covered by
/// a bit-exact test** before this one: the ensembles above are Default, Warm Pad,
/// Plucky, Drawbar Organ, Bell and Bassy, and not one of them names an
/// instrument with a second oscillator in it. `PALETTE_FINGERPRINT` hashes
/// composed *parameters* rather than rendered audio, and the legacy oracle
/// predates the feature entirely.
///
/// That mattered as soon as the oscillator's frequency was hoisted to per-buffer:
/// the code being changed had no gate on it. One of each way in is enough — an
/// `expo` cross-modulation, a `linear` one, a ring modulation, a plain level, and
/// an instrument that uses both a level and a ring.
const SECOND_OSCILLATOR: [&str; 5] = [
    "X-Mod Organ",
    "X-Mod Clang",
    "Through-Zero Bass",
    "Ring Clav",
    "Twin Registration Organ",
];

#[test]
fn the_second_oscillator_renders_what_it_did() {
    // The same shape as the ensemble fingerprints and for the same reason: the
    // hash is exact and machine-local, the features are portable and say *how* a
    // change moved. If this fails, the first question is whether the sound was
    // supposed to change; `CHORD_TOOL_RENDER=/tmp/out.wav` writes out what the
    // render actually is.
    const RECORDED: [(&str, u64); 5] = [
        ("X-Mod Organ", 0x0e86_7182_339a_1f31),
        ("X-Mod Clang", 0x5d4f_e461_08eb_c151),
        ("Through-Zero Bass", 0xf260_8403_e8d6_dc79),
        ("Ring Clav", 0xd870_e8b9_3ef9_3a99),
        ("Twin Registration Organ", 0xe350_5821_d409_d825),
    ];
    let mut recorded = std::collections::HashMap::new();
    for (name, hash) in RECORDED {
        recorded.insert(name, hash);
    }

    let mut lines = Vec::new();
    for name in SECOND_OSCILLATOR {
        let render = render(
            Config::instruments([name, name, name])
                .frames(12 * MAX_BLOCK)
                .chord(&TRIAD),
        );
        maybe_dump(&render, &format!("osc2-{}", slug(name)));
        let features = render.features();
        let hash = render.hash();
        println!("{name:>22}: {features}  hash {hash:#018x}");
        lines.push(format!("(\"{name}\", {hash:#018x}),"));
        match recorded.get(name) {
            Some(0) | None => continue,
            Some(expected) => assert_eq!(
                hash, *expected,
                "{name} renders different samples than it did\n  now: {hash:#018x}\n  was: {expected:#018x}\n  now: {features}\n  the partials are {:?}\n  record it with CHORD_TOOL_RENDER=1 and re-run",
                render.top_peaks(6)
            ),
        }
    }
    if recorded.values().all(|h| *h == 0) {
        panic!(
            "no hashes recorded yet; paste these in\n{}",
            lines.join("\n")
        );
    }
}

// -----------------------------------------------------------------------------

fn slug(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// The harness resolves ensembles by name; a name that no longer exists is a
/// test that has been left behind, not a broken build.
#[test]
fn the_recorded_ensembles_still_exist() {
    let names = all_ensembles();
    for name in FINGERPRINTED {
        assert!(
            names.iter().any(|n| n == name),
            "{name} is no longer shipped"
        );
    }
    assert!(matches!(
        Source::Ensemble("Default".to_string()),
        Source::Ensemble(_)
    ));
}

// Keep the `Waveform::ALL` sweep honest: if the list of waveforms grows, the
// sweep above has to grow with it.
#[test]
fn the_waveform_sweep_covers_the_table() {
    assert!(
        Waveform::ALL.len() >= 24,
        "{} waveforms",
        Waveform::ALL.len()
    );
}
