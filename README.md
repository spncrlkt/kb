# chord-tool

A terminal chord instrument. You play it with your two hands resting on the home
row: the **left hand picks a scale degree**, the **right hand picks a chord
transformation**, and the combination sounds immediately through a built-in
polyphonic synth. Chords you like can be appended to a looping progression and
played back against a metered transport.

The crate is named `chord-tool` (the repository is `kb`).

**This prose is the *why*.** Every feature and every key, tersely and completely,
is in **[REFERENCE.md](REFERENCE.md)** — that is the document to read if you just
want to know what the tool does. **[CHEATSHEET.md](CHEATSHEET.md)** is the one page
of it worth keeping open beside the app while you play.

## Status

**Version 1.0.0** — working and feature-complete. The whole suite runs without an
audio device (`cargo test`) — including the audio itself, which is rendered
offline and checked against recorded fingerprints — `cargo clippy --all-targets`
is clean, and the documented keys are checked against the keyboard map by a test,
so [REFERENCE.md](REFERENCE.md) cannot drift from the code without failing the
build. [PERFORMANCE.md](PERFORMANCE.md) covers how the sound and the load are
tested and measured.

What is here: a playable two-handed chord grammar, latched registers, a looping
progression with per-chord rhythm patterns, a metronome with its own panel,
a synth with three channels and a master block, a thirteen-band equaliser and a
live spectrum per register, a per-register effect rack of six insert slots plus a
reverb and a delay send, ensembles built from an instrument library, and MIDI
export and import that carry the session. What is not: progressions are not
persisted on their own (export them), and there are no odd meters.
[TODO.md](TODO.md) is the backlog.

## Requirements

- Rust **1.87 or newer** (edition 2021; `is_multiple_of` on integers is the
  newest thing used). Declared as `rust-version` in `Cargo.toml`, so an older
  toolchain says so rather than failing deep in a build. Tested with cargo 1.91.
- A working audio output device — the app exits at startup with
  `no audio output device available` if none is found.
- **80 columns** — below that the frame is replaced by a line saying what the
  window is and what it needs, because the panels are fixed grids and reflowing
  them into less room would mean cutting data rather than arranging it. 15 rows
  draws the default view and 35 draws every panel in full, except the ensemble
  list, which is as long as the library. A wider window is used
  rather than wasted: the chord list and the transport sit at opposite edges, and
  the bar grid takes the spare columns, up to 120.
- macOS, Linux, or Windows (audio via [cpal], terminal via [crossterm]).

## Build and run

```sh
cargo run            # debug build
cargo run --release  # smoother audio; recommended for actually playing
cargo test           # the whole suite, no audio device required
cargo clippy --all-targets   # clean
```

The binary is `target/{debug,release}/chord-tool`.

### Testing the audio

`cargo test` renders the real signal path offline — there is no audio device
anywhere in it — and takes about thirty seconds. The interesting parts:

```sh
cargo test --test render        # the sound: invariants, fingerprints, block and rate sweeps
cargo test --test stress        # load: the whole voice pool, the whole rack, the whole matrix
cargo test --test allocation    # the audio callback must not allocate
cargo test --release --test stress -- --ignored   # the 60-second soak and the deep fuzz

scripts/check.sh                # all of the above, plus clippy and a release run
scripts/check.sh --load         # and the soak and the deep fuzz
```

Measuring is a separate command, because none of it can fail:

```sh
scripts/bench.sh before         # criterion, on the real audio path
scripts/bench.sh after
critcmp before after            # cargo install critcmp
python3 scripts/bench_table.py  # or just the current numbers
```

`debug.log` gets a `[TIME]` line once a second with the audio callback's share of
its buffer deadline and how many buffers missed; `CHORD_TOOL_TIMING=1` adds the
scheduler, the draw loop, import, export and start-up to it.

A failing fingerprint prints the strongest partials beside the number that moved,
and `CHORD_TOOL_RENDER=<path> cargo test --test render` writes what it actually
rendered to a `.wav` so it can be listened to. **[PERFORMANCE.md](PERFORMANCE.md)**
is the document for all of this: what is measured, what the measurements have
found, and what is left to optimise.

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
key. So do the two rows that open *sideways* rather than into a prompt — the
metronome panel and the `[MIDI]` chooser — because `Esc` there closes the row you
opened rather than stopping the transport underneath it.

The transport also carries the **master volume**, and it is the *same* number as
the one in the Synth panel's master block: one value with two rows, so a level is
a Tab away wherever you are rather than three panels away. And the two file
actions are now one row. `Enter` on `[MIDI]` opens it sideways:

```
  ▸ [MIDI] ─ [EXPORT] import
```

with `←`/`→` picking a side and the bracketed one being what a second `Enter`
runs. Two rows of file actions were the only two rows on the panel doing the same
kind of thing, and the chooser costs nothing to leave closed: `[MIDI]` is a button
like any other, and shows whichever of the two ran last.

**Three settings survive a restart**: the track key, the tempo and the master
volume. They are written to a gitignored `settings.toml` a moment after they stop
moving — so holding an arrow is one write rather than thirty a second — and read
back at start-up. The progression is deliberately *not* among them: a progression
is a document, and there is already a way to keep one.

The **metronome** is a plain click, the downbeat stronger. `&` — the key next to
`$` — toggles it from any panel, and the Transport panel's `metronome` row does
the same with `←`/`→`. It runs whether or not the transport is playing, so it
doubles as something to practise against with the progression stopped; it sits on
a voice of its own, so it neither retriggers the progression nor follows its mute.
Arming a rhythm take needs the click too, so the row reads `on (recording)` when a
take is forcing it, and your own switch is remembered across the take.

`Enter` on that row opens the **metronome panel**, which covers the Transport slot
and closes on `Esc`:

```
── Metronome ──────────────────────────────────
  ▸ click       on
    sound       Wood   [3/5]
    volume       80%  [########··]
    subdivision 1/8   (2 clicks per beat)
    swing         35%   (light)   —  every pattern without its own follows this
```

- **`sound`** picks one of five generated click timbres — `Blip`, `Tick`, `Wood`,
  `Beep`, `Two Tone` — which differ in pitch, edge and ring. There is no sample:
  a click is a very short voice on the mid channel, so "which timbre" is really
  "how high, how sharp, how long".
- **`volume`** is the click's own level, applied on that voice, so it is
  independent of the mid channel it borrows its tone from.
- **`subdivision`** is clicks per beat: the beats themselves, the `&`, or the
  sixteenths. The offbeat clicks swing with the row below, which is how the click
  states the groove the patterns are about to play in.
- **`swing`** is the transport's groove, `0%` (straight) to `100%` (the triplet
  feel) in 5% steps. Every pattern without a `swing` of its own follows it, the
  metronome's offbeats follow it, and the exported file follows it — playback and
  export share one arrangement, so a groove cannot be heard but not written.

The Transport panel's rows are a fixed shape: the key is left-aligned in a
14-column field and the value is right-aligned in an 18-column one, so every
value ends on the same edge. The panel is the right-hand column and sits against
the screen edge, and a value column that followed its longest row would move
every time a tempo grew a digit — which is not an alignment. The metronome panel
borrows the same shape, so opening it does not change the column's width.

