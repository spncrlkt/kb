# chord-tool

A terminal chord instrument. You play it with your two hands resting on the home
row: the **left hand picks a scale degree**, the **right hand picks a chord
transformation**, and the combination sounds immediately through a built-in
polyphonic synth. Chords you like can be appended to a looping progression and
played back against a metered transport.

The crate is named `chord-tool` (the repository is `kb`).

## Status

Working, but early. The audio path, music theory core, and TUI are functional
and covered by 92 unit tests. Several features are implemented in the data layer
but not yet wired to the UI — see [TODO.md](TODO.md).

The project did not compile as committed; two build errors (`E0063` in
`synth.rs`, `E0382` in `transport.rs`) were fixed so that `cargo run` works.

## Requirements

- A recent stable Rust toolchain (edition 2021; `let-else` is used, so 1.65+).
  Tested with cargo 1.91.
- A working audio output device — the app exits at startup with
  `no audio output device available` if none is found.
- A generous terminal. The UI is drawn as plain text with a full-screen clear
  each frame, and the focused **Synth Mixer** panel alone is 15 rows. Budget
  roughly **100×45**; below ~40 rows the layout will scroll.
- macOS, Linux, or Windows (audio via [cpal], terminal via [crossterm]).

## Build and run

```sh
cargo run            # debug build
cargo run --release  # smoother audio; recommended for actually playing
cargo test           # 92 tests, no audio device required
```

The binary is `target/{debug,release}/chord-tool`.

## Playing chords

Hold keys down together — chords are read from the *set* of keys currently held,
not from a sequence. Only the ten home-row keys participate in the chord
grammar.

### Left hand: scale degree

| Keys (QWERTY names) | Degree |
| ------------------- | ------ |
| `f`                 | I      |
| `a`                 | ii     |
| `a` `s`             | iii    |
| `d` `f`             | IV     |
| `d`                 | V      |
| `s`                 | vi     |
| `a` `s` `d`         | vii    |

`g` (left inner) is not used by the grammar. If you hold left-hand keys that
don't form one of these shapes, no degree resolves and no chord sounds.

### Right hand: transformation

There are two modes. Holding `h` selects **h-mode** (diatonic additions, always
in key); otherwise **j-mode** (absolute chord qualities, may leave the key).
Adding `;` to an h-mode shape is invalid by design.

**h-mode** — press `h` plus:

| Keys        | Suffix    | Meaning                    |
| ----------- | --------- | -------------------------- |
| (h alone)   | `maj7`    | diatonic seventh           |
| `h` `j`     | `add9`    | diatonic ninth             |
| `h` `k`     | `sus4`    | suspended fourth           |
| `h` `l`     | `6`       | diatonic sixth             |
| `h` `j` `k` | `maj9`    | seventh + ninth            |
| `h` `j` `l` | `13`      | seventh + thirteenth       |
| `h` `k` `l` | `7sus4`   | sus4 + seventh             |
| `h` `j` `k` `l` | `13(9)` | the full diatonic stack  |

**j-mode** — with no `h` held:

| Keys            | Suffix    | | Keys        | Suffix    |
| --------------- | --------- | - | ----------- | --------- |
| `j`             | `7`       | | `j` `k`     | `sus2`    |
| `k`             | `7b9`     | | `l` `;`     | `6/9`     |
| `l`             | `9`       | | `j` `l`     | `dim7`    |
| `;`             | `m7b5`    | | `j` `;`     | `7#9`     |
| `k` `l`         | `m9`      | | `k` `;`     | `aug`     |
| `j` `k` `l`     | `maj7#11` | | `k` `l` `;` | `7#11`    |
| `j` `k` `;`     | `mMaj7`   | | `j` `l` `;` | `11`      |
| `j` `k` `l` `;` | `13`      | |             |           |

Holding a left-hand key with no right-hand key gives the plain diatonic triad
(`C`, `Dm`, `Bdim`, …). The suffix is derived from the intervals actually
produced, so it is correct in both major and minor without special cases.

### Registers (latching one hand)

Playing a chord one-handed is awkward, so each hand can be latched:

- `'` locks the **right** register (physical `z` on a QWERTY keycap)
- `z` locks the **left** register (physical `/` on a QWERTY keycap)

Those are the Programmer Dvorak characters you actually type. The on-screen hint
still prints the QWERTY names, so it lists the two registers backwards — see
"Known issues".

A lock captures whatever that hand is holding at that moment. Afterwards, live
input wins **per side**: hold any left-hand key and it overrides the locked left
set, while the locked right set keeps filling in. This is how you audition
progressions with one hand free.

The `Chord:` readout shows the resolved result, so it reflects a latched
register exactly as the audio and `Enter` do.

### Transport

| Input            | Action                                  |
| ---------------- | --------------------------------------- |
| `Space` (once)   | play / pause                            |
| `Space` (twice)  | jump to the middle of the progression   |
| `Space` (3+)     | restart from bar 1                      |
| `Esc`            | quit (when no modal or edit is open)    |

Taps are resolved 300 ms after the last press, so a single tap has a short
delay before it registers.

While playing, the live chord is appended as one extra bar after the
progression, so you can jam over your own loop. Note length is the fraction of a
bar a chord sustains (`1/4`, `1/2`, `3/4`, `whole`) and is cycled from the mixer
panel.

### Panels

`Tab` / `Shift+Tab` cycles focus:

```
Transport → Progression → Synth Mixer → Synth Low → Synth Mid → Synth High → Synth Presets → (wrap)
```

Within a panel: `↑`/`↓` move the row and `←`/`→` adjust the selected value.

