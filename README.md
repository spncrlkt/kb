# chord-tool

A terminal chord instrument. You play it with your two hands resting on the home
row: the **left hand picks a scale degree**, the **right hand picks a chord
transformation**, and the combination sounds immediately through a built-in
polyphonic synth. Chords you like can be appended to a looping progression and
played back against a metered transport.

The crate is named `chord-tool` (the repository is `kb`).

## Status

Working, but early. The audio path, music theory core, and TUI are functional
and covered by 559 unit tests. A progression can be exported as a Standard MIDI
File for Ableton from the Transport panel, and imported back — session and all.
Chords can be given **rhythm patterns** tapped on the `$` key, and moved off the
downbeat across the bar line. Several features are implemented in the data layer
but not yet wired to the UI — see [TODO.md](TODO.md).

The project did not compile as committed; two build errors (`E0063` in
`synth.rs`, `E0382` in `transport.rs`) were fixed so that `cargo run` works.

## Requirements

- A recent stable Rust toolchain (edition 2021; `let-else` is used, so 1.65+).
  Tested with cargo 1.91.
- A working audio output device — the app exits at startup with
  `no audio output device available` if none is found.
- **80 columns** — below that the frame is replaced by a line saying what the
  window is and what it needs, because the panels are fixed grids and reflowing
  them into less room would mean cutting data rather than arranging it. 16 rows
  draws the default view; 34 draws every panel in full. A wider window is used
  rather than wasted: the chord list and the transport sit at opposite edges, and
  the bar grid takes the spare columns, up to 120.
- macOS, Linux, or Windows (audio via [cpal], terminal via [crossterm]).

## Build and run

```sh
cargo run            # debug build
cargo run --release  # smoother audio; recommended for actually playing
cargo test           # 559 tests, no audio device required
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

### Registers (latching a hand, or both)

Playing a chord one-handed is awkward, so the registers can be latched:

- **`Space`** latches **both hands at once** — the one-key version, and the one
  to reach for: play a two-handed chord, press it, and both hands are free.
- `'` locks the **right** register (physical `z` on a QWERTY keycap)
- `z` locks the **left** register (physical `/` on a QWERTY keycap)

The last two are the Programmer Dvorak characters you actually type. They are
not printed on screen any more: the key row draws QWERTY keycaps, and every line
of that kind of reminder cost a row the panels could use. This table is the
reference.

A lock captures whatever that hand is holding at that moment. Afterwards, live
input wins **per side**: hold any left-hand key and it overrides the locked left
set, while the locked right set keeps filling in. This is how you audition
progressions with one hand free.

Pressing `Space` with nothing held captures *empty* for both sides, which is the
same "explicitly cleared" state the per-side locks produce — so the gesture is a
toggle: **press it twice to clear**.

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
| `{` (once)       | play / pause                            |
| `{` (twice)      | restart from bar 1                      |
| `{` (3+)         | jump to the middle of the progression   |
| `&`              | metronome click on / off                |
| `Space`          | latch both registers (the chord hold)   |
| `Esc`            | stop; a quick second `Esc` quits        |

The play key used to be the space bar; the space bar is the both-hands chord
latch now, and `{` — three keys along the number row from `$` — took the
transport. `Enter` on the Transport panel's `playing` row does the same thing for
a hand that is already on the arrows.

**`Esc` stops rather than quits.** The first press stops the transport *and
silences it immediately* — the screen flashes a red `STOPPED` banner across the
title line — and a second press within half a second quits. The window starts at
the first press, so a lone `Esc` stops at once instead of waiting to see whether
another is coming, and a slow second press just stops again.

Stopping is not pausing: a pause lets the bar you are in finish, so a chord can
ring for up to a bar, whereas `Esc` abandons the bar and releases every voice. It
leaves the *position* alone, so the next play resumes where you were. It also
leaves the metronome and an armed rhythm take alone — those have their own
switches (`&`, and `Enter` on `record`).

Prompts and editors take `Esc` first, where it means "cancel": an open value
editor reverts and closes, and a prompt closes, before the stop gesture sees the
key.

