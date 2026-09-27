# Changeset — 1.0.0, the working tree against `4e42a5b`

What is in this batch, what it replaces, where to look hardest, and what was run
to check it. This is a **review document**, not release notes: `REFERENCE.md` is
what the program does and `PERFORMANCE.md` is how the audio is tested.

`Cargo.toml` is at **1.0.0**. The version is not shown anywhere in the program and
is not in `REFERENCE.md`; it exists in the manifest and in `Cargo.lock`, so
bumping it is a one-line change plus a build.

Everything here is uncommitted. `4e42a5b` ("big changes to ui/sinko") is the last
commit, so this batch is the whole build-out since then — the library split, the
instrument/effect/EQ engine, the panels, and the testing and performance
infrastructure.

## The short version

| | |
| --- | --- |
| Modified | 19 tracked files, ~23 600 insertions |
| Added | 29 files, ~22 000 lines (including these three documents) |
| Deleted | `patches.toml`, `src/presets.rs` |
| Tests | 985 unit + 37 integration, all green |
| Benchmarks | 37 criterion measurements on the real audio path |
| Dependencies | one new dev-dependency (`criterion`); no new runtime ones |

The batch grew in layers, and the layers are worth reading separately:

1. **The engine became a library** so the audio path could be tested at all
   (`src/lib.rs`, thin `src/main.rs`).
2. **The sound model was rebuilt** — `patches.toml` and `src/presets.rs` are
   gone, replaced by instruments, placements and ensembles.
3. **The interface grew** five more panel views, a rhythm editor and a chord log.
4. **The audio path was instrumented and measured**, and two optimisations came
   out of that.

## Read this first: breaking changes

**`patches.toml` is deleted and nothing migrates it.** The old file held the
whole sound-design state as `[[patches]]` entries; the library is now split into
`instruments.toml` (register-neutral voices) and `ensembles.toml` (three
placements plus a mixer). Any local `patches.toml` and any saved patch names are
**not** read, not converted and not warned about. This was a deliberate decision
rather than an oversight — *"blow away all my local user data. we don't want to
use the old patches terminology"* — but it is the one change in the batch that
destroys something a user could have made.

**`settings.toml` is new and gitignored.** The key, tempo and master volume are
written a moment after they stop moving and read back at start-up. Not created
until one of them changes, so a fresh checkout has nothing extra.

**Five `*.user.toml` files are new and gitignored.** Ships-empty files the app
writes: `instruments`, `ensembles`, `eq_presets`, `fx_presets`, `rhythms`. Each
layers over its tracked default *by name*, so a user entry with a shipped name
wins.

**The binary is now thin.** `src/main.rs` calls `chord_tool::tui::run_interactive()`;
the modules are declared in `src/lib.rs`. This is what makes `tests/` and
`cargo bench` possible, and it is why four modules that were private are now `pub`.

## What changed, by area

Feature-level detail is in [REFERENCE.md](REFERENCE.md); this is the map.

### The sound model — instruments, placements, ensembles

Replaces `patches.toml` / `src/presets.rs` (both deleted).

| New file | What it is |
| --- | --- |
| `src/voice.rs` | `VoicePatch` — what a sound *is*, register-neutral. Plus `ComposedChannel`, the voice-plus-placement pair the audio layer takes. |
| `src/instrument.rs` | The named library: 155 shipped voices, plus `*.user.toml`. |
| `src/ensemble.rs` | `Ensemble` = three `Placement`s + `MixerSettings`. 42 shipped, plus `*.user.toml`. |
| `src/wavetable.rs` | Single-cycle tables from harmonic recipes, plus the drawbar registrations. |

The split is the load-bearing idea: a `Placement` carries volume, transpose, pan,
sends, EQ and the insert chain, and a `VoicePatch` carries none of it, so the same
instrument can be auditioned in any register without disturbing the mix.

### The voice and the effects

| File | What it is |
| --- | --- |
| `src/synth.rs` | Grew from a small synth to the full engine: a 37-parameter voice, 24 waveforms, cross-modulation, phase distortion, Karplus-Strong, the bus, the mixer. |
| `src/fx.rs` | What an effect is: 15 kinds, 54 variants between them, 6 parameter slots each, and the preset library. |
| `src/fx_dsp.rs` | The algorithms, and the bank of twenty the callback runs. |
| `src/eq.rs` | The 13-band equaliser, the biquad cascade, and the 25 shipped curves. |
| `src/analyzer.rs` | The live spectrum: a bandpass bank per tap, four taps. |
| `src/timing.rs` | The callback's share of its buffer deadline, and the named scopes behind `CHORD_TOOL_TIMING`. |

### The interface

`src/tui.rs` is the bulk of the diff (+18 968 lines). It became seven panel
stops, four of which share one slot on screen. The notable additions: the paged
Synth table, the Ensembles and EQ panels, the Spectrum readout, the FX rack
panel, the Sinko rhythm editor, the History/Top log, the metronome panel, and the
`[MIDI]` chooser folded into one transport row.

