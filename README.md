# chord-tool

A terminal chord instrument. You play it with your two hands resting on the home
row: the **left hand picks a scale degree**, the **right hand picks a chord
transformation**, and the combination sounds immediately through a built-in
polyphonic synth. Chords you like can be appended to a looping progression and
played back against a metered transport.

The crate is named `chord-tool` (the repository is `kb`).

## Status

Working, but early. The audio path, music theory core, and TUI are functional
and covered by 221 unit tests. A progression can be exported as a Standard MIDI
File for Ableton from the Transport panel, and imported back — session and all.
Several features are implemented in the data layer but not yet wired to the UI —
see [TODO.md](TODO.md).

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
cargo test           # 221 tests, no audio device required
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

`g` (left inner) is **not** a chord key — it carries the recall hotkey, so it
never joins the held set. If you hold left-hand keys that don't form one of
these shapes, no degree resolves and no chord sounds.

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
register exactly as the audio and `Enter` do. It names the chord and its scale
degree relative to the track key — `F7 (V)`, `Dm (ii)`, `Bdim (vii)` — using the
same degree the register lines show, so live playing and a latched hand read
identically.

Each progression entry records the gesture that produced it. With the cursor on
a chord in the Progression panel, press `g` to **recall** that entry's registers
and voice it. This is deliberately explicit rather than automatic on selection:
moving the cursor with `↑`/`↓` never touches the registers, so scrolling can't
clobber a latched register mid-performance.

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

### Editing hotkeys

The row **below the home row** is the hotkey row, plus `g` on the home row
itself — the one home-row position the chord grammar never uses. None of them
ever join the held set, so they stay usable for editing while both hands are
holding a chord. This is the only editing surface; there is no modifier-based
shortcut layer.

| Keycap | You type | Action | Scope |
| --- | --- | --- | --- |
| `g` | `i` | recall the chord under the cursor into the registers | Progression |
| `z` | `'` | lock the right register | any panel |
| `/` | `z` | lock the left register | any panel |
| `x` | `q` | copy the chord under the cursor | Progression |
| `c` | `j` | paste the clipboard after the cursor | Progression |
| `v` | `k` | delete the chord under the cursor | Progression |
| `b` | `x` | undo | Progression |
| `b` + Shift | `X` | redo | Progression |

"Keycap" is the QWERTY label printed on the key; "you type" is the character
that actually reaches the app under Programmer Dvorak. In the key row drawn at
the top of the screen, hotkey positions are shown in cyan rather than dark grey
so `g` doesn't read as a chord key.

The progression actions are scoped to the Progression panel, where the cursor
lives, so they can never act on a row you can't see — press `Tab` to get there.
Pressing one elsewhere flashes rather than doing nothing silently. Register
locks are performance controls and work everywhere.

The four unassigned slots (`n`, `m`, `,`, `.` — you type `b`, `m`, `w`, `v`) are
deliberately inert, reserved for future operations.

Undo covers every structural change: add, delete, paste, move. History is 128
edits deep, and a fresh edit discards the redo stack. The Progression panel
header shows `undo: yes` / `redo: yes` when there is history to step through.
An edit that changes nothing (deleting past the end, pasting with an empty
clipboard, undo with no history) flashes instead.

Modifier keys are deliberately **not** part of the chord grammar, so `Ctrl`/`Alt`
combinations are ignored rather than sounding a chord — that keeps them free for
bindings later.

## Exporting and importing MIDI

The **Transport** panel's last two rows are `[Export MIDI]` and `[Import MIDI]`.
Select one with `↓` and press `Enter`.

### Export

The progression is written as a Standard MIDI File that Ableton imports
directly:

```
progression-2026-09-23_18-03-45.mid
```

The name is the local wall-clock time of the export, so files never collide
within a second and sort chronologically. The file lands in the `progressions/`
directory under the working directory — created on first launch and
**gitignored**, so a session's exports never show up as untracked changes. If
that directory cannot be created, the app falls back to the working directory
rather than refusing to start, and says so in `debug.log`. The outcome (the file
name, or the error) is shown beside the button in green or red and echoed to
`debug.log`. An empty progression exports nothing, since there is no loop to
write.

A successful filename is confirmation, so it does not linger: it holds at full
green for **5 seconds**, fades out over the next second, and then disappears.
A failure stays on screen until the next export or import — an error is usually
telling you to do something, so it should not vanish before it is read. Both go
to `debug.log` regardless.

What goes in the file:

- **One track on channel 1** (SMF format 0), so Ableton makes exactly one clip.
- **Tempo**, **4/4 time signature** and **key signature** from the transport.
- **One bar per slot**, matching playback, with the note released at
  `note_length` — so a half-bar setting exports half-bar notes and the gap is
  silent. A rest slot is a genuinely empty bar, and a rest at the end still
  lengthens the loop.
- **The chord tones you played**, not the synth's voicing. The audio path
  doubles sparse chords an octave out for timbre; that is sound design, not
  musical content, so it stays out of the file.
- The live chord is **not** included — an export is a function of the
  progression alone.
- **An embedded session document**, described below. It is invisible to a DAW.