| Row | `←`/`→` | `Enter` |
| --- | --- | --- |
| `bpm` | one bpm, or ten with `Shift` | type a value |
| `loop` | on / off | on / off |
| `metronome` | click on / off | open the metronome panel |
| `playing` | play / pause | play / pause |
| `key` | every key in turn (see below) | edit the key |
| `[Export MIDI]` | — | write a `.mid` |
| `[Import MIDI]` | — | read one back |

There is no mute-progression control: the row was removed from the transport, and
with it the parameter it set.

**The `key` row walks every key there is**, not just the mode: `←`/`→` moves
through C major, C minor, C# major, C# minor, D major and so on, wrapping at the
octave. `Shift+←/→` moves six choices at a time, which is three semitones — a
fourth up, the interval a modulation usually moves in. Inside the editor `↑`/`↓`
still step a semitone while keeping the mode, so both questions ("a semitone up"
and "the same key in the other mode") have a gesture.

Taps are resolved 300 ms after the last press, so a single tap has a short
delay before it registers.

### Auditioning a chord

There is no "live bar" at the end of the loop any more. Instead, what you are
playing right now — the latched registers plus whatever is under your hands, with
live input winning per side — is the **computed chord**, and there are two ways
to hear it depending on what the transport is doing.

**Stopped, the computed chord is the instrument.** Every change speaks the moment
it happens: no waiting for a bar line at whatever tempo is set, so stepping
through shapes on the keyboard plays them. The note is held while the chord is
held, and when the keys come off it is left to ring for a long half-second rather
than being cut — an organ with a long tail. A new chord arriving inside that tail
*takes the note over* instead of stacking on top of it, which is what keeps a run
of changes legato rather than a smear. The audition has a voice of its own, one
past the four the scheduler uses, so trying a chord can never retrigger or cut
one the loop is playing; starting the transport hands that voice straight back.

**Playing, a change is heard in place.** Select a chord, press `g` to recall it
into the registers, and then modify it: while the loop runs, the computed chord
stands in for *that slot* — its rhythm, its offset and its place in the loop are
all still the entry's, so the change is heard against the rest of the progression
rather than on its own. The `playing` row marks this with a `*`. Moving the
selection exits the audition, as does any edit to the progression, because the
slot it was armed for may no longer be the one you are looking at.

Note length is the fraction of a bar a *patternless* chord sustains (`1/4`,
`1/2`, `3/4`, `whole`) and is cycled from the Synth panel's `note length` row.

### The chord log

Every chord you play this run is written down, and there are two ways to read it
back: the **exact history** — every play in order, duplicates and all — and the
**top list**, the same plays deduped by chord and ranked by how often each came
up, with the ones that are in your progression marked in green. `l` cycles the log
away, on to the history, on to the top, and away again.

```
━━ History [9 played · 5 chords] ━━
       #  chord      deg   notes
      #1  C          (I)   C4 E4 G4              ●
      #2  G          (V)   G4 B4 D5              ●
      #3  C          (I)   C4 E4 G4              ●
      #4  Am         (VI)  A4 C5 E5              ●
  ▸   #5  Dm9        (II)  D4 F4 A4 C5 E5        ●
```

It borrows the rhythm panel's slot, which is the widest thing on screen, so the
chord list and the readout above it stay visible while you read back what you
played. From there the right hand's row above home does the work: `g` and `c`
walk the list and sound each row for 200 ms, `r` sounds the selected chord for as
long as it is held and lets it fade when you let go, and `f` puts it back in the
registers so it can be played — or committed — properly.

**What counts as a play** is the one interesting decision in it. The audition
speaks the instant a chord changes, so pressing the left hand and then the right
hand of one shape sounds *two* chords: the plain triad, then the shape you meant.
A shape the hands merely passed through is not a chord you played, so a chord is
written down once it has been the sounding chord for 150 ms — long enough for both
hands of one chord, short enough that a deliberate change is never missed. And it
is only written down while it is *audible*: with the loop running and nothing
armed, holding a chord is silent, and calling that a play would be a lie about
what you played.

Duplicates are the point of the exact view, and they mean separate holds: the
number beside a chord in the top list is how many times you picked it up, not how
many frames it sounded. There is no persistence — the log is the run, and closing
the app ends it.

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
    pattern  Offbeat Eighths  (edited)   [9/23]
    offset   -1/8   (-480 ticks)
    quant    1/8  —  8 steps per bar, 4 hits
    swing    follow transport  (35%, light)
    hits     on        3 of 8
    length   default  (1/4  —  960 ticks)
    accent   100%  (full)
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
  8, 16, 32 or 64 steps per bar, which is half notes through sixty-fourths, plus
  the two **triplet** grids, 12 and 24 steps per bar — eighth- and sixteenth-note
  triplets, which is what a shuffle, a swing line or a 12/8 blues needs. One
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
- **`hold`** is the *default* length a hit rings, in **ticks**, from a 32nd up to
  the whole bar. `←`/`→` walks the note ladder — a 32nd, a 16th, an 8th, a 3/16, a
  quarter, a 3/8, a half, a 3/4, a whole — so a press lands on a note length
  rather than an arbitrary count. Because it is ticks, changing `quant` does not
  move it: a half note stays a half note on any grid.
- **`length`** overrides that for one hit — the one under the cursor, which stays
  inverted in the grid while this row is selected. `←`/`→` starts at `default`
  and then walks the same ladder, so a Charleston is a dotted quarter *and* an
  eighth rather than two of whichever one the pattern was assigned with. Setting
  a hit back to the default drops the override rather than storing a copy of it.
- **`accent`** is how hard that one hit plays, 20% to 100%, multiplied by the
  take's own level. It is how a metre gets a backbeat that is quieter than its
  downbeat without a second layer — `Accented Eighths` in the palette is one
  layer and three accents. Both rows refuse a cell with no hit on it rather than
  changing a number you cannot hear.
- **`swing`** is this pattern's own groove: `follow transport`, then straight
  (0%) through to the triplet feel (100%) in tenths. A pattern that names its own
  swing ignores the transport's; one that follows it moves with the metronome
  panel. Swing pushes every *second* cell of the grid later, so downbeats never
  move — at 100% an offbeat lands exactly where the triplet would, which is why
  the same amount means the same groove on any straight grid. A pattern written
  on the 12 or 24 triplet grid ignores swing entirely: there is no straight
  offbeat pair left in it to stretch.
- **`mute`** silences the end of the bar, up to a quarter note, on the same
  ladder. Nothing *starts* inside the muted tail and anything ringing into it is
  cut at the boundary, which is what makes a pattern stop short instead of
  bleeding into the next bar. The tail is a position in the *pattern's* bar, so it
  moves with the chord's offset — an anticipated chord's tail is anticipated too,
  and a downbeat hit (position 0 of its own bar) can never be swallowed by it.
  `none` is the default and means no mute at all: a hold still crosses
  the bar line.
- **Every one of these is live.** Each chord *owns* its rhythm: the panel edits
  the copy the selected entry plays, and the scheduler reads that entry, so a
  hold, an accent, a swing or a mute is heard on the next bar with nothing to save
  and nothing written to disk. Two chords can both have been given `Quarters` and
  then drift apart, because assigning takes a copy rather than pointing at a
  shared library entry.
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
- **The `chord` row** (`←`/`→`) picks which entry the panel describes. The panel
  has no cursor of its own: this row *is* the **Progression** panel's selection,
  so stepping it here moves that cursor and every other row — pattern, offset,
  grid — follows in the same press. Pressing `←` past the first chord stops
  rather than wrapping, and a rest is a stop like any other
  (`#2  —  (a rest)`). There is nothing to save first: every row writes through,
  so stepping away never leaves an edit behind.