The **metronome** is a plain click on every beat, the downbeat stronger. `&` —
the key next to `$` — toggles it from any panel, and the Transport panel's
`metronome` row does the same with `←`/`→` or `Enter`. It runs whether or not the
transport is playing, so it doubles as something to practise against with the
progression stopped; it sits on a voice of its own, so it neither retriggers the
progression nor follows its mute. Arming a rhythm take needs the click too, so the row reads
`on (recording)` when a take is forcing it, and your own switch is remembered
across the take.

Taps are resolved 300 ms after the last press, so a single tap has a short
delay before it registers.

While playing, the live chord is appended as one extra bar after the
progression, so you can jam over your own loop. Note length is the fraction of a
bar a chord sustains (`1/4`, `1/2`, `3/4`, `whole`) and is cycled from the mixer
panel.

### Rhythm patterns (sinko)

A progression entry can play a **one-bar rhythm pattern** instead of one
whole-bar chord, and can be **offset** across the bar line. The **Sinko** panel
is where both are set.

Press `$` — the top-left key — to tap a beat. The key is a *physical position*,
not a character, and like the below-home-row row it never joins the held set, so
you can hold a two-handed chord and tap with the left pinky without breaking the
shape. Tapping works from any panel: the point is to watch the progression while
you play.

```
── Sinko  [recording] ─────────────────────────────────────────────
  ▸ chord    #3  Am
    pattern  Offbeat Eighths  (edited)
    offset   -1/8   (-480 ticks)
    quant    1/8  —  8 steps per bar, 4 hits
    hits     on        3 of 8
    hold     1/4  —  960 ticks
    mute     1/8  —  480 ticks
    smooth   1 take per layer
    record   [● recording]
     1.00  |xxxxxx|······|xxxxxx|······|
     0.70  |······|xxxxxx|······|xxxxxx|
     live  |······|······|······|xxxxxx|
    ·     |······|······|······|······|
   [New Pattern]
   [Save Pattern As...]
   [Copy Sinko]
   [Paste Sinko: Quarters]
```

- **The grid** is the bar, drawn at whatever resolution the pattern uses: 2, 4,
  8, 16, 32 or 64 steps per bar, which is half notes through sixty-fourths. One
  line per *layer*, so the stack is visible; the `live` line is the take being
  tapped, snapped to the grid as you go. The cell the bar clock is in is marked
  `X` on a hit and `+` on a rest.
- **`hits`** edits the grid cell by cell, so a take you almost like does not have
  to be tapped again. `←`/`→` walks the cell cursor — the cell it is on is
  *inverted* in the grid rather than given another character, so the bar still
  reads as hits and rests — and `Enter` toggles. Turning a hit **off** clears that
  cell in *every* take; turning one **on** puts it in the newest take. Like every
  other row it writes through, so the change is heard on the next bar, and a
  silent pattern can be built up one cell at a time. (This replaces the earlier
  `cells` row, which cycled through every way of filling the grid: at 32 and 64
  steps those lists run to billions, and the useful operation is taking a hit out
  of a take you already played.)
- **Takes stack.** Each bar you tap commits one take as a layer: the newest at
  full level and each older one a `TAKE_DECAY` (0.7) step quieter, so the stack
  fades as it builds. There are four layers, one per stab group in the synth, and
  a hit on one layer never cuts another. The `smooth` row (`←`/`→`) can instead
  average the last `N` takes into the top layer, which is how a figure tapped
  several times converges on one clean line — but it merges *different* figures
  too, so it starts at `1 take per layer`.
- **`hold`** is how long each hit rings, in **ticks**, from a 32nd up to the
  whole bar. `←`/`→` walks the note ladder — a 32nd, a 16th, an 8th, a 3/16, a
  quarter, a 3/8, a half, a 3/4, a whole — so a press lands on a note length
  rather than an arbitrary count. Because it is ticks, changing `quant` does not
  move it: a half note stays a half note on any grid.
- **`mute`** silences the end of the bar, up to a quarter note, on the same
  ladder. Nothing *starts* inside the muted tail and anything ringing into it is
  cut at the boundary, which is what makes a pattern stop short instead of
  bleeding into the next bar. It is a bar position, so it follows the chord's
  offset. `none` is the default and means no mute at all: a hold still crosses
  the bar line.
