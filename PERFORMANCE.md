# Testing and measuring the audio

How the sound is regression-tested, how the program is stressed, how it is
profiled, and what the measurements have already found. The narrative version of
*why* the DSP is shaped as it is stays in [README.md](README.md); the parameter
reference stays in [REFERENCE.md](REFERENCE.md). This file is about machinery.

The plan below is staged, and each stage is useful on its own: the offline render
harness (1) is worth having even if nothing after it ever happens, and the stress
matrix (2) is worth having even if the profiling (3, 4) never finds anything.

## 1. Where we are

| | |
| --- | --- |
| Correctness | 985 unit tests, `cargo test`, no audio device needed |
| Audio regression | 23 render tests in `tests/render.rs` |
| Stress | 10 load tests in `tests/stress.rs`, 3 more under `--ignored` |
| Allocation checking | 4 tests in `tests/allocation.rs` — the callback makes zero passes |
| Benchmarks | 37 criterion benchmarks on the real path, in seven groups |
| Profiling | `samply`, with a documented loop; nothing automatic |
| Instrumentation | the callback's share of its buffer deadline in `debug.log`, and named scopes behind `CHORD_TOOL_TIMING` |
| CI | none, by choice: `scripts/check.sh` is the gate and it is run by hand |

`cargo test` is about thirty seconds in a debug build and under four in release;
the audio suites are most of it, because a debug build's DSP runs at roughly real
time and there is a lot of audio. `scripts/bench.sh` is four minutes and measures
nothing that can fail.

### What was here before, and why it was replaced

Six `#[ignore]`d tests in `src/synth.rs` printed a wall-clock number and a share
of one core. They were honest and they were weak in two specific ways, and both
of them are why stage 4 exists rather than a fifth one being added:

- **They measured the wrong thing.** `the_voice_pool_costs_what_we_think_it_does`
  built voice groups and ticked them directly, so it measured `Voice::tick` — a
  real part of the cost, but not the bus (EQ, chains, faders, pan, sends, aux
  returns, master curve, clip, analyser). Nothing measured the actual signal path
  end to end.
- **They were not comparable across runs.** A first pass measured the *baseline*
  slower than everything it was the baseline for, because the first case in the
  list ran with a cold cache and a CPU that had not settled. That was fixed once,
  by hand, with a warm-up pass. The same trap was waiting in every new
  benchmark, and a threshold that fires on it is a threshold somebody turns off.

They are still there, and two of them are still worth running: they are the home
of the legacy oracle and the palette fingerprint, which are the *gates* for every
optimisation in §8. What was retired is their authority over the numbers.

## 2. The engine, out of the cpal closure — done

`Engine` is extracted. It owns the voices, the effect bank, the analyser and the
four curves, and `process(&mut self, data, params, peak)` is the body of the old
closure, moved rather than rewritten:

```rust
pub struct Engine { /* groups, click, fx, analyzer, four curves, five EQ states */ }

impl Engine {
    /// Build the audio side: the voice pool, the effect bank, the curves and the
    /// analyser. The trigger handles come back separately because they belong to
    /// the interface thread.
    pub fn build(params: &SynthParams, sample_rate: f32, channels: usize) -> (Self, Voices);

    /// One buffer of the real signal path. Allocates nothing, locks nothing.
    pub fn process(&mut self, data: &mut [f32], params: &SynthParams, peak: Option<&AtomicU32>);
}
```

Two things fell out of doing it:

- **The trigger side had to become a type.** `Synth` used to hold
  `Vec<StabGroup>` and `Vec<VoiceHandle>` *and* the audio-side voices, so a public
  constructor taking the voices could not be written without exposing two private
  types. [`Voices`](src/synth.rs) is the fix and is a better shape anyway: it
  is what the interface thread owns, every method on it is a handful of atomic
  stores, and `play_stab` now takes `&SynthParams` because the numbers that decide
  how a note is *triggered* — unison, detune, glide — are the same live parameters
  the audio thread reads.
- **`apply_channels` had to stop being a method.** It writes three composed
  channels and a mixer block into a `SynthParams`, which needs nothing from a
  `Synth` — and a `Synth` owns an audio device that a test cannot create. It sits
  beside the `apply_mixer` and `apply_channel` free functions it was already
  built out of, for the reason the comment there already gave.

The extraction is a **pure move**. The bit-identity oracle
(`every_old_waveform_and_envelope_shape_still_renders_the_same` against
`LegacyVoice`) and `PALETTE_FINGERPRINT` both still hold, and they are what makes
the stage 5 optimisations checkable.

## 3. Stage 1 — the offline render harness — done

`tests/harness/mod.rs` is the harness; `tests/render.rs` is what it asserts. A
config says what to play, a render is the samples that came out:

```rust
let render = harness::render(
    &Config::ensemble("Drawbar Organ")
        .block(64)
        .frames(4 * MAX_BLOCK)
        .chord(&TRIAD),
);
```

`Source` is an ensemble by name, three instruments by name, or one `VoicePatch`
in every register. `Event` is a stab, a release, a silence, a metronome click, or
a `Tweak(&SynthParams)` — a real parameter change, applied on the main thread
between two buffers, exactly as the interface applies one. A `Render` answers
`peak`, `rms`, `hash`, `is_silent`, `non_finite`, `top_peaks`, `features` and
`write_wav`.

Events are quantised down to the block that contains them, which is where the
scheduler puts them, and `MAX_BLOCK` is 4096: an event on a `MAX_BLOCK` boundary
is exactly on time at every block size that divides it. That is what makes a
block-size comparison a comparison of the *sound*.

### What is asserted

Nineteen tests, in rough order of value per line:

1. **Invariants, swept over the whole shipped library.** Nothing non-finite,
   nothing above full scale, everything audible — for all 42 shipped ensembles
   and all 155 shipped instruments, each played in all three registers.
   A shipped preset that renders silence, or a patch that only behaves in its own
   register, is invisible in the file and obvious here.