- **The `pattern` row** (`←`/`→`) cycles `(none)` and then every pattern in the
  library, so assigning and clearing are the same gesture — and assigning takes a
  copy. It shows the chord's own rhythm, with `(edited)` when that rhythm no
  longer matches the library pattern it is named after, and its place in the
  palette as `[8/23]` so a long walk says how far it has to go. `Shift+←/→` jumps
  five at a time; the count is what tells you whether the jump overshot.
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

`rhythms.toml` ships with a palette of patterns, ordered by feel so that the
`pattern` row — which cycles one press at a time — never jumps between unrelated
things:

- **Held** — `Held Whole`, `Held 3/4`, `Held Half` and `Two Feel`: one hit held
  for a note value, from a whole-bar pad down to the half notes on 1 and 3.
- **Straight** — `Quarters`, `Eighths`, `Offbeat Eighths`: the grid filled at a
  note value, and the same with every hit moved onto the "&".
- **Sixteenths** — `Offbeat 16ths` (the "e" and "a" of every beat, the gap
  between `Offbeat Eighths` and `Sixteenth Pulse`), `Dembow` (the 3-3-2
  reggaeton cell with the beat displaced), `Charleston` (dotted quarter answered
  by the "&" of 2), `Tresillo` (3+3+2), `Syncopated 16ths` and `Sixteenth Pulse`.
- **Triplets** — `Swung Eighths`: the first and third triplet of every beat,
  which is a shuffle written straight.
- **Texture** — `Damped Quarters` (damped, with the last quarter muted) and
  `Accented Eighths` (downbeats at full level, offbeats at 55%): the two built-ins
  that ship a mute and a per-cell accent, because otherwise neither row is
  discoverable from the palette. `32nd Roll` is a fill.
- **Phrases** — see below.

`Charleston` and `Tresillo` also ship per-cell lengths rather than one uniform
hit: the Charleston's first hit is a dotted quarter and its answer an eighth, and
the tresillo is really 3+3+2. Each pattern writes its default `hold` in ticks, so
a pattern's length is legible in the file, and the per-cell overrides appear only
on the patterns that need them.

### Multi-bar phrases

A pattern is always **one bar**. A figure that spans two or four bars is
therefore a *set* of one-bar patterns whose names carry their place in it:
`Jazz Chorus 1/4`, `2/4`, `3/4`, `4/4`, or `Son Clave 1/2` and `2/2`. Assign
them to consecutive chords and the phrase is the progression; the numbering is
the whole contract, so each set is kept together as one run in the palette.

- **`Jazz Chorus 1/4`–`4/4`** is a four-bar comp on the sixteenth grid, short
  holds throughout: state the pulse on 1 and 3, answer it with a Charleston, add
  the "&" of 3, then tighten the same figure into a turnaround fill.
- **`Son Clave 1/2`, `2/2`** is the 3-2 son clave: the three-side (1, the "&" of
  2, beat 4) and then the two-side (beats 2 and 3), on the eighth grid.

This is what the one-bar model buys: a phrase is data, not a mode, and any bar of
it can be swapped, offset or muted on the chord it lands on. The cost is that the
bars have to be lined up by hand — assign `1/4` to the first chord, `2/4` to the
second, and so on — and a set used out of order is just four unrelated bars.

That palette lives in the **tracked** `rhythms.toml`, and your own patterns live
in `rhythms.user.toml`, which is gitignored and created empty the first time the
app runs. Loading is the defaults with your entries layered over them by name, so
a pattern you saved or edited — including one edited under a built-in's name,
like `Eighths` with a different hold — is always yours, while a built-in you
never touched keeps coming from the tracked file and can therefore still change
under a later build. `[Save Pattern As...]` writes only your file. The two are
kept honest by a test: the tracked file must equal the built-ins compiled into
the binary, which are also what stands in if the file goes missing.

### Panels

`Tab` / `Shift+Tab` cycles focus:

```
Progression → Transport → Sinko → Synth → Ensembles → EQ → Spectrum → FX → (wrap)
```

Within a panel: `↑`/`↓` move the row and `←`/`→` adjust the selected value.
`Shift+←/→` is the coarse step wherever one makes sense — ten bpm on the tempo,
five entries at a time in the pattern list, six key choices, four rungs of
offset — and is the plain step everywhere else, because a toggle is a toggle. `Shift+↑/↓` extends a
range in the Progression panel, which is the one place `↑`/`↓` picks more than a
row.

Two panels are two-dimensional, and take plain `←`/`→` as "pick the column"
with `Shift+←/→` as the value nudge: the **Synth** table, and the **Progression**
panel while a run of chords is selected (its menu column appears then — see
below).

Four views share one slot on the screen — **Synth**, **Ensembles**, **EQ** and
**Spectrum** — and a fifth, **FX**, is the effect rack opened out. They are
separate stops on `Tab` rather than sub-tabs of one panel because each is a page
of its own height: a thirteen-band curve and a six-slot rack would cost the whole
layout on every panel that had to leave room for them.

The **Synth** panel is three channels side by side as columns with the master
block underneath, **paged** because two dozen settings per channel do not fit on
one screen:

```
━━ Synth [filter 5/7] ━━
    param           low           mid           high
    instrument      Upright Bass  Grand Piano   Choir Ooh
  ▸ cutoff          [700 Hz]      1200 Hz       2200 Hz
    resonance       35%           35%           30%
    filter type     LP            LP            LP
    filter env      +45%          +50%          +50%
    filter attack   2 ms          2 ms          2 ms
    filter decay    220 ms        180 ms        140 ms
    key track       35%           40%           45%

    subtype         hall          preset          Hall
    size            50%           damp            20%
    predelay        0 ms          reverb level    22%
    subtype         digital       preset          Digital Delay
    time            375 ms        feedback        35%
    tone            85%           sync            off
    division        1/4  500 ms   delay level     0%
    master volume   5             master mute     off
    lfo rate        5.00 Hz       lfo wave        sine
    note length     whole
```

(The values are `Plucky`'s filter page, with three instruments picked from the
library for the row above; the screen itself draws no key reminders — this
document is the reference for those.)

The **`instrument` row** is the one to know about. `Shift+←`/`→` on it steps
through the library and loads each one into whichever register the cursor is in,
**live** — so auditioning an instrument is holding `Shift` and tapping an arrow
with the loop playing. `Enter` opens a picker where `↑`/`↓` auditions, `Enter`
keeps and `Esc` puts the register back exactly as it was; pressing `s` in there
keeps the register's current design as a new instrument, named and written to
`instruments.user.toml`. The row shows `custom` for a register that never came
from the library and a leading `*` for one that has been changed since, so it
never names an instrument the register no longer is.

Swapping an instrument changes **only what the sound is**. Its level, pan,
transpose and reverb send belong to the register rather than to the instrument, so
they stay exactly where you set them — auditioning never rebalances the mix you
are auditioning in.

**Three levels, not two.** An *instrument* is a register-neutral voice. A
*placement* is a named instrument plus where it sits. An *ensemble* is three
placements and the mixer, which is what the palette ships forty-two of. The split
is what lets `Celesta` — Marimba's top register, an octave up — be dropped into
the bass and still be a celesta.