- **Every one of these is live.** Each chord *owns* its rhythm: the panel edits
  the copy the selected entry plays, and the scheduler reads that entry, so a
  hold or a mute is heard on the next bar with nothing to save and nothing
  written to disk. Two chords can both have been given `Quarters` and then drift
  apart, because assigning takes a copy rather than pointing at a shared
  library entry.
- **A take closes when the bar does**, and its taps are quantized to the nearest
  cell of the chosen grid. Taps closer together than 30 ms are treated as key
  repeat, not a second tap.
- **`Enter` on `record`** arms or disarms. Armed, the click runs (see the
  metronome above) and each tap sounds the selected chord so you hear the rhythm
  in context.
- **`[New Pattern]`** gives the selected chord a blank rhythm of its own — named,
  editable and audible from the first tap, with the library left alone. There is
  no hidden draft: a chord with no rhythm of its own cannot be edited, and the
  shape rows say so rather than changing numbers you cannot hear. Recording does
  the same thing implicitly: tapping a take on a chord that has no rhythm gives
  it one, built from the take.
- **`[Copy Sinko]`** / **`[Paste Sinko]`** (`Shift+Q` / `Shift+J`, see the key
  table) move a rhythm between chords. Copy
  takes the selected chord's rhythm — grid, hits, hold and muted tail — to a
  clipboard, and paste gives it to whichever chord is selected next, as its own
  copy, in one undoable edit. The offset stays behind: where a chord sits in the
  bar is placement, not rhythm. The paste row names what is waiting
  (`[Paste Sinko: Quarters]`), so an empty clipboard is visible — and pasting with
  nothing copied says `nothing copied yet` in red and then fades, like every other
  message that is feedback about a key press rather than an error to act on.
- **`[Save Pattern As...]`** copies the chord's rhythm into the *palette* under a
  new name — that is how a rhythm you built on one chord becomes something other
  chords can start from. A name already in the library is never overwritten; the
  new one becomes `Name 2`.
- **The playhead** marks the cell the bar clock is in — during playback, and
  also against a metronome click with the transport stopped, which is how you
  tap a pattern before you have any chords.
- **The `pattern` row** (`←`/`→`) cycles `(none)` and then every pattern in the
  library, so assigning and clearing are the same gesture — and assigning takes a
  copy. It shows the chord's own rhythm, with `(edited)` when that rhythm no
  longer matches the library pattern it is named after. The panel has no cursor
  of its own: it always describes the entry the **Progression** panel has
  selected.
- **The `offset` row** (`←`/`→`, or `←`/`→` in the Progression panel) moves the
  chord along the same note ladder, up to a whole note either way. It is
  deliberately independent of the pattern: what a 3/16 offset means does not
  change because the chord happens to play an eighth-note pattern. The
  Progression panel shows it inline as `Offbeat Eighths -1/8`.
- A slot with **no pattern** behaves exactly as it always has: one chord at its
  downbeat, holding for `note_length`.

Offsets and the loop wrap: a chord pulled before its bar sounds at the end of the
previous bar, and a chord that spills past the last bar continues at the start of
the loop. A hold that crosses a bar line is released in the bar it ends in, which
is also where the exported file puts its note-off.

`rhythms.toml` ships with seven patterns. `Quarters`, `Eighths` and `Sixteenth
Pulse` fill the bar at that note value, `Offbeat Eighths` and `Syncopated 16ths`
are the syncopated ones — the reason the feature exists, since neither can be
played as a whole-bar chord — and `Held Half` and `Held 3/4` are a single hit
that rings for a half and a 3/4 note. Each writes its `hold` in ticks, so a
pattern's length is legible in the file.

The built-ins are written **once**, when the file is first created, exactly like
`patches.toml`. A changed or added built-in therefore reaches an existing install
only by deleting or editing `rhythms.toml` — your saved patterns live in that file
too, and nothing in the app ever overwrites a pattern you named yourself.

### Panels

`Tab` / `Shift+Tab` cycles focus:

```
Transport → Progression → Sinko → Synth → Synth Presets → (wrap)
```

Within a panel: `↑`/`↓` move the row and `←`/`→` adjust the selected value.

