//! The audio callback must not allocate.
//!
//! `Engine::process` runs on the audio thread, and an allocation there is not a
//! performance problem but a correctness one: the allocator takes a lock, and a
//! lock the audio thread waits on is a dropout of unbounded length. The code is
//! written to allocate nothing — the voice pool, the string buffers, the effect
//! bank, the reverb tanks, the delay lines and the analyser are all sized before
//! the stream starts — and this is the test that keeps it true.
//!
//! The counter itself lives in the harness (`harness::count_allocations`), which
//! installs the global allocator for every test binary that includes it. It is
//! armed per thread, so the rest of a parallel suite — and the test harness
//! itself — can carry on allocating while one test watches its own thread.

mod harness;

use chord_tool::synth::{Engine, SynthParams, Waveform};
use chord_tool::timing::Callback;
use harness::{
    count_allocations, fill_the_pool, prepare, worst_case_ensemble, Config, Event::Tweak, MAX_BLOCK,
};

// -----------------------------------------------------------------------------
// The test
// -----------------------------------------------------------------------------

#[test]
fn the_counter_can_see_an_allocation() {
    // First, that the hook works at all. A counter that never fires would make
    // every assertion below pass for the wrong reason, and this is the cheapest
    // possible way to know that it does not.
    let (value, passes) = count_allocations(|| {
        let v: Vec<u32> = (0..4096).collect();
        v.iter().sum::<u32>()
    });
    assert_eq!(value, 4096 * 4095 / 2);
    assert!(
        passes > 0,
        "the allocation counter saw nothing while a 16 kB vector was built"
    );
}

/// The engine's own build, run under the counter.
///
/// The tests below say what the *callback* does not allocate, which is only a
/// claim with content if the engine is built by allocating. A pool that grew on
/// first use would make them pass and mean nothing.
fn assert_the_engine_is_built_by_allocating() {
    let params = SynthParams::defaults();
    let (_, passes) = count_allocations(|| Engine::build(&params, 48_000.0, 2, Callback::shared()));
    assert!(
        passes > 100,
        "an engine with a 121-voice pool and twenty effect slots was built with {passes} \
         allocator passes; something is being allocated lazily instead"
    );
}

/// A config with every voice sounding and the whole bus running.
fn worst_case(frames: usize) -> Config {
    let config = Config::built(worst_case_ensemble())
        .frames(frames)
        .block(MAX_BLOCK);
    // A plucked string is the one voice with a `Vec` whose length is a function
    // of the sample rate, so it is the one worth watching for a buffer that is
    // grown on first use.
    let config = config.at(
        0,
        Tweak(Box::new(|p: &SynthParams| {
            p.mid.waveform.set(Waveform::Pluck as i32 as f32);
        })),
    );
    fill_the_pool(config)
}

#[test]
fn the_callback_allocates_nothing() {
    assert_the_engine_is_built_by_allocating();

    // Everything that happens before the first buffer happens here: the engine,
    // the pool, the racks, the reverb tanks, and the handful of allocator passes
    // per `play_stab` that `allocate` makes on the *interface* thread when it
    // splits a chord across the registers. None of that is the callback's
    // problem.
    let mut prepared = prepare(worst_case(8 * MAX_BLOCK));
    prepared.trigger_everything();

    let mut buffer = vec![0.0f32; prepared.block * prepared.channels];
    let buffers = prepared.frames / prepared.block;
    let (loudest, passes) = count_allocations(|| {
        let mut loudest = 0.0f32;
        for _ in 0..buffers {
            prepared.engine.process(&mut buffer, &prepared.params, None);
            for sample in &buffer {
                loudest = loudest.max(sample.abs());
            }
        }
        loudest
    });

    assert_eq!(
        passes, 0,
        "{passes} allocator passes over {buffers} buffers of {} frames, with the whole pool \
         sounding and every effect running",
        prepared.block
    );
    assert!(
        loudest > 0.0,
        "the callback ran but never produced a sample, so nothing above meant anything"
    );
}

#[test]
fn the_callback_allocates_nothing_at_any_block_size() {
    // A block size is the device's choice, not the program's, and a piece of
    // state sized per buffer rather than per second would only allocate at the
    // sizes that do not divide it.
    assert_the_engine_is_built_by_allocating();

    for block in [16usize, 64, 512, 1024, MAX_BLOCK] {
        let mut prepared = prepare(worst_case(4 * MAX_BLOCK).block(block));
        prepared.trigger_everything();

        let mut buffer = vec![0.0f32; prepared.block * prepared.channels];
        let buffers = prepared.frames / prepared.block;
        let (_, passes) = count_allocations(|| {
            for _ in 0..buffers {
                prepared.engine.process(&mut buffer, &prepared.params, None);
            }
        });
        assert_eq!(passes, 0, "{passes} allocator passes at a block of {block}");
    }
}

#[test]
fn the_callback_allocates_nothing_with_the_analyser_and_the_meter_running() {
    // The analyser publishes its levels into shared slots at the end of every
    // buffer and the meter reads an atomic. Both are easy to write in a way that
    // allocates once per buffer, and neither is exercised if the taps are empty.
    let mut prepared = prepare(worst_case(2 * MAX_BLOCK));
    prepared.trigger_everything();
    let peak = std::sync::atomic::AtomicU32::new(0);

    let mut buffer = vec![0.0f32; prepared.block * prepared.channels];
    let (_, passes) = count_allocations(|| {
        for _ in 0..prepared.frames / prepared.block {
            prepared
                .engine
                .process(&mut buffer, &prepared.params, Some(&peak));
        }
    });
    assert_eq!(
        passes, 0,
        "{passes} allocator passes with the meter running"
    );
}