Saving the **orchestra** is the existing `[Save As...]` on the Ensembles panel:
an ensemble is three placements plus the mixer, so the whole thing travels
together under one name — and each placement's equaliser curve travels with it.

`PageUp`/`PageDown` change page and the current one is named in the title; the
seven pages are `tone`, `osc`, `pluck`, `env`, `filter`, `mod` and `fx`. The master block is on
all of them, so the effects and the master volume are always one keystroke away.
Every page is padded to the same height, so paging never moves the panels below
it.

The master block is where the **two aux units** live — the reverb and the delay
that every register sends to. Each is a whole effect: a `subtype` row, a `preset`
row that walks the library for that variant, and the unit's own parameters, all
sitting beside the return level that decides how much of it you hear. Both units
run **fully wet**, because the return level is the one number that decides the
balance; two numbers doing that job would be one too many, so the units' own
`mix` never appears. The reverb's `size`, `damp` and `predelay` and the delay's
`time`, `feedback`, `tone`, `sync` and `division` are the same parameters the FX
panel edits in an insert slot — the same effect, in a different position.

The **`fx` page** is a rack seen from above: the two sends, then one row per
insert slot, showing what each of the three registers has in that place.
`Shift+←`/`→` on a slot row swaps the whole family, which is the fast way to find
out whether a rack wants a phaser at all, and `Enter` opens the FX panel on that
slot to edit it properly.

There, `←`/`→` moves between the channel columns, **`Shift+←`/`→` changes the
value**, and `Enter` opens an edit on the selected cell: the arrows adjust it
while you hear the result, `Enter` keeps it and `Esc` puts the old value back.
Editing with a chord held needs `Shift`+arrows, because `Enter` still commits the
chord first — the same rule as every other value row.

`Esc` reaches the open editor before it can mean "stop": it reverts and closes,
and only a plain `Esc` with nothing open is the stop gesture.

Paging is the honest answer to a panel that outgrew the screen. An earlier
version had merged four synth sub-tabs into one table precisely so that a
channel's volume and its cutoff could be seen together; at eleven settings that
worked, and at twenty-four it cannot. Grouping the settings by what they act on
keeps each page short enough to read at a glance, and the mixer never leaves.

The **EQ** panel is the sixth stop, and it borrows the Synth panel's slot: a
thirteen-band graphic equaliser, one curve at a time, for the three registers and
the mix.

```
━━ EQ [mid] ━━
    target   mid — Rhodes Dark
    preset   De-Mud
    band     6/13  250
    gain     -3.0 dB
  ▸ [Save Curve As...]

           -12 dB──────0──────+12 dB
       20  ──────────██┼────────────    -1.5
     31.5  ──────────██┼────────────    -1.5
       50  ─────────███┼────────────    -3.0
       80  ───────█████┼────────────    -4.5
      125  ───────█████┼────────────    -4.5
  ▸   250  ─────────███┼────────────    -3.0
      500  ──────────██┼────────────    -1.5
       1k  ────────────┼────────────     0.0
       2k  ────────────┼────────────     0.0
       4k  ────────────┼────────────     0.0
       8k  ────────────┼────────────     0.0
    12.5k  ────────────┼────────────     0.0
      16k  ────────────┼────────────     0.0
```

The four rows are `target` (`low`/`mid`/`high`/`master`), `preset`, `band` and
`gain`; the curve underneath is the thirteen bands the rows make. `←`/`→` on
`target` cycles the four equalisers, on `preset` steps through the library
(`Shift` five at a time) and writes the whole curve, on `band` walks the thirteen
bands (`Shift` four at a time) and on `gain` moves that band half a decibel
(`Shift` three). `Enter` on `gain` returns that band to zero, which is the one
"put it back" worth a keystroke mid-sweep. `Enter` on `[Save Curve As...]` names
the curve on screen and writes it to `eq_presets.user.toml`, where it joins the
twenty-five shipped curves the `preset` row offers.

The `preset` row's value is *derived* from the thirteen gains, not remembered:
`Flat` when the curve is bypassed, a library name when it matches one exactly,
and `custom` otherwise — so the row can never name a curve that is not on the
screen. Your own curves shadow the shipped ones by name, exactly as instruments
and ensembles do.

Both ends of the range are shelves and the eleven between them are bells. The
bands are `20 31.5 50 80 125 250 500 1k 2k 4k 8k 12.5k 16k` Hz, each band spans
±12 dB, and **your EQ is part of the placement, not the instrument**: swapping the
instrument in a register leaves its curve exactly where it was, because a curve
sees the register while an instrument is register-neutral. That also means the
`instrument` row's `*` marker never appears because you moved an EQ band — the
sound has not changed, only where it sits.

The **Spectrum** panel is the seventh stop: a live readout of thirteen band
levels, for whichever register the shared `target` row is pointed at, or for the
mix.

```
━━ Spectrum [master] ━━
    target   master
    range    medium  60 dB
    speed    medium  24 dB/s
    hold     on
  ▸ [Reset Peaks]

   0 ┤     ▄▄▄▄▄▄▄▄▄▄
           ██████████▄▄▄▄▄
      ████████████████████▄▄▄▄▄
 -15 ┤██████████████████████████████▄▄▄▄▄
      ████████████████████████████████████████
      █████████████████████████████████████████████
 -30 ┤██████████████████████████████████████████████████
      ███████████████████████████████████████████████████████
      ███████████████████████████████████████████████████████▄▄▄▄▄
 -45 ┤████████████████████████████████████████████████████████████▄▄▄▄▄
      █████████████████████████████████████████████████████████████████
 -60 ┤█████████████████████████████████████████████████████████████████
         20  31.5  50   80   125  250  500  1k   2k   4k   8k  12.5k 16k
```

The bands are **the EQ's own ladder**, one level per band, so a level and the
curve shaping it are read on the same thirteen columns. `target` is the *same
cursor* the EQ panel uses — tabbing between the two keeps you on the same part,
which is the whole reason both exist. `range` picks a 48, 60 or 72 dB span,
`speed` the envelope release (48 / 24 / 8 dB per second: fast shows a rhythm,
slow shows a balance) and `hold` whether the held peaks are drawn as a tick above
each bar. `Enter` on `[Reset Peaks]` drops them.

Three of the four taps are taken **after each register's curve, its effect rack
and its fader**, so a register reads what it contributes to the mix: mute it and
its meter goes quiet, a distortion shows up as its harmonics, pan it hard and it
still reads its own level. The fourth is the finished mix,
after the master curve, the master gain and the soft clip — so it is what leaves
the device. A register can therefore read past full scale, because it is measured
before the clip; it pegs at the top rather than being hidden, which is the honest
thing for a meter to do.

It is a **filter bank rather than a transform**: thirteen bandpass biquads per
tap, reusing the same biquad the EQ's bells use. No dependency, no window, no
buffered block, and a cost that is a fixed 52 filters a sample — **about 1 % of
one core**, measured. Peaks are held by the panel rather than the audio thread,
because the panel reads all 52 published levels every frame and so never misses
one.

The **FX** panel is the eighth stop: one register's insert rack, six slots, with
the one under the cursor opened out into its kind, its variant, its preset and
its parameters.