The **Synth** panel is the exception, because it is the only two-dimensional
surface in the app. Every setting is on one screen — the three channels side by
side as columns, with the master block underneath:

```
── Synth  (tab out · ←/→ column · shift+←/→ adjust · enter edit) ────
   param          low        mid        high
 ▸ volume         4          4          4
   waveform       sine       saw        sine
   attack         5 ms       200 ms     300 ms
   ...
   reverb level   [0%]       reverb size   50%
   master volume  5          master mute   off
   preview fade   30 ms      note length   1/2
```

There, `←`/`→` moves between the channel columns, **`Shift+←`/`→` changes the
value** (the same nudge the old mixer had), and `Enter` opens an edit on the
selected cell: the arrows adjust it while you hear the result, `Enter` keeps it
and `Esc` puts the old value back. Editing with a chord held needs `Shift`+arrows,
because `Enter` still commits the chord first — the same rule as every other value
row.

`Esc` reaches the open editor before it can mean "stop": it reverts and closes,
and only a plain `Esc` with nothing open is the stop gesture.

Collapsing the four synth subtabs into one table did not make the layout smaller
(the Mixer subtab was already 15 rows); it made it **constant**, and it put a
channel's volume and its cutoff on the same screen for the first time.

`Enter` commits the chord you are currently playing — the latched registers plus
whatever keys are down right now, with live input winning per side. It works
from **any** panel, so you don't have to navigate to the Progression panel to
capture a chord. In the Progression panel it inserts after the selected row;
anywhere else it appends. `Ctrl+Enter` always appends, even when no chord
resolves.

`b` **replaces** the selected slot with what the registers resolve to, rather
than inserting beside it. That is the gesture for fixing a chord you already
placed: the slot's **rhythm pattern and offset stay with it**, because they
belong to the entry and not to the chord. Delete-and-re-insert would lose them.
Selecting a rest and pressing `b` turns that rest into a chord, since there is no
rhythm to keep; pressing it twice changes nothing and does not touch the undo
history.

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
| `x` | `q` | copy the chord under the cursor (chord, registers, rhythm, offset) | Progression |
| `c` | `j` | paste the clipboard after the cursor | Progression |
| `x` + Shift | `Q` | copy the cursor chord's **rhythm** to the sinko clipboard | Progression, Sinko |
| `c` + Shift | `J` | give the cursor chord a copy of that rhythm | Progression, Sinko |
| `v` | `k` | delete the chord under the cursor | Progression |
| `n` | `b` | set the selected slot to the chord in the registers, keeping its rhythm | Progression |
| `b` | `x` | undo | Progression |
| `b` + Shift | `X` | redo | Progression |
| `` ` `` | `$` | tap a beat of the rhythm being recorded | any panel |
| `1` | `&` | metronome click on / off | any panel |
| `3` | `{` | tap the transport: 1 play/pause, 2 restart, 3+ seek middle | any panel |
| `4` | `}` | the same — the key next door, so a miss still taps | any panel |
| `Space` | `Space` | latch both registers | any panel |

"Keycap" is the QWERTY label printed on the key; "you type" is the character
that actually reaches the app under Programmer Dvorak. In the key row drawn at
the top of the screen, hotkey positions are shown in cyan rather than dark grey
so `g` doesn't read as a chord key.

The progression actions are scoped to the Progression panel, where the cursor
lives, so they can never act on a row you can't see — press `Tab` to get there.
The two **rhythm** clipboard keys are the exception, because replicating one
rhythm across the chord list is the thing you do *from* that list: they work in
the Progression panel and in Sinko, and refuse anywhere else.

`{` and `}` are the one place where two keys do the *same* thing, because they
are adjacent and the transport tap is a performance control: fumbling it mid-take
would be worse than the small redundancy. They are two physical positions
(`TopRow3` and `TopRow4`) with the same hotkey, so neither is a chord key.

`Shift` always selects a second action on one physical key, never a new key:
`Shift+X` is redo, `Shift+Q`/`Shift+J` are the rhythm clipboard. So the pairs
read as "the whole entry" versus "just the rhythm" — `q` copies the chord with
its registers, rhythm and offset, `Shift+Q` copies only the rhythm; `j` inserts
a new slot after the cursor, `Shift+J` retimes the slot you are on.
Pressing one elsewhere flashes rather than doing nothing silently. Register
locks are performance controls and work everywhere.

Three unassigned slots remain (`m`, `,`, `.` — you type `m`, `w`, `v`), reserved
for the progression reordering and clear-all in [TODO.md](TODO.md).

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
green for **5 seconds**, fades out over the next second, and then disappears. The
same timing applies to a **refusal** — pressing Export on an empty progression, or
Import with no file name — which holds in red and then fades, because a refused
key press has nothing to act on. A genuine **failure** stays on screen until the
next export or import: an error is usually telling you to do something, so it
should not vanish before it is read. All of them go to `debug.log` regardless.

What goes in the file:

- **One track on channel 1** (SMF format 0), so Ableton makes exactly one clip.
- **Tempo**, **4/4 time signature** and **key signature** from the transport.
- **One bar per slot**, matching playback. A slot with no rhythm pattern is
  released at `note_length` — so a half-bar setting exports half-bar notes and
  the gap is silent; a slot with a pattern exports that pattern's onsets and
  holds; a slot with an offset exports the chord where it actually sounds, across
  the bar line. A rest slot is a genuinely empty bar, and a rest at the end still
  lengthens the loop.
- **The chord tones you played**, not the synth's voicing. The audio path
  doubles sparse chords an octave out for timbre; that is sound design, not
  musical content, so it stays out of the file.
- **Velocities from the take gains.** The synth has no velocity, so a quieter
  layer is only audible as a quieter layer; writing `gain × 100` is where the
  rhythm's dynamics reach the file.
- **Timings from the plan the scheduler plays.** `arrangement.rs` is the single
  source of truth for when a note starts, so an export cannot disagree with what
  you heard — which was the one place this could have drifted.
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
not a chord-tool file (no embedded progression)
```