2. **Block-size invariance**, at 16, 64, 512, 1024 and 4096, with a held chord
   and again with a parameter moving mid-render. Bit-identical, asserted on the
   samples and not only on the hash. This is the single most valuable structural
   test in the plan: it catches every per-buffer cache that was not invalidated,
   every piece of state that leaked across a buffer boundary, and every
   coefficient computed at the wrong granularity. It is also what will make the
   stage 5 optimisations safe.
3. **Sample-rate invariance of *pitch*, not of samples.** 44.1 and 48 kHz cannot
   produce identical samples, but they must produce the same note: the
   fundamental is measured to within 2 cents. A note is then checked against the
   keyboard at 48, 60, 69 and 76.
4. **The analysis itself.** A known tone at 55, 440, 1000 and 3000 Hz, at both
   rates, has to come back within a cent, and two tones 1000 Hz apart have to
   come back as two tones. Every other number in the file is produced by that
   transform, and a transform that is subtly wrong would make all of them agree
   with each other and none of them agree with the sound.
5. **Null tests.** A muted master, three registers at zero volume, an empty
   schedule: all exactly zero, not nearly. A render has to *end* at rest too —
   the envelope reaches zero and the voice goes idle, so a render whose tail is
   nonzero is a state that never settled.
6. **Edges.** Notes at 0, 12, 96, 108, 126 and 127; every one of the 24
   waveforms, at unison 1 and at unison 4.
7. **Golden hashes and five spectral features**, recorded per ensemble in
   `tests/render.rs`. The hash is exact and therefore only meaningful on the
   machine that recorded it; the features — peak, centroid, RMS, crest and the
   time to fall 30 dB — are the portable half, compared with a 2 % tolerance, so
   a failure says *how* the sound moved rather than only that it did.

### One thing the sweep found

A single note is not one note. `allocate` spreads it over the registers as
itself, an octave below and an octave above, so a "pitch" test has to silence the
neighbours to have a pitch to measure at all. `one_note_is_voiced_as_the_note_and_its_octaves`
is the assertion that says so, and `Config::middle_register_only` is the helper
the pitch tests use. That is a design decision being written down where it can
break, not a workaround.

### Golden audio, and the human in the loop

No `.wav` files are committed — a megabyte of binary in review is worse than
useless. The committed artifact is a hash plus the five features, and
`CHORD_TOOL_RENDER=<path>` makes every render write a WAV as it runs:

```
CHORD_TOOL_RENDER=/tmp/out.wav cargo test --test render \
    the_fingerprints -- --nocapture
```

The machine decides *that* something changed; the ear decides whether it was an
improvement. That escape hatch is the whole reason this is worth automating
rather than eyeballing, and it is why a fingerprint failure prints the features
and the top six partials next to the number that moved.


## 4. Stage 2 — load and stress — done

`tests/stress.rs`, plus `tests/allocation.rs` for the callback's allocation count
and the counter it needs. Three commands, in the order you would reach for them:

```
cargo test                                          # everything, ~38 s
cargo test --release --test stress                  # the same in ~4 s
cargo test --release --test stress -- --ignored     # the soak and the deep fuzz
```

### The matrix

Every dimension is a value a user can reach, so the matrix is not a hypothetical
worst case but the corner of the reachable space. What runs by default:

| Test | What it sweeps |
| --- | --- |
| `the_worst_case_survives_every_rate_and_block_size` | 44.1 / 48 / 96 kHz × blocks 16 / 512 / 4096 |
| `a_parameter_moved_mid_render_does_not_break_anything` | eight synth rows, swept both ways, mid-render |
| `every_voice_of_the_pool_survives_every_waveform` | all 24 waveforms with all 120 voices sounding |
| `a_dense_plan_plays_through_the_engine` | 64 cells × 4 takes at 40, 120 and 240 bpm, through the planner |
| `a_long_progression_plans_in_reasonable_time` | 256 bars, timed |
| `the_worst_case_allocates_nothing_once_the_pool_is_full` | the callback, with the racks running |
| `hostile_files_never_panic` | 200 mutated files × 6 parsers |
| `a_binary_file_never_panics_the_midi_reader` | 2,000 random and truncated MIDI files |
| `a_corrupt_project_file_never_panics` | 1,000 random and every truncation of a real project |

"The worst case" is one ensemble, built in the harness rather than shipped:
every one of the eighteen insert slots filled with a maximised effect, every band
of every register's curve at an end of its range, both sends wide open, both aux
units running, the master at the top of its range, and 120 of the 121 voices
sounding at four voices of unison each. Whether that is *reachable* is not in
doubt — it is eighteen clicks and a fader.

`maxed()` pushes every declared parameter to the top of its range except `mix`,
which stays where the kind put it. That exception is not squeamishness: a fully
wet reverb with its predelay at 120 ms and a fully wet delay with its time at two
seconds both, correctly, produce nothing at all for the first tenth of a second.
An insert with no dry path and a tail that has not arrived yet is a true reading
of the effect and a useless stress case.

### The soak

Sixty seconds at the top of the matrix, the whole pool struck at the start, and a
long tail behind it. It asserts that no sample is ever non-finite, that the
output stays inside full scale, that the callback allocates nothing across 5,625
buffers, that nothing *accumulates* — measured as a level in the last ten seconds
against a level ten seconds in, because the master clip puts a ceiling of exactly
one on the peak and hides a system quietly charging up — and, the half a clipped
output cannot make on its own, that the last ten seconds are not dramatically
more expensive per frame than the first ten.

That last assertion is the one that earns its place. A tail that has decayed into
the subnormal range sounds *identical* and costs tens to hundreds of times more
per operation on a CPU that has not been told to flush it, and the output peak
cannot see it. Timing can.

What the soak deliberately does **not** assert is that the tail reaches silence.
The maximised aux delay has its feedback at 0.95 on a two-second line and the aux
reverb has its size at the top of its range, so a wash that is still audible a
minute later is the effect working, not failing. The level is printed.

### Hostile files