`Enter` commits the chord you are currently playing — the latched registers plus
whatever keys are down right now, with live input winning per side. It works
from **any** panel, so you don't have to navigate to the Progression panel to
capture a chord. In the Progression panel it inserts after the selected row;
anywhere else it appends. `Ctrl+Enter` always appends, even when no chord
resolves.

When nothing resolves, `Enter` falls through to the focused panel's own action:
edit BPM or track key, toggle loop or mute, load a preset, or — in the
Progression panel — offer to add a rest.

The **Presets** panel's last row, `[Save As...]`, opens a text prompt and writes
the full sound design to `patches.toml`.

## Layouts

The grammar is defined on **physical key positions**, never on the characters a
layout produces, so gestures are layout-independent.

```rust
// src/keyboard.rs
pub const ACTIVE_LAYOUT: Layout = Layout::ProgrammerDvorak;
```

Programmer Dvorak is **fixed for this version** — not configurable, by design.
Changing it (say, for QWERTY testing) means editing that line and recompiling,
so `Layout::Qwerty` is unreachable at runtime and shows up as a dead-code
warning.

The grammar is correct for Programmer Dvorak: the physical keys labelled `f`,
`h`, `k` emit `u`, `d`, `t`, which resolve to `LeftIndex`, `RightInner`,
`RightMiddle` — degree I with `sus4`, i.e. Csus4.

Two display rough edges remain:

- The key row and the register lines print *QWERTY* names via `qwerty_label()`,
  so the letters drawn are not the letters you type. Cosmetic.
- The lock-key hint is a hardcoded QWERTY string, and under Programmer Dvorak it
  is actively wrong — see "Known issues".

## Architecture

| File               | Responsibility                                                        |
| ------------------ | --------------------------------------------------------------------- |
| `main.rs`          | Declares modules; calls `tui::run_interactive()`.                     |
| `music.rs`         | Theory core: scales, degrees, transformations, voicing, note labels.  |
| `keyboard.rs`      | Physical positions, layout translation, `ACTIVE_LAYOUT`.              |
| `grammar.rs`       | `PositionSet` → (degree, transformation). Pure, layout-free.          |
| `progression.rs`   | Slots, rests, registers, clipboard, edit operations.                  |
| `transport.rs`     | Shared transport state + background scheduler thread.                 |
| `chime.rs`         | The five idle chime gestures.                                         |
| `synth.rs`         | cpal stream, voices, ADSR, SVF filter, reverb, patch apply/capture.   |
| `presets.rs`       | Patch structs and the TOML store.                                     |
| `debug_log.rs`     | `debug.log` writer and the output-level tap thread.                    |
| `tui.rs`           | App state, event loop, rendering, key handling.                       |

### Threading

Three threads plus the audio callback:

- **Audio callback** (`cpal`): renders voices. It reads parameters exclusively
  through lock-free `AtomicU32`-backed `SharedF32` values, so the UI can never
  block or be blocked by audio.
- **Scheduler** (`transport.rs`): walks the bar clock, decides what each bar is
  (`Play` / `Chime` / `Silent`), and posts `SchedulerEvent`s over an mpsc
  channel. It sleeps in 5 ms slices so seeks and stops feel immediate.
- **Output tap** (`debug_log.rs`): samples the peak level at 60 Hz into
  `debug.log`.
- **Main thread**: renders at a 5 ms poll and drains scheduler events.

The progression is shared as `Arc<Mutex<Progression>>`; the lock is only ever
held briefly by the UI or the scheduler, never by audio.

### Notes on the synth

Three channels (`low`/`mid`/`high`) split a chord by register: the lowest note
goes low, the highest goes high, everything between goes to mid. Each channel
has its own waveform, ADSR, cutoff, resonance, transpose, reverb send, and pan.
The filter is a Chamberlin state-variable lowpass and the reverb is a
Schroeder-style network of four combs into two allpasses. Master output is
soft-clipped with `tanh`.

## Runtime files

| File          | Notes                                                                  |
| ------------- | ---------------------------------------------------------------------- |
| `patches.toml` | Generated with five built-in patches if missing; rewritten by "Save As". Tracked in git. |
| `debug.log`    | **Truncated and rewritten on every launch.** Tracks input events and output levels; ~113k lines / 2.7 MB after one session. Should not be committed — see TODO. |

## Known issues

- **`debug.log` is committed.** It is regenerated at startup, so it churns on
  every run. It is listed in `.gitignore` but was added to the index before
  that, so it still needs `git rm --cached debug.log`.
- **The lock-key hint is wrong under Programmer Dvorak.** The hint line claims
  `z -> right register  / -> left register`. In P.D. the real lock keys are `'`
  (physical `z`) for the **right** register and `z` (physical `/`) for the
  **left** — the two registers are swapped relative to the hint, and `/` is a
  top-row P.D. character the grammar never maps.
- **`bug01` in `bugs.txt` is not a layout problem.** The `f`/`h`/`k` gesture
  resolves correctly to Csus4 under Programmer Dvorak, so the cause is still
  open. Unverified leading candidate: the held `PositionSet` not clearing when a
  key release is missed, which would break hand-played chords while
  register-locked playing keeps working.
- **Full-screen redraw.** Every frame clears the terminal and reprints
  everything, which can flicker on slow terminals.
- Progression copy/paste/reorder and chord deletion are implemented and tested
  but have no working UI path.
- `ProgressionEntry` stores a snapshot of the registers that is never read.
- Below-home-row keys other than the two lock keys are inserted into the held
  `PositionSet`, contradicting the doc comment in `keyboard.rs`. It is harmless
  today only because `is_left`/`is_right` filter them out.

## See also

- [TODO.md](TODO.md) — short-term next steps.
- `bugs.txt` — scratch notes on open defects.

[cpal]: https://github.com/RustAudio/cpal
[crossterm]: https://github.com/crossterm-rs/crossterm