The message is clipped to one fixed-width row, so it can never reflow the
Transport panel; the full text always goes to `debug.log`.

That refusal is the point. A MIDI file only holds absolute notes, and the
original degrees and transformations cannot be recovered from them — C-E-G is I
in C, IV in G and V in F, and all three are the same three notes. Guessing would
silently disagree with what you played. Exports made *before* import existed
therefore will not import; re-export them from a saved progression.

### How the session travels

The exporter embeds the session as a **sequencer-specific meta event**
(`FF 7F`, non-commercial manufacturer id `0x7D`, magic `CTP1`) holding a
small versioned **TOML** document: key, BPM, note length, and each slot's degree,
transformation, register gesture, offset and **its own rhythm**. DAWs ignore
sequencer-specific data, so it never shows up as lyrics or a marker, while our
own reader finds it by magic — so most other tools' files can be positively
identified rather than guessed at.

The document **embeds the pattern library** as a palette, so an import lands with
the starting points the export had, and each slot carries its rhythm *inline*, so
a chord's edited hits travel with it and a slot can never dangle: there is no name
to resolve and nothing for the file and the audio to disagree about.

The register fields keep `None` ("never set") distinct from `Some([])`
("explicitly cleared"), because the two resolve differently and `g`-to-recall
would otherwise change behaviour after a round trip.

The format is versioned (`project::VERSION`, currently 3). Older documents still
import: a **version 1** export has none of the rhythm fields and defaults to the
progression it described — no patterns, no offsets, which is what it sounded
like. A **version 2** slot held the *name* of a library pattern, so it is
resolved against the document's own `rhythms` on the way in and each slot gets
its own copy of what it used to share; a name the file does not carry is refused
rather than quietly importing as a whole-bar chord. A document from a *newer*
version is refused with a clear message rather than misread.

The code is split so it can grow:

| File | Role |
| ---- | ---- |
| `midi.rs` | Pure model — a progression becomes a `Score` of timed notes tagged Low/Mid/High. No audio, no terminal, no filesystem. |
| `arrangement.rs` | The timing seam. A progression — each entry carrying its own rhythm — becomes timed stabs; the scheduler takes a per-bar slice and the exporter renders the whole thing, so the two cannot disagree. Pure, and it needs nothing but the slots. |
| `rhythm.rs` | Rhythm patterns: the step grid, its layers, quantization and the tap maths. Pure. |
| `rhythm_store.rs` | The pattern *palette* in `rhythms.toml`: what `[Save Pattern As...]` writes and what the `pattern` row offers. The only filesystem code in the rhythm feature. |
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