```
━━ FX [mid 3/6  distortion · soft] ━━
  ▸ rack          mid
    slot          3 of 6
    type          distortion
    subtype       soft
    preset        Soft Clip
    param         1 of 4  drive
    value         +18.0 dB
    [Move Earlier]
    [Move Later]
    [Empty This Slot]
    [Save Effect As...]
```

`rack` cycles `low`, `mid` and `high`, and `slot` cycles the six places in it.
`type` cycles the fifteen kinds — `none`, then `reverb`, `delay`, `chorus`,
`flanger`, `phaser`, `distortion`, `fuzz`, `bitcrusher`, `ringmod`, `tremolo`,
`filter`, `wah`, `compressor`, `gate` — and loads the new kind's own defaults, because a
kind that arrived wearing the last kind's numbers would be a rename rather than a
sound. `subtype` is the variant within the family: `hall`/`room`/`plate`/`chamber`/
`ambience` for the reverb, `overdrive`/`soft`/`hard`/`tube`/`fold`/`rectify` for
the distortion, and so on; choosing one loads *its* defaults for the same reason.
`preset` walks the shipped and saved settings that match the kind and variant on
screen, and reads `custom` the moment a parameter moves.

Six parameters is more than a fixed row list can hold: a slot whose kind changed
would move every row below it out from under the cursor. So the parameters are
reached through a **selector** — `param` chooses which one, `value` moves it,
exactly as the EQ panel's `band` and `gain` rows work for its thirteen bands.
`[Move Earlier]` and `[Move Later]` swap the slot with its neighbour and the
cursor follows the *effect*, because the reason to move a distortion is to put it
in front of the chorus; the ends of the rack refuse rather than wrapping, because
a rack is an order and not a wheel. `[Empty This Slot]` sets it back to `none`,
and `[Save Effect As...]` names it and writes it to `fx_presets.user.toml`.

An empty slot costs nothing at all: the audio thread checks each rack once per
buffer and skips a rack with nothing in it before touching its first slot, which
is why every shipped ensemble — none of which uses a rack — sounds and costs
exactly as it did before this feature existed.

### Selecting more than one chord

The Progression panel's cursor is a **selection**. Plain `↑`/`↓` moves it and
selects exactly the row it lands on; `Shift+↑/↓` extends a range from wherever
the cursor was, so three presses down from the first chord selects four of them.
`Cmd+A` (or `Ctrl+A` where the terminal does not report `Cmd`) selects the whole
progression. The header shows `2 selected` whenever a range is up, because a
count is what tells you whether the next action will hit four chords or three.

```
━━ Progression  4 selected ━━
  ▸   C      Held 3/4        0     replace
      C7     Held 3/4      -1/16    reverse
      C      Quarters       1/8     rotate
      F      Offbeat Eighths 0      clear
```

The cursor row is bold inside the range, so the moving end stays findable; a
chord that is *sounding* is still red and outranks both.

Every progression action takes the whole selection:

| Action | Key | What a range does |
| --- | --- | --- |
| copy | `x` (`q`) | copies the run, rests included, in order |
| paste | `c` (`j`) | inserts the run after the block, and selects what it pasted |
| delete | `v` (`k`) | removes exactly the run, as one undoable edit |
| replace | menu | gives every selected slot the register chord, each keeping its own rhythm |
| reverse | menu | turns the run around in place |
| rotate | menu | rolls it one place, last chord to the front |
| clear sinko | menu | takes the rhythms and offsets off, leaving the chords |
| undo / redo | `b` / `Shift+B` | one undo per group action, not one per chord |

Copy, reverse, rotate and clear leave the list the same length, so the selection
survives them — pressing rotate twice is how you get the other direction, and
`len - 1` times walks a phrase right round.

The **rhythm** clipboard is a list too. `Shift+Q` copies one rhythm per selected
chord — and a chord with no rhythm copies as "no rhythm", so a phrase's shape
survives the round trip. `Shift+J` lays that list across the selection **in
order, repeating**: a single copied rhythm lands on every selected chord, four
copied rhythms land one per chord, and two copied rhythms alternate across five.
A Progression selection outranks the cursor in either panel, so tabbing to Sinko
to press `[Paste Sinko]` does not quietly narrow "all of these" down to one
chord.

**Pasting into position 1.** `paste` inserts *after* the cursor, which cannot
reach the front of the list. So `↑` from the first chord moves the cursor into
the gap above it, drawn as an orange rule:

```
━━ Progression ━━
  ────────────────────
      C      Held 3/4        0
      C7     Held 3/4      -1/16
```

`c` there pastes at position 1, and `Enter` adds the chord you are playing at the
front. Nothing else acts in the gap — copy, delete and the reordering keys all
say so rather than guessing which chord you meant — and `↓` comes back onto the
first chord.

`Enter` commits the chord you are currently playing — the latched registers plus
whatever keys are down right now, with live input winning per side. It works
from **any** panel, so you don't have to navigate to the Progression panel to
capture a chord. In the Progression panel it inserts after the selected row;
anywhere else it appends. `Ctrl+Enter` always appends, even when no chord
resolves.

The menu's **`replace`** sets the selected slots to what the registers resolve
to, rather than inserting beside them. That is the gesture for fixing a chord you
already placed: each slot's **rhythm pattern and offset stay with it**, because
they belong to the entry and not to the chord. Delete-and-re-insert would lose
them. Selecting a rest turns that rest into a chord, since there is no rhythm to
keep; running it twice changes nothing and does not touch the undo history.

When nothing resolves, `Enter` falls through to the focused panel's own action:
edit BPM or track key, toggle loop or mute, load a preset, or — in the
Progression panel — offer to add a rest.

The **Presets** panel's last row, `[Save As...]`, opens a text prompt and writes
the full sound design to `ensembles.user.toml`.

### Editing hotkeys

The row **below the home row** is the hotkey row, plus `g` on the home row
itself — the one home-row position the chord grammar never uses — and the right
hand's row **above** the home row, which the log owns. None of them ever join the
held set, so they stay usable for editing while both hands are holding a chord.

Two modifiers exist and neither is a shortcut layer: `Shift` selects the *second*
action on a physical key (redo on undo, the rhythm clipboard on the chord
clipboard), and `Cmd`/`Ctrl` is used exactly once, for `Cmd+A` select-all — which
has to be intercepted before the chord grammar, or `a` would simply be held.