Every `from_toml` in the crate promises that a wrong-shaped file reads as a
default rather than a panic, and the promise is worth more than the tests that
assert it on well-shaped input, because the input that breaks it is the input
nobody thought of. `mutate()` applies one of eight structural mutations —
truncate, cut a run out of the middle, duplicate a run, double the whole file,
remove a closing quote, replace a number with `nan`/`inf`/`1e999`, swap a byte for
one of thirty structurally interesting ones, chop the end — driven by a seeded
xorshift, so a failure is reproducible from its seed and there is no corpus to
keep. Every mutant goes to *every* parser, not just the matching one: a file of
the wrong shape for a parser is exactly the case where "reads as a default" has
to hold.

Two hundred mutants by default — 1,200 parses, since every mutant goes to every
parser — and 60,000 mutants, or 360,000 parses, under `--ignored`. No panics in
either. The MIDI reader and the project decoder get bytes rather than
TOML, including every truncation of a real file, which is the mutation most
likely to walk off the end of a length field.

### Where the allocations went

The counter itself is in the harness (`harness::count_allocations`) and installs
a replacing global allocator for every test binary that includes it — the one
`unsafe` in the repository, in a test, about which the library's
`#![forbid(unsafe_code)]` has nothing to say. It counts *passes* rather than
allocations: a buffer allocated and freed once per buffer is exactly as
unacceptable on the audio thread as one that is only allocated.

The first version of the test armed the counter around the whole render and
failed with about two thousand passes. None of them were in the callback. The
engine's own state — 121 voices with their buffers, twenty effect slots, the
tanks, the lines — is *built* by allocating and dropped at the end of a render,
and `Drop` is a pass through the allocator like any other. What the callback
does had to be measured on its own, which is why the harness has a `Prepared`
that can be driven a buffer at a time.

The callback is clean: zero passes at 16, 64, 512, 1024 and 4096 frames, with the
whole pool sounding, eighteen inserts and both aux units in the path, the
analyser publishing its levels and the meter reading its atomic.

## 5. Stage 3 — instrumentation — done

`src/timing.rs`. Two halves, because they answer two different questions and have
two different costs.

### The callback's deadline

The number that matters for a real-time thread is not milliseconds but the
fraction of the buffer deadline that was used. At 48 kHz and 512 frames the
deadline is 10.7 ms, and a callback that averages 10 % of it is comfortable right
up to the buffer where it takes 105 %, which is a dropout.

`Callback` keeps that fraction's last, mean and peak value plus a count of buffers
that went over — a **software xrun counter**. `Engine::process` opens with one
`Instant::now()` and closes with `self.timing.record(started.elapsed(), deadline)`,
where the deadline is derived from the buffer's own frame count and the sample
rate rather than assumed:

```rust
let deadline = Duration::from_secs_f64(data.len() as f64 / self.channels as f64 / self.sample_rate as f64);
```

Two `Instant`s and four relaxed atomic stores per buffer is about forty
nanoseconds against a ten millisecond deadline — four parts per million. **This
half is always on.** A diagnostic that has to be switched on before it can be
asked for is not there when the question is asked, and "my sound is crackling" is
exactly the question nobody can reproduce on demand. The peak is a
compare-exchange rather than a load-then-store, because a machine with more than
one audio thread can run two callbacks at once and the loser of that race would
silently drop the worst buffer of the two.

`debug.log` gets one line a second, from the output tap thread that was already
ticking at 60 Hz:

```
[TIME] callbacks 4687, load 2.9% last / 3.4% mean / 41.7% peak, 0 over deadline
```

### Everything else

The scheduler's per-bar planning, the interface's per-frame draw, import, export
and start-up are not real-time and are not measured against a deadline. What is
useful there is a distribution, and a distribution costs a mutex and a `VecDeque`
push per call — so those are `Scope`s, RAII timers that add to a named bucket when
they drop, and they are **off** unless `CHORD_TOOL_TIMING` is set.

```rust
let planning = crate::timing::Scope::new("scheduler.bar");
// … decide what this bar holds …
drop(planning);   // before the wait, or the scope reports the tempo
```

The scopes are `scheduler.bar`, `tui.frame`, `import`, `export`,
`startup.synth` and `startup.library`. Each keeps a running count, total and
longest, plus a 512-sample window for the percentiles — so a scope running for an
hour costs what it cost in its first second, and the log line describes the *last
second* rather than the whole history. Percentiles are nearest-rank, which never
interpolates between two samples and so never reports a time that did not happen:

```
[TIME] callbacks 4687, … | scheduler.bar 2x mean 0.84ms p50 0.81ms p99 1.42ms max 1.42ms | tui.frame 187x mean 0.21ms …
```

Dropping the scope where the work ends matters. The scheduler's planning scope is
dropped before `play_bar`, which waits out the rest of the bar; the draw loop's is
dropped before `event::poll`, which waits for a key. A scope that included either
wait would report the tempo and the typist.

### What is verified

- Nine unit tests on `Callback` and the scope arithmetic, including the boundary
  case that finishing exactly at the deadline is a miss, that a zero-length
  buffer does not divide by zero, and that the recent window does not grow.
- `the_engine_counts_the_buffers_it_processed` — the counters read through the
  real engine, so the wiring is tested and not only the arithmetic. It makes no
  claim about a *debug* build staying inside its deadline, because a debug build's
  DSP genuinely runs several times slower than the audio it produces; that is what
  the release profile is for.
- `a_timing_line_is_tagged_so_it_can_be_grepped_apart_from_the_rest` and
  `the_output_tap_writes_the_timing_line_once_a_second` — the log path end to end,
  including the second of real time it takes the tap to tick over.
- **The golden hashes still pass.** They were recorded before any of this existed,
  so a counter that perturbed a single sample would have failed the render suite.
  That is the acceptance test for instrumentation on the audio path, and it is
  what `the_instrumentation_does_not_change_the_sound` says out loud.


## 6. Stage 4 — the profiling workflow — done

`benches/audio.rs`, `scripts/bench.sh`, `scripts/bench_table.py` and
`scripts/check.sh`. Criterion is the only dev-dependency, with default features
off: the plotting half pulls in a rendering stack for charts nobody reads, and
`--save-baseline` plus a table is the whole of what is wanted here.

