# Changeset — 1.0.0, the working tree against `4e42a5b`

What is in this batch, what it replaces, where to look hardest, and what was run
to check it. This is a **review document**, not release notes: `REFERENCE.md` is
what the program does and `PERFORMANCE.md` is how the audio is tested.

`Cargo.toml` is at **1.0.0**. The version is not shown anywhere in the program and
is not in `REFERENCE.md`; it exists in the manifest and in `Cargo.lock`, so
bumping it is a one-line change plus a build.

The batch has been committed in pieces. `4e42a5b` ("big changes to ui/sinko") is
the base, and `1478a59` is the last commit, so the range from `4e42a5b` is the
whole build-out — the library split, the instrument/effect/EQ engine, the panels,
and the testing and performance infrastructure. What is still in the working tree
is the newest layer, listed under "Added after the batch was first written" below.

## The short version

| | |
| --- | --- |
| Range | `4e42a5b` … working tree: 51 files, ~50 300 insertions, ~7 600 deletions |
| Added | 29 files, ~22 000 lines (including the documentation) |
| Deleted | `patches.toml`, `src/presets.rs` |
| Tests | 1011 unit + 42 integration, all green |
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
| `src/ensemble.rs` | `Ensemble` = three `Placement`s + `MixerSettings`. 74 shipped, plus `*.user.toml`. |
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

`src/tui.rs` is the bulk of the diff. It became nine panel stops across three
blocks — Progression and Transport side by side, Sinko full width (the chord log
borrows its slot), and a six-view shared slot — of which the last six share one
slot on screen. The notable additions: the paged Synth table, the Ensembles and
EQ panels, the Spectrum readout, the FX rack panel, the Sinko rhythm editor, the
four-bar Loops recorder, the History/Top log, the metronome panel, and the
`[MIDI]` chooser folded into one transport row.

`src/history.rs` is the per-run chord log — exact history plus a deduped top list.

### Rhythm

`src/rhythm.rs` and `src/rhythm_store.rs`: a **bar** grid — steps, per-cell holds
and accents, per-bar swing and mute — gathered into named **patterns of one to
four bars**, and 19 shipped patterns. The four-bar ones are *phrases*: assigning
one spreads its bars across four consecutive chords. `src/arrangement.rs` is the
seam playback and export share, so a groove heard in the app is the groove in the
file, and it is also where the looper's four-bar bed is planned.

### Export and import

`src/midi.rs`, `src/smf.rs`, `src/project.rs`, `src/export.rs`: a progression
becomes Standard MIDI File notes, with the session document embedded, so an
export can be imported back with its rhythms intact.

### Testing, stress and measurement — the newest layer

| File | What it is |
| --- | --- |
| `tests/harness/mod.rs` | The offline render harness: configs, events, spectral analysis, the worst-case ensemble, the allocation counter. |
| `tests/render.rs` | 24 tests: invariants over the whole library, block and rate invariance, golden fingerprints. |
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

### Added after the batch was first written

The batch was reviewed by hand, and the review turned up seven things. Two are
bugs and they are the ones to read first; the rest are features, tests and one
piece of curation.