`src/history.rs` is the per-run chord log — exact history plus a deduped top list.

### Rhythm

`src/rhythm.rs` and `src/rhythm_store.rs`: a one-bar step grid with four stacked
takes, per-cell holds and accents, per-pattern swing and mute, and 23 shipped
patterns. `src/arrangement.rs` is the seam playback and export share, so a groove
heard in the app is the groove in the file.

### Export and import

`src/midi.rs`, `src/smf.rs`, `src/project.rs`, `src/export.rs`: a progression
becomes Standard MIDI File notes, with the session document embedded, so an
export can be imported back with its rhythms intact.

### Testing, stress and measurement — the newest layer

| File | What it is |
| --- | --- |
| `tests/harness/mod.rs` | The offline render harness: configs, events, spectral analysis, the worst-case ensemble, the allocation counter. |
| `tests/render.rs` | 23 tests: invariants over the whole library, block and rate invariance, golden fingerprints. |
| `tests/stress.rs` | Matrices, the 60-second soak, the effect-tail gate, the file fuzzers. |
| `tests/allocation.rs` | The audio callback allocates nothing. |
| `benches/audio.rs` | 37 criterion measurements, in seven groups. |
| `scripts/check.sh` | The gate: tests, clippy, release tests, and optionally the soak and fuzz. |
| `scripts/bench.sh`, `scripts/bench_table.py` | Measure, and read the numbers back as a table. |
| `PERFORMANCE.md` | The record: what is measured, how, every number, and what is left. |
| `REFERENCE.md` | Every feature and every key, tersely and completely. A test parses its hotkey table and resolves it through the real keyboard map, so it cannot drift from the code. |
| `CHANGESET.md`, `MANUAL_TESTING.md`, `CHEATSHEET.md` | This document, the hands-on checklist, and the one-page key card. |

The measurement work found three things worth knowing: the analyser was running
52 filters a sample whether or not anyone was looking at the panel; five
maximised effects parked in the subnormal range and never decayed; and the engine
was paying a function call per silent voice per sample, which turned out to be
most of the floor. All three are fixed and documented.

## Where to look hardest

Ordered by how much a mistake would cost, not by diff size.

1. **`src/synth.rs`'s voice loop.** It is the hottest code in the program and it
   changed twice — the per-buffer coefficient cache, and the split of `tick` into
   an inlined idle check and an out-of-line `tick_sounding`. Both are meant to be
   bit-identical, and the golden hashes are what says so.
2. **The `patches.toml` removal.** Confirm you are happy losing any local patch
   library, because nothing reads the old format.
3. **`src/tui.rs`.** Nearly nineteen thousand lines of change, and the layout
   tests pin the geometry rather than the behaviour. Worth a real session with
   hands on the keys — that is what `MANUAL_TESTING.md` is for.
4. **The effect rack's state machine.** `FxState` holds the union of every
   algorithm's state so a slot can change type without allocating. The subnormal
   flush now touches every recursive state in it.
5. **The cheat sheet's hotkey table** is checked against `REFERENCE.md`'s by
   `the_cheatsheet_says_what_the_reference_says`, so a rebound key or a reworded
   action has to be made in both. That is deliberate, and worth knowing before you
   edit either.
5. **The `*.user.toml` layering.** A user entry silently wins over a shipped one
   of the same name; that is intentional but it is the kind of thing that hides a
   bad save.

## Verified — what was actually run

| Check | Result |
| --- | --- |
| `cargo build --release` | clean |
| `cargo test` (debug, ~32 s) | 985 + 4 + 23 + 10 passed, 0 failed |
| `cargo test --release` (~3.5 s) | same, 0 failed |
| `cargo clippy --all-targets` | 0 warnings |
| `rustfmt --check` on every touched file | clean |
| `cargo bench --no-run` | 0 warnings |
| `scripts/check.sh --load` | passed, including the soak and the 60 000-file fuzz |
| Release binary | starts, opens the audio stream, writes `debug.log` |

The suite is run in four test binaries, and the counts above are per binary. The
soak (`--ignored`) is a minute of audio asserting the engine does not grow, drift
or allocate; the deep fuzz is 360 000 parses across every TOML reader plus the
MIDI and project decoders, asserting no panic.

One caveat on the numbers in `PERFORMANCE.md`: this machine sat at a load average
of about 4 for the whole of the last session from something outside it, so any
figure quoted across two runs carries 3–5 % drift. The ones that matter are
same-run pairs or within-build A/Bs, and they say so where they appear.

## Not in this batch

- **The channel-parameter snapshot** — the one measured-adjacent optimisation
  left. It wants a profile first; `PERFORMANCE.md` §10 says why.
- **Block processing and SIMD.**
- **Re-applying the second oscillator's hoist**: built, proven bit-identical,
  measured at 4.6 % against a pre-registered 10 % bar, and reverted on principle.
  `PERFORMANCE.md` §8a has the measurement.
- **`bugs.txt` and `notes.txt`** are tracked and stale — `TODO.md` flags
  `bugs.txt` as three empty stubs. Left alone because deleting them is your call.