```sh
scripts/check.sh              # the gate: tests, clippy, and the tests in release
scripts/check.sh --load       # and the soak and the deep fuzz
scripts/bench.sh before       # measure, record as "before"
scripts/bench.sh after        # ... make a change, measure again ...
critcmp before after          # cargo install critcmp
python3 scripts/bench_table.py   # or just print the current numbers
```

`check.sh` is the deterministic half and every step in it either passes or fails,
on any machine, at any speed. `bench.sh` is the wall-clock half and never fails.

### The benchmarks

Thirty-seven measurements, in seven groups, each **single-variable** — which took
two attempts to get right. The first version varied the voice count *and* the
rack at once, and the resulting table said that one voice cost 388 µs and no
voices cost 524 µs, because "no voices" was the worst-case bus and "one voice"
was a bare one. A benchmark group whose rows differ in two things measures
neither.

| Group | The variable | Rows |
| --- | --- | --- |
| `engine` | voices | none, a triad, 6, 12, 120 — a bare bus, so the difference between two rows is the difference between those voice counts |
| `engine` | which oscillator | 120 voices on a neutral sine, on an `expo` cross-modulation, and on a ring modulation |
| `rack` | the rack | nothing, one insert, six inserts, thirteen bands a register, both aux units — on **silence**, so the rack is the whole signal |
| `worst` | nothing | one row: 120 voices and the whole rack at once, which is the headline everything else decomposes |
| `ablate` | one subsystem on or off | the analyser, on three signals. The pair *is* the measurement: two rows a few seconds apart in one process, which is the only comparison here that does not have to fight the machine's five per cent run-to-run drift |
| `bus` | rate × block | 44.1 / 48 / 96 kHz × 64 / 512 / 4096 frames, at the worst case with no voices |
| `planner` | the plan | 1 and 256 bars at 16 cells, 1 bar at 64 cells |
| `render` | the ensemble | four ensembles, through the whole harness including sample collection |

The `rack` group's first version ran at 120 voices, and the differences it was
built to show — 0.06 to 0.24 ms — were three to nine per cent of the total, which
is inside the noise of a hundred and twenty voices and a settling delay line. On
silence the same differences are the whole number. A maximised effect on silence
is not a maximised effect on a chord — a compressor's envelope and a gate's
threshold both behave differently — but the arithmetic a slot does per sample is
the same arithmetic, and this is the version that can be read.

Throughput is reported in **frames per second**, which is the unit that means
something for an audio engine: the device needs one frame per frame time, so
`scripts/bench_table.py` prints the ratio as `headroom`. 30× means the callback is
using about three per cent of its deadline; 1× means it cannot keep up at all.
A `bus` row is measured against its own rate rather than against 48 kHz, because
the same per-frame cost at 96 kHz leaves half the headroom.

Three things make the numbers trustworthy. The notes are struck **once, before
the measurement, and held** — none of the configs carries a release — so
criterion's warm-up covers the attack and the filter's settling and every measured
buffer is a sustaining one. Each group uses a `Prepared` built once outside the
timed closure, so nothing allocates inside it. And every group is a *difference*
away from a floor: `engine/no voices` is the same bus with the same racks and
nothing playing.

### The measurement discipline

1. **Release builds, always.** Debug is for correctness. Every number in this
   file is `--release`.
2. **Warm up, then measure.** Pass one is thrown away. The feature benchmark that
   used to live in `src/synth.rs` learned this the expensive way.
3. **`std::hint::black_box` on every input and every result**, or the optimiser
   deletes the work.
4. **Parametrise, do not fix.** A bench that only measures 120 voices cannot see
   a cost that is per-voice-pool-slots rather than per-sounding-voice.
5. **Compare, do not threshold.** `--save-baseline before`, make the change,
   `critcmp before after`. A threshold on a laptop either never fires or fires on
   the weather.
6. **Prefer deterministic counts where they exist.** Instruction counts are
   unavailable on macOS, so the deterministic axes here are *bytes allocated*
   (zero, asserted) and *samples rendered* (golden hashes). Gate on those; report
   wall-clock.

### The tooling, and what each is for

Two constraints shape this list: the machine is a Mac, and a wall-clock threshold
on a laptop is noise.