| | |
| --- | --- |
| **The registers were not voiced** ⚠ | `allocate` handed the three registers the chord's *tones* at their written pitch — low took the bottom note, high the top, mid whatever was left — and spread them by an octave only for a single note. A triad on middle C came out as **C4, E4 and G4**: three registers inside a major third, which the ear fuses into one instrument no matter how different the three patches are. Reported as "they all sound like the same instrument played three ways", and it was true. Past the fix a triad is a bass an octave below the root, the triad itself in the middle and the top note an octave above; the median high/low centroid ratio across the palette went from 2.80× to 7.12×. |
| **A register loaded half-applied** ⚠ | `apply_channel` — the function that loads an ensemble into the live parameters — wrote twenty-two of a voice's thirty-seven fields and dropped the other fifteen: the drive, the second oscillator (waveform, interval, level, FM amount, mode, ring), the wavetable position, the phase distortion, the feedback, the velocity contour, and the three string-model settings. `apply_voice`, which is what the instrument picker calls, wrote all thirty-seven. So loading an ensemble left every register holding the *defaults* for those fifteen, and a register only came fully alive when you picked an instrument for it — including re-picking the one it was already set to. **Fifty of the seventy-four ensembles lost at least one field, across a hundred and four register-slots.** |
| **`n` replaces one slot** | The group-action menu only opens for a run of two or more, so `replace` — set this slot to the chord being held, keep its rhythm and offset — had no way to be reached on a single slot. It has a key of its own again: `n`, which types `b` on Programmer Dvorak. `reverse`, `rotate` and `clear` stay menu-only, because none of them means anything applied to one slot. A test asserts that exactly these three have no key. |
| **The Ensembles panel is a window** | The list drew every row, so on any real terminal it ran off the bottom and the rows past the screen edge could be selected and never read. It now draws `ENSEMBLE_ROWS = 20` and scrolls with the cursor, which brings the panel to the same 34 rows as the Synth and EQ panels. Reached by arrowing, by `Tab`, and by a remembered row. |
| **Register isolation is now tested** | Five tests, at four levels — the live parameters, the effect bank, the rendered mix and the FX panel — each verified by breaking the code it covers and watching it fail. The audit found the racks are per register and the sends are post-fader; the tests are what keep that from being a fact about today's code. Detail below. |
| **The palette is re-ordered by role, and re-ordering is now free** | The showcases had been inserted as a block at the head, which pushed every everyday instrument — piano, Rhodes, Wurli, strings, brass, winds — past the first screen and left it full of metallic and experimental demos. The list is now a curation in four parts: twenty one-per-role instruments first, then the usable half of the showcase, then its edges, then the original palette by family. Two things had to change for the order to be a curation rather than a fact: `PALETTE_FINGERPRINT` sorts by name before hashing, so moving an ensemble up or down no longer looks like a change to its sound, and the showcase is identified by name rather than by position. Verified by shuffling the file and watching the suite stay green. |
| **Thirty-two showcase ensembles** | The palette had 42 ensembles and 32 of them were the *same instrument design* in all three registers — the ten that looked layered only were because a design had no bare name and another was substituted. The whole point of an ensemble is three different instruments, so the palette grew 32 ensembles at the head, one per design, each a genuinely different design low, mid and high, and 21 of them use the insert rack (which no shipped ensemble had used before). `Default` is still first. This was the first pass; the second pass below is what actually made every ensemble in the file three different designs. |
| **Every ensemble is three different instruments, and no two share a middle** | The first showcase pass fixed the head of the list and left the tail — the original palette and the drum kits aside — as one design copied across all three registers, which is exactly the thing the showcase existed to demonstrate. All 74 were re-voiced: each got a bass in `low` (from ten bass designs), the instrument its name promises in `mid`, and a lead in `high`, with `mid` forced to be a design **no other ensemble uses**. The whole `ensembles.toml` was rewritten. `every_shipped_ensemble_has_a_bass_a_mid_and_a_lead` now asserts the rule, including that the low register is a bass and the high one cannot be; the only exception is the three drum kits, where all three registers are percussion because a drum has no register. |
| **A pattern is one bar or four** | The rhythm model used to be *one bar per chord, four stacked takes per bar* — a take was a layer, and they averaged and decayed. That is gone: `RhythmBar` is a single grid, and a `RhythmPattern` holds **one to four bars**. A four-bar pattern is a *phrase*, and assigning one spreads its bars across four consecutive chords in a single undoable edit. `Jazz Chorus` and `Son Clave` ship as phrases rather than as one-bar fragments whose names carried the position. `rhythms.toml` was regenerated; the palette is 19 patterns. |
| **The four-bar Loops panel** ⚠ | A tap recorder for a *progression*, not a chord. It loads a bed of four bars — one chord four times, or the first four chords of the progression — loops it with the click, and records `` ` `` taps into **the bar the clock is on**, each sounding that bar's chord as it lands. On each pass the recorded hits play back with their own bar's chord, so the phrase is auditioned as it will sound. `[Record]` is three-state and non-destructive (load and loop → stop recording → unload), `Esc` stops without discarding the bars, `[Clear Bar]` empties one bar, and `[Save Loop As...]` writes all four into the pattern palette as one pattern that Sinko can then assign. It is a sixth view of the shared Synth slot, so it costs the layout no rows. This replaces the Sinko panel's `record` row, its four take lines and its `smooth` row outright. |
| **The looper shows all four bars, and records a key not a click** | Two things a test pass turned up. The panel drew one bar at a time, which hides the shape of the loop — whether the turnaround lands, which bars are empty — so it now draws **four grids**, one per line, with the cursor's bar marked and the clock's bar carrying the playhead — and a cell a hit is still sounding through draws as `=`, so a recorded length is visible as a line rather than inferred from a dot. `RhythmBar::sustain` is what computes it, and Sinko's grid draws the same way. And the tap recorded a *point*: `$` wrote a hit, so a note could never be longer than the bar's default hold. It now records the **key's down and up** — the press is the note's start, the key-up its end, and the distance between them becomes the hit's `hold`, rounded to the grid. A tap inside one cell is an ordinary hit; a note held over a bar line carries into the next bar; the cells it was held through are cleared, so a hold replaces the taps under it. Auto-repeat is absorbed (and the window slides, so holding the key can never become a burst), and a press that arrives with a note still open closes that note first — which is the fallback for a terminal that does not report key releases. |
| **A tap could leave a chord ringing** ⚠ | The reported "chord that doesn't stop playing after recording a few bars". The tap sounds on the audition voice, a stab group with no scheduled note-off — the release the rest of the engine relies on comes from the plan, and a tap has no plan. Every tap reset the group and started a new note, and the *last* one was never released, so it sustained until something else took the voice. The key-up now hands the voice back, and `pump_looper` is the belt to that pair of braces: a stop, an unload, a pause or a recording that ends while the key is still down all release it on the next frame. |

#### The registers were not voiced, in detail

`allocate` is the one function that decides what the three registers *play*. It
handed them the chord's tones at their written pitch:

```
1 note:   low = n−12,  mid = [n],        high = n+12
2 notes:  low = s0,    mid = [s1],       high = s1+12
3+ notes: low = s0,    mid = s1..sn−2,   high = sn−1
```

Only the single-note case spread anything. Every chord put the register named
**low** on middle C and the one named **high** a fifth above it, so a triad covered
0.63 octaves and a pad — where the high's transpose was 0 — covered 0.58. Three
patches inside a major third do not read as three parts; they read as one
instrument, whatever they are made of. Twelve of the thirty-two showcases measured
a high/low centroid ratio under 1.6×, five of them inverted (Twin Drawbars 0.30×,
CZ Organ 0.54×, Morph Choir 0.55×).

The block sizes said what the shape was always meant to be: `LOW_NOTES` is 1,
`MID_NOTES` is 4, `HIGH_NOTES` is 1, and the mid block's comment reads "the low
channel takes one, the mid up to four and the high one". It is a bass, a chord and
a descant. It is now voiced that way, with the top left to the descant past four
notes rather than the middle block being filled until it overflows — which keeps
every pitch class sounding, because the low and high are already doubling the ends.

**What moved.** The 20 `transpose = 12` values the showcase had grown were the
fudge that stood in for a real voicing; `allocate` owns the octaves now, so they
are 0 and those ensembles are re-voiced by the engine instead. `PALETTE_FINGERPRINT`
moved with them — it hashes the composed channels including their transposes — and
so did all six ensemble fingerprints with their feature vectors, all five
second-oscillator hashes, and the loudness envelope.

**Where it landed.** The median high/low centroid ratio across the palette went
from 2.80× to 7.12×. `Default`'s strongest partial moved from 329.6 Hz to 783.99 Hz
— the descant — and `Warm Pad`'s, `Bassy`'s and `Drawbar Organ`'s to 130.8 Hz, the
bass. The loudness spread went from 18.9 dB to 19.2 dB and the loudest peak from
0.7710 to 0.9365, which is what happens when the outer registers move an octave
out; nothing clips, and the spread's bar moved with it rather than being relaxed.
Percussion is the one place the model does not fit — a drum has no register — and
the three kit ensembles stay approximate.

#### The half-applied register, in detail

What it sounded like was **bleed between the registers**, which is how it was
reported: change the high instrument and the low voice seems to change. Nothing
was leaking — `ChannelParams` is three independent blocks, each owning its own
atomics, and neither writer can reach past the register it was handed. What was
happening is that the two writers disagreed about what a voice *is*, so the three
registers were not in the same state: the one you touched came alive and the other
two did not, and the mix shifted against itself.

What it cost in sound is larger than a balance shift. Every synthesis method that
lives in those fifteen fields was **not running** in the live engine:
`X-Mod Organ` and `X-Mod Clang` had no cross-modulation at all, `Through-Zero Bass`
had no through-zero FM, `Ring Clav` had neither ring nor drive, `Twin Registration
Organ` had one registration rather than two, every plucked-string voice used the
default string damping, and every wavetable morph sat at position zero. The
`the_second_oscillator_renders_what_it_did` golden had recorded the silent
behaviour, so it was passing while testing nothing.

The fix is structural rather than a longer field list: `apply_channel` now calls
`apply_voice` and adds only the placement, so there is exactly one list of what a
sound is. Two tests hold it: `loading_a_channel_writes_the_whole_voice` asserts
over the whole shipped palette that what the engine holds after a load equals what
the file says, and `changing_one_registers_instrument_leaves_the_others_alone`
asserts that editing one register leaves the other two bit-identical.

**Consequences for this batch's goldens.** The 5 second-oscillator hashes were
re-recorded, because the oscillators now run. The 6 ensemble fingerprints and all
6 feature vectors are **unchanged** — those six ensembles use none of the fifteen
fields, which is itself a useful confirmation that the fingerprint is measuring
the palette and not the transport. `PALETTE_FINGERPRINT` is unchanged, because it
hashes the resolved composed channel from the file rather than the live
parameters — the split doing its job. The palette's loudness spread is still
18.9 dB, `Snare` to `PWM Pad`, and the loudest peak is still 0.7710: the fix
changed timbre, not level.

The third one moved two numbers and one rule. `PALETTE_FINGERPRINT` covers the
rack and the mixer now — before this a chain was provably empty, so a fingerprint
that skipped it was complete; now a rack is part of a shipped sound and leaving it
out would have been a promise with a hole in it. The old test that asserted *no*
shipped chain existed is now a narrower one that keeps the half a fingerprint
still cannot make: the two aux units are the ones the old file implied, and the
delay send ships at zero everywhere. And the palette's own guard — that no two
shipped ensembles are the same sound — compares **middle registers**, which
discovered that the middle instrument is an ensemble's identity and forced every
showcase to pick a design no other ensemble uses. All 74 middles are now distinct.

#### Register isolation, and the tests that hold it

The half-applied register above was reported as *bleed* — "I change the high
instrument and the low voice sounds different" — and the follow-up question was the
obvious one: is the effect path per channel too? It is, and there are now five tests that
say so at four different levels, each verified by deliberately breaking the code
it covers and watching it fail:

| Test | What it holds | The mutation it catches |
| --- | --- | --- |
| `synth::tests::changing_one_registers_instrument_leaves_the_others_alone` | Editing one register's *voice* leaves the other two bit-identical | writing the voice into the wrong register |
| `fx_dsp::tests::one_registers_rack_state_cannot_reach_another` | The three racks are disjoint *runs of state*, not just disjoint parameters | `let start = 0` in `FxBank::chain`, so every register shares the low rack |
| `render::a_change_to_one_registers_rack_cannot_move_another` | A parameter of one register's rack cannot move another register in the rendered mix, and the send is **post-fader** | the same slicing, and separately a pre-fader send |
| `render::a_curve_on_one_register_cannot_move_another` | A curve on one register cannot move another | the mid bank reading the low bank's *state*, and the mid bank taking the high register's *coefficients* |
| `tui::tests::editing_one_registers_rack_leaves_the_other_two_alone` | The FX panel's column cursor writes the rack it names | `fx_slot_at` always addressing the low rack |

Three of these tests were wrong when first written, in the same way, and it is
worth recording because the mistake is invisible without a mutation:

- the first rack test used a *different effect kind in every register*. A change of
  kind makes `FxState::reconfigure` clear the slot on the next tick, so three
  registers pointed at one slice would each wipe the other and look correct. It
  passed with `start = 0`. Every register now runs the same kind and differs only
  in parameters;
- the first EQ test measured a register whose curve was **flat**, and `Eq::tick`
  returns its input untouched when the bank is flat — so the shared state was
  never read or written and the bug could not show. The measured register now
  carries a non-flat curve;
- both had a *mirror* assertion added afterwards, which is what stops them passing
  because the effect never ran at all.

That is the same lesson as the second-oscillator golden, one level up: an
assertion about a difference is only as good as the difference it can see, and the
only way to know is to break the thing on purpose and watch.

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
6. **The `*.user.toml` layering.** A user entry silently wins over a shipped one
   of the same name; that is intentional but it is the kind of thing that hides a
   bad save.

## Verified — what was actually run

| Check | Result |
| --- | --- |
| `cargo build --release` | clean |
| `cargo test` (debug) | 1011 unit + 42 integration, 0 failed |
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
same-run pairs or within-build A/Bs, and they say so where they appear. That drift
did eventually fail the soak on one run, which is worth recording: the soak
compares the cost of the first ten seconds against the last ten, and the last ten
are about half a second of *wall clock*, so a single scheduler stall doubled the
mean and failed a test about a sound. It compares **medians** per buffer now, which
a handful of stalls cannot move and a cost that really has doubled still does.

## Not in this batch

- **The channel-parameter snapshot** — the one measured-adjacent optimisation
  left. It wants a profile first; `PERFORMANCE.md` §10 says why.
- **Block processing and SIMD.**
- **Re-applying the second oscillator's hoist**: built, proven bit-identical,
  measured at 4.6 % against a pre-registered 10 % bar, and reverted on principle.
  `PERFORMANCE.md` §8a has the measurement.
- **`bugs.txt` and `notes.txt`** are tracked and stale — `TODO.md` flags
  `bugs.txt` as three empty stubs. Left alone because deleting them is your call.