The table of every key — which physical position, which character reaches the
app, and which command it runs — is in
[REFERENCE.md § Editing hotkeys](REFERENCE.md#editing-hotkeys). It is not
repeated here, because a second copy is a second thing to keep true: the test
that checks the table checks that one.

"Keycap" there is the QWERTY label printed on the key; "you type" is the
character that reaches the app under Programmer Dvorak. In the key row drawn at
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
a new slot after the block, `Shift+J` retimes the slots you are on.
Pressing one elsewhere flashes rather than doing nothing silently. Register
locks are performance controls and work everywhere.

**Replace, reverse, rotate and clear-sinko have no key of their own.** They are
the Progression panel's menu, drawn as a right-aligned column beside the chord
list, because they are operations you think about rather than reach for
mid-performance. `↑/↓` walks the items and `Enter` runs one, even while a chord
is held.

**The menu appears only while a run is selected**, because every command in it
acts on a group: with one chord they would all be no-ops, and the column would
cost width for nothing. That is also what the arrows mean — with one chord the
panel has a single column and `←/→` is the offset nudge it has always been; with
a run selected it has two, so `←/→` picks the column and `Shift+←/→` is the
nudge. Collapsing the run back to one chord folds the menu away and brings the
cursor back to the chords.

```
━━ Progression  2 selected ━━
      C      replace
  ▸   G    ▸ reverse
      Am      rotate
      F       clear

━━ Progression ━━
  ▸   C      Held 3/4        0
      C7     Held 3/4      -1/16
```

Undo covers every structural change: add, delete, paste, replace, reverse,
rotate and clear. One group action is one undo, however many chords it touched.
History is 128 edits deep, and a fresh edit discards the redo stack. The Progression panel
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
- The computed chord is **not** included — an export is a function of the
  progression alone, audition or no audition.
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
| `rhythm_store.rs` | The pattern *palette*: the tracked `rhythms.toml` plus the user's `rhythms.user.toml`, which is what `[Save Pattern As...]` writes and what the `pattern` row offers. The only filesystem code in the rhythm feature. |
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
list, then the transport), then down through the rhythm panel to the synth slot —
so `Shift+Tab` retraces it and the cursor always moves the way the eye does. The
focused panel is the one wearing the heavy rule and the cyan band. The Sinko panel
and the Synth slot expand when focused; the chord list, the transport and the
metronome are always open, which is what keeps the default view 15 rows.

Four `Tab` stops share that last slot — **Synth**, **Ensembles**, **EQ** and
**Spectrum** — and it draws whichever one has focus. That is why a page of
thirteen EQ bands and a twelve-row spectrum cost the other views nothing: they
are four faces of one panel, not four panels.

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
| `lib.rs`           | Declares the modules, so the audio path is reachable from `tests/` and `cargo bench`. |
| `main.rs`          | The binary: one call to `tui::run_interactive()`.                     |
| `music.rs`         | Theory core: scales, degrees, transformations, voicing, note labels.  |
| `midi.rs`          | Pure MIDI model: `Score`, layers, `split_layers`, `render_progression`. |
| `smf.rs`           | Pure Standard MIDI File writer and reader (format 0 single track / format 1 per layer). |
| `project.rs`       | The versioned session document embedded in an export, so it can be imported back. |
| `export.rs`        | MIDI export/import filenames and file I/O.                            |
| `keyboard.rs`      | Physical positions, layout translation, `ACTIVE_LAYOUT`.              |
| `grammar.rs`       | `PositionSet` → (degree, transformation). Pure, layout-free.          |
| `progression.rs`   | Slots, rests, registers, clipboard, edit operations.                  |
| `transport.rs`     | Shared transport state + background scheduler thread, walking `arrangement`'s plan against absolute deadlines. |
| `synth.rs`         | cpal stream, voices, ADSR, SVF filter, reverb, channel apply/capture. |
| `wavetable.rs`     | Stored single cycles built from harmonic recipes, and the drawbar registrations. |
| `voice.rs`         | `VoicePatch` — what a sound is — and `ComposedChannel`, the voice-plus-placement pair the audio layer takes. |
| `instrument.rs`    | The instrument library: named register-neutral voices.                |
| `ensemble.rs`      | `Ensemble` and `Placement`, and the palette store.                    |
| `eq.rs`            | The thirteen-band EQ: the curve type, the biquad cascade, and the shipped curve library. |
| `fx.rs`            | What an effect *is*: the fifteen kinds, their variants and parameters, and the preset library. |
| `fx_dsp.rs`        | The effect algorithms themselves, and the bank of twenty the callback runs. |
| `analyzer.rs`      | The live spectrum: a bandpass filter bank and envelope followers, one bank per register plus the mix. |
| `debug_log.rs`     | `debug.log` writer and the output-level tap thread.                    |
| `timing.rs`        | The callback's share of its buffer deadline, and the named scopes behind `CHORD_TOOL_TIMING`. |
| `history.rs`       | What has been played this run: the exact log and the deduped top list. |
| `settings.rs`      | The handful of things that survive a restart: key, tempo, master volume. |
| `tui.rs`           | App state, event loop, rendering, key handling.                       |

And beside `src/`: `tests/` renders the audio offline and stresses it, `benches/`
measures it with criterion, and `scripts/` is the two commands worth remembering
(`check.sh` gating, `bench.sh` reporting).

### Threading

Three threads plus the audio callback:

- **Audio callback** (`cpal`): renders voices, shapes the buses, and measures the
  spectrum. It reads parameters exclusively through lock-free
  `AtomicU32`-backed `SharedF32` values, so the UI can never block or be blocked
  by audio — and it publishes the spectrum's 52 levels back the same way, once
  per buffer.
- **Scheduler** (`transport.rs`): lays the loop out through
  `arrangement::arrangement`, slices the bar it is on, and posts a
  `SchedulerEvent` for each onset, release and metronome click over an mpsc
  channel. Every event is timed against an **absolute deadline** from the bar's
  start instant, so a bar of 64th notes cannot accumulate drift; it checks for
  stops and seeks every 5 ms, so a seek still feels immediate. It also publishes
  each bar's start instant, which is the clock tap capture reads.
- **Output tap** (`debug_log.rs`): samples the peak level at 60 Hz into
  `debug.log`, and once a second writes the audio timing beside it — the
  callback's share of its buffer deadline, its worst buffer, its count of missed
  deadlines, and (with `CHORD_TOOL_TIMING` set) the last second's distribution
  for the scheduler, the draw loop, import, export and start-up.
- **Main thread**: renders at a 5 ms poll and drains scheduler events.

The progression is shared as `Arc<Mutex<Progression>>`; the lock is only ever
held briefly by the UI or the scheduler, never by audio.

### Notes on the synth

Three channels (`low`/`mid`/`high`) split a chord by register: the lowest note
goes low, the highest goes high, everything between goes to mid. Each channel has
**two oscillators** — the second with its own waveform, interval, level, a depth
and a choice of what that depth bends: the first one's phase, its frequency in
hertz, or its frequency by a ratio — drawing on four computed shapes, a plucked string, eighteen
stored wavetables and a noise source, with pulse width, a wavetable position, phase
distortion and extra white noise mixed in alongside them. Then amp ADSR and
envelope curve, a filter envelope with its own attack and decay, key tracking,
cutoff and resonance, a choice of five filter outputs, drive, four LFO
destinations, velocity to cutoff and pulse width, unison and detune, glide,
transpose, reverb send, delay send, and pan. The filter is a Chamberlin
state-variable design, and the reverb the sends feed is a Schroeder-style network
of four combs into two allpasses — the same one this crate has always had. Master
output is soft-clipped with `tanh`.

The LFO's **rate and shape are global** while its four destinations (pitch,
cutoff, amp and pulse width) are per channel: one vibrato wobbling the whole
chord is what the ear expects, and three LFOs at three rates is a chorus, which
is a different feature. Each voice restarts its
own LFO phase on trigger, so a chord's vibrato arrives with the chord rather
than wherever a free-running LFO happened to be.

**Unison is normalised by how the stack actually sums.** Detuned copies of one
note add incoherently, so their sum grows with `√n`; copies at the *same*
frequency add coherently and grow with `n`. Dividing by the wrong one is a bug in
both directions — `√n` on a coherent stack is up to 2× too loud at four voices —
so the divisor follows the detune. Widening a voice changes its sound, not its
level.