| Tool | What it gives | Verdict here |
| --- | --- | --- |
| [`criterion`](https://github.com/bheisler/criterion.rs) | statistical wall-clock, `--save-baseline` / `critcmp` | **in use**: the local regression-hunt tool, not a gate |
| `divan` | lighter criterion alternative, attribute API | upstream is stale; skip unless it revives |
| [`iai-callgrind`](https://github.com/iai-callgrind/iai-callgrind) | instruction counts, deterministic | best signal-per-flake, but needs valgrind and is Linux-only — no use on a Mac |
| [`samply`](https://github.com/mstange/samply) | sampling profiler, Firefox Profiler UI | **the primary profiler here**: one command, no `sudo`, works on macOS |
| `cargo instruments` / `xctrace` | Apple's Time Profiler, signposts | the fallback when samply's output is too coarse, and the only option for OS-level detail |
| `perf` + `cargo flamegraph` | Linux sampling | not available |
| [`pprof-rs`](https://github.com/tikv/pprof-rs) | in-process sampling, prints a flamegraph | for profiling inside a soak test where an external profiler cannot attach |
| [`puffin`](https://github.com/EmbarkStudios/puffin) / `tracing-flame` | manual instrumentation scopes | the Stage 3 `Scope` covers what is wanted; revisit only if a visual timeline is ever needed |
| [`dhat`](https://github.com/nnethercote/dhat-rs) | heap profile, dhat-viewer | for the *load* paths (import, export, library load), not the callback |
| `assert_no_alloc` | a global allocator that panics inside a no-allocation zone | **not needed**: the hand-rolled counting allocator in `tests/harness` was twenty lines and has no cross-thread warnings to filter |
| `proptest` | property-based parameter search | the upgrade path for the "every combination stays finite" tests |
| `cargo-fuzz` | coverage-guided fuzzing | the upgrade path for the hostile-file tests |

### How to profile it, concretely

The benchmarks say *that* something is slow; the profiler says *where*. Neither
is a substitute for the other, and neither is a threshold.

```sh
cargo bench --bench audio -- --profile-time 10 'engine/120 voices'
```

`--profile-time` runs the benchmark for ten seconds without the measurement
machinery and leaves the process running under whatever profiler you attach. That
is the loop: `cargo build --release`, run something that plays the engine hard,
attach `samply record -- <the binary>`, and read the flamegraph in the Firefox
Profiler. On a Mac with no `sudo`, that is the whole workflow.

For the load paths — start-up, import, export — `dhat` or a `CHORD_TOOL_TIMING`
run answers the allocation and wall-clock questions without a profiler at all,
which is usually faster than reaching for one.


## 7. What the measurements already say

Everything in this section is the **before** picture: the numbers that said where
to look, taken with the benchmarks in §6 and the stage 2 harness. What was done
about them, and what it bought, is §8.

A first pass, run while writing this file, measuring the two unconditional
per-sample transcendental calls in `Voice::tick` at the same 5.76 M calls/second
that 120 voices at 48 kHz produce:

| Call | Cost | Share of one core |
| --- | --- | --- |
| `midi_to_hz` (`440 · 2.0f32.powf(…)`) | 25.9 ms/s | **2.6 %** |
| the SVF coefficient `(2π·cutoff/sr).sin()` | 15.9 ms/s | **1.6 %** |
| `exp2` (for comparison, and what `powf` could become) | 13.4 ms/s | 1.3 % |

And the whole bus, measured through the real path on this machine — a MacBook,
one audio thread, `cargo bench`, release, 48 kHz and 512 frames unless the row
says otherwise. One core is 100 %; the deadline at 512 frames is 10.67 ms:

| Case | Per 512 frames | ns a frame | Share of one core | Share of the deadline |
| --- | --- | --- | --- | --- |
| bus floor, bare (no voices, nothing in the rack) | 312 µs | 610 | 2.9 % | 2.9 % |
| 3 voices | 383 µs | 747 | 3.6 % | 3.6 % |
| 6 voices | 429 µs | 838 | 4.0 % | 4.0 % |
| 12 voices | 539 µs | 1 053 | 5.1 % | 5.1 % |
| 120 voices | 2 682 µs | 5 238 | 25.1 % | 25.1 % |
| **120 voices, the whole rack** | **2 830 µs** | **5 527** | **26.5 %** | **26.5 %** |
| — the same case after §8 | 2 094 µs | 4 090 | **19.6 %** | **19.6 %** |
| — and after the second pass | 2 001 µs | 3 908 | **18.8 %** | **18.8 %** |
| the worst-case bus at 96 kHz, no voices | 534 µs | 1 043 | 5.0 % | **10.0 %** |

And the floor itself, which is where the second pass found its win. The final
numbers, from the recorded `pass2` baseline, against where the document first
measured them:

| Case | First measured | After both passes | |
| --- | --- | --- | --- |
| bus floor, bare | 312 µs | **75.6 µs** | **−76 %** |
| 3 voices | 383 µs | 130.9 µs | −66 % |
| 6 voices | 429 µs | 163.3 µs | −62 % |
| 12 voices | 539 µs | 246.3 µs | −54 % |
| 120 voices | 2 682 µs | 1 788 µs | −33 % |
| **120 voices, the whole rack** | **2 830 µs** | **2 001 µs** | **−29 %** |
| a whole offline render, `render/Default` | 11.6 ms | 3.80 ms | **−67 %** |

Four things fall out of that table.

**The headline is 26.5 % of the deadline before either pass and 18.8 % after**,
so the worst configuration the panels can produce runs at 5.3× real time. At 512
frames that is 2.00 ms of work in a 10.67 ms buffer, which is the margin a slower
machine or a busier one has to eat into before anything crackles.

**The marginal voice costs 38.6 ns a frame**, from `(5238 − 610) / 120`, so
0.185 % of a core per voice at 48 kHz. A hundred and twenty of them is 22.2
points of the 26.5, and the bus they are mixed through is the other 4.3.

**The rack is cheap; the floor is not.** Eighteen maximised insert slots under a
full pool add 148 µs — 1.4 points of a core, for eighteen effects. The *floor* —
four EQ stages, twenty effect slots with nothing in them, the master curve, the
clip and the analyser — is 2.9 % of a core on silence, before a note is played,
and it is a *bigger* number than the whole rack. What that 2.9 % is made of has
not been measured yet: the EQ is not it, because `Eq::tick` returns before
touching a section when the curve is flat, and the aux units are not it either,
because the Default ensemble's `reverb_mix` is zero. That leaves the analyser and
twenty early-outs, and separating them is a `--profile-time` run rather than a
guess — so it is on the §8 list as a measurement to make, not a claim.

**The sample rate is free per frame and expensive per second.** The 96 kHz rows
cost what the 44.1 kHz rows cost per frame — the DSP is per sample and a
coefficient that has already been computed does not care what rate it was derived
from — but the deadline is half as long, so the headroom halves with it.

So **two coefficients that are constant for an entire note, recomputed 48 000
times a second per voice, are about 3.9 points of the 26.5** — 2.6 for
`midi_to_hz` and 1.6 for the SVF's `sin`, scaled from the probe above. With
`key track`, `filter env`, `lfo cutoff` and `velocity` on they are joined by up to
four more `exp2` calls per voice per sample, which is most of the gap between a
plain chord and one with every modulation running. The voices are the cost; the
rack is not, and neither is the EQ.

The rack ladder, on silence, which is the version of it that can be read — the
same group at 120 voices buries differences of this size:

| Rack | Per 512 frames | Over the floor | Share of one core |
| --- | --- | --- | --- |
| nothing | 308 µs | — | 2.9 % |
| one insert (a maximised chorus, every register) | 355 µs | 47 µs | 0.4 % |
| six inserts, every register | 480 µs | 172 µs | 1.6 % |
| thirteen bands a register, at ±12 dB | 345 µs | 37 µs | 0.3 % |
| both aux units, maximised | 336 µs | 28 µs | 0.3 % |
| all of it at once | 545 µs | 237 µs | 2.2 % |

Which was worth measuring, because it kills a hypothesis: eighteen insert slots
filled with maximised effects is **1.6 % of a core**, not the "second measured
worst case" the plan assumed. The EQ at ±12 dB on all three registers is 0.3 %.
The aux reverb and delay, tank and all, are 0.3 %. None of that is worth
optimising, and the plan's assumption that the effect rack would be the second
hot spot was wrong.

One thing the numbers cannot say. `engine/120 voices` measured 2.62, 2.68, 2.74
and 2.79 ms across four runs of identical code — a six per cent spread on a
laptop with thermal throttling and a browser open. **_Differences below about
five per cent at a full pool are not resolvable run to run_**, which is exactly
why the discipline is `--save-baseline` and `critcmp` rather than a threshold,
and why the gate is the deterministic half: the hashes and the allocation count,
neither of which can be talked into moving.

And the rest, produced by the stage 2 harness rather than by a throwaway test:

| Finding | Where | Number |
| --- | --- | --- |
| The callback allocates nothing | `tests/allocation.rs` | 0 allocator passes across 5,625 buffers, at every block size from 16 to 4096, with the whole pool and the whole rack running |
| Five maximised effects settle into the subnormal range instead of reaching zero | `where_the_effect_tails_settle` | tail 12 s after release: flanger `1.4e-45`, phaser `1.3e-44`, filter `4.1e-44`, distortion `1.7e-42`, wah `1.5e-42` |
| The maximised rack leaves a self-sustaining tail | `a_minute_of_the_worst_case_does_not_drift` | rack alone settles at `1.1e-37` and stays there; rack + both aux units + 120 voices is still at full scale a minute in |
| The worst case costs 27 % of its buffer deadline | `scripts/bench.sh` | 2.88 ms for 512 frames, against a deadline of 10.67 ms — 3.7× headroom |

The subnormal tails are the interesting one. A one-pole or an allpass whose input
is gone computes `z = z * coef`, and once `z` is small enough that the product
rounds back to `z`, the state stops decaying — it parks, forever, at whatever the
last representable value was. In the subnormal range the representation is
coarser, so it parks *higher* than it otherwise would. Nothing is audible: `1e-45`
is not a sound. But every subsequent operation on that state is on a subnormal,
and on a CPU without flush-to-zero that is tens to hundreds of times the cost of
the same operation on a normal float, for as long as the effect is loaded. The
analyser has had a `FLOOR` for exactly this reason since before this file existed;
the effects do not.

Those are the findings the plan exists to produce, and the instrumentation is now
in place to produce the next one without writing a throwaway test first.

## 8. The optimisations this implied — stage 5, done

The order changed once the numbers came in. The rack, the EQ and the aux — the
things the plan assumed were the expensive half — are 2.1 % of a core between
them, and a voice is 0.185 % each. Everything worth doing was in the voice.

### 1. The per-sample constants, hoisted to per-buffer — done, and it is the win

`midi_to_hz(note)` and the state-variable filter's `2·sin(πf/sr)` depend on the
note and on parameters, and the note only moves when the voice is gliding or a
pitch LFO is running. The cutoff only moves when the filter envelope, a cutoff
LFO, key tracking or velocity-to-cutoff is in use. `Voice::cache` runs once per
buffer, before a sample, and proves which of the two cannot move; `Voice::tick`
uses the cached value when the proof holds and runs the original expression when
it does not.

| | before | after | |
| --- | --- | --- | --- |
| 120 voices, bare bus | 2.682 ms | 1.886 ms | **−30 %** |
| 120 voices, the whole rack | 2.830 ms | 2.094 ms | **−26 %** |
| 12 voices | 539 µs | 461 µs | −14 % |
| 6 voices | 429 µs | 402 µs | −6 % |
| 3 voices | 383 µs | 358 µs | −7 % |
| six inserts in every register, on silence | 480 µs | 512 µs | **+7 %** |
| silent floor | 312 µs | 329 µs | +5 %, all of it noise |

The worst case goes from **26.5 % of the buffer deadline to 19.6 %** — 3.8×
headroom becomes 5.1×. The measured saving is much larger than the 3.9 points the
libm probe predicted, and the reason is that the two transcendentals were never
the whole cost: hoisting them also hoists the dozen atomic loads, clamps and
`exp2`s that fed them, all of which were per-sample.

The last two rows are the *other* change: the subnormal flush adds a compare and
a select to every state update, and it costs about seven per cent on a rack of
eighteen maximised effects — two per cent once a hundred and twenty voices are in
front of it. That is the price of item 4 below, and it is worth stating as a price
rather than hiding it inside a total.

The acceptance gates were all **unchanged**: the legacy oracle bit-for-bit, the
six golden hashes, block-size invariance at five sizes, the pitch across two
sample rates, and the allocation count. That is what made this safe to do to the
hottest function in the program, and it is why the harness was built first.

The trade is the one §9 records: a note struck while a buffer is being rendered
is heard at the start of the next one, up to 10.7 ms later. Offline, every event
is applied between buffers, so the render suite cannot see it at all.

### 2. `powf` → `exp2` — dropped, and the measurement is why

Once the frequency is computed once per buffer per voice, the whole population of
`midi_to_hz` calls is 121 voices × 93.75 buffers a second ≈ 11,300 a second. The
`exp2` version saves 12.5 ns of the 25.9, so it is 0.14 ms a second: **0.014 % of
a core**, in exchange for re-recording every hash in the repository. Not worth a
line of code, and this is the clearest example in the file of a plan item that a
measurement retired.

### 3. The depth guards — audited, nothing to change

Every per-sample transcendental in `Voice::tick` was already behind a zero guard:
`env_factor` behind `env_amount != 0 && contour != 0`, `lfo_factor` behind
`lfo_cut_depth > 0`, `vel_factor` behind `vel_cutoff > 0`, `key_factor` behind
`key_track > 0`, and the LFO itself behind a `wanted` that covers all four of its
destinations. The audit's result is that the guards are the reason the plain
120-voice case costs 25 % rather than 40, and that there was nothing to fix.

### 4. The subnormal flush — done

`settle` in `fx_dsp.rs`: below `FLOOR` — the level that file has treated as
silence since before the effects existed, 140 dB below full scale — a recursive
state is stored as zero. It is applied to the one-pole, the first-order allpass,
the delay line, the two state-variable pairs, the flanger's and the phaser's
feedback registers, the bitcrusher's sample-and-hold and the two envelope
followers.

| Tail twelve seconds after the note, maximised effect | before | after |
| --- | --- | --- |
| flanger | `1.4e-45` | `0` |
| phaser | `1.3e-44` | `0` |
| filter | `4.1e-44` | `7.7e-7`, still decaying in the normal range |
| distortion | `1.7e-42` | `0` |
| wah | `1.5e-42` | `0` |
| the whole maximised rack | `1.1e-37`, parked | `0` |

Every effect's **head is bit-identical** — `0.9922` for the flanger, `0.7182` for
the filter, and so on, the same numbers the diagnostic printed before the flush —
which is the evidence that this is a change to the tails and not to the sound.
The render suite's hashes are unchanged too, because no shipped ensemble puts an
effect in a register's chain, so nothing the suite pins ever reached the code.

`no_effect_parks_at_any_setting` is the gate: every kind, at the top of every
parameter range, driven hard and then given ten seconds of silence, asserting that
the output either reaches exactly zero or stays in the normal range. It caught a
second offender the deliberate measurement had missed — the phaser's feedback
register, which held its own output and parked at `2.9e-36` — which is the
argument for writing a property rather than re-recording five numbers.

### 5. Everything else on the old list

- **A cheaper SVF coefficient** is subsumed by (1): the `sin` is now computed once
  per buffer, where a polynomial approximation would save 0.014 % of a core.
- **Block processing** and **SIMD** stay on the list and stay last. After (1) the
  voice is 4,061 ns a frame for a hundred and twenty of them — 19.3 % of the
  deadline — and the only way to know what is left inside it is a
  `--profile-time` run rather than a guess.
- **What the silent floor is made of** is still unmeasured, and it is now the
  *larger* half of the remaining cost at a low voice count: 2.9 % of a core with
  nothing playing at all.

### The acceptance gate, and how it was used

Every change above went through the same five checks, and the results are part of
the record rather than a footnote:

| Check | (1) hoist | (4) flush |
| --- | --- | --- |
| `every_old_waveform_and_envelope_shape_still_renders_the_same` | unchanged | unchanged |
| `the_hashes_are_what_they_were` | unchanged | unchanged |
| `the_block_size_does_not_change_the_sound` | unchanged | unchanged |
| `the_sample_rate_does_not_change_the_pitch` | unchanged | unchanged |
| `the_callback_allocates_nothing` | unchanged | unchanged |

Neither change needed a re-record. The flush would have, if any shipped ensemble
had an effect in a chain; the fact that none does is itself worth knowing, and it
is why `the_hall_tank_is_the_one_that_shipped` — the reverb's own bit-exact
oracle — still passes untouched: the tank does not park, so it was left alone.

---

## 8a. The second pass — the floor, and where it actually was

Three changes, run against pre-registered thresholds. One missed its bar and was
reverted; one cleared its bar by a hair; and the third was not on the list at all
and turned out to be the whole story.

### The ablation that started it

The open question §10 left behind was what the 2.9 % silent floor was made of. The
obvious suspect was the analyser — four taps of thirteen bandpass biquads is 52
filters a sample, run whether or not anybody is looking at the Spectrum panel — so
the first change was to gate it, and the benchmark grew a pair of rows that switch
it on and off. **A pair in one run, seconds apart, is the only number here that
does not have to fight the machine's five per cent run-to-run drift**, and it is
the method the rest of this section uses.

| Signal | Analyser on | Off | Saving |
| --- | --- | --- | --- |
| silence, bare bus | 329.8 µs | 281.4 µs | 48.4 µs = **14.7 %** |
| silence, whole rack | 558.2 µs | 515.1 µs | 43.1 µs = 7.7 % |
| 120 voices, whole rack | 2 206 µs | 2 149 µs | 57.8 µs = 2.6 % |

So the analyser is **half a per cent of one core**, not the two-thirds of the
floor the hypothesis needed it to be. The measurement was worth making before
writing any code, and it is why the next question was the right one: if 48 µs of
319 µs is the analyser, what is the other 270?

### The answer: the engine walks a silent pool

The engine ticks **every voice in the pool, every sample**. At unison one only six
of the twenty-four voices in a group can ever sound, so at any moment most of the
hundred and twenty-one are silent — and each one was paying a call into a
five-hundred-line function to find that out. `Voice::tick` was too big to inline,
so the early-out that says "this voice has nothing to say" cost a call frame,
a stack spill and a return, a hundred and twenty-one times a sample.

The fix is three lines of structure and no arithmetic at all: the early-out
becomes the whole of an `#[inline]` `tick`, and the five hundred lines move to
`tick_sounding`.

| Row | Before | After | |
| --- | --- | --- | --- |
| `engine/no voices` | 325.4 µs | **75.6 µs** | **−77 %** |
| `engine/3 voices` | 351.5 µs | 130.9 µs | −63 % |
| `engine/6 voices` | 376.9 µs | 163.3 µs | −57 % |
| `engine/12 voices` | 444.0 µs | 246.3 µs | −45 % |
| `engine/120 voices` | 1 835.7 µs | 1 788 µs | −3 %, inside the noise |
| `worst/120 voices, whole rack` | 2 094 µs | 2 001 µs | −5 %, inside the noise |
| `render/Default` (the whole harness) | 11.6 ms | 3.80 ms | **−67 %** |

A sounding voice pays exactly one call either way — the wrapper is inlined, so
there is no extra hop — which is why the full pool does not move. The A/B that
says so, on the same row minutes apart and with twelve-second sampling as a
release build: **1.897 ms with the split against 1.922 ms without, with the
confidence intervals overlapping.** The compiler had never inlined the big
function, which is what made the split both possible and free.

This is the largest single number in this file. The floor a machine has to clear
before it plays a note went from 610 ns a frame to 147.

### The analyser gate, kept

The Spectrum panel is one of seven stops on the Tab key and the levels it draws
reach nothing else, so the interface now tells the callback whether that panel is
on screen and the callback skips the banks when it is not. It clears them on the
way back in, so the panel opens on the sound that is playing rather than the one
that was.

That is worth **2.6 % of the worst case and 15 % of the floor**, which is a hair
over the bar set for it — 16.7 % on the first run, 14.7 % on the second. It is
kept because the cost it removes is paid continuously, whether or not anything is
playing, and because it cannot possibly be wrong: `Analyzer::tick` returns nothing
and writes only its own banks, and `the_analyser_cannot_change_the_audio` renders
the same configuration with the banks on and off and asserts the hashes are equal.

### The second oscillator's hoist, measured and reverted

Thirteen of the hundred and fifty-five shipped instruments run the second
oscillator, and its frequency was still a `midi_to_hz` per sample — the stage 5
hoist had caught the first oscillator and missed the second. Extending the same
cache to it, and to the `expo_mean_shift` the exponential cross-modulation divides
by, is bit-identical: forcing both back to the live path leaves every hash
unchanged.

It also **missed the bar this pass set for it**. The threshold was 10 % on a
cross-modulated row; the ablation says 4.6 %, and 2.4 % on a ring-modulated one.
The two numbers decompose the way they should — the ring row never touches the
expo shift, so its 2.4 % is the frequency alone and the other 2.2 % is the mean —
which is what makes them believable rather than drift, but believable at 4.6 % is
still under 10 %.

So it was reverted, and the numbers above are what it was worth. The threshold
was set against a libm probe that turned out to overestimate the marginal cost of
a `powf` inside real code by about three times, so the bar was probably too high —
but a rule that gets overridden whenever the measurement is inconvenient is not a
rule. Re-applying it is a fifteen-minute change to `Coefficients` and `cache`, and
what it was measured at is written down here.

### The test gap it left behind — closed

The second oscillator had **no bit-exact test at all**. The six fingerprinted
ensembles are Default, Warm Pad, Plucky, Drawbar Organ, Bell and Bassy and not one
of them names an instrument with a second oscillator in it;
`PALETTE_FINGERPRINT` hashes composed *parameters* rather than rendered audio; and
the legacy oracle predates the feature. The code the hoist touched had no gate on
it.

`the_second_oscillator_renders_what_it_did` closes it: five instruments, one for
each way into the second oscillator — an `expo` cross-modulation, a `linear` one,
a ring modulation, a plain level, and an instrument that uses both a level and a
ring — each with a recorded hash. It stays whether or not the hoist ever comes
back, because the gap was there either way.

### The two thresholds that were set and what happened to them

| Change | Bar | Measured | Outcome |
| --- | --- | --- | --- |
| the analyser gate | the floor drops ≥ 15 % | 16.7 % / 14.7 % | kept |
| the second oscillator's hoist | ≥ 10 % on its row | 4.6 % | **reverted** |
| the channel snapshot | ≥ 8 % on `engine/120 voices` | not attempted — see below | — |

### The channel snapshot — not attempted

The plan's third item was to snapshot the forty-four channel parameters into plain
floats once per buffer, on the theory that thirty-two atomic loads through an
`Arc` per voice per sample were a large part of what was left. The second-pass
measurements changed the arithmetic on that:

- the split removed 250 µs from the floor, which was the part of the theory that
  was testable without writing the change;
- what is left of a voice is **28 ns a voice-frame**, and the loads are the
  cheapest thing in it — all voices in a register share one `ChannelParams`, so
  the thirty-two loads hit the same handful of cache lines and pipeline three to a
  cycle. That is a guess at four to seven nanoseconds, not a measurement.

It is a medium-sized change to the hottest function in the program, and it now
rests on an estimate rather than on a measurement — which is exactly the position
this file says not to be in. It wants a profile of `Voice::tick`, not another
guess, and it is the first item in §10.

## 9. What you decided

All three questions this section used to ask are answered, and the answers are
what the three stages above are built on:

1. **Split into a library and a binary.** Done. `src/lib.rs` is 25 `pub mod`
   lines and `src/main.rs` is two, and the audio path is reachable from `tests/`
   and from `cargo bench` because of it.
2. **Buffer-granularity control is acceptable.** Verbatim: *"i'd rather the
   underlying engine be testably correct and a bit of a buffer lag between UI and
   engine, rather than the ui to be exactly to the ms perfect to the output but
   the synth engine suffers in any way."* That clears the largest item in §8 —
   the per-sample coefficients may become per-buffer coefficients — and the
   block-size-invariance test from Stage 1 is the gate that keeps it honest.
3. **Automation is my call.** So: deterministic checks gate `cargo test`;
   wall-clock numbers are reported, not thresholded, except where a threshold is
   meaningful (the soak's cost-per-frame comparison, the planner's two-second
   budget); no CI, no nightly runner, no committed audio.

## 10. What is left

The worst case is **18.8 % of its buffer deadline** where it started at 26.5 %,
the floor a machine clears before it plays a note is **a quarter of what it was**,
and nothing in the effect state parks in the subnormal range.

What remains is one change worth making and two that are not yet worth guessing
at:

1. **The channel snapshot** — forty-four `ChannelParam` reads per voice per sample
   turned into plain fields once per buffer (§8a, last part). Thirty-two atomic
   loads through an `Arc` is the last obvious per-voice cost, and the estimate is
   four to seven nanoseconds of the twenty-eight a voice-frame costs. It needs a
   profile first: `cargo bench -- --profile-time` against `Voice::tick`, which is
   now a five-hundred-line function with an inlined wrapper and therefore easy to
   find in a flamegraph.
2. **Block processing** and **SIMD**, last, and still for the same reason.
3. **Re-apply the second oscillator's hoist** if 4.6 % on the thirteen
   cross-modulated and ring-modulated instruments is worth twenty lines to you. It
   is written down in §8a with its measurement and the one command that brings it
   back.

The infrastructure is done: the render harness, the stress matrix, the allocation
counter, the deadline counters, the benchmarks, the two scripts, and
`scripts/check.sh` as the gate. The measurement discipline in §6 is the part to
keep, and the second pass is the argument for it — the change that mattered was
not on the list, and the one that was on the list and measured well was worth
4.6 %.