One display rough edge remains:

- The key row and the register lines print *QWERTY* names via `qwerty_label()`,
  so the letters drawn are not the letters you type. Cosmetic, and deliberate:
  the keycap is what your fingers find.

The top of the screen is four rows of *data* and nothing else: the whole screen's
two facts, the hands, each register **under the hand that fills it**, and then
what they add up to — which sits at the left margin, above the chord list it
belongs to:

```
                        Chord Tool  ·  Programmer Dvorak  ·  C major
                       [a] [s] [d] [f] [g]  │ [h] [j] [k] [l] [;]
                       L [ads]  → vii (B4)    R [jk]   → sus2 (J)
  Bsus2  (vii)   B4 C#5 F#5
── Progression  undo: yes ──       ── Transport ──
      C                                bpm              120
      G                                loop             on
  ▸   Am   Offbeat Eighths   -1/8      [Export MIDI]
      F                                [Import MIDI]
── Sinko ── #3  Am   Offbeat Eighths   -1/8
── Synth ──
  low   sine     mid   sine     high  sine     | master 5  reverb 0%  whole
```

**Progression sits in the left column and Transport in the right**, the list at
the margin and the transport against the far edge, so the pair spans whatever
window it is given rather than huddling at the left. Both are short, so the pair
costs one block of rows instead of two, and the rows a column does not use are
simply not drawn.

The left column is as wide as its widest line, so nothing shifts as values change.
When the pair cannot both fit, the **right** column is what gives: the left one is
the chord list, where a shortened row would be a lost chord or a lost rhythm name,
while the right one is the transport, whose only long lines are action statuses —
those are cut with `…`, and `debug.log` keeps them in full. If even that would
leave the transport less than 24 columns, the two **stack** instead — the chord
list above the transport — because a clipped-to-nothing panel is worse than a
taller one.

A wider window puts the extra columns into the bar grid rather than into a wider
margin, since the grid is the one thing on screen that is a drawing.

`Tab` walks the panels **in that layout order** — along the first row (the chord
list, then the transport), then down through the rhythm panel to the synth — so
`Shift+Tab` retraces it and the cursor always moves the way the eye does. The
focused panel is the one wearing the heavy rule and the cyan band. Only the sinko
and synth panels expand when focused; the other two are always open, which is what
keeps the default view 15 rows.

A held key is a green block, a hotkey position is cyan in brackets, a chord key
is dark grey in brackets; an unlatched register reads `—`, one explicitly
cleared reads `(empty)`, and a latched one reads its keys and what they mean.
The register cells are padded, so a column never moves as keys come and go.

**The window.** 80 columns is the minimum, and below it the frame is replaced by
a line saying what the window is and what it needs. At 80 and above, every row is
cut to the window once, on the way out — a wrapped row would cost two, shift
everything below it and push the bottom panels off, so no panel is allowed to do
it. Content that overflows even at 80 (a long rhythm name in the chord list, an
export path in a status) is what the ellipsis is for, and a *pair* that cannot fit
stacks rather than squeezing either panel to nothing.

**The title and the keyboard are centred** on the window, and the registers are
part of the keyboard's block — one width, written down rather than measured, so
neither row moves as keys are pressed or chords change. The chord readout is not
centred with them: it is the heading of the chord list below it, so it sits at the
left margin. Each panel's rule is centred over its own body, filled with `─` — or
`━` for the focused panel, whose coloured band stays the width of its name rather
than being smeared across the screen.

**Where the focus is.** Three levels, each said twice — once in colour and once
in weight — so neither has to carry it alone:

| What | Colour | Weight |
| --- | --- | --- |
| The focused panel | black on a cyan header band | a heavy `━━` rule, and a bold header |
| Its selected row | white on a grey band | bold |
| The panels you are not in | dark grey headers | a light `──` rule |
| The chord under the cursor (progression) | yellow | bold |
| The chord sounding now (progression) | red | bold |
| The chord being played (header) | yellow | bold |
| The keys a register holds | white | bold |