**Reverb is additive.** The three per-channel sends feed one mono tank, and the
reverb's *return level* sums its output *on top of* the dry signal — it never
attenuates it. At level 0 the dry path is untouched; turning reverb up only ever
adds. The per-channel sends decide how much each register feeds the tank, so
with every send at zero the level does nothing. The delay's return works the same
way, at unity rather than at the tank's three, because a delay's output is a copy
of the signal where the tank's is quiet by construction. A return at level 0 is
not run at all, so turning a send up starts its unit from silence rather than
releasing a tail from a bar ago.

Both units are **fully wet** and their own `mix` is never written: how much of
them you hear is the return level, and two controls doing one job is one too
many. That is also why the master block shows each unit's `subtype` and `preset`
but not its `mix` — the position decides it.

**The equaliser runs on the buses, not in the voices.** Each placement — each of
the three registers — has its own thirteen-band curve, and the mix has one more.
They are four coefficient sets driving five filters, 65 biquads a sample in
all — measured at **1–1.5 % of one core** with every band moved, against
11–29 % for the voice pool alone. The same thing per voice would be thirteen biquads
times 121 voices: more than the entire rest of the callback, for a decision that
belongs to the part rather than to the note. The register curve sits *before* that
register's fader, so it shapes the reverb send
as well as the dry sound; the master curve sits after the reverb and before the
master gain, on the finished stereo pair, so `master` means what it says. A curve
that is flat in all thirteen bands is not run at all — exactly, not nearly — which
is what lets every shipped ensemble sound bit-for-bit as it did before the
equaliser existed.

**The spectrum is a filter bank, not a transform.** Thirteen bandpass biquads per
tap over four taps — the three registers and the mix — each with an envelope
follower: 52 filters a sample, measured at about 1 % of one core, with no
dependency,
no window function and no buffered block. That is a deliberate trade against an
FFT. A transform would give more resolution than a thirteen-column display can
show, and it would have to be computed for all of it and thrown away; a bank can
be pointed at exactly the ladder the equaliser uses, so a level and the curve
shaping it share a column. The filter is the same `Section::bandpass` the
equaliser's bells are built from, which is what keeps the two from disagreeing
about what a band *is*.

**The effect rack is bus-level work, and that is the whole design.** Each register
has a chain of six insert slots and a send to each of two master units, so a
fully loaded rig is twenty effects — three racks of six, plus the reverb and the
delay. All twenty, every slot filled and driven with a real signal, measure
**about 2 % of one core**, against 11–29 % for the voice pool alone. That is not
because the effects are cheap; it is because they are done once per sample on the
bus rather than once per voice, and the same distortion inside every voice would
be 121 copies of it. The bank is sized and allocated when the synth is built —
about 8 MB, mostly delay lines — and the callback never allocates. A rack with
nothing in it is skipped before its first slot is touched, which is what keeps
every shipped ensemble bit-for-bit and as cheap as it was before the feature
existed.

Fourteen families ship: reverb, delay, chorus, flanger, phaser, distortion, fuzz,
bitcrusher, ring modulation, tremolo, filter, wah, compressor and gate, with
fifty-four variants between them and six parameter slots each. An effect is a
*kind*, a *variant* and up to six numbers, and a **variant is a starting point
rather than a label**: choosing one loads that variant's own defaults, because
`plate` and `hall` are the same four slots with different numbers and a choice
that did not move them would be inaudible. The delay's `sync` is the one control
that needed a sixth slot: a delay locked to a note value is locked to the
*tempo*, so what it stores is the note value rather than the milliseconds, and it
follows every tempo change from then on. `REFERENCE.md` §9 lists every family and
its parameters, and says plainly what is *not* modelled — no convolution reverb,
no tape hysteresis, no oversampling, no true stereo.

**The wavetables are sourced, not invented.** The tonewheel registrations come
from a drawbar reference, the pipe-organ spectra from the stop families — a
diapason, a gedeckt, a reed — and the vowel tables from measured formant
frequencies. Two things that looked promising turned out to be impossible and are
recorded as such: the Risset bell's partials sit at 0.56, 1.19 and 2.74 times the
fundamental, and a periodic table can only hold whole multiples, so inharmonic
spectra are out of reach. The same limit makes a saxophone unrepresentable, since
its character is a formant that moves rather than a fixed spectrum.

**Wavetables are cheaper than the sine.** A stored cycle is read with one
interpolated lookup, where `sine` is a libm call — so the additivity is not a
performance compromise but a small win. Measured on the worst case the scheduler
can produce, 120 voices all sounding: 15.0 % of one core with drawbar tables
against 19.7 % with sines.

**Tonewheel organs are now real.** `Drawbar Organ` used to be three sines
standing in for nine drawbars, because a wavetable is periodic and two of the
nine drawbar pitches — the 16' and the 5 1/3' — are not whole-number multiples of
the played note. The fix is to put the table's fundamental an octave *below* the
key: multiply the drawbar set by two and it becomes 1, 3, 2, 4, 6, 8, 10, 12, 16,
all integers, with the eight-foot drawbar as the second harmonic. Press C4, the
table's fundamental is C3, and all nine drawbars land where a tonewheel organ
puts them. This is a *timbre* fix and not a voicing one: `allocate()` is
untouched.

**The voice grew a second oscillator, a string, and two ways to move a spectrum.**
Each of these is skipped at its neutral value, so every sound written before them
renders to the sample — which the legacy oracle and the palette fingerprint both
hold. Measured on 120 voices: `position`, `phase dist`, `drive`, `feedback` and
`pluck` all sit within noise of the plain voice — each is one branch and a couple
of multiplies — and *running the second oscillator at all* is the thing that
costs, about half again as much. Which domain it modulates in adds nothing
measurable; the expensive column is the oscillator, not the arithmetic.

- **The filter has five outputs.** A notch (`low + high`) and a peak
  (`low - high`) fall out of the three the state-variable form already computes,
  for one add each. A notch is a hollow, phasey colour no setting of the other
  three reaches.
- **Drive** is a pre-gain into a saturator with no make-up gain: a driven filter is
  louder as well as richer, which is what the knob does on the hardware. The
  saturator is a divide rather than a `tanh`, because this runs once per voice
  instead of once on the bus.
- **Velocity is the accent.** It already reached the voice as the note's gain —
  that is what an accent is — and it now arrives a second time so a patch can make
  it a change of *tone* too: `vel cutoff` closes the filter, `vel pwm` widens the
  pulse. Both are zero at full velocity, so a hand-played chord is untouched.
- **`position` blends the chosen waveform into the next one in its octave group.**
  This is the one thing a filter cannot do: a filter tilts the *envelope* of a
  spectrum, while a position moves between two sets of partials — `glass` into
  `vox`, `vox aah` into `vox ooh`. The pairs never cross an octave, because a
  drawbar table advances its phase at half the rate of everything else and the two
  could not be blended at one phase.
- **Phase distortion** bends the cycle with a moving breakpoint, turning a sine
  into a ramp **without moving its period**. Zero is exactly identity.
- **A second oscillator** with its own waveform, interval and level, plus FM: it
  is added to the *first* oscillator's phase before the lookup, so the carrier's
  pitch never moves and the sidebands stay symmetric. That is the difference
  between a bell and a wobbly detune.