Selecting a button and pressing `Enter` always activates it, even if you are
still holding a chord. Buttons win over chord-commit; the same rule applies to
the Presets panel's `[Save As...]`.

### Import

`[Import MIDI]` opens a prompt pre-filled with the **newest export**, so the
common case is just `Enter`. You can edit the name, or type an absolute path to
reach a file anywhere. A successful import replaces the progression and restores
the **key, BPM and note length** too, so a file round-trips exactly. It is one
undoable edit: a single undo returns the whole previous progression.

A file that this tool did not write is **refused**, not guessed at:

```
not a chord-tool file (no embedded progression; files exported before
MIDI import existed will not have one)
```

That refusal is the point. A MIDI file only holds absolute notes, and the
original degrees and transformations cannot be recovered from them — C-E-G is I
in C, IV in G and V in F, and all three are the same three notes. Guessing would
silently disagree with what you played. Exports made *before* import existed
therefore will not import; re-export them from a saved progression.

### How the session travels

The exporter embeds the session as a **sequencer-specific meta event**
(`FF 7F`, non-commercial manufacturer id `0x7D`, magic `CTP1`) holding a
small versioned **TOML** document: key, BPM, note length, and each slot's
degree, transformation and register gesture. DAWs ignore sequencer-specific
data, so it never shows up as lyrics or a marker, while our own reader finds it
by magic — so most other tools' files can be positively identified rather than
guessed at.

The register fields keep `None` ("never set") distinct from `Some([])`
("explicitly cleared"), because the two resolve differently and `g`-to-recall
would otherwise change behaviour after a round trip.

The format is versioned (`project::VERSION`); a document from a different
version is refused with a clear message rather than misread.

The code is split so it can grow:

| File | Role |
| ---- | ---- |
| `midi.rs` | Pure model — a progression becomes a `Score` of timed notes tagged Low/Mid/High. No audio, no terminal, no filesystem. |
| `smf.rs` | Pure byte writer and reader. `TrackLayout::Single` writes the current one-track file; `TrackLayout::PerLayer` is already implemented for routing Low/Mid/High to separate channels. Framing of the embedded event lives here. |
| `project.rs` | The session document itself: versioned TOML, and the domain conversions. |
| `export.rs` | The only filesystem code: timestamped names, reading, writing, and turning a failed read into a specific error. |

Because the layers survive into the score and `ChannelMap` already maps them to
channels, per-channel export is a UI change rather than a format rewrite. The
same `Score` is the intended seam for live MIDI output later — the scheduler
already emits the chord events a live sink would consume.

Time signature is hardcoded to 4/4 today, because the transport is; the score
carries `beats_per_bar` so that will not require reworking the writer.

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
| `midi.rs`          | Pure MIDI model: `Score`, layers, `split_layers`, `render_progression`. |
| `smf.rs`           | Pure Standard MIDI File writer and reader (format 0 single track / format 1 per layer). |
| `project.rs`       | The versioned session document embedded in an export, so it can be imported back. |
| `export.rs`        | MIDI export/import filenames and file I/O.                            |
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

**Reverb is additive.** The three per-channel sends feed one mono tank, and the
`reverb level` control sums its output *on top of* the dry signal — it never
attenuates it. At level 0 the dry path is untouched; turning reverb up only ever
adds. The per-channel sends decide how much each register feeds the tank, so
with every send at zero the level does nothing. (The stored key is still called
`reverb_mix` in `patches.toml`, so patches written before this change load
unchanged.)

## Runtime files

| File          | Notes                                                                  |
| ------------- | ---------------------------------------------------------------------- |
| `patches.toml` | Generated with five built-in patches if missing; rewritten by "Save As". Tracked in git. |
| `progressions/` | Where MIDI exports are written and imported from, created on first launch. **Gitignored** — these are session data, not source. |
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
- **The lock-key hint line is stale.** It still prints the QWERTY names
  `z -> right register  / -> left register`, which under P.D. are backwards and
  refer to a key (`/`) the grammar never maps. The README table above is
  correct; the on-screen line has not been updated to derive from the active
  layout.
- **Progression reordering and clear-all have no UI path.** `move_up`,
  `move_down` and `delete_all` are implemented, undoable and tested, but no
  hotkey reaches them yet — four below-home-row slots are reserved for them.
- **Progressions are not persisted.** `patches.toml` survives restarts; the
  progression does not.

## Fixed in this session

- Progression **delete, copy and paste** now have hotkeys and are undoable; the
  code existed but was unreachable.
- `ProgressionEntry.registers` is now **read** (it drives `g`-to-recall) and
  captures the *resolved* gesture rather than just the latch state, which was
  why it was previously unusable.
- Below-home-row keys no longer leak into the held `PositionSet`, so
  `keyboard.rs` now matches its own documentation. `g` no longer silently breaks
  a chord it is held alongside.

## See also

- [TODO.md](TODO.md) — short-term next steps.
- `bugs.txt` — scratch notes on open defects.

[cpal]: https://github.com/RustAudio/cpal
[crossterm]: https://github.com/crossterm-rs/crossterm