The playhead in the Sinko grid is coloured as well as drawn as `X`/`+`, and a
register that holds nothing stays dim rather than disappearing. A terminal that
drops colour still shows the heavy rule, the bold rows and the `▸`, which is why
every cue is doubled.

Colour is **forced on** rather than left to `NO_COLOR`: in a full-screen
instrument, colour is state rather than decoration, and `NO_COLOR` is a
convention for piped text. Without the override, a shell that exports it (as
some setups do) silently turns every cue above into a bare reset.

Nothing on that block — or anywhere else on screen — explains a key. The focused
panel is marked by a cyan header rather than a `(tab to switch)` note, and the
panels run together with no blank line between them, because the `── name ──`
rules already separate them and each blank cost a row. Every instruction that
used to sit in the frame is in this README, and the layout is the denser for it.

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
| `transport.rs`     | Shared transport state + background scheduler thread, walking `arrangement`'s plan against absolute deadlines. |
| `synth.rs`         | cpal stream, voices, ADSR, SVF filter, reverb, patch apply/capture.   |
| `presets.rs`       | Patch structs and the TOML store.                                     |
| `debug_log.rs`     | `debug.log` writer and the output-level tap thread.                    |
| `tui.rs`           | App state, event loop, rendering, key handling.                       |

### Threading

Three threads plus the audio callback:

- **Audio callback** (`cpal`): renders voices. It reads parameters exclusively
  through lock-free `AtomicU32`-backed `SharedF32` values, so the UI can never
  block or be blocked by audio.
- **Scheduler** (`transport.rs`): lays the loop out through
  `arrangement::arrangement`, slices the bar it is on, and posts a
  `SchedulerEvent` for each onset, release and metronome click over an mpsc
  channel. Every event is timed against an **absolute deadline** from the bar's
  start instant, so a bar of 64th notes cannot accumulate drift; it checks for
  stops and seeks every 5 ms, so a seek still feels immediate. It also publishes
  each bar's start instant, which is the clock tap capture reads.
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
| `rhythms.toml` | The rhythm pattern palette, generated with seven built-ins **if missing** and rewritten only by `[Save Pattern As...]`. A chord's own rhythm is *not* here — it lives in the session document, so export to keep it. Delete the file to pick up changed built-ins. Tracked in git. |
| `progressions/` | Where MIDI exports are written and imported from, created on first launch. **Gitignored** — these are session data, not source. |
| `debug.log`    | **Truncated and rewritten on every launch.** Tracks input events and output levels; ~113k lines / 2.7 MB after one session. Should not be committed — see TODO. |

## Known issues

- **`debug.log` is committed.** It is regenerated at startup, so it churns on
  every run. It is listed in `.gitignore` but was added to the index before
  that, so it still needs `git rm --cached debug.log`.
- **`bug01` in `bugs.txt` is not a layout problem.** The `f`/`h`/`k` gesture
  resolves correctly to Csus4 under Programmer Dvorak, so the cause is still
  open. Unverified leading candidate: the held `PositionSet` not clearing when a
  key release is missed, which would break hand-played chords while
  register-locked playing keeps working.
- **Full-screen redraw, and a tall layout.** Every frame clears the terminal
  and reprints everything, which can flicker on slow terminals; and the focused
  Sinko panel needs 32 rows and the Synth table 29, mostly because the panels
  around it are all still there. The default view is 16; on a short terminal the
  synth goes first, since it is the last panel drawn.
- **Progression reordering and clear-all have no UI path.** `move_up`,
  `move_down` and `delete_all` are implemented, undoable and tested, but no
  hotkey reaches them yet — four below-home-row slots are reserved for them.
- **Progressions are not persisted.** `patches.toml` survives restarts; the
  progression does not — and since a chord now *owns* its rhythm rather than
  naming one in the palette, that includes the rhythm edits. Keep a session by
  exporting it, or push a rhythm you want to keep into `rhythms.toml` with
  `[Save Pattern As...]`.

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