- **Cross-modulation, in three domains.** `fm mode` chooses *what* the second
  oscillator bends. `phase` is the DX sound and carries the same index at every
  pitch; `linear` bends the frequency in hertz, so the index grows as the note
  falls — the growl, and through-zero when the deviation is wider than the note;
  `expo` bends it by a ratio, which is the analog X-Mod and keeps the clang fixed
  across the keyboard. The exponential one needed its mean divided back out: the
  mean of `2^(D sin)` is above one, so it played up to a hundred and forty cents
  sharp, which in a chord is a wrong note rather than a colour.
- **Operator feedback.** One sample of the oscillator's own output bending its own
  phase: a sine folds into a ramp, which is a saw for the price of a compare, and
  past the fold it goes broadband, which is a percussion and breath source.
- **Per-voice ring modulation**, which the rack's `ringmod` cannot do — its
  oscillator is a fixed hertz, so it rings at one pitch whatever is played, while
  this one puts the sum and the difference of the two oscillators into the output
  and both move with the note.
- **`pluck`** is a waveform and the only one that is a *model* rather than a shape:
  a delay line whose length is the note's period, a lowpass in the loop, a burst of
  noise to start it. Karplus-Strong, and it is why a plucked note sounds like a
  string rather than a filtered saw — its partials die at different rates and sit a
  few cents off the harmonic series, which a single-cycle table cannot be. The
  loop's loss is levied once per *pass* rather than once per sample, so the decay
  is the same number of seconds at every pitch; the delay is the period less the
  damping filter's own group delay, which is what puts the string in tune instead
  of a little flat. The whole delay line is preallocated with the voice pool and
  only a `pluck` voice ever touches it.

**What is still approximated.** `allocate()` hands each note to exactly one
channel, so two different treatments of the *same* note — a percussive pluck
under a sustained pad — have nowhere to live; that would need layer mode, which
carries a real voice-pool cost. Noise is either the selected waveform or a level
mixed alongside a tonal one — enough for hats, snare bodies, breath and wind, but
not a separate envelope-contoured source. And the wavetables are up to sixteen
partials with no band-limiting beyond that, so they are clean across the tool's
own key range but will alias if a voice also transposes up near the top of the
keyboard; they alias less badly there than the naive saw always does.
`REFERENCE.md` §8 has the detail.

## Runtime files

| File          | Notes                                                                  |
| ------------- | ---------------------------------------------------------------------- |
| `ensembles.toml` | **Tracked.** The forty-two shipped ensembles. Compiled in with `include_str!`, so editing it is how the palette is changed. |
| `ensembles.user.toml` | **Gitignored**, created empty when missing. What `[Save As...]` writes; layered over the defaults by name at start-up. |
| `instruments.toml` | **Tracked.** The shipped instrument library — 155 voices. Also compiled in with `include_str!`. |
| `instruments.user.toml` | **Gitignored**, created empty when missing. What saving an instrument writes; layered over the defaults by name. |
| `eq_presets.toml` | **Tracked.** The shipped EQ curve library (25 curves). Compiled in with `include_str!`, like the ensembles and instruments. |
| `eq_presets.user.toml` | **Gitignored**, created empty when missing. What `[Save Curve As...]` writes; layered over the defaults by name. |
| `fx_presets.toml` | **Tracked.** The shipped effect preset library (72 presets, one or two per kind and variant). Compiled in with `include_str!`, like the others. |
| `fx_presets.user.toml` | **Gitignored**, created empty when missing. What the FX panel's `[Save Effect As...]` writes; layered over the defaults by name. |
| `rhythms.toml` | **Tracked.** The shipped pattern palette (23 patterns). Never written. A test keeps it equal to the built-ins; rewrite it with `cargo test regenerate_the_tracked_patterns -- --ignored`. |
| `rhythms.user.toml` | **Gitignored**, created empty when missing. What `[Save Pattern As...]` writes; layered over the palette by name. A chord's own rhythm is *not* here — it lives in the session document, so export to keep it. |
| `progressions/` | Where MIDI exports are written and imported from, created on first launch. **Gitignored** — these are session data, not source. |
| `settings.toml` | **Gitignored.** The key, tempo and master volume of the last run, written once they settle and read back at start-up. Not created until one of them changes; a missing file simply means the defaults. |
| `debug.log`    | **Truncated and rewritten on every launch.** Every input event, a 60 Hz output-level tap, and a once-a-second audio timing line (`[TIME]`). Grows quickly; should not be committed — see below. |
| `REFERENCE.md` | The feature and keyboard reference, checked against the code by a test. |

## Known issues

- **`bug01` in `bugs.txt` is not a layout problem.** The `f`/`h`/`k` gesture
  resolves correctly to Csus4 under Programmer Dvorak, so the cause is still
  open. Unverified leading candidate: the held `PositionSet` not clearing when a
  key release is missed, which would break hand-played chords while
  register-locked playing keeps working.
- **Full-screen redraw, and a tall layout.** Every frame clears the terminal
  and reprints everything, which can flicker on slow terminals; and the focused
  Sinko panel needs 35 rows, the Synth and EQ panels 34 and the Spectrum 33,
  mostly because the panels around them are all still there. The FX panel is 25.
  The default view is 15; on a short terminal the panels go from the bottom up,
  since the synth slot is drawn last — and the ensemble list is as long as the
  library, so it is the one view that scrolls off the bottom of any terminal
  rather than being clipped.
- **Clear-all is two presses.** `Cmd+A` then delete. The model has one delete —
  a range — so there is nothing a dedicated key could do that this does not.
- **The audition's release is a constant, not a setting.** `preview fade`
  turned out to be a dead control — offered in the Synth panel, stored in
  the shipped palette, read by nothing — so it was removed rather than wired up. If a
  settable audition tail is wanted, that is where it should go, and the 500 ms in
  `AUDITION_RELEASE` is what it would replace.
- **Progressions are not persisted.** The key, tempo and master volume now are,
  and the shipped ensembles always were; the progression is not — and since a
  chord *owns* its rhythm rather than naming one in the palette, that includes the
  rhythm edits. Keep a session by exporting it, or push a rhythm you want to keep
  into `rhythms.user.toml` with `[Save Pattern As...]`.

## History

The first commit did not build. Since then, the changes worth remembering:

- Progression **delete, copy and paste** gained hotkeys and undo; the code
  existed but was unreachable.
- `ProgressionEntry.registers` became **read** (it drives `g`-to-recall) and
  captures the *resolved* gesture rather than the latch state, which was why it
  had been unusable.
- Below-home-row keys stopped leaking into the held `PositionSet`, so
  `keyboard.rs` matched its own documentation and `g` stopped breaking a chord it
  was held alongside.
- The palette grew from 7 patterns to 23, the triplet grids (12, 24) arrived, and
  gaps in the old set were filled.
- Patterns gained per-cell lengths and accents, and an adjustable swing.
- The loop lost its appended live bar; auditioning replaced it.

## See also

- [REFERENCE.md](REFERENCE.md) — every feature and key, checked against the code.
- [TODO.md](TODO.md) — the backlog. Some of it predates the current state; where
  the two disagree, this file and `REFERENCE.md` are right.
- `bugs.txt` — scratch notes on open defects.

[cpal]: https://github.com/RustAudio/cpal
[crossterm]: https://github.com/crossterm-rs/crossterm
